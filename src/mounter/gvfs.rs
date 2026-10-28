use crate::ui::widget;
use gio::glib;
use gio::prelude::*;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use super::{
    Answer, DirInfo, IconRef, Mounter, MounterAuth, MounterItem, MounterItems, MounterMessage,
    NetworkEntry, NetworkListing, Outcome,
};
use crate::config::IconSizes;
use crate::err_str;
use crate::mime_app::Mime;

/// What `Cmd::DirInfo` answers: content type, display name and local path
type DirInfoResult = Result<(String, String, Option<PathBuf>), glib::Error>;

const TARGET_URI_ATTRIBUTE: &str = "standard::target-uri";

// Attributes requested when listing a network directory.
const SCAN_ATTRIBUTES: &str = "standard::name,\
standard::display-name,\
standard::type,\
standard::size,\
standard::icon,\
standard::is-hidden,\
standard::content-type,\
time::modified";

/// Entries fetched per round trip while listing a directory.
///
/// Each batch is one request to the server, so a larger batch means fewer
/// round trips; between batches the listing yields, so a smaller one means the
/// GLib thread comes back sooner to whatever else is waiting on it.
const SCAN_BATCH: i32 = 128;

/// Network requests allowed in flight at once on the GLib thread.
const GVFS_CONCURRENCY: usize = 4;

/// Follow `uri` to what it actually points at.
///
/// Asynchronous because it is a query like any other: for a network URI it
/// goes to the server. Doing it synchronously held the GLib thread before any
/// of the asynchronous work below even began, which made the rest of the
/// asynchrony worth nothing.
async fn resolve_uri(uri: &str) -> (String, gio::File) {
    let file = gio::File::for_uri(uri);
    // Resolve the target-uri if it exists
    if let Ok(file_info) = file
        .query_info_future(
            TARGET_URI_ATTRIBUTE,
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
        && let Some(resolved_uri) = file_info.attribute_as_string(TARGET_URI_ATTRIBUTE)
    {
        let resolved_uri = String::from(resolved_uri);
        let file = gio::File::for_uri(&resolved_uri);
        return (resolved_uri, file);
    }

    (uri.to_string(), file)
}

/// `icon` as theme names and an image file, for the app to look up.
fn icon_ref(icon: &gio::Icon) -> IconRef {
    IconRef {
        names: icon
            .downcast_ref::<gio::ThemedIcon>()
            .map(|themed| themed.names().iter().map(|name| name.to_string()).collect())
            .unwrap_or_default(),
        file: icon
            .downcast_ref::<gio::FileIcon>()
            .and_then(|file_icon| file_icon.file().path()),
    }
}

fn gio_icon_to_path(icon: &gio::Icon, size: u16) -> Option<PathBuf> {
    if let Some(themed_icon) = icon.downcast_ref::<gio::ThemedIcon>() {
        for name in themed_icon.names() {
            let named = widget::icon::from_name(name.as_str()).size(size);
            if let Some(path) = named.path() {
                return Some(path);
            }
        }
    }
    // An icon given as an image file, e.g. a volume's custom icon
    if let Some(file_icon) = icon.downcast_ref::<gio::FileIcon>() {
        return file_icon.file().path();
    }
    None
}

/// Whether the filesystem behind `root` is remote, asked asynchronously.
///
/// A mount whose server has gone away answers this slowly or not at all, and
/// everything on the GLib thread waits behind a synchronous answer. Defaults
/// to remote, as the synchronous version did, so a failed query skips
/// per-entry work rather than retrying it.
async fn is_remote(root: &gio::File) -> bool {
    root.query_filesystem_info_future(
        gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE,
        glib::Priority::DEFAULT,
    )
    .await
    .ok()
    .map(|info| info.boolean(gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE))
    .unwrap_or(true)
}

/// List the mounts and volumes GVFS knows about.
///
/// Asynchronous because each mount has to be asked whether it is remote.
/// Spawning this on the GLib context is not enough on its own: a task there
/// runs on that same thread, so a synchronous query inside it blocks
/// everything else just as surely as one in the command loop would.
async fn items(monitor: &gio::VolumeMonitor, sizes: IconSizes) -> MounterItems {
    let mut items: MounterItems = MounterItems::new();
    for mount in monitor.mounts() {
        // Hide shadowed mounts
        if mount.is_shadowed() {
            continue;
        }
        let root = MountExt::root(&mount);
        let is_remote = is_remote(&root).await;

        let uri: String = root.uri().into();
        items.push(MounterItem::Gvfs(Item {
            id: uri.clone(),
            uri,
            kind: ItemKind::Mount,
            name: mount.name().into(),
            is_mounted: true,
            is_remote,
            icon_opt: gio_icon_to_path(&MountExt::icon(&mount), sizes.grid()),
            icon_symbolic_opt: gio_icon_to_path(&MountExt::symbolic_icon(&mount), 16),
            path_opt: root.path(),
        }));
    }
    items.extend(
        (monitor.volumes().into_iter())
            // Volumes with mounts are already listed by mount
            .filter(|volume| volume.get_mount().is_none())
            .map(|volume| {
                let uri = VolumeExt::activation_root(&volume)
                    .map(|f| f.uri().into())
                    .unwrap_or_default();
                MounterItem::Gvfs(Item {
                    uri,
                    kind: ItemKind::Volume,
                    id: volume_id(&volume),
                    name: volume.name().into(),
                    is_mounted: false,
                    is_remote: false,
                    icon_opt: gio_icon_to_path(&VolumeExt::icon(&volume), sizes.grid()),
                    icon_symbolic_opt: gio_icon_to_path(&VolumeExt::symbolic_icon(&volume), 16),
                    path_opt: None,
                })
            }),
    );
    items
}

/// List `uri`, using GIO's asynchronous calls throughout.
///
/// Every query here can go to a server. Done synchronously they hold the GLib
/// thread for as long as the server takes, and that thread is what dispatches
/// every other network command and mount callback: one slow listing used to
/// stop all of them. Awaited, the thread stays free to run the rest.
async fn network_scan(uri: &str) -> Result<NetworkListing, String> {
    let all_dirs = uri.starts_with("network:///");
    let (_, file) = resolve_uri(uri).await;

    // Read .hidden file if present. This one is ordinary local I/O rather than
    // GIO, and only for a URI that maps to a path, so it goes to a worker.
    let hidden_files: Box<[String]> = match file.path() {
        Some(path) => gio::spawn_blocking(move || {
            let hidden_file_path = path.join(".hidden");
            if hidden_file_path.is_file() {
                crate::tab::parse_hidden_file(&hidden_file_path)
            } else {
                Box::from([])
            }
        })
        .await
        .unwrap_or_else(|err| {
            log::warn!("failed to read .hidden: {err:?}");
            Box::from([])
        }),
        None => Box::from([]),
    };

    // `filesystem::remote` belongs to the filesystem namespace, which `enumerate_children`
    // never fills in, so it has to be queried once for the directory being listed.
    let remote = match file
        .query_filesystem_info_future(
            gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE,
            glib::Priority::DEFAULT,
        )
        .await
    {
        Ok(info) => info.boolean(gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE),
        Err(err) => {
            log::warn!("failed to get GIO filesystem info for {uri}: {err}");
            // Assume remote, so per-entry work is skipped rather than retried
            true
        }
    };

    let enumerator = file
        .enumerate_children_future(
            SCAN_ATTRIBUTES,
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
        .map_err(err_str)?;

    let mut items = Vec::new();
    // Fetched a batch at a time. Each batch is one round trip, and between
    // batches this task yields, so a directory with thousands of entries does
    // not hold the thread for the whole listing.
    loop {
        let batch = enumerator
            .next_files_future(SCAN_BATCH, glib::Priority::DEFAULT)
            .await
            .map_err(err_str)?;
        if batch.is_empty() {
            break;
        }
        for info in batch {
            let name = info.name().to_string_lossy().into_owned();
            let display_name = String::from(info.display_name());
            let child = file.child(info.name());
            let is_dir = all_dirs || matches!(info.file_type(), gio::FileType::Directory);
            // gio reports the content type per entry; a folder has none
            let mime = (!is_dir)
                .then(|| info.content_type())
                .flatten()
                .and_then(|t| Mime::from_media_type(&t).ok());
            let hidden = name.starts_with('.')
                || info.boolean(gio::FILE_ATTRIBUTE_STANDARD_IS_HIDDEN)
                || hidden_files.contains(&name);
            items.push(NetworkEntry {
                display_name,
                uri: String::from(child.uri()),
                path: child.path(),
                is_dir,
                size: (!is_dir).then_some(info.size() as u64),
                mtime: info.attribute_uint64(gio::FILE_ATTRIBUTE_TIME_MODIFIED),
                mime,
                hidden,
                icon: info.icon().as_ref().map(icon_ref).unwrap_or_default(),
                name,
            });
        }
    }
    Ok(NetworkListing {
        entries: items,
        all_dirs,
        remote,
    })
}

async fn dir_info(uri: &str) -> Result<(String, String, Option<PathBuf>), glib::Error> {
    let (resolved_uri, file) = resolve_uri(uri).await;
    let info = file
        .query_info_future(
            gio::FILE_ATTRIBUTE_STANDARD_DISPLAY_NAME,
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await?;

    Ok((resolved_uri, info.display_name().into(), file.path()))
}

fn mount_op(
    uri: String,
    event_tx: std::sync::Weak<mpsc::UnboundedSender<Event>>,
) -> gio::MountOperation {
    let mount_op = gio::MountOperation::new();
    mount_op.connect_ask_password(
        move |mount_op, message, default_user, default_domain, flags| {
            let auth = MounterAuth {
                message: message.to_string(),
                username_opt: flags
                    .contains(gio::AskPasswordFlags::NEED_USERNAME)
                    .then(|| default_user.to_string()),
                domain_opt: flags
                    .contains(gio::AskPasswordFlags::NEED_DOMAIN)
                    .then(|| default_domain.to_string()),
                password_opt: flags
                    .contains(gio::AskPasswordFlags::NEED_PASSWORD)
                    .then(String::new),
                remember_opt: flags
                    .contains(gio::AskPasswordFlags::SAVING_SUPPORTED)
                    .then_some(false),
                anonymous_opt: flags
                    .contains(gio::AskPasswordFlags::ANONYMOUS_SUPPORTED)
                    .then_some(false),
            };
            let (auth_tx, mut auth_rx) = mpsc::channel(1);
            if let Some(event_tx) = event_tx.upgrade() {
                let _ = event_tx.send(Event::NetworkAuth(uri.clone(), auth, auth_tx));
            }
            // Answer later rather than waiting here. This callback runs on the
            // thread that serves every gvfs command, so blocking it until the
            // user answers the dialog would stall unmounts, rescans and other
            // mounts for as long as the dialog is open. GMountOperation allows
            // the handler to return and reply afterwards.
            let mount_op = mount_op.clone();
            glib::MainContext::ref_thread_default().spawn_local(async move {
                match auth_rx.recv().await {
                    Some(auth) => {
                        if auth.anonymous_opt == Some(true) {
                            mount_op.set_anonymous(true);
                        } else {
                            mount_op.set_username(auth.username_opt.as_deref());
                            mount_op.set_domain(auth.domain_opt.as_deref());
                            mount_op.set_password(auth.password_opt.as_deref());
                            if auth.remember_opt == Some(true) {
                                mount_op.set_password_save(gio::PasswordSave::Permanently);
                            }
                        }
                        mount_op.reply(gio::MountOperationResult::Handled);
                    }
                    // The dialog was dismissed, or its sender was dropped
                    // because the mount was torn down: abort, do not hang
                    None => mount_op.reply(gio::MountOperationResult::Aborted),
                }
            });
        },
    );
    mount_op
}

enum Cmd {
    Rescan,
    Mount(MounterItem, tokio::sync::oneshot::Sender<Outcome>),
    NetworkDrive(String, tokio::sync::oneshot::Sender<Outcome>),
    NetworkScan(String, mpsc::Sender<Result<NetworkListing, String>>),
    DirInfo(String, mpsc::Sender<DirInfoResult>),
    Unmount(MounterItem, tokio::sync::oneshot::Sender<Outcome>),
}

enum Event {
    Changed,
    Items(MounterItems),
    MountResult(MounterItem, Result<bool, String>),
    NetworkAuth(String, MounterAuth, mpsc::Sender<MounterAuth>),
    NetworkResult(String, Result<bool, String>),
}

#[derive(Clone, Debug)]
enum ItemKind {
    Mount,
    Volume,
}

/// A stable identity for a volume across rescans: its UUID, else its device
/// node, else its activation root, else its name.
fn volume_id(volume: &gio::Volume) -> String {
    VolumeExt::uuid(volume)
        .or_else(|| VolumeExt::identifier(volume, gio::VOLUME_IDENTIFIER_KIND_UNIX_DEVICE))
        .map(String::from)
        .or_else(|| VolumeExt::activation_root(volume).map(|f| f.uri().into()))
        .unwrap_or_else(|| VolumeExt::name(volume).into())
}

#[derive(Clone, Debug)]
pub struct Item {
    uri: String,
    kind: ItemKind,
    /// Stable identity used to find the gio object again when acting on the
    /// item: the root URI of a mount, or [`volume_id`] of a volume
    id: String,
    name: String,
    is_mounted: bool,
    is_remote: bool,
    icon_opt: Option<PathBuf>,
    icon_symbolic_opt: Option<PathBuf>,
    path_opt: Option<PathBuf>,
}

impl Item {
    pub fn name(&self) -> String {
        self.name.clone()
    }

    pub const fn is_mounted(&self) -> bool {
        self.is_mounted
    }

    pub const fn is_remote(&self) -> bool {
        self.is_remote
    }

    pub fn uri(&self) -> String {
        self.uri.clone()
    }

    /// A stable identity: the volume's id for a volume, the mount root's URI
    /// for a mount.
    pub fn id(&self) -> String {
        self.id.clone()
    }

    /// Path of this item's icon.
    ///
    /// Split out of `icon()` so `src/dialog.rs` can build its handle from the
    /// same path.
    pub fn icon_path(&self, symbolic: bool) -> Option<PathBuf> {
        if symbolic {
            self.icon_symbolic_opt.clone()
        } else {
            self.icon_opt.clone()
        }
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.path_opt.clone()
    }
}

pub struct Gvfs {
    command_tx: mpsc::UnboundedSender<Cmd>,
    event_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Event>>>,
}

impl Gvfs {
    pub fn new() -> Self {
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let event_tx = Arc::new(event_tx);
        std::thread::spawn(move || {
            let main_loop = glib::MainLoop::new(None, false);
            main_loop.context().spawn_local(async move {
                let event_tx = Arc::downgrade(&event_tx);
                // Network requests allowed to be in flight at once. More than
                // one, so a slow listing does not hold up an unrelated tab;
                // bounded, because each one holds a connection and a mount
                // prompt, and a burst of tabs opening at once should not open a
                // burst of connections.
                let requests = Arc::new(tokio::sync::Semaphore::new(GVFS_CONCURRENCY));
                // Which mount list is the newest asked for. Everything here
                // runs on this one thread, so a plain cell is enough.
                let newest_rescan = Rc::new(std::cell::Cell::new(0u64));
                let monitor = gio::VolumeMonitor::get();
                {
                    let event_tx = event_tx.clone();
                    monitor.connect_mount_changed(move |_monitor, mount| {
                        log::info!("mount changed {}", MountExt::name(mount));
                        if let Some(event_tx) = event_tx.upgrade() {
                            let _ = event_tx.send(Event::Changed);
                        }
                    });
                }
                {
                    let event_tx = event_tx.clone();
                    monitor.connect_mount_added(move |_monitor, mount| {
                        log::info!("mount added {}", MountExt::name(mount));
                        if let Some(event_tx) = event_tx.upgrade() {
                            let _ = event_tx.send(Event::Changed);
                        }
                    });
                }
                {
                    let event_tx = event_tx.clone();
                    monitor.connect_mount_removed(move |_monitor, mount| {
                        log::info!("mount removed {}", MountExt::name(mount));
                        if let Some(event_tx) = event_tx.upgrade() {
                            let _ = event_tx.send(Event::Changed);
                        }
                    });
                }

                {
                    let event_tx = event_tx.clone();
                    monitor.connect_volume_changed(move |_monitor, volume| {
                        log::info!("volume changed {}", VolumeExt::name(volume));
                        if let Some(event_tx) = event_tx.upgrade() {
                            let _ = event_tx.send(Event::Changed);
                        }
                    });
                }
                {
                    let event_tx = event_tx.clone();
                    monitor.connect_volume_added(move |_monitor, volume| {
                        log::info!("volume added {}", VolumeExt::name(volume));
                        if let Some(event_tx) = event_tx.upgrade() {
                            let _ = event_tx.send(Event::Changed);
                        }
                    });
                }
                {
                    let event_tx = event_tx.clone();
                    monitor.connect_volume_removed(move |_monitor, volume| {
                        log::info!("volume removed {}", VolumeExt::name(volume));
                        if let Some(event_tx) = event_tx.upgrade() {
                            let _ = event_tx.send(Event::Changed);
                        }
                    });
                }

                while let Some(command) = command_rx.recv().await {
                    match command {
                        Cmd::Rescan => {
                            // Spawned like the rest, and asynchronous inside:
                            // listing the mounts asks each one whether it is
                            // remote, and a task on this context runs on this
                            // thread, so a synchronous answer in there would
                            // stall everything anyway.
                            let event_tx = event_tx.clone();
                            let requests = Arc::clone(&requests);
                            let newest = Rc::clone(&newest_rescan);
                            let rescan = newest.get() + 1;
                            newest.set(rescan);
                            glib::MainContext::ref_thread_default().spawn_local(async move {
                                let Ok(_permit) = requests.acquire_owned().await else {
                                    return;
                                };
                                let monitor = gio::VolumeMonitor::get();
                                let listed = items(&monitor, IconSizes::default()).await;
                                // A scan can hold a mount, wait on it, and come
                                // back after a later scan has already reported
                                // that mount gone. Publishing this now would put
                                // it back in the sidebar.
                                if newest.get() != rescan {
                                    log::debug!("discarding mount list {rescan}: superseded");
                                    return;
                                }
                                let Some(event_tx) = event_tx.upgrade() else {
                                    return;
                                };
                                let _ = event_tx.send(Event::Items(listed));
                            });
                        }
                        Cmd::Mount(mounter_item, complete_tx) => {
                            let MounterItem::Gvfs(ref item) = mounter_item else {
                                log::warn!("mount: item is not a gvfs item");
                                _ = complete_tx.send(Outcome::Failed);
                                continue;
                            };
                            let ItemKind::Volume = item.kind else {
                                log::warn!("mount: {:?} is not a gvfs volume", item.name);
                                _ = complete_tx.send(Outcome::Failed);
                                continue;
                            };
                            let Some(volume) = monitor
                                .volumes()
                                .into_iter()
                                .find(|volume| volume_id(volume) == item.id)
                            else {
                                log::warn!("volume {:?} is no longer present", item.name);
                                _ = complete_tx.send(Outcome::Failed);
                                continue;
                            };
                            {
                                let name = VolumeExt::name(&volume);
                                log::info!("mount {name}");
                                let mount_op = mount_op(name.to_string(), event_tx.clone());
                                let event_tx = event_tx.clone();
                                let mounter_item = mounter_item.clone();
                                let volume_for_callback = volume.clone();
                                VolumeExt::mount(
                                    &volume,
                                    gio::MountMountFlags::NONE,
                                    Some(&mount_op),
                                    gio::Cancellable::NONE,
                                    move |res| {
                                        log::info!("mount {name}: result {res:?}");
                                        // The rest runs in a task rather than
                                        // here: asking the freshly mounted
                                        // filesystem whether it is remote is a
                                        // request like any other, and this
                                        // callback runs on the GLib thread,
                                        // where it cannot be awaited.
                                        glib::MainContext::ref_thread_default().spawn_local(
                                            async move {
                                                // Update the mounter_item with mount information after successful mount
                                                let mut updated_item = mounter_item.clone();
                                                if res.is_ok()
                                                    && let MounterItem::Gvfs(ref mut item) =
                                                        updated_item
                                                    && let Some(mount) =
                                                        volume_for_callback.get_mount()
                                                {
                                                    let root = MountExt::root(&mount);
                                                    item.path_opt = root.path();
                                                    item.is_mounted = true;
                                                    // Query if remote
                                                    item.is_remote = is_remote(&root).await;
                                                }
                                                _ = complete_tx.send(outcome(&res));
                                                let Some(event_tx) = event_tx.upgrade() else {
                                                    return;
                                                };
                                                let _ = event_tx.send(Event::MountResult(
                                                    updated_item,
                                                    match res {
                                                        Ok(()) => Ok(true),
                                                        Err(err) => {
                                                            match err.kind::<gio::IOErrorEnum>() {
                                                                Some(
                                                                    gio::IOErrorEnum::FailedHandled,
                                                                ) => Ok(false),
                                                                _ => Err(format!("{err}")),
                                                            }
                                                        }
                                                    },
                                                ));
                                            },
                                        );
                                    },
                                );
                            }
                        }
                        Cmd::NetworkDrive(uri, result_tx) => {
                            let file = gio::File::for_uri(&uri);
                            let mount_op = mount_op(uri.clone(), event_tx.clone());
                            let event_tx = event_tx.clone();
                            file.mount_enclosing_volume(
                                gio::MountMountFlags::NONE,
                                Some(&mount_op),
                                gio::Cancellable::NONE,
                                move |res| {
                                    log::info!("network drive {uri}: result {res:?}");
                                    _ = result_tx.send(outcome(&res));
                                    let Some(event_tx) = event_tx.upgrade() else {
                                        return;
                                    };
                                    let _ = event_tx.send(Event::NetworkResult(
                                        uri,
                                        match res {
                                            Ok(()) => Ok(true),
                                            Err(err) => match err.kind::<gio::IOErrorEnum>() {
                                                Some(gio::IOErrorEnum::FailedHandled) => Ok(false),
                                                _ => Err(format!("{err}")),
                                            },
                                        },
                                    ));
                                },
                            );
                        }
                        Cmd::NetworkScan(uri, items_tx) => {
                            // Spawned rather than awaited here, and nothing is
                            // resolved before the spawn: every step below can
                            // go to the server, and doing any of them in this
                            // loop keeps it from taking the next command until
                            // the server answers -- which is the serialization
                            // the asynchronous calls exist to avoid.
                            let event_tx = event_tx.clone();
                            let requests = Arc::clone(&requests);
                            glib::MainContext::ref_thread_default().spawn_local(async move {
                                let Ok(_permit) = requests.acquire_owned().await else {
                                    return;
                                };

                                let (resolved_uri, file) = resolve_uri(&uri).await;

                                // No asynchronous binding for this one, and it
                                // needs none: it asks the local mount table what
                                // covers this URI, not the server.
                                let needs_mount = resolved_uri != "network:///"
                                    && match file.find_enclosing_mount(gio::Cancellable::NONE) {
                                        Ok(_) => false,
                                        Err(err) => matches!(
                                            err.kind::<gio::IOErrorEnum>(),
                                            Some(gio::IOErrorEnum::NotMounted)
                                        ),
                                    };

                                if needs_mount {
                                    let mount_op = mount_op(resolved_uri.clone(), event_tx.clone());
                                    let res = file
                                        .mount_enclosing_volume_future(
                                            gio::MountMountFlags::empty(),
                                            Some(&mount_op),
                                        )
                                        .await;
                                    log::info!(
                                        "network scan mounted {resolved_uri}: result {res:?}"
                                    );
                                    // FIXME sometimes a uri can be mounted and then not recognized as mounted...
                                    // seems to be related to uri with a path
                                    let scanned = network_scan(&uri).await;
                                    if items_tx.send(scanned).await.is_err() {
                                        log::warn!("nothing is waiting for the scan of {uri}");
                                    }
                                    let Some(event_tx) = event_tx.upgrade() else {
                                        return;
                                    };
                                    let _ = event_tx.send(Event::NetworkResult(
                                        resolved_uri,
                                        match res {
                                            Ok(()) => Ok(true),
                                            Err(err) => match err.kind::<gio::IOErrorEnum>() {
                                                Some(gio::IOErrorEnum::FailedHandled) => Ok(false),
                                                _ => Err(format!("{err}")),
                                            },
                                        },
                                    ));
                                } else {
                                    let scanned = network_scan(&uri).await;
                                    if items_tx.send(scanned).await.is_err() {
                                        log::warn!("nothing is waiting for the scan of {uri}");
                                    }
                                }
                            });
                        }
                        Cmd::DirInfo(uri, result_tx) => {
                            // Spawned for the same reason: naming a directory
                            // is a request to the server, and the command loop
                            // must stay free to dispatch the next one.
                            let requests = Arc::clone(&requests);
                            glib::MainContext::ref_thread_default().spawn_local(async move {
                                let Ok(_permit) = requests.acquire_owned().await else {
                                    return;
                                };
                                if result_tx.send(dir_info(&uri).await).await.is_err() {
                                    log::warn!(
                                        "nothing is waiting for the directory info of {uri}"
                                    );
                                }
                            });
                        }
                        Cmd::Unmount(mounter_item, complete_tx) => {
                            let MounterItem::Gvfs(item) = mounter_item else {
                                log::warn!("unmount: item is not a gvfs item");
                                _ = complete_tx.send(Outcome::Failed);
                                continue;
                            };
                            let ItemKind::Mount = item.kind else {
                                log::warn!("unmount: {:?} is not a gvfs mount", item.name);
                                _ = complete_tx.send(Outcome::Failed);
                                continue;
                            };
                            let Some(mount) = monitor
                                .mounts()
                                .into_iter()
                                .find(|mount| MountExt::root(mount).uri() == item.id)
                            else {
                                log::warn!("mount {:?} is no longer present", item.name);
                                _ = complete_tx.send(Outcome::Failed);
                                continue;
                            };
                            {
                                let name = MountExt::name(&mount);
                                if MountExt::can_eject(&mount) {
                                    log::info!("eject {name}");
                                    MountExt::eject_with_operation(
                                        &mount,
                                        gio::MountUnmountFlags::NONE,
                                        gio::MountOperation::NONE,
                                        gio::Cancellable::NONE,
                                        move |result| {
                                            log::info!("eject {name}: result {result:?}");
                                            _ = complete_tx.send(outcome(&result));
                                        },
                                    );
                                } else {
                                    log::info!("unmount {name}");
                                    MountExt::unmount_with_operation(
                                        &mount,
                                        gio::MountUnmountFlags::NONE,
                                        gio::MountOperation::NONE,
                                        gio::Cancellable::NONE,
                                        move |result| {
                                            log::info!("unmount {name}: result {result:?}");
                                            _ = complete_tx.send(outcome(&result));
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
            });
            main_loop.run();
        });
        Self {
            command_tx,
            event_rx: Arc::new(tokio::sync::Mutex::new(event_rx)),
        }
    }
}

/// Maps a mount/unmount/eject result to an [`Outcome`]: success is `Done`,
/// `FailedHandled` (the error was already shown to, or dismissed by, the
/// user) is `Cancelled`, and anything else is `Failed`.
fn outcome(res: &Result<(), glib::Error>) -> Outcome {
    match res {
        Ok(()) => Outcome::Done,
        Err(err) => match err.kind::<gio::IOErrorEnum>() {
            Some(gio::IOErrorEnum::FailedHandled) => Outcome::Cancelled,
            _ => Outcome::Failed,
        },
    }
}

/// How a mounter command ended. A command whose sender was dropped
/// unanswered counts as failed.
fn done(result: Result<Outcome, oneshot::error::RecvError>) -> Outcome {
    match result {
        Ok(outcome) => outcome,
        Err(err) => {
            log::error!("mounter command dropped unanswered: {err}");
            Outcome::Failed
        }
    }
}

impl Gvfs {
    /// Sends `command` made with where its outcome goes, and answers with
    /// that outcome.
    fn ask(&self, command: impl FnOnce(oneshot::Sender<Outcome>) -> Cmd) -> Answer<Outcome> {
        let (res_tx, res_rx) = oneshot::channel();
        let sent = self.command_tx.send(command(res_tx));
        Box::pin(async move {
            if sent.is_err() {
                log::error!("the gvfs thread is gone");
                return Outcome::Failed;
            }
            done(res_rx.await)
        })
    }
}

impl Mounter for Gvfs {
    fn mount(&self, item: MounterItem) -> Answer<Outcome> {
        self.ask(|res_tx| Cmd::Mount(item, res_tx))
    }

    fn network_drive(&self, uri: String) -> Answer<Outcome> {
        self.ask(|res_tx| Cmd::NetworkDrive(uri, res_tx))
    }

    fn network_scan(&self, uri: &str) -> Answer<Option<Result<NetworkListing, String>>> {
        let (items_tx, mut items_rx) = mpsc::channel(1);
        let _ = self
            .command_tx
            .send(Cmd::NetworkScan(uri.to_string(), items_tx));
        Box::pin(async move { items_rx.recv().await })
    }

    fn dir_info(&self, uri: &str) -> Answer<Option<DirInfo>> {
        let (result_tx, mut result_rx) = mpsc::channel(1);
        let _ = self
            .command_tx
            .send(Cmd::DirInfo(uri.to_string(), result_tx));
        Box::pin(async move { result_rx.recv().await.and_then(Result::ok) })
    }

    fn unmount(&self, item: MounterItem) -> Answer<Outcome> {
        self.ask(|res_tx| Cmd::Unmount(item, res_tx))
    }

    fn rescan(&self) {
        let _ = self.command_tx.send(Cmd::Rescan);
    }

    fn next_event(&self) -> Answer<Option<MounterMessage>> {
        let (events, command_tx) = (Arc::clone(&self.event_rx), self.command_tx.clone());
        Box::pin(async move {
            let mut events = events.lock().await;
            loop {
                let message = match events.recv().await? {
                    // A change asks for the list again; the list is the event
                    Event::Changed => {
                        let _ = command_tx.send(Cmd::Rescan);
                        continue;
                    }
                    Event::Items(items) => MounterMessage::Items(items),
                    Event::MountResult(item, res) => MounterMessage::MountResult(item, res),
                    Event::NetworkAuth(uri, auth, auth_tx) => {
                        MounterMessage::NetworkAuth(uri, auth, auth_tx)
                    }
                    Event::NetworkResult(uri, res) => MounterMessage::NetworkResult(uri, res),
                };
                return Some(message);
            }
        })
    }
}
