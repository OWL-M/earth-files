use crate::ui::iced::advanced::graphics;
use crate::ui::iced::advanced::text::{self, Paragraph};
use crate::ui::iced::alignment::Vertical;
use crate::ui::iced::futures::{self, SinkExt};
use crate::ui::iced::keyboard::Modifiers;
use crate::ui::iced::widget::{rule, stack};
use crate::ui::iced::{
    Alignment, Color, ContentFit, Length, Point, Rectangle, Size, Subscription, padding, stream,
    window,
};
use crate::ui::iced_core::mouse::ScrollDelta;
use crate::ui::theme;
use crate::ui::widget::menu::action::MenuAction;
use crate::ui::widget::menu::key_bind::KeyBind;
use crate::ui::widget::scrollable::{self, AbsoluteOffset, Viewport};
use crate::ui::widget::{self, Id, space};
use crate::ui::{Apply, Element, font};
#[cfg(feature = "desktop")]
use freedesktop_desktop_entry::{DesktopEntry, get_languages_from_env};
use i18n_embed::LanguageLoader;
use icu::datetime::input::DateTime;
use icu::datetime::options::TimePrecision;
use icu::datetime::{DateTimeFormatter, DateTimeFormatterPreferences, fieldsets};
use image::{DynamicImage, ImageReader};
use jiff_icu::ConvertFrom;
use mime_guess::{Mime, mime};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::cell::{Cell, OnceCell};
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::fmt::{self, Display};
use std::fs::{self, File, Metadata};
use std::hash::Hash;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{self, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, RwLock, atomic};
use std::time::{Duration, Instant, SystemTime};
use tempfile::NamedTempFile;
use tokio::sync::mpsc;
use trash::{TrashItem, TrashItemMetadata, TrashItemSize};
use walkdir::WalkDir;

use crate::app::{Action, PreviewItem, PreviewKind};
use crate::config::{
    ContextActionPreset, ICON_SCALE_MAX, ICON_SIZE_GRID, IconSizes, TabConfig, ThumbCfg,
};
use crate::dialog::DialogKind;
use crate::large_image::{
    LargeImageManager, decode_large_image, exceeds_memory_limit, should_use_dedicated_worker,
    should_use_tiling,
};
use crate::localize::{LANGUAGE_SORTER, LOCALE};
use crate::mime_icon::mime_for_path;
use crate::mounter::MOUNTERS;
use crate::operation::{Controller, OperationError};
use crate::thumbnail_cacher::{CachedThumbnail, ThumbnailCacher, ThumbnailSize};
use crate::thumbnailer::thumbnailer;
use crate::trash::{Trash, TrashExt};
use crate::ui::convert::{ToColor, ToRadius};
use crate::ui::convert::{ToPadding, ToPixels};
use crate::ui::theme::{Button, Container, Layer, Rule, Spacing, spacing};
use crate::{FxOrderMap, fl, menu, mime_app, mouse_area};

pub const DOUBLE_CLICK_DURATION: Duration = Duration::from_millis(500);
pub const TYPE_SELECT_TIMEOUT: Duration = Duration::from_millis(1000);
const MAX_SEARCH_LATENCY: Duration = Duration::from_millis(20);
const THUMBNAIL_SIZE: u32 = (ICON_SIZE_GRID as u32) * (ICON_SCALE_MAX as u32);
/// Maximum bytes of text to pass to the editor for preview; caps shaping work to avoid blocking.
/// Files larger than this get a truncated preview (first N bytes only).
const TEXT_PREVIEW_MAX_BYTES: usize = 256 * 1024; // 256 KiB
/// Maximum file size (bytes) to attempt text preview; files larger than this are skipped entirely.
const TEXT_PREVIEW_MAX_FILE_BYTES: u64 = 8 * 1000 * 1000; // 8 MiB

// Thumbnail generation semaphore - limits parallel thumbnail workers
// Uses 4 workers for balanced throughput and memory usage
pub static THUMB_SEMAPHORE: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::const_new(num_cpus::get().min(4)));

pub(crate) static SORT_OPTION_FALLBACK: LazyLock<FxHashMap<String, (HeadingOptions, bool)>> =
    LazyLock::new(|| {
        FxHashMap::from_iter(dirs::download_dir().into_iter().map(|dir| {
            (
                Location::Path(dir).normalize().to_string(),
                (HeadingOptions::Modified, false),
            )
        }))
    });

static MODE_NAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
    vec![
        // Mode 0
        fl!("none"),
        // Mode 1
        fl!("execute-only"),
        // Mode 2
        fl!("write-only"),
        // Mode 3
        fl!("write-execute"),
        // Mode 4
        fl!("read-only"),
        // Mode 5
        fl!("read-execute"),
        // Mode 6
        fl!("read-write"),
        // Mode 7
        fl!("read-write-execute"),
    ]
});

/// Names for user and group ids, remembered for the life of the process.
///
/// Resolving an id can reach a directory service over the network, and these
/// names are read while building the details pane, which happens on every
/// frame and once per selected item. The set of distinct ids in a directory
/// is small, so the map stays small; names rarely change within a session.
static USER_NAMES: LazyLock<RwLock<FxHashMap<u32, String>>> = LazyLock::new(Default::default);
static GROUP_NAMES: LazyLock<RwLock<FxHashMap<u32, String>>> = LazyLock::new(Default::default);

fn cached_name(
    cache: &LazyLock<RwLock<FxHashMap<u32, String>>>,
    id: u32,
    lookup: impl FnOnce(u32) -> String,
) -> String {
    if let Ok(names) = cache.read()
        && let Some(name) = names.get(&id)
    {
        return name.clone();
    }
    let name = lookup(id);
    if let Ok(mut names) = cache.write() {
        names.insert(id, name.clone());
    }
    name
}

pub fn user_name(uid: u32) -> String {
    cached_name(&USER_NAMES, uid, |uid| {
        uzers::get_user_by_uid(uid)
            .and_then(|user| user.name().to_str().map(ToOwned::to_owned))
            .unwrap_or_default()
    })
}

pub fn group_name(gid: u32) -> String {
    cached_name(&GROUP_NAMES, gid, |gid| {
        uzers::get_group_by_gid(gid)
            .and_then(|group| group.name().to_str().map(ToOwned::to_owned))
            .unwrap_or_default()
    })
}

static SPECIAL_DIRS: LazyLock<FxHashMap<PathBuf, &'static str>> = LazyLock::new(|| {
    let mut special_dirs = FxHashMap::default();
    if let Some(dir) = dirs::document_dir() {
        special_dirs.insert(dir, "folder-documents");
    }
    if let Some(dir) = dirs::download_dir() {
        special_dirs.insert(dir, "folder-download");
    }
    if let Some(dir) = dirs::audio_dir() {
        special_dirs.insert(dir, "folder-music");
    }
    if let Some(dir) = dirs::picture_dir() {
        special_dirs.insert(dir, "folder-pictures");
    }
    if let Some(dir) = dirs::public_dir() {
        special_dirs.insert(dir, "folder-publicshare");
    }
    if let Some(dir) = dirs::template_dir() {
        special_dirs.insert(dir, "folder-templates");
    }
    if let Some(dir) = dirs::video_dir() {
        special_dirs.insert(dir, "folder-videos");
    }
    if let Some(dir) = dirs::desktop_dir() {
        special_dirs.insert(dir, "user-desktop");
    }
    if let Some(dir) = dirs::home_dir() {
        special_dirs.insert(dir, "user-home");
    }
    special_dirs
});

fn button_appearance(
    theme: &theme::Theme,
    selected: bool,
    highlighted: bool,
    cut: bool,
    focused: bool,
    accent: bool,
    condensed_radius: bool,
) -> widget::button::Style {
    let cosmic = theme.cosmic();
    let mut appearance = widget::button::Style::new();
    if selected {
        if accent {
            appearance.background = Some(cosmic.accent_color().to_color().into());
            appearance.icon_color = Some(cosmic.on_accent_color().to_color());
            if cut {
                appearance.text_color = Some(cosmic.accent.on_disabled.to_color());
            } else {
                appearance.text_color = Some(cosmic.on_accent_color().to_color());
            }
        } else {
            appearance.background = Some(cosmic.bg_component_color().to_color().into());
        }
    } else if highlighted {
        if accent {
            appearance.background = Some(cosmic.bg_component_color().to_color().into());
            appearance.icon_color = Some(cosmic.on_bg_component_color().to_color());
            appearance.text_color = Some(cosmic.on_bg_component_color().to_color());
            if cut {
                appearance.text_color = Some(
                    cosmic
                        .background(theme.transparent)
                        .component
                        .on_disabled
                        .to_color(),
                );
            } else {
                appearance.text_color = Some(cosmic.on_bg_component_color().to_color());
            }
        } else {
            appearance.background = Some(cosmic.bg_component_color().to_color().into());
        }
    } else if cut {
        appearance.text_color = Some(
            cosmic
                .background(theme.transparent)
                .component
                .on_disabled
                .to_color(),
        );
    }
    if focused && accent {
        appearance.outline_width = 1.0;
        appearance.outline_color = cosmic.accent_color().to_color();
        appearance.border_width = 2.0;
        appearance.border_color = Color::TRANSPARENT;
    }
    if condensed_radius {
        appearance.border_radius = cosmic.radius_xs().to_radius();
    } else {
        appearance.border_radius = cosmic.radius_s().to_radius();
    }
    appearance
}

fn button_style(
    selected: bool,
    highlighted: bool,
    cut: bool,
    accent: bool,
    condensed_radius: bool,
) -> Button {
    Button::Custom {
        active: Box::new(move |focused, theme| {
            button_appearance(
                theme,
                selected,
                highlighted,
                cut,
                focused,
                accent,
                condensed_radius,
            )
        }),
        disabled: Box::new(move |theme| {
            button_appearance(
                theme,
                selected,
                highlighted,
                cut,
                false,
                accent,
                condensed_radius,
            )
        }),
        hovered: Box::new(move |focused, theme| {
            button_appearance(
                theme,
                selected,
                highlighted,
                cut,
                focused,
                accent,
                condensed_radius,
            )
        }),
        pressed: Box::new(move |focused, theme| {
            button_appearance(
                theme,
                selected,
                highlighted,
                cut,
                focused,
                accent,
                condensed_radius,
            )
        }),
    }
}

/// XDG icon name for a directory, without the `-symbolic` suffix.
///
/// Split out of `folder_icon`/`folder_icon_symbolic` so `src/dialog.rs` can
/// build its icon from the same name without duplicating the `SPECIAL_DIRS`
/// lookup.
pub fn folder_icon_name(path: &PathBuf) -> &'static str {
    SPECIAL_DIRS.get(path).map_or("folder", |x| *x)
}

/// Folder icons, kept by name and size.
///
/// Resolving one searches the icon theme on disk and costs tens of
/// milliseconds. Without this a directory of a hundred folders pays that for
/// every folder at every size, although they nearly all share the one name.
/// Folder icon name, size, and whether the symbolic variant is wanted
type FolderIconKey = (&'static str, u16, bool);

static FOLDER_ICONS: LazyLock<Mutex<FxHashMap<FolderIconKey, widget::icon::Handle>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));

/// The folder icon for `name`, only if it is already resolved
fn try_folder_icon(name: &'static str, icon_size: u16) -> Option<widget::icon::Handle> {
    let cache = FOLDER_ICONS.lock().unwrap_or_else(|err| err.into_inner());
    cache.get(&(name, icon_size, false)).cloned()
}

fn cached_folder_icon(name: &'static str, icon_size: u16, symbolic: bool) -> widget::icon::Handle {
    let mut cache = FOLDER_ICONS.lock().unwrap_or_else(|err| err.into_inner());
    cache
        .entry((name, icon_size, symbolic))
        .or_insert_with(|| {
            if symbolic {
                widget::icon::from_name(format!("{name}-symbolic"))
                    .size(icon_size)
                    .handle()
            } else {
                widget::icon::from_name(name)
                    .prefer_svg(true)
                    .size(icon_size)
                    .handle()
            }
        })
        .clone()
}

pub fn folder_icon(path: &PathBuf, icon_size: u16) -> widget::icon::Handle {
    cached_folder_icon(folder_icon_name(path), icon_size, false)
}

/// The folder icon if it is cached, else the generic placeholder. Used while
/// scanning, which must not wait on the icon theme.
fn folder_icon_or_placeholder(path: &PathBuf, icon_size: u16) -> widget::icon::Handle {
    try_folder_icon(folder_icon_name(path), icon_size)
        .unwrap_or_else(|| crate::mime_icon::placeholder_icon(icon_size))
}

pub fn folder_icon_symbolic(path: &PathBuf, icon_size: u16) -> widget::icon::Handle {
    cached_folder_icon(folder_icon_name(path), icon_size, true)
}

fn has_trailing_sep(path: &Path) -> bool {
    path.as_os_str()
        .as_encoded_bytes()
        .last()
        .copied()
        .is_some_and(|b| path::is_separator(b as char))
}

fn tab_complete(path: &Path) -> Result<Vec<(String, PathBuf)>, Box<dyn Error>> {
    let parent = if has_trailing_sep(path) && path.is_dir() {
        // Show completions inside existing child directory instead of parent
        path
    } else {
        path.parent()
            .ok_or_else(|| format!("path has no parent {}", path.display()))?
    };

    let child_os = path.strip_prefix(parent)?;
    let child = child_os
        .to_str()
        .ok_or_else(|| format!("invalid UTF-8 {}", child_os.display()))?;

    let pattern = format!("^{}", regex::escape(child));
    let regex = regex::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()?;

    let mut completions = Vec::new();
    for entry_res in fs::read_dir(parent)? {
        let entry = entry_res?;
        let file_name_os = entry.file_name();
        let Some(file_name) = file_name_os.to_str() else {
            continue;
        };
        // Don't list hidden files before entering a pattern
        if pattern == "^" && file_name.starts_with('.') {
            continue;
        }
        if regex.is_match(file_name) {
            completions.push((file_name.to_string(), entry.path()));
        }
    }

    completions.sort_by(|a, b| LANGUAGE_SORTER.compare(&a.0, &b.0));
    completions.truncate(8);
    Ok(completions)
}

fn format_size(size: u64) -> String {
    const KB: u64 = 1000;
    const MB: u64 = 1000 * KB;
    const GB: u64 = 1000 * MB;
    const TB: u64 = 1000 * GB;

    if size >= TB {
        format!("{:.1} TB", size as f64 / TB as f64)
    } else if size >= GB {
        format!("{:.1} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.1} KB", size as f64 / KB as f64)
    } else {
        format!("{size} B")
    }
}

const MODE_SHIFT_USER: u32 = 6;
const MODE_SHIFT_GROUP: u32 = 3;
const MODE_SHIFT_OTHER: u32 = 0;

const fn get_mode_part(mode: u32, shift: u32) -> u32 {
    (mode >> shift) & 0o7
}

fn set_mode_part(mode: u32, shift: u32, bits: u32) -> u32 {
    assert!(bits <= 0o7);
    (mode & !(0o7 << shift)) | (bits << shift)
}

fn date_time_formatter() -> DateTimeFormatter<fieldsets::YMDT> {
    let prefs = DateTimeFormatterPreferences::from(LOCALE.clone());

    let mut fs = fieldsets::YMDT::medium();
    fs = fs.with_time_precision(TimePrecision::Minute);

    DateTimeFormatter::try_new(prefs, fs).expect("failed to create DateTimeFormatter")
}

fn time_formatter() -> DateTimeFormatter<fieldsets::T> {
    let prefs = DateTimeFormatterPreferences::from(LOCALE.clone());

    let mut fs = fieldsets::T::medium();
    fs = fs.with_time_precision(TimePrecision::Minute);

    DateTimeFormatter::try_new(prefs, fs).expect("failed to create DateTimeFormatter")
}

struct FormatTime<'a> {
    pub time: SystemTime,
    pub date_time_formatter: &'a DateTimeFormatter<fieldsets::YMDT>,
    pub time_formatter: &'a DateTimeFormatter<fieldsets::T>,
}

impl<'a> FormatTime<'a> {
    fn from_secs(
        secs: i64,
        date_time_formatter: &'a DateTimeFormatter<fieldsets::YMDT>,
        time_formatter: &'a DateTimeFormatter<fieldsets::T>,
    ) -> Option<Self> {
        // This looks convoluted because we need to ensure the units match up
        let secs: u64 = secs.try_into().ok()?;
        let now = SystemTime::now();
        let filetime_diff = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|from_epoch| from_epoch.as_secs())
            .ok()
            .and_then(|now_secs| now_secs.checked_sub(secs))
            .map(Duration::from_secs)?;
        now.checked_sub(filetime_diff).map(|time| Self {
            time,
            date_time_formatter,
            time_formatter,
        })
    }
}

impl Display for FormatTime<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Ok(zoned) = jiff::Zoned::try_from(self.time) else {
            return Ok(());
        };
        let now = jiff::Zoned::now();
        let icu_datetime = DateTime::convert_from(zoned.datetime());
        if zoned.date() == now.date() {
            f.write_str(fl!("today").as_str())?;
            f.write_str(", ")?;
            self.time_formatter.format(&icu_datetime).fmt(f)
        } else {
            self.date_time_formatter.format(&icu_datetime).fmt(f)
        }
    }
}

const fn format_time<'a>(
    time: SystemTime,
    date_time_formatter: &'a DateTimeFormatter<fieldsets::YMDT>,
    time_formatter: &'a DateTimeFormatter<fieldsets::T>,
) -> FormatTime<'a> {
    FormatTime {
        time,
        date_time_formatter,
        time_formatter,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsKind {
    Local,
    Remote,
    Gvfs,
}

/// Device numbers of the mounted filesystems and their kind, read from
/// `/proc/self/mountinfo` and refreshed after [`FS_KINDS_TTL`] so mounts made
/// after startup are classified too.
static FS_KINDS: Mutex<Option<(Instant, FxHashMap<u64, FsKind>)>> = Mutex::new(None);
const FS_KINDS_TTL: Duration = Duration::from_secs(5);

pub fn fs_kind(metadata: &Metadata) -> FsKind {
    let mut guard = FS_KINDS.lock().unwrap_or_else(|e| e.into_inner());
    if guard
        .as_ref()
        .is_none_or(|(read_at, _)| read_at.elapsed() >= FS_KINDS_TTL)
    {
        *guard = Some((Instant::now(), read_fs_kinds()));
    }
    let (_, devices) = guard.as_ref().expect("filled above");
    devices.get(&metadata.dev()).map_or(FsKind::Local, |x| *x)
}

fn read_fs_kinds() -> FxHashMap<u64, FsKind> {
    let mut devices = FxHashMap::default();
    {
        match procfs::process::Process::myself() {
            Ok(process) => match process.mountinfo() {
                Ok(mount_infos) => {
                    devices = FxHashMap::from_iter(mount_infos.iter().filter_map(|mount_info| {
                        let mut parts = mount_info.majmin.split(':');
                        let major_str = parts.next()?;
                        let minor_str = parts.next()?;
                        let major = major_str.parse::<libc::c_uint>().ok()?;
                        let minor = minor_str.parse::<libc::c_uint>().ok()?;
                        let dev = libc::makedev(major, minor);
                        // Network and distributed filesystem types
                        // Based on common remote filesystem types found in /proc/mounts
                        let kind = match mount_info.fs_type.as_str() {
                            // SMB/CIFS variants
                            "cifs" | "smb" | "smb2" | "smbfs" => FsKind::Remote,

                            // NFS variants
                            "nfs" | "nfs4" => FsKind::Remote,

                            // FUSE-based remote filesystems
                            "fuse.rclone" | "fuse.sshfs" | "fuse.davfs2" | "fuse.ceph"
                            | "fuse.glusterfs" | "fuse.s3fs" | "fuse.goofys" | "fuse.gcsfuse"
                            | "fuse.afp" | "fuse.afpfs" => FsKind::Remote,

                            // Other network protocols
                            "afs" | "coda" | "ncpfs" | "davfs" | "davfs2" | "shfs" => {
                                FsKind::Remote
                            }

                            // Cluster/distributed filesystems
                            "ceph" | "glusterfs" | "lustre" | "gfs" | "gfs2" | "ocfs2" => {
                                FsKind::Remote
                            }

                            // GVFS (GNOME Virtual File System)
                            "fuse.gvfsd-fuse" => FsKind::Gvfs,

                            // Everything else is local
                            _ => FsKind::Local,
                        };
                        Some((dev, kind))
                    }));
                }
                Err(err) => {
                    log::warn!("failed to get mount info: {err}");
                }
            },
            Err(err) => {
                log::warn!("failed to get process info: {err}");
            }
        }
    }
    devices
}

#[cfg(not(feature = "desktop"))]
fn get_desktop_file_display_name(path: &Path) -> Option<String> {
    None
}

#[cfg(feature = "desktop")]
fn get_desktop_file_display_name(path: &Path) -> Option<String> {
    let locales = get_languages_from_env();
    let entry = match DesktopEntry::from_path(path, Some(&locales)) {
        Ok(ok) => ok,
        Err(err) => {
            log::warn!("failed to parse {}: {}", path.display(), err);
            return None;
        }
    };

    entry.name(&locales).map(|s| s.into_owned())
}

#[cfg(not(feature = "desktop"))]
fn get_desktop_file_icon(path: &Path) -> Option<String> {
    None
}

#[cfg(feature = "desktop")]
fn get_desktop_file_icon(path: &Path) -> Option<String> {
    let entry = match DesktopEntry::from_path::<&str>(path, None) {
        Ok(ok) => ok,
        Err(err) => {
            log::warn!("failed to parse {}: {}", path.display(), err);
            return None;
        }
    };

    entry.icon().map(str::to_string)
}

/// Creates an icon handle from a desktop file's Icon field value.
/// Supports both icon names (looked up in theme) and absolute paths (used directly).
fn desktop_icon_handle(icon: &str, size: u16) -> widget::icon::Handle {
    let icon_path = Path::new(icon);
    if icon_path.is_absolute() && icon_path.exists() {
        widget::icon::from_path(icon_path.to_path_buf())
    } else {
        widget::icon::from_name(icon)
            .prefer_svg(true)
            .size(size)
            .handle()
    }
}

#[cfg(feature = "desktop")]
pub fn parse_desktop_file(path: &Path) -> (Option<String>, Option<String>) {
    let locales = get_languages_from_env();
    let entry = match DesktopEntry::from_path(path, Some(&locales)) {
        Ok(ok) => ok,
        Err(err) => {
            log::warn!("failed to parse {}: {}", path.display(), err);
            return (None, None);
        }
    };
    (
        entry.name(&locales).map(|s| s.into_owned()),
        entry.icon().map(str::to_string),
    )
}

fn display_name_for_file(path: &Path, name: &str, get_from_gvfs: bool, is_desktop: bool) -> String {
    if is_desktop {
        return get_desktop_file_display_name(path).map_or_else(
            || Item::display_name(name),
            |desktop_name| Item::display_name(desktop_name.as_str()),
        );
    } else if get_from_gvfs {
        #[cfg(feature = "gvfs")]
        {
            let file = gio::File::for_path(path);
            if let Ok(info) = gio::prelude::FileExt::query_info(
                &file,
                "standard::display-name",
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            ) {
                return Item::display_name(info.display_name().as_str());
            }
        }
    }
    Item::display_name(name)
}

// Whether a dir lives on a remote filesystem, according to GIO.
#[cfg(feature = "gvfs")]
fn gvfs_dir_is_remote(dir: &Path) -> bool {
    static REMOTE_CACHE: LazyLock<RwLock<FxHashMap<PathBuf, bool>>> =
        LazyLock::new(|| RwLock::new(FxHashMap::default()));

    if let Some(remote) = REMOTE_CACHE.read().unwrap().get(dir) {
        return *remote;
    }

    let remote = match gio::prelude::FileExt::query_filesystem_info(
        &gio::File::for_path(dir),
        gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE,
        gio::Cancellable::NONE,
    ) {
        Ok(info) => info.boolean(gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE),
        Err(err) => {
            log::warn!(
                "failed to get GIO filesystem info for {}: {}",
                dir.display(),
                err
            );

            //Assume remote so that the "expensive tasks" are rather skipped then actually executed.
            true
        }
    };

    REMOTE_CACHE
        .write()
        .unwrap()
        .insert(dir.to_path_buf(), remote);
    remote
}

/// The mime every directory item carries.
pub static DIRECTORY_MIME: LazyLock<Mime> = LazyLock::new(|| "inode/directory".parse().unwrap());

/// Icons for a file item, using the launcher's own icon when `path` is a
/// desktop entry. Returns whether it was one, so callers can also take the
/// display name from the entry.
/// File types and folder names whose icons still need resolving
#[derive(Clone, Debug, Default)]
pub struct IconWarmup {
    mimes: Vec<mime::Mime>,
    folders: Vec<&'static str>,
}

impl IconWarmup {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mimes.is_empty() && self.folders.is_empty()
    }
}

/// Take any icons the cache has learned since these items were built.
///
/// Cheap: cache lookups only, never resolution. Called wherever items are
/// installed as well as when a worker reports back, because another tab may
/// have warmed the same types in between, in which case there is nothing to
/// warm and no report would arrive to swap the placeholders out.
///
/// Items showing a thumbnail or a desktop entry's own icon are left alone:
/// theirs did not come from these caches.
pub fn refresh_icons(items: &mut [Item], sizes: IconSizes) -> IconWarmup {
    let mut mimes = std::collections::HashSet::new();
    let mut folders = std::collections::HashSet::new();
    for item in items {
        if item.mime == "application/x-desktop"
            || matches!(
                item.thumbnail_opt,
                Some(ItemThumbnail::Image(..) | ItemThumbnail::Svg(..))
            )
        {
            continue;
        }
        // The same lookups decide both what to draw now and what still needs
        // resolving. Asking twice would leave a gap in which another tab's
        // worker fills the cache: the refresh would keep its placeholders and
        // the second question would answer "nothing missing", so no report
        // would ever come back to swap them out.
        let sizes_wanted = [sizes.grid(), sizes.list(), sizes.list_condensed()];
        if item.metadata.is_dir() {
            let Some(path) = item.path_opt() else {
                continue;
            };
            let name = folder_icon_name(path);
            let found = sizes_wanted.map(|size| try_folder_icon(name, size));
            if found.iter().any(Option::is_none) {
                folders.insert(name);
            }
            let [grid, list, condensed] = found;
            if let Some(handle) = grid {
                item.icon_handle_grid = handle;
            }
            if let Some(handle) = list {
                item.icon_handle_list = handle;
            }
            if let Some(handle) = condensed {
                item.icon_handle_list_condensed = handle;
            }
        } else {
            // One look per size answers both questions, so the cache cannot
            // change between them and strand this item with a placeholder
            // nobody will come back to replace
            let found =
                sizes_wanted.map(|size| crate::mime_icon::lookup_mime_icon(&item.mime, size));
            if found
                .iter()
                .any(|state| matches!(state, crate::mime_icon::CachedIcon::Unknown))
            {
                mimes.insert(item.mime.clone());
            }
            let [grid, list, condensed] = found;
            if let crate::mime_icon::CachedIcon::Found(handle) = grid {
                item.icon_handle_grid = handle;
            }
            if let crate::mime_icon::CachedIcon::Found(handle) = list {
                item.icon_handle_list = handle;
            }
            if let crate::mime_icon::CachedIcon::Found(handle) = condensed {
                item.icon_handle_list_condensed = handle;
            }
        }
    }
    IconWarmup {
        mimes: mimes.into_iter().collect(),
        folders: folders.into_iter().collect(),
    }
}

/// Resolve everything in `warmup` into the icon caches. Each lookup searches
/// the icon theme on disk, so this belongs on a worker.
pub fn warm_icons(warmup: &IconWarmup, sizes: IconSizes) {
    let all = [sizes.grid(), sizes.list(), sizes.list_condensed()];
    crate::mime_icon::warm_mime_icons(&warmup.mimes, &all);
    for name in &warmup.folders {
        for size in all {
            let _ = cached_folder_icon(name, size, false);
        }
    }
}

/// The icon for `mime`, or the generic one when it has not been resolved yet
fn cached_or_placeholder(mime: &Mime, size: u16) -> widget::icon::Handle {
    crate::mime_icon::try_mime_icon(mime, size)
        .unwrap_or_else(|| crate::mime_icon::placeholder_icon(size))
}

fn file_icons(
    path: &Path,
    mime: &Mime,
    sizes: IconSizes,
) -> (
    bool,
    widget::icon::Handle,
    widget::icon::Handle,
    widget::icon::Handle,
) {
    let is_desktop = *mime == "application/x-desktop";
    match is_desktop.then(|| get_desktop_file_icon(path)).flatten() {
        Some(icon_name) => (
            true,
            desktop_icon_handle(&icon_name, sizes.grid()),
            desktop_icon_handle(&icon_name, sizes.list()),
            desktop_icon_handle(&icon_name, sizes.list_condensed()),
        ),
        // Only what is already cached. Resolving an icon searches the icon
        // theme on disk and costs tens of milliseconds, and a directory of a
        // hundred files holds enough distinct types to add seconds to the
        // scan before anything can be drawn. The placeholder stands in until
        // `Message::IconsReady` swaps in the real one.
        None => (
            is_desktop,
            cached_or_placeholder(mime, sizes.grid()),
            cached_or_placeholder(mime, sizes.list()),
            cached_or_placeholder(mime, sizes.list_condensed()),
        ),
    }
}

/// Read an image's pixel size into its cache.
///
/// Only scans call this. They run on a worker, where a slow header read costs
/// a slower listing; item constructors do not, because items are also built
/// while handling input, filesystem notifications and search results, and an
/// image on a stalled mount would freeze the window. An item built outside a
/// scan keeps an empty cache and shows no size.
pub fn fill_image_dimensions(item: &Item) {
    if item.mime.type_() != mime::IMAGE {
        return;
    }
    // For a trashed item this is the copy inside the trash, not the path it
    // came from, which by now holds nothing or something else
    let Some(path) = item.path_opt() else {
        return;
    };
    let _ = item
        .image_dimensions
        .set(image::image_dimensions(path).ok());
}

#[cfg(feature = "gvfs")]
pub fn item_from_gvfs_info(path: PathBuf, file_info: gio::FileInfo, sizes: IconSizes) -> Item {
    let file_name = file_info
        .attribute_as_string(gio::FILE_ATTRIBUTE_STANDARD_NAME)
        .unwrap_or_default();
    let mtime = file_info.attribute_uint64(gio::FILE_ATTRIBUTE_TIME_MODIFIED);
    let remote = path.parent().is_none_or(gvfs_dir_is_remote);
    let is_dir = matches!(file_info.file_type(), gio::FileType::Directory);

    let size_opt = (!is_dir).then_some(file_info.size() as u64);

    let (is_desktop, mime, icon_handle_grid, icon_handle_list, icon_handle_list_condensed) =
        if is_dir {
            (
                false,
                DIRECTORY_MIME.clone(),
                folder_icon_or_placeholder(&path, sizes.grid()),
                folder_icon_or_placeholder(&path, sizes.list()),
                folder_icon_or_placeholder(&path, sizes.list_condensed()),
            )
        } else {
            // ALWAYS assume we're remote for mime guessing here, since gvfs reading can be expensive
            // @todo - expose this as a config option?
            let mime = mime_for_path(&path, None, true);
            let (is_desktop, grid, list, condensed) = file_icons(&path, &mime, sizes);
            (is_desktop, mime, grid, list, condensed)
        };

    // Children are counted in the background by the tab's subscription
    let children_opt = None;
    let dir_size = if is_dir && !remote {
        DirSize::Calculating(Controller::default())
    } else {
        DirSize::NotDirectory
    };

    let display_name = display_name_for_file(&path, &file_info.display_name(), false, is_desktop);
    let hidden = file_name.starts_with('.');

    Item {
        name: file_name.into(),
        display_name,
        is_mount_point: false,
        metadata: ItemMetadata::GvfsPath {
            mtime,
            size_opt,
            children_opt,
            is_dir,
        },
        hidden,
        // Only the gvfs listing builds these, and it runs on a worker.
        // Remote paths are left alone: reading content over the network to
        // learn a pixel size is what the remote guard exists to avoid.
        image_dimensions: OnceCell::from(
            (!remote && mime.type_() == mime::IMAGE)
                .then(|| image::image_dimensions(&path).ok())
                .flatten(),
        ),
        location_opt: Some(Location::Path(path)),
        mime,
        icon_handle_grid,
        icon_handle_list,
        icon_handle_list_condensed,
        thumbnail_opt: if remote {
            Some(ItemThumbnail::NotImage)
        } else {
            None
        },
        button_id: widget::Id::unique(),
        pos_opt: Cell::new(None),
        rect_opt: Cell::new(None),
        selected: false,
        highlighted: false,
        overlaps_drag_rect: false,
        dir_size,
        cut: false,
        checksums: ChecksumState::default(),
    }
}

pub fn item_from_search_item(search_item: SearchItem, sizes: IconSizes) -> Item {
    match search_item {
        SearchItem::Path(path, name, metadata) => item_from_entry(path, name, metadata, sizes),
        SearchItem::Trash(entry, metadata) => item_from_trash_entry(entry, metadata, sizes),
    }
}

pub fn item_from_entry(
    path: PathBuf,
    name: String,
    metadata: fs::Metadata,
    sizes: IconSizes,
) -> Item {
    let mut is_gvfs = false;

    let hidden = name.starts_with('.');

    let remote = match fs_kind(&metadata) {
        FsKind::Local => false,
        FsKind::Remote => true,
        #[cfg(feature = "gvfs")]
        FsKind::Gvfs => {
            is_gvfs = true;
            path.parent().is_none_or(gvfs_dir_is_remote)
        }
        #[cfg(not(feature = "gvfs"))]
        FsKind::Gvfs => {
            log::info!(
                "gvfs feature not enabled, info may be inaccurate for {}",
                path.display()
            );
            true
        }
    };

    let (is_desktop, mime, icon_handle_grid, icon_handle_list, icon_handle_list_condensed) =
        if metadata.is_dir() {
            (
                false,
                DIRECTORY_MIME.clone(),
                folder_icon_or_placeholder(&path, sizes.grid()),
                folder_icon_or_placeholder(&path, sizes.list()),
                folder_icon_or_placeholder(&path, sizes.list_condensed()),
            )
        } else {
            let mime = mime_for_path(&path, Some(&metadata), remote);
            let (is_desktop, grid, list, condensed) = file_icons(&path, &mime, sizes);
            (is_desktop, mime, grid, list, condensed)
        };

    // Children are counted in the background by the tab's subscription
    let children_opt = None;
    let dir_size = if metadata.is_dir() && !remote {
        DirSize::Calculating(Controller::default())
    } else {
        DirSize::NotDirectory
    };

    let display_name = display_name_for_file(&path, &name, is_gvfs, is_desktop);

    Item {
        name,
        display_name,
        is_mount_point: false,
        metadata: ItemMetadata::Path {
            metadata,
            children_opt,
        },
        hidden,
        image_dimensions: OnceCell::new(),
        location_opt: Some(Location::Path(path)),
        mime,
        icon_handle_grid,
        icon_handle_list,
        icon_handle_list_condensed,
        thumbnail_opt: remote.then_some(ItemThumbnail::NotImage),
        button_id: widget::Id::unique(),
        pos_opt: Cell::new(None),
        rect_opt: Cell::new(None),
        selected: false,
        highlighted: false,
        overlaps_drag_rect: false,
        dir_size,
        cut: false,
        checksums: ChecksumState::default(),
    }
}

pub fn item_from_trash_entry(
    entry: TrashItem,
    metadata: TrashItemMetadata,
    sizes: IconSizes,
) -> Item {
    let original_path = entry.original_path();
    let name = entry.name.to_string_lossy().into_owned();
    let display_name = Item::display_name(&name);

    let trash_path = crate::trash::trash_item_path(&entry);
    let location = trash_path.clone().map(Location::Path);

    let (mime, icon_handle_grid, icon_handle_list, icon_handle_list_condensed) = match metadata.size
    {
        trash::TrashItemSize::Entries(_) => (
            DIRECTORY_MIME.clone(),
            folder_icon_or_placeholder(&original_path, sizes.grid()),
            folder_icon_or_placeholder(&original_path, sizes.list()),
            folder_icon_or_placeholder(&original_path, sizes.list_condensed()),
        ),
        trash::TrashItemSize::Bytes(_) => {
            // This passes remote = true so it does not read from the original path
            let mime = mime_for_path(&original_path, None, true);
            // The launcher icon is read from the copy inside the trash
            let icon_path = trash_path.as_deref().unwrap_or(&original_path);
            let (_, grid, list, condensed) = file_icons(icon_path, &mime, sizes);
            (mime, grid, list, condensed)
        }
    };

    Item {
        name,
        display_name,
        is_mount_point: false,
        metadata: ItemMetadata::Trash { metadata, entry },
        hidden: false,
        location_opt: location,
        image_dimensions: OnceCell::new(),
        mime,
        icon_handle_grid,
        icon_handle_list,
        icon_handle_list_condensed,
        thumbnail_opt: Some(ItemThumbnail::NotImage),
        button_id: widget::Id::unique(),
        pos_opt: Cell::new(None),
        rect_opt: Cell::new(None),
        selected: false,
        highlighted: false,
        overlaps_drag_rect: false,
        dir_size: DirSize::NotDirectory,
        cut: false,
        checksums: ChecksumState::default(),
    }
}

fn item_from_trash_child(
    path: PathBuf,
    name: String,
    metadata: fs::Metadata,
    sizes: IconSizes,
) -> Option<Item> {
    let Some(original_path) = crate::trash::original_path_for_trash_child(&path) else {
        log::warn!(
            "failed to resolve original path for trash item {}, skipping entry",
            path.display()
        );
        return None;
    };
    let Some(original_parent) = original_path.parent() else {
        log::warn!(
            "trash item {} has no original parent, skipping entry",
            path.display()
        );
        return None;
    };
    let entry = trash::TrashItem {
        id: path.as_os_str().to_os_string(),
        name: std::ffi::OsString::from(&name),
        original_parent: original_parent.to_path_buf(),
        time_deleted: 0,
    };
    let size = if metadata.is_dir() {
        trash::TrashItemSize::Entries(0)
    } else {
        trash::TrashItemSize::Bytes(metadata.len())
    };
    Some(item_from_trash_entry(
        entry,
        trash::TrashItemMetadata { size },
        sizes,
    ))
}

fn get_filename_from_path(path: &Path) -> Result<String, String> {
    Ok(match path.file_name() {
        Some(name_os) => name_os
            .to_str()
            .ok_or_else(|| {
                format!(
                    "failed to parse file name for {}: {name_os:?} is not valid UTF-8",
                    path.display()
                )
            })?
            .to_string(),
        None => fl!("filesystem"),
    })
}

pub fn item_from_path<P: Into<PathBuf>>(path: P, sizes: IconSizes) -> Result<Item, String> {
    let path = path.into();
    let name = get_filename_from_path(&path)?;
    let metadata = fs::metadata(&path)
        .map_err(|err| format!("failed to read metadata for {}: {}", path.display(), err))?;
    Ok(item_from_entry(path, name, metadata, sizes))
}

pub fn scan_path(tab_path: &PathBuf, sizes: IconSizes) -> Vec<Item> {
    let mut items = Vec::new();
    let mut hidden_files = Box::from([]);
    let mut remote_scannable = false;

    #[cfg(feature = "gvfs")]
    {
        if let Ok(path_meta) = fs::metadata(tab_path)
            && fs_kind(&path_meta) == FsKind::Gvfs
        {
            let file = gio::File::for_path(tab_path);

            // gio crate expects a comma delimited string
            let attr_string = [
                gio::FILE_ATTRIBUTE_STANDARD_DISPLAY_NAME.as_str(),
                gio::FILE_ATTRIBUTE_TIME_MODIFIED.as_str(),
                gio::FILE_ATTRIBUTE_STANDARD_SIZE.as_str(),
                gio::FILE_ATTRIBUTE_STANDARD_TYPE.as_str(),
                gio::FILE_ATTRIBUTE_STANDARD_NAME.as_str(),
            ]
            .join(",");

            match gio::prelude::FileExt::enumerate_children(
                &file,
                attr_string.as_str(),
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            ) {
                Ok(res) => {
                    remote_scannable = true;
                    items = res
                        .filter_map(|file| {
                            let file = file.ok()?;
                            Some(item_from_gvfs_info(tab_path.join(file.name()), file, sizes))
                        })
                        .collect();
                }
                Err(err) => {
                    log::warn!(
                        "could not enumerate {} via gio: {}",
                        tab_path.display(),
                        err
                    );
                }
            }
        }
    }

    if !remote_scannable {
        match fs::read_dir(tab_path) {
            Ok(entries) => {
                let trash = crate::trash::is_trash_path(tab_path);
                items = entries
                    .filter_map(|entry_res| {
                        let entry = entry_res
                            .inspect_err(|err| {
                                log::warn!(
                                    "failed to read entry in {}: {}",
                                    tab_path.display(),
                                    err
                                )
                            })
                            .ok()?;

                        let path = entry.path();

                        let name = entry
                            .file_name()
                            .into_string()
                            .inspect_err(|name_os| {
                                log::warn!(
                                    "failed to parse entry at {}: {:?} is not valid UTF-8",
                                    path.display(),
                                    name_os
                                )
                            })
                            .ok()?;

                        if name == ".hidden" && path.is_file() {
                            hidden_files = parse_hidden_file(&path);
                        }

                        // Fall back to the link itself when the target cannot
                        // be read, so a broken symlink is still listed and can
                        // be selected and deleted instead of being invisible
                        let metadata = match fs::metadata(&path) {
                            Ok(metadata) => metadata,
                            Err(err) => match fs::symlink_metadata(&path) {
                                Ok(metadata) => metadata,
                                Err(_) => {
                                    log::warn!(
                                        "failed to read metadata for entry at {}: {}",
                                        path.display(),
                                        err
                                    );
                                    return None;
                                }
                            },
                        };

                        let item = if trash {
                            item_from_trash_child(path, name, metadata, sizes)
                        } else {
                            Some(item_from_entry(path, name, metadata, sizes))
                        };
                        if let Some(item) = &item {
                            fill_image_dimensions(item);
                        }
                        item
                    })
                    .collect();
            }
            Err(err) => {
                log::warn!("failed to read directory {}: {}", tab_path.display(), err);
            }
        }
    }
    items.sort_unstable_by(|a, b| match (a.metadata.is_dir(), b.metadata.is_dir()) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => LANGUAGE_SORTER.compare(&a.display_name, &b.display_name),
    });
    for item in &mut items {
        if hidden_files.contains(&item.name) {
            item.hidden = true;
        }
    }
    items
}

pub fn scan_search<F: Fn(SearchItem) -> bool + Sync>(
    search_location: &SearchLocation,
    term: &str,
    show_hidden: bool,
    callback: F,
) {
    if term.is_empty() {
        return;
    }

    let pattern = regex::escape(term);
    let regex = match regex::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
    {
        Ok(ok) => ok,
        Err(err) => {
            log::warn!("failed to parse regex {pattern:?}: {err}");
            return;
        }
    };

    match search_location {
        SearchLocation::Path(tab_path) => {
            ignore::WalkBuilder::new(tab_path)
                .standard_filters(false)
                .hidden(!show_hidden)
                .same_file_system(true)
                .build_parallel()
                .run(|| {
                    Box::new(|entry_res| {
                        let Ok(entry) = entry_res else {
                            // Skip invalid entries
                            return ignore::WalkState::Skip;
                        };

                        let Some(file_name) = entry.file_name().to_str() else {
                            // Skip anything with an invalid name
                            return ignore::WalkState::Skip;
                        };

                        if regex.is_match(file_name) {
                            let path = entry.path();

                            let metadata = match entry.metadata() {
                                Ok(ok) => ok,
                                Err(err) => {
                                    log::warn!(
                                        "failed to read metadata for entry at {}: {}",
                                        path.display(),
                                        err
                                    );
                                    return ignore::WalkState::Continue;
                                }
                            };

                            if !callback(SearchItem::Path(
                                path.to_path_buf(),
                                file_name.to_string(),
                                metadata,
                            )) {
                                return ignore::WalkState::Quit;
                            }
                        }

                        ignore::WalkState::Continue
                    })
                });
        }
        SearchLocation::Recents => {
            let recent_files = match recently_used_xbel::parse_file() {
                Ok(recent_files) => recent_files,
                Err(err) => {
                    log::warn!("Error reading recent files: {err:?}");
                    return;
                }
            };

            for bookmark in recent_files.bookmarks {
                let path = uri_to_path(bookmark.href);
                if let Some(path) = path
                    && path.exists()
                {
                    let file_name = path.file_name();
                    if let Some(file_name) = file_name {
                        let file_name = file_name.to_string_lossy();
                        if regex.is_match(&file_name) {
                            match path.metadata() {
                                Ok(metadata) => {
                                    if !callback(SearchItem::Path(
                                        path.to_path_buf(),
                                        file_name.to_string(),
                                        metadata,
                                    )) {
                                        break;
                                    }
                                }
                                Err(err) => {
                                    log::warn!(
                                        "failed to read metadata for entry at {}: {}",
                                        path.display(),
                                        err
                                    );
                                }
                            };
                        }
                    }
                }
            }
        }
        SearchLocation::Trash => {
            Trash::scan_search(callback, &regex);
        }
    }
}

fn uri_to_path(uri: String) -> Option<PathBuf> {
    let url = uri.parse::<url::Url>().ok()?;
    if url.scheme() == "file" {
        return url.to_file_path().ok();
    }
    // Anything else, such as a document on a share, is reachable through
    // its GVFS mount when it has one
    #[cfg(feature = "gvfs")]
    {
        gio::prelude::FileExt::path(&gio::File::for_uri(url.as_str()))
    }
    #[cfg(not(feature = "gvfs"))]
    {
        None
    }
}

pub fn has_recents() -> bool {
    match recently_used_xbel::parse_file() {
        Ok(recent_files) => !recent_files.bookmarks.is_empty(),
        Err(_) => false,
    }
}

pub fn scan_recents(sizes: IconSizes) -> Vec<Item> {
    let recent_files = match recently_used_xbel::parse_file() {
        Ok(recent_files) => recent_files,
        Err(err) => {
            log::warn!("Error reading recent files: {err:?}");
            return Vec::new();
        }
    };
    let mut recents: Vec<_> = recent_files
        .bookmarks
        .into_iter()
        .filter_map(|bookmark| {
            let path = uri_to_path(bookmark.href)?;
            let last_edit = bookmark.modified.parse::<jiff::Timestamp>().ok()?;
            let last_visit = bookmark.visited.parse::<jiff::Timestamp>().ok()?;

            if path.exists() {
                let file_name = path.file_name()?;
                let name = file_name.to_string_lossy().to_string();

                let metadata = match path.metadata() {
                    Ok(ok) => ok,
                    Err(err) => {
                        log::warn!(
                            "failed to read metadata for entry at {}: {}",
                            path.display(),
                            err
                        );
                        return None;
                    }
                };

                let item = item_from_entry(path, name, metadata, sizes);
                fill_image_dimensions(&item);
                Some((item, last_edit.min(last_visit)))
            } else {
                log::warn!("recent file path does not exist: {}", path.display());
                None
            }
        })
        .collect();

    recents.sort_by_key(|recent| Reverse(recent.1));

    recents.into_iter().take(50).map(|(item, _)| item).collect()
}

pub fn scan_network(uri: &str, sizes: IconSizes) -> Vec<Item> {
    for mounter in MOUNTERS.values() {
        match mounter.network_scan(uri, sizes) {
            Some(Ok(items)) => return items,
            Some(Err(err)) => {
                log::warn!("failed to scan {uri:?}: {err}");
            }
            None => {}
        }
    }
    Vec::new()
}

#[derive(Clone, Debug)]
pub struct EditLocation {
    pub location: Location,
    pub completions: Option<Vec<(String, PathBuf)>>,
    pub selected: Option<usize>,
}

impl EditLocation {
    pub fn resolve(&self) -> Option<Location> {
        if let Location::Network(uri, ..) = &self.location {
            MOUNTERS
                .values()
                .find_map(|mounter| mounter.dir_info(uri))
                .map(|(uri, display_name, path_opt)| Location::Network(uri, display_name, path_opt))
        } else {
            let Some(selected) = self.selected else {
                return Some(self.location.clone());
            };
            let completions = self.completions.as_ref()?;
            let completion = completions.get(selected)?;
            Some(self.location.with_path(completion.1.clone()).normalize())
        }
    }

    pub fn select(&mut self, forwards: bool) {
        if let Some(completions) = &self.completions {
            if completions.is_empty() {
                self.selected = None;
            } else {
                let mut selected = if forwards {
                    self.selected.and_then(|x| x.checked_add(1)).unwrap_or(0)
                } else {
                    self.selected
                        .and_then(|x| x.checked_sub(1))
                        .unwrap_or(completions.len() - 1)
                };
                if selected >= completions.len() {
                    selected = 0;
                }
                self.selected = Some(selected);

                // Automatically resolve if there is only one completion
                if completions.len() == 1
                    && let Some(resolved) = self.resolve()
                {
                    self.location = resolved;
                    self.selected = None;
                }
            }
        } else {
            self.selected = None;
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SearchLocation {
    Path(PathBuf),
    Recents,
    Trash,
}

impl std::fmt::Display for SearchLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::Recents => write!(f, "recents"),
            Self::Trash => write!(f, "trash"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum SearchItem {
    Path(PathBuf, String, fs::Metadata),
    Trash(TrashItem, TrashItemMetadata),
}

impl From<Location> for EditLocation {
    fn from(location: Location) -> Self {
        Self {
            location,
            completions: None,
            selected: None,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Location {
    Network(String, String, Option<PathBuf>),
    Path(PathBuf),
    Recents,
    Search(SearchLocation, String, bool, Instant),
    Trash,
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(uri, ..) => write!(f, "{uri}"),
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::Recents => write!(f, "recents"),
            Self::Search(location, term, ..) => {
                write!(f, "search {} for {}", location, term)
            }
            Self::Trash => write!(f, "trash"),
        }
    }
}

impl Location {
    pub fn normalize(&self) -> Self {
        if let Location::Network(uri, ..) = self {
            if !uri.ends_with('/') {
                let mut uri = uri.clone();
                uri.push('/');
                self.with_uri(uri)
            } else {
                self.clone()
            }
        } else if let Some(mut path) = self.path_opt().cloned() {
            // Canonicalize path, if possible
            if let Ok(canonical) = fs::canonicalize(&path) {
                path = canonical;
            }
            // Add trailing slash if location is a directory
            if path.is_dir() {
                path.push("");
            }
            self.with_path(path)
        } else {
            self.clone()
        }
    }

    pub fn ancestors(&self) -> Vec<(Self, String)> {
        self.path_opt().map_or_else(Default::default, |path| {
            path.ancestors()
                .scan(false, |found_home, ancestor| {
                    (!*found_home).then(|| {
                        let (name, is_home) = folder_name(ancestor);
                        *found_home = is_home;
                        (self.with_path(ancestor.to_path_buf()), name)
                    })
                })
                .collect()
        })
    }

    pub const fn path_opt(&self) -> Option<&PathBuf> {
        match self {
            Self::Path(path) => Some(path),
            Self::Search(SearchLocation::Path(path), ..) => Some(path),
            Self::Network(_, _, path) => path.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn into_path_opt(self) -> Option<PathBuf> {
        match self {
            Self::Path(path) => Some(path),
            Self::Search(SearchLocation::Path(path), ..) => Some(path),
            Self::Network(_, _, path) => path,
            _ => None,
        }
    }

    pub fn with_path(&self, path: PathBuf) -> Self {
        let path = Self::expand_tilde(path);
        match self {
            Self::Path(..) => Self::Path(path),
            Self::Search(SearchLocation::Path(_), term, show_hidden, time) => Self::Search(
                SearchLocation::Path(path),
                term.clone(),
                *show_hidden,
                *time,
            ),

            other => other.clone(),
        }
    }

    pub fn with_uri(&self, uri: String) -> Self {
        if let Self::Network(_, name, path) = self {
            Self::Network(uri, name.clone(), path.clone())
        } else {
            self.clone()
        }
    }

    pub fn scan(&self, sizes: IconSizes) -> (Option<Box<Item>>, Vec<Item>) {
        let items = match self {
            Self::Path(path) => scan_path(path, sizes),
            Self::Search(..) => {
                // Search is done incrementally
                Vec::new()
            }
            Self::Trash => Trash::scan(sizes),
            Self::Recents => scan_recents(sizes),
            Self::Network(uri, _, _) => scan_network(uri, sizes),
        };
        let parent_item_opt = match self.path_opt() {
            Some(path) => match item_from_path(path, sizes) {
                Ok(item) => Some(Box::new(item)),
                Err(err) => {
                    log::warn!("failed to get item for {}: {}", path.display(), err);
                    None
                }
            },
            None => None,
        };
        (parent_item_opt, items)
    }

    pub fn title(&self) -> String {
        match self {
            Self::Path(path) => {
                let (name, _) = folder_name(path);
                name
            }
            Self::Search(location, term, ..) => {
                let name = match location {
                    SearchLocation::Path(path) => folder_name(path).0,
                    SearchLocation::Trash => fl!("trash"),
                    SearchLocation::Recents => fl!("recents"),
                };

                fl!("search-title", term = term.as_str(), name = name)
            }
            Self::Trash => {
                fl!("trash")
            }
            Self::Recents => {
                fl!("recents")
            }
            Self::Network(display_name, ..) => display_name.clone(),
        }
    }

    /// Expand a path that starts with "~" with the
    /// user's home directory
    pub fn expand_tilde(path: PathBuf) -> PathBuf {
        let mut components = path.components();
        match components.next() {
            Some(path::Component::Normal(os_str)) if os_str == "~" => {
                if let Some(home) = dirs::home_dir() {
                    home.join(components.as_path())
                } else {
                    path
                }
            }
            _ => path,
        }
    }

    pub fn is_trash(&self) -> bool {
        matches!(
            self,
            Location::Trash | Location::Search(SearchLocation::Trash, ..)
        )
    }

    pub fn is_recents(&self) -> bool {
        matches!(
            self,
            Location::Recents | Location::Search(SearchLocation::Recents, ..)
        )
    }

    /// Returns true if this location supports paste operations (not Trash)
    pub fn supports_paste(&self) -> bool {
        matches!(
            self,
            Self::Path(..) | Self::Search(..) | Self::Recents | Self::Network(_, _, Some(_))
        )
    }
}

pub struct TaskWrapper(pub crate::ui::Task<Message>);

impl From<crate::ui::Task<Message>> for TaskWrapper {
    fn from(task: crate::ui::Task<Message>) -> Self {
        Self(task)
    }
}

impl fmt::Debug for TaskWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskWrapper").finish()
    }
}

#[derive(Debug)]
pub enum Command {
    Action(Action),
    /// Resolve these icons on a worker, then send [`Message::IconsReady`].
    /// Scanning and search both leave placeholders rather than waiting on the
    /// icon theme, which costs tens of milliseconds per icon.
    WarmIcons(IconWarmup, IconSizes),
    Surface(crate::ui::surface::Action<Message>),
    AddNetworkDrive,
    AddToSidebar(PathBuf),
    AutoScroll(Option<f32>),
    ChangeLocation(String, Location, Option<Vec<PathBuf>>),
    Delete(Vec<PathBuf>),
    /// Files were dropped on this tab: read the drag payload and move them into
    /// `to`, or copy them when the second field is set.
    DropFiles(PathBuf, bool),
    ClearRecents,
    EmptyTrash,
    #[cfg(feature = "desktop")]
    ExecEntryAction(crate::desktop_entry::DesktopEntryData, usize),
    Iced(TaskWrapper),
    OpenFile(Vec<PathBuf>),
    OpenInNewTab(PathBuf),
    OpenInNewWindow(PathBuf),
    Preview(PreviewKind),
    RunContextAction(usize),
    SetOpenWith(Mime, String),
    SetPermissions(PathBuf, u32),
    SetMultiplePermissions(Vec<(PathBuf, u32)>),
    SetSort(String, HeadingOptions, bool),
    WindowDrag,
    WindowToggleMaximize,
}

#[derive(Clone, Debug)]
pub enum Message {
    AddNetworkDrive,
    /// The icon cache has been filled, so placeholders can be replaced
    IconsReady,
    AutoScroll(Option<f32>),
    Click(Option<usize>),
    DoubleClick(Option<usize>),
    ClickRelease(Option<usize>),
    Config(TabConfig),
    ContextAction(Action),
    RightClickBackground,
    Surface(crate::ui::surface::Action<Message>),
    LocationContextMenuIndex(Option<usize>),
    LocationMenuAction(LocationMenuAction),
    Drag(Option<Rectangle>),
    /// Start a Wayland drag carrying the selection, because the pointer moved
    /// away from a press on item `usize`.
    DragFiles(usize),
    /// A Wayland file drag moved over, was dropped on, or left this tab.
    Dnd(crate::mouse_area::DndDrag),
    /// The same, for the breadcrumb segment of the ancestor at `usize`, whose
    /// directory the drop would move the files into.
    DndAncestor(usize, PathBuf, crate::mouse_area::DndDrag),
    DragEnd,
    EditLocation(Option<EditLocation>),
    EditLocationComplete(usize),
    EditLocationEnable,
    EditLocationSubmit,
    EditLocationTab,
    OpenInNewTab(PathBuf),
    ClearRecents,
    EmptyTrash,
    #[cfg(feature = "desktop")]
    ExecEntryAction(Option<PathBuf>, usize),
    Gallery(bool),
    GalleryPrevious,
    GalleryNext,
    GalleryToggle,
    GoNext,
    GoPrevious,
    ItemDown,
    ItemLeft,
    ItemPageDown,
    ItemPageUp,
    ItemRight,
    ItemUp,
    Location(Location),
    LocationUp,
    Open(Option<PathBuf>),
    Reload,
    RightClick(Option<Point>, Option<usize>),
    MiddleClick(usize),
    Resize(Rectangle),
    Scroll(Viewport),
    ScrollTab(f32),
    ScrollToFocused,
    /// Apply the scroll offset restored from history once the items are in
    ScrollRestore,
    SearchContext(Location, SearchContextWrapper),
    SearchReady(bool),
    SelectAll,
    SelectFirst,
    SelectLast,
    SetOpenWith(Mime, String),
    RunContextAction(usize),
    SetPermissions(PathBuf, u32),
    ShiftPermissions(Option<(PathBuf, u32)>, u32, u32),
    SetSort(HeadingOptions, bool),
    TabComplete(PathBuf, Vec<(String, PathBuf)>),
    Thumbnail(PathBuf, ItemThumbnail),
    ToggleSort(HeadingOptions),
    ColumnResizeStart(ColumnDivider),
    ColumnResizeDrag(f32),
    ColumnResizeEnd,
    WindowDrag,
    WindowToggleMaximize,
    ZoomIn,
    ZoomOut,
    HighlightDeactivate(usize),
    HighlightActivate(usize),
    DirectorySize(PathBuf, DirSize),
    DirectoryChildren(PathBuf, usize),
    Checksums(PathBuf, ChecksumState),
    CalculateChecksums(PathBuf),
    CopyChecksum(String),
    ImageDecoded(PathBuf, u32, u32, Vec<u8>, Option<(u32, u32)>, u64), // path, width, height, pixels, display_size, generation
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LocationMenuAction {
    OpenInNewTab(usize),
    OpenInNewWindow(usize),
    Preview(usize),
    AddToSidebar(usize),
}

impl MenuAction for LocationMenuAction {
    type Message = Message;

    fn message(&self) -> Self::Message {
        Message::LocationMenuAction(*self)
    }
}

#[derive(Clone, Debug)]
pub enum DirSize {
    Calculating(Controller),
    Directory(u64),
    NotDirectory,
    Error(String),
}

/// Checksums computed for a file. Only SHA256 is currently exposed; see
/// [`calculate_checksums`] for how to add more.
#[derive(Clone, Debug, Default)]
pub struct FileChecksums {
    pub sha256: String,
}

/// State of checksum computation for a file.
#[derive(Clone, Debug, Default)]
pub enum ChecksumState {
    #[default]
    NotCalculated,
    Calculating,
    Calculated(FileChecksums),
    Error(String),
}

#[derive(Clone, Debug)]
pub enum ItemMetadata {
    Path {
        metadata: Metadata,
        children_opt: Option<usize>,
    },
    Trash {
        metadata: trash::TrashItemMetadata,
        entry: trash::TrashItem,
    },
    SimpleDir {
        entries: u64,
    },
    SimpleFile {
        size: u64,
    },
    #[cfg(feature = "gvfs")]
    GvfsPath {
        mtime: u64,
        size_opt: Option<u64>,
        children_opt: Option<usize>,
        is_dir: bool,
    },
}

impl ItemMetadata {
    pub fn is_dir(&self) -> bool {
        match self {
            Self::Path { metadata, .. } => metadata.is_dir(),
            Self::Trash { metadata, .. } => match metadata.size {
                trash::TrashItemSize::Entries(_) => true,
                trash::TrashItemSize::Bytes(_) => false,
            },
            Self::SimpleDir { .. } => true,
            Self::SimpleFile { .. } => false,
            #[cfg(feature = "gvfs")]
            Self::GvfsPath { is_dir, .. } => *is_dir,
        }
    }

    pub fn modified(&self) -> Option<SystemTime> {
        match self {
            Self::Path { metadata, .. } => metadata.modified().ok(),
            #[cfg(feature = "gvfs")]
            Self::GvfsPath { mtime, .. } => {
                SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(*mtime))
            }
            _ => None,
        }
    }

    pub fn file_size(&self) -> Option<u64> {
        match self {
            Self::Path { metadata, .. } => (!metadata.is_dir()).then_some(metadata.len()),
            Self::Trash { metadata, .. } => match metadata.size {
                TrashItemSize::Bytes(size) => Some(size),
                TrashItemSize::Entries(_) => None,
            },
            #[cfg(feature = "gvfs")]
            Self::GvfsPath { size_opt, .. } => *size_opt,
            _ => None,
        }
    }

    pub fn children_count(&self) -> Option<&usize> {
        match &self {
            ItemMetadata::Path { children_opt, .. } => children_opt.as_ref(),
            #[cfg(feature = "gvfs")]
            ItemMetadata::GvfsPath { children_opt, .. } => children_opt.as_ref(),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum ItemThumbnail {
    NotImage,
    Image(widget::image::Handle, Option<(u32, u32)>),
    Svg(widget::svg::Handle),
    Text(widget::text_editor::Content<crate::ui::Renderer>),
}

impl Clone for ItemThumbnail {
    fn clone(&self) -> Self {
        match self {
            Self::NotImage => Self::NotImage,
            Self::Image(handle, size_opt) => Self::Image(handle.clone(), *size_opt),
            Self::Svg(handle) => Self::Svg(handle.clone()),
            // Content cannot be cloned simply
            Self::Text(content) => {
                Self::Text(widget::text_editor::Content::with_text(&content.text()))
            }
        }
    }
}

impl ItemThumbnail {
    pub fn new(
        path: &Path,
        metadata: ItemMetadata,
        mime: mime::Mime,
        mut thumbnail_size: u32,
        max_mem: u64,
        jobs: usize,
        max_size_mb: u64,
    ) -> Self {
        let thumbnail_cacher =
            ThumbnailCacher::new(path, ThumbnailSize::from_pixel_size(thumbnail_size));
        match thumbnail_cacher.as_ref() {
            Ok(cache) => match cache.get_cached_thumbnail() {
                CachedThumbnail::Valid((thumbnail_path, size)) => {
                    // Check original image dimensions even when loading cached thumbnail
                    // This prevents trying to load huge images in preview mode
                    let original_dims = match image::image_dimensions(path) {
                        Ok((width, height)) => Some((width, height)),
                        Err(_) => size.map(|s| (s.pixel_size(), s.pixel_size())),
                    };

                    return Self::Image(
                        widget::image::Handle::from_path(thumbnail_path),
                        original_dims,
                    );
                }
                CachedThumbnail::Failed => {
                    if mime.type_() != mime::IMAGE {
                        return Self::NotImage;
                    }
                }
                CachedThumbnail::RequiresUpdate(size) => {
                    thumbnail_size = size.pixel_size();
                }
            },
            Err(err) => {
                log::warn!(
                    "failed to create ThumbnailCache for {}: {}",
                    path.display(),
                    err
                );
            }
        }

        let size = metadata.file_size().unwrap_or_default();
        let check_size = |thumbnailer: &str, max_size| {
            if size <= max_size {
                true
            } else {
                log::warn!(
                    "skipping internal {} thumbnailer for {}: file size {} is larger than {}",
                    thumbnailer,
                    path.display(),
                    format_size(size),
                    format_size(max_size)
                );
                false
            }
        };

        let mut tried_supported_file = false;
        // First try built-in image thumbnailer
        if mime.type_() == mime::IMAGE && check_size("image", max_size_mb * 1000 * 1000) {
            // Check if image dimensions would exceed available memory budget
            // The GPU tiling system can handle large images, but we still need to decode them first
            let dimensions_ok = match image::image_dimensions(path) {
                Ok((width, height)) => {
                    if exceeds_memory_limit(width, height, max_mem) {
                        log::warn!(
                            "skipping thumbnail generation for {}: {}x{} image would exceed {}MB memory budget",
                            path.display(),
                            width,
                            height,
                            max_mem
                        );
                        false
                    } else {
                        if should_use_tiling(width, height) {
                            log::info!(
                                "Large image {}x{} detected, will use GPU tiling for display",
                                width,
                                height
                            );
                        }
                        true
                    }
                }
                Err(err) => {
                    log::debug!(
                        "failed to read dimensions for {}: {}, will try decoding",
                        path.display(),
                        err
                    );
                    true // If we can't read dimensions, try anyway
                }
            };

            if !dimensions_ok {
                // Skip this image entirely since it is too large to safely decode
                return Self::NotImage;
            }

            tried_supported_file = true;
            let dyn_img = match image::ImageReader::open(path)
                .and_then(image::ImageReader::with_guessed_format)
            {
                Ok(mut reader) => {
                    let mut limits = image::Limits::default();
                    let max_ram = max_mem * 1000 * 1000 / jobs as u64;
                    limits.max_alloc = Some(max_ram);
                    reader.limits(limits);
                    match reader.decode() {
                        Ok(reader) => Some(reader),
                        Err(err) => {
                            log::warn!("failed to decode {}: {}", path.display(), err);
                            None
                        }
                    }
                }
                Err(err) => {
                    log::warn!("failed to read {}: {}", path.display(), err);
                    None
                }
            };

            if let Some(dyn_img) = dyn_img {
                let (img_width, img_height) = (dyn_img.width(), dyn_img.height());

                if let Ok(cacher) = thumbnail_cacher.as_ref() {
                    match cacher.update_with_image(dyn_img) {
                        Ok(thumb_path) => {
                            return Self::Image(
                                widget::image::Handle::from_path(thumb_path),
                                Some((img_width, img_height)),
                            );
                        }
                        Err(err) => {
                            log::warn!("cacher failed to decode {}: {}", path.display(), err);
                        }
                    }
                } else {
                    // Fallback for when thumbnail cacher isn't available.
                    let thumbnail = dyn_img
                        .thumbnail(thumbnail_size, thumbnail_size)
                        .into_rgba8();
                    return Self::Image(
                        widget::image::Handle::from_rgba(
                            thumbnail.width(),
                            thumbnail.height(),
                            thumbnail.into_raw(),
                        ),
                        Some((img_width, img_height)),
                    );
                }
            }
        }

        // Try external thumbnailers.
        let thumbnail_dir = thumbnail_cacher
            .as_ref()
            .ok()
            .map(ThumbnailCacher::thumbnail_dir);
        if let Some((item_thumbnail, temp_file)) =
            Self::generate_thumbnail_external(path, &mime, thumbnail_size, thumbnail_dir)
        {
            if let Ok(cache) = thumbnail_cacher
                && let Err(err) = cache.update_with_temp_file(temp_file)
            {
                log::warn!("failed to update cache for {}: {}", path.display(), err);
            }
            return item_thumbnail;
        }

        tried_supported_file = tried_supported_file || !thumbnailer(&mime).is_empty();

        // Try internal thumbnailers that don't get cached.
        if mime.type_() == mime::IMAGE
            && mime.subtype() == mime::SVG
            && check_size("svg", 8 * 1000 * 1000)
        {
            tried_supported_file = true;
            // Try built-in svg thumbnailer
            match fs::read(path) {
                Ok(data) => {
                    return Self::Svg(widget::svg::Handle::from_memory(data));
                }
                Err(err) => {
                    log::warn!("failed to read {}: {}", path.display(), err);
                }
            }
        } else if mime.type_() == mime::TEXT && check_size("text", TEXT_PREVIEW_MAX_FILE_BYTES) {
            tried_supported_file = true;
            if size > 0 {
                // Reuse size from metadata above; cap allocation and read
                let read_cap = (size.min(TEXT_PREVIEW_MAX_BYTES as u64)) as usize;
                let mut buf = vec![0u8; read_cap];
                match File::open(path).and_then(|f| {
                    let n = Read::read(&mut f.take(read_cap as u64), &mut buf)?;
                    buf.truncate(n);
                    Ok(())
                }) {
                    Ok(()) => {
                        let text = match std::str::from_utf8(&buf) {
                            Ok(s) => s.to_string(),
                            Err(e) => {
                                // Use only the valid UTF-8 prefix (slice is guaranteed valid by valid_up_to())
                                std::str::from_utf8(&buf[..e.valid_up_to()])
                                    .unwrap_or("")
                                    .to_string()
                            }
                        };
                        if !text.is_empty() {
                            return Self::Text(widget::text_editor::Content::with_text(&text));
                        }
                    }
                    Err(err) => {
                        log::warn!("failed to read {}: {}", path.display(), err);
                    }
                }
            }
            // size == 0: empty file or unknown size; skip read and allocation
        }

        // If we weren't able to create a thumbnail, but we should have
        // been able to, create a fail marker so that it isn't tried the
        // next time.
        if let Ok(cacher) = thumbnail_cacher
            && tried_supported_file
            && let Err(err) = cacher.create_fail_marker()
        {
            log::warn!(
                "failed to create thumbnail fail marker for {}: {}",
                path.display(),
                err
            );
        }

        Self::NotImage
    }

    fn generate_thumbnail_external(
        path: &Path,
        mime: &mime::Mime,
        thumbnail_size: u32,
        thumbnail_dir: Option<&Path>,
    ) -> Option<(Self, NamedTempFile)> {
        // Try external thumbnailers
        for thumbnailer in thumbnailer(mime) {
            let is_evince = thumbnailer.exec.starts_with("evince-thumbnailer ");
            let prefix = if is_evince {
                "gnome-desktop-"
            } else {
                "earth-files-"
            };

            // It's preferable to create the tempfile in the same directory as the final cached
            // thumbnail to ensure that no copies across filesytems need to be made. However,
            // the apparmor config for evince-thumbnailer does not allow this, so we need to
            // fallback to the system tempdir.
            let dir = if is_evince { None } else { thumbnail_dir };
            let file = match dir {
                Some(d) => tempfile::Builder::new().prefix(prefix).tempfile_in(d),
                None => tempfile::Builder::new().prefix(prefix).tempfile(),
            };
            let file = match file {
                Ok(ok) => ok,
                Err(err) => {
                    log::warn!(
                        "failed to create temporary file for thumbnail of {}: {}",
                        path.display(),
                        err
                    );
                    continue;
                }
            };

            let Some(mut command) = thumbnailer.command(path, file.path(), thumbnail_size) else {
                continue;
            };
            match command.status() {
                Ok(status) => {
                    if status.success() {
                        match image::ImageReader::open(file.path())
                            .and_then(ImageReader::with_guessed_format)
                        {
                            Ok(reader) => match reader.decode().map(DynamicImage::into_rgba8) {
                                Ok(image) => {
                                    return Some((
                                        Self::Image(
                                            widget::image::Handle::from_rgba(
                                                image.width(),
                                                image.height(),
                                                image.into_raw(),
                                            ),
                                            None,
                                        ),
                                        file,
                                    ));
                                }
                                Err(err) => {
                                    log::warn!("failed to decode {}: {}", path.display(), err);
                                }
                            },
                            Err(err) => {
                                log::warn!("failed to read {}: {}", path.display(), err);
                            }
                        }
                    } else {
                        log::warn!(
                            "failed to run {:?} for {}: {}",
                            thumbnailer,
                            path.display(),
                            status
                        );
                    }
                }
                Err(err) => {
                    log::warn!(
                        "failed to run {thumbnailer:?} for {}: {}",
                        path.display(),
                        err
                    );
                }
            }
        }

        None
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub is_mount_point: bool,
    pub display_name: String,
    pub metadata: ItemMetadata,
    pub hidden: bool,
    pub location_opt: Option<Location>,
    pub mime: Mime,
    /// Pixel size of an image, filled when it is first needed.
    ///
    /// The scan paths that already know it fill it in up front. The rest
    /// leave it empty, because items are also built while handling input and
    /// filesystem notifications, where opening a file to read its header
    /// would block the interface. Only the details pane fills it on demand,
    /// for the one item it is describing.
    pub image_dimensions: OnceCell<Option<(u32, u32)>>,
    pub icon_handle_grid: widget::icon::Handle,
    pub icon_handle_list: widget::icon::Handle,
    pub icon_handle_list_condensed: widget::icon::Handle,
    pub thumbnail_opt: Option<ItemThumbnail>,
    pub button_id: widget::Id,
    pub pos_opt: Cell<Option<(usize, usize)>>,
    pub rect_opt: Cell<Option<Rectangle>>,
    pub selected: bool,
    pub highlighted: bool,
    pub cut: bool,
    pub overlaps_drag_rect: bool,
    pub dir_size: DirSize,
    pub checksums: ChecksumState,
}

impl Item {
    /// Rebuild this item from disk after its contents changed, so the mime,
    /// icons and thumbnail follow the new data. Selection and layout state
    /// the view relies on is kept.
    pub fn refresh(&mut self, sizes: IconSizes) -> Result<(), String> {
        let path = self.path_opt().ok_or("item has no path")?.clone();
        let fresh = item_from_path(path, sizes)?;
        *self = Item {
            button_id: self.button_id.clone(),
            pos_opt: Cell::new(self.pos_opt.get()),
            rect_opt: Cell::new(self.rect_opt.get()),
            selected: self.selected,
            highlighted: self.highlighted,
            cut: self.cut,
            overlaps_drag_rect: self.overlaps_drag_rect,
            ..fresh
        };
        Ok(())
    }

    fn display_name(name: &str) -> String {
        // In order to wrap at periods and underscores, add a zero width space after each one
        name.replace('.', ".\u{200B}").replace('_', "_\u{200B}")
    }

    /// Text widget for a filename in grid/icon view: word-or-glyph wrapping, middle-ellipsized to 3 lines.
    fn grid_display_name<'a>(
        name: impl Into<Cow<'a, str>> + 'a,
    ) -> widget::Ellipsize<'a, crate::ui::Theme, crate::ui::Renderer> {
        widget::ellipsize::body(name, widget::EllipsizeMode::Middle(3))
            .wrapping(text::Wrapping::WordOrGlyph)
            .align_x(text::Alignment::Center)
    }

    /// Text widget for a filename in list view: word-or-glyph wrapping, middle-ellipsized to 1 line.
    fn list_display_name<'a>(
        name: impl Into<Cow<'a, str>> + 'a,
    ) -> widget::Ellipsize<'a, crate::ui::Theme, crate::ui::Renderer> {
        widget::ellipsize::body(name, widget::EllipsizeMode::Middle(1))
            .wrapping(text::Wrapping::WordOrGlyph)
    }

    pub fn path_opt(&self) -> Option<&PathBuf> {
        self.location_opt.as_ref()?.path_opt()
    }

    pub fn can_gallery(&self) -> bool {
        self.mime.type_() == mime::IMAGE || self.mime.type_() == mime::TEXT
    }

    pub fn file_metadata(&self) -> Option<Metadata> {
        match &self.metadata {
            ItemMetadata::Path { metadata, .. } => Some(metadata.clone()),
            // Trashed and GVFS items have a readable path of their own
            _ => self.path_opt().and_then(|p| fs::metadata(p).ok()),
        }
    }

    fn preview(&self) -> Element<'_, Message> {
        let spacing = spacing();
        // This loads the image only if thumbnailing worked
        let icon = widget::icon::icon(self.icon_handle_grid.clone())
            .content_fit(ContentFit::Contain)
            .size(IconSizes::default().grid())
            .into();
        match self
            .thumbnail_opt
            .as_ref()
            .unwrap_or(&ItemThumbnail::NotImage)
        {
            ItemThumbnail::NotImage => icon,
            ItemThumbnail::Image(handle, _original_dims) => {
                // Preview pane: ALWAYS show thumbnail for instant, responsive UI
                // Full resolution loading happens in gallery mode
                widget::image(handle.clone()).into()
            }
            ItemThumbnail::Svg(handle) => widget::svg(handle.clone()).into(),
            ItemThumbnail::Text(content) => widget::text_editor::text_editor(content)
                .style(text_editor_class)
                .width(THUMBNAIL_SIZE as f32)
                .height(Length::Fixed(THUMBNAIL_SIZE as f32))
                .padding(spacing.space_xxs)
                .into(),
        }
    }

    pub fn preview_actions(&self) -> Element<'_, Message> {
        let mut row = widget::Row::with_capacity(3)
            .align_y(Alignment::Center)
            .spacing((spacing().space_xxs).to_pixels())
            .push(
                widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                    .on_press(Message::ItemLeft),
            )
            .push(
                widget::button::icon(widget::icon::from_name("go-next-symbolic"))
                    .on_press(Message::ItemRight),
            );
        if self.can_gallery()
            && let Some(_path) = self.path_opt()
        {
            row = row.push(
                widget::button::icon(widget::icon::from_name("view-fullscreen-symbolic"))
                    .on_press(Message::Gallery(true)),
            );
        }
        row.into()
    }

    pub fn preview_view<'a>(
        &'a self,
        mime_app_cache_opt: Option<&'a mime_app::MimeAppCache>,
    ) -> Element<'a, Message> {
        let Spacing {
            space_xxxs,
            space_m,
            ..
        } = spacing();

        let mut column = widget::Column::with_capacity(4).spacing(space_m.to_pixels());

        column = column.push(
            widget::container(self.preview())
                .center_x(Length::Fill)
                .max_height(THUMBNAIL_SIZE as f32),
        );

        let mut details = widget::Column::with_capacity(8).spacing(space_xxxs.to_pixels());
        details = details.push(widget::selectable_text::heading(self.name.clone()));
        details = details.push(widget::text::body(fl!(
            "type",
            mime = self.mime.to_string()
        )));
        let mut settings = Vec::new();
        if let Some(mime_app_cache) = mime_app_cache_opt {
            let mime_apps = mime_app_cache.get_apps_for_mime(&self.mime, false);
            if !mime_apps.is_empty() {
                let (names, icons) = mime_apps
                    .iter()
                    .map(|(app, _)| (Cow::Owned(app.name.clone()), app.icon()))
                    .collect::<(Vec<_>, Vec<_>)>();
                settings.push(
                    widget::settings::item::builder(fl!("open-with")).control(
                        Element::from(
                            widget::dropdown(
                                names,
                                mime_apps.iter().position(|(x, _)| x.is_default(&self.mime)),
                                move |index| index,
                            )
                            .icons(Cow::Owned(icons)),
                        )
                        .map(move |index| {
                            let mime_app = &mime_apps[index].0;
                            Message::SetOpenWith(self.mime.clone(), mime_app.id.clone())
                        }),
                    ),
                );
            }
        }

        if let Some(metadata) = self.file_metadata() {
            if metadata.is_dir() {
                if let Some(children) = self.metadata.children_count() {
                    details = details.push(widget::text::body(fl!("items", items = children)));
                }
                let size = match &self.dir_size {
                    DirSize::Calculating(_) => fl!("calculating"),
                    DirSize::Directory(size) => format_size(*size),
                    DirSize::NotDirectory => String::new(),
                    DirSize::Error(err) => err.clone(),
                };
                if !size.is_empty() {
                    details = details.push(widget::text::body(fl!("item-size", size = size)));
                }
            } else {
                details = details.push(widget::text::body(fl!(
                    "item-size",
                    size = format_size(metadata.len())
                )));
            }

            let date_time_formatter = date_time_formatter();
            let time_formatter = time_formatter();

            if let Ok(time) = metadata.created() {
                details = details.push(widget::selectable_text::body(fl!(
                    "item-created",
                    created = format_time(time, &date_time_formatter, &time_formatter).to_string()
                )));
            }

            if let Ok(time) = metadata.modified() {
                details = details.push(widget::selectable_text::body(fl!(
                    "item-modified",
                    modified = format_time(time, &date_time_formatter, &time_formatter).to_string()
                )));
            }

            if let Ok(time) = metadata.accessed() {
                details = details.push(widget::selectable_text::body(fl!(
                    "item-accessed",
                    accessed = format_time(time, &date_time_formatter, &time_formatter).to_string()
                )));
            }

            if let Some(path) = self.path_opt() {
                use std::os::unix::fs::MetadataExt;

                let mode = metadata.mode();

                let user_name = user_name(metadata.uid());
                let user_path = path.clone();
                settings.push(
                    widget::settings::item::builder(user_name)
                        .description(fl!("owner"))
                        .control(widget::dropdown(
                            Cow::Borrowed(MODE_NAMES.as_slice()),
                            Some(get_mode_part(mode, MODE_SHIFT_USER).try_into().unwrap()),
                            move |selected| {
                                Message::SetPermissions(
                                    user_path.clone(),
                                    set_mode_part(
                                        mode,
                                        MODE_SHIFT_USER,
                                        selected.try_into().unwrap(),
                                    ),
                                )
                            },
                        )),
                );

                let group_name = group_name(metadata.gid());
                let group_path = path.clone();
                settings.push(
                    widget::settings::item::builder(group_name)
                        .description(fl!("group"))
                        .control(widget::dropdown(
                            Cow::Borrowed(MODE_NAMES.as_slice()),
                            Some(get_mode_part(mode, MODE_SHIFT_GROUP).try_into().unwrap()),
                            move |selected| {
                                Message::SetPermissions(
                                    group_path.clone(),
                                    set_mode_part(
                                        mode,
                                        MODE_SHIFT_GROUP,
                                        selected.try_into().unwrap(),
                                    ),
                                )
                            },
                        )),
                );

                let other_path = path.clone();
                settings.push(widget::settings::item::builder(fl!("other")).control(
                    widget::dropdown(
                        Cow::Borrowed(MODE_NAMES.as_slice()),
                        Some(get_mode_part(mode, MODE_SHIFT_OTHER).try_into().unwrap()),
                        move |selected| {
                            Message::SetPermissions(
                                other_path.clone(),
                                set_mode_part(mode, MODE_SHIFT_OTHER, selected.try_into().unwrap()),
                            )
                        },
                    ),
                ));
            }
        }

        // Only what the scan already read. Opening the file here would put a
        // filesystem read in the render pass, which on a slow mount freezes
        // the window; an item built outside a scan simply shows no size.
        if let Some((width, height)) = self.image_dimensions.get().copied().flatten() {
            details = details.push(widget::text::body(format!("{width}x{height}")));
        }
        column = column.push(details);

        if let Some(metadata) = self.file_metadata()
            && !metadata.is_dir()
            && let Some(path) = self.path_opt()
        {
            let control: Element<'_, Message> = match &self.checksums {
                ChecksumState::NotCalculated => widget::button::standard(fl!("calculate"))
                    .on_press(Message::CalculateChecksums(path.clone()))
                    .into(),
                ChecksumState::Calculating => widget::Row::with_capacity(2)
                    .align_y(Alignment::Center)
                    .spacing(space_xxxs.to_pixels())
                    .push(widget::indeterminate_circular().size(16.0))
                    .push(widget::text::body(fl!("calculating")))
                    .into(),
                ChecksumState::Calculated(checksums) => {
                    let value = checksums.sha256.clone();
                    // Middle-ellipsize the digest to fit, full value on hover.
                    let value_text = widget::tooltip(
                        widget::ellipsize::body(value.clone(), widget::EllipsizeMode::Middle(1))
                            .font(crate::ui::font::mono())
                            .width(Length::Fill)
                            .wrapping(text::Wrapping::None),
                        widget::text::body(value.clone()),
                        widget::tooltip::Position::Bottom,
                    );
                    let copy_button = widget::button::icon(
                        widget::icon::from_name("edit-copy-symbolic").size(16),
                    )
                    .on_press(Message::CopyChecksum(value.clone()))
                    .tooltip(fl!("copy"));
                    widget::Row::with_capacity(2)
                        .align_y(Alignment::Center)
                        .spacing(space_xxxs.to_pixels())
                        .push(value_text)
                        .push(copy_button)
                        .into()
                }
                ChecksumState::Error(err) => {
                    widget::text::body(format!("{}: {}", fl!("error"), err)).into()
                }
            };
            settings.push(
                widget::settings::item::builder(fl!("checksum", kind = "SHA256")).control(control),
            );
        }

        if let Some(path) = self.path_opt()
            && self.selected
        {
            column = column.push(
                widget::button::standard(fl!("open")).on_press(Message::Open(Some(path.clone()))),
            );
        }

        if !settings.is_empty() {
            let mut section = widget::settings::section();
            section = section.extend(settings);
            column = column.push(section);
        }

        column.into()
    }

    pub fn replace_view(&self, heading: String) -> Element<'_, Message> {
        let Spacing { space_xxxs, .. } = spacing();

        let mut row = widget::Row::with_capacity(2).spacing(space_xxxs.to_pixels());
        row = row.push(self.preview());

        let mut column = widget::Column::with_capacity(3).spacing(space_xxxs.to_pixels());
        column = column.push(widget::text::heading(heading));

        if let Some(metadata) = self.file_metadata() {
            if metadata.is_dir() {
                if let Some(children) = self.metadata.children_count() {
                    column = column.push(widget::text::body(fl!("items", items = children)));
                }
            } else {
                column = column.push(widget::text::body(fl!(
                    "item-size",
                    size = format_size(metadata.len())
                )));
            }
            if let Ok(time) = metadata.modified() {
                let date_time_formatter = date_time_formatter();
                let time_formatter = time_formatter();

                column = column.push(widget::text::body(fl!(
                    "item-modified",
                    modified = format_time(time, &date_time_formatter, &time_formatter).to_string()
                )));
            }
        }

        row = row.push(column);
        row.into()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum View {
    Grid,
    List,
}
/// Room the fixed list-view columns must leave for everything left of them:
/// outer padding, the icon, the gaps and the name text.
pub const NAME_MIN: f32 = 300.0;
/// Narrowest a fixed column can be dragged or shrunk to.
pub const COLUMN_MIN: f32 = 48.0;
/// How far a divider's grab zone extends into the column on its left, on top
/// of the gap between cells.
const DIVIDER_GRAB: f32 = 6.0;

/// A draggable boundary between two list-view columns. Dragging it transfers
/// width between its two neighbours, so the boundary follows the pointer.
/// The last column ends at the frame edge, which cannot move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColumnDivider {
    NameModified,
    ModifiedSize,
    /// Only while the Type column is shown
    SizeType,
}

/// Requested widths of the fixed list-view columns, in pixels. Kept per tab
/// for the session; not saved. Name takes whatever is left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColumnWidths {
    pub modified: f32,
    pub size: f32,
    pub type_: f32,
}

impl ColumnWidths {
    fn fixed_total(&self, show_type: bool) -> f32 {
        self.modified + self.size + if show_type { self.type_ } else { 0.0 }
    }
}

/// The requested widths fitted into the current view width: what is laid out.
#[derive(Clone, Copy, Debug)]
struct ListGeometry {
    /// Effective widths, each at least [`COLUMN_MIN`]
    widths: ColumnWidths,
    show_type: bool,
    /// View width minus the effective fixed widths, at least [`NAME_MIN`]
    name_room: f32,
}

/// A divider drag in progress, with the effective widths when it started.
#[derive(Clone, Copy, Debug)]
struct ColumnResize {
    divider: ColumnDivider,
    start: ColumnWidths,
    start_name_room: f32,
}

impl Default for ColumnWidths {
    fn default() -> Self {
        Self {
            modified: 200.0,
            size: 100.0,
            type_: 120.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, PartialOrd, Ord, Eq, Deserialize, Serialize)]
pub enum HeadingOptions {
    Name = 0,
    Modified,
    Size,
    TrashedOn,
    Type,
}

impl fmt::Display for HeadingOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name => write!(f, "{}", fl!("name")),
            Self::Modified => write!(f, "{}", fl!("modified")),
            Self::Size => write!(f, "{}", fl!("size")),
            Self::TrashedOn => write!(f, "{}", fl!("trashed-on")),
            Self::Type => write!(f, "{}", fl!("type-heading")),
        }
    }
}

impl HeadingOptions {
    pub fn names() -> Vec<String> {
        vec![
            Self::Name.to_string(),
            Self::Modified.to_string(),
            Self::Size.to_string(),
            Self::TrashedOn.to_string(),
            Self::Type.to_string(),
        ]
    }
}

#[derive(Clone, Debug)]
pub enum Mode {
    App,
    Dialog(DialogKind),
}

impl Mode {
    /// Whether multiple files can be selected in this mode
    pub fn multiple(&self) -> bool {
        match self {
            Self::App => true,
            Self::Dialog(dialog) => dialog.multiple(),
        }
    }
}

struct SearchContext {
    results_rx: mpsc::Receiver<SearchItem>,
    ready: Arc<atomic::AtomicBool>,
    last_modified_opt: Arc<RwLock<Option<SystemTime>>>,
}

pub struct SearchContextWrapper(Option<SearchContext>);

impl Clone for SearchContextWrapper {
    fn clone(&self) -> Self {
        Self(None)
    }
}

impl fmt::Debug for SearchContextWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SearchContextWrapper").finish()
    }
}

pub struct Tab {
    pub location: Location,
    pub location_ancestors: Vec<(Location, String)>,
    pub location_title: String,
    /// Breadcrumb whose context menu is open, drawn as active while it shows
    pub location_context_menu_index: Option<usize>,
    pub mode: Mode,
    pub scroll_opt: Option<AbsoluteOffset>,
    pub size_opt: Cell<Option<Size>>,
    pub content_height_opt: Cell<Option<f32>>,
    pub viewport_opt: Option<Rectangle>,
    pub item_view_size_opt: Cell<Option<Size>>,
    pub edit_location: Option<EditLocation>,
    pub edit_location_id: widget::Id,
    pub history_i: usize,
    pub history: Vec<Location>,
    /// Scroll offset of each history entry when it was left, restored on
    /// going back or forward
    history_scroll: Vec<Option<AbsoluteOffset>>,
    /// Offset to apply once the items of a restored history entry are in
    pending_scroll: Option<AbsoluteOffset>,
    pub config: TabConfig,
    pub thumb_config: ThumbCfg,
    pub sort_name: HeadingOptions,
    pub sort_direction: bool,
    pub gallery: bool,
    pub(crate) parent_item_opt: Option<Box<Item>>,
    pub(crate) items_opt: Option<Vec<Item>>,
    // `iced_core::widget::Id` has no `Display` and a private `Internal`, so the
    // name cannot be recovered from an `Id`. Store it alongside the id; every
    // producer already has it as a literal.
    pub(crate) scrollable_id: widget::Id,
    /// The string `scrollable_id` was built from; see the note above.
    pub(crate) scrollable_name: std::borrow::Cow<'static, str>,
    select_focus: Option<usize>,
    select_range: Option<(usize, usize)>,
    clicked: Option<usize>,
    /// Set by a double click so the release that follows it does not open the
    /// item a second time. In single-click mode the first release has already
    /// opened it, and a double click is still two presses and two releases.
    opened_by_this_click: bool,
    last_right_click: Option<usize>,
    search_context: Option<SearchContext>,
    date_time_formatter: DateTimeFormatter<fieldsets::YMDT>,
    time_formatter: DateTimeFormatter<fieldsets::T>,
    watch_drag: bool,
    /// This tab started the file drag currently in flight, so its `on_drag`
    /// must not start a second one for every further pixel of motion.
    dnd_source: bool,
    /// The item a live file drag is over and would drop into, drawn as
    /// highlighted. Only ever a directory.
    dnd_target: Option<usize>,
    /// The breadcrumb ancestor under a file drag, indexed by `Location::path_opt()`'s
    /// `ancestors()`. This is the index used by `location_context_menu_index`; the drag
    /// highlight uses the same `Button::LinkActive` style as an open breadcrumb context
    /// menu.
    dnd_ancestor: Option<usize>,
    window_id: Option<window::Id>,
    large_image_manager: LargeImageManager,
    column_widths: ColumnWidths,
    column_resize: Option<ColumnResize>,
}

async fn calculate_dir_size(path: &Path, controller: Controller) -> Result<u64, OperationError> {
    let mut total = 0;
    for entry_res in WalkDir::new(path) {
        controller
            .check()
            .await
            .map_err(|s| OperationError::from_state(s, &controller))?;

        if let Ok(entry) = entry_res
            && let Ok(metadata) = entry.metadata()
            && metadata.is_file()
        {
            total += metadata.len();
        }

        // Yield in case this process takes a while.
        tokio::task::yield_now().await;
    }
    Ok(total)
}

/// Calculate file checksums in a single pass over the file. To add another
/// digest, hash it alongside `sha256_hasher` in the loop below and add a field
/// to [`FileChecksums`]; the file is only read once.
async fn calculate_checksums(path: &Path) -> Result<FileChecksums, String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = File::open(&path).map_err(|e| e.to_string())?;
        let mut sha256_hasher = Sha256::new();

        let mut buffer = [0u8; 8192];
        loop {
            let bytes_read = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if bytes_read == 0 {
                break;
            }
            sha256_hasher.update(&buffer[..bytes_read]);
        }

        Ok(FileChecksums {
            sha256: crate::hex::lower(sha256_hasher.finalize()),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn folder_name<P: AsRef<Path>>(path: P) -> (String, bool) {
    let path = path.as_ref();
    let mut found_home = false;
    let name = match path.file_name() {
        Some(name) => {
            if path == crate::home_dir() {
                found_home = true;
                fl!("home")
            } else {
                match (get_filename_from_path(path), fs::metadata(path)) {
                    (Ok(name), Ok(metadata)) => {
                        let is_gvfs = fs_kind(&metadata) == FsKind::Gvfs;
                        display_name_for_file(path, &name, is_gvfs, false)
                    }
                    _ => name.to_string_lossy().into_owned(),
                }
            }
        }
        None => {
            fl!("filesystem")
        }
    };
    (name, found_home)
}

// parse .hidden file and return files path
pub fn parse_hidden_file(path: &PathBuf) -> Box<[String]> {
    let Ok(file) = File::open(path) else {
        return Default::default();
    };

    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let line = line.trim();
            (!line.is_empty()).then_some(line.to_owned())
        })
        .collect()
}

impl Tab {
    pub fn new(
        location: Location,
        config: TabConfig,
        thumb_config: ThumbCfg,
        sorting_options: Option<&FxOrderMap<String, (HeadingOptions, bool)>>,
        scrollable_name: std::borrow::Cow<'static, str>,
        window_id: Option<window::Id>,
    ) -> Self {
        let scrollable_id = match scrollable_name.clone() {
            std::borrow::Cow::Borrowed(name) => widget::Id::new(name),
            std::borrow::Cow::Owned(name) => widget::Id::from(name),
        };
        let location_str = location.to_string();
        let (sort_name, sort_direction) = sorting_options
            .and_then(|opts| opts.get(&location_str))
            .or_else(|| SORT_OPTION_FALLBACK.get(&location_str))
            .copied()
            .unwrap_or((HeadingOptions::Name, true));
        let location = location.normalize();
        let location_ancestors = location.ancestors();
        let location_title = location.title();
        let history = vec![location.clone()];
        Self {
            location,
            location_ancestors,
            location_title,
            location_context_menu_index: None,
            mode: Mode::App,
            scroll_opt: None,
            size_opt: Cell::new(None),
            content_height_opt: Cell::new(None),
            viewport_opt: None,
            item_view_size_opt: Cell::new(None),
            edit_location: None,
            edit_location_id: widget::Id::unique(),
            history_i: 0,
            history_scroll: vec![None; history.len()],
            pending_scroll: None,
            history,
            config,
            thumb_config,
            sort_name,
            sort_direction,
            gallery: false,
            parent_item_opt: None,
            items_opt: None,
            scrollable_id,
            scrollable_name,
            select_focus: None,
            select_range: None,
            clicked: None,
            opened_by_this_click: false,
            last_right_click: None,
            search_context: None,
            date_time_formatter: date_time_formatter(),
            time_formatter: time_formatter(),
            watch_drag: true,
            dnd_source: false,
            dnd_target: None,
            dnd_ancestor: None,
            window_id,
            large_image_manager: LargeImageManager::new(),
            column_widths: ColumnWidths::default(),
            column_resize: None,
        }
    }

    pub fn title(&self) -> String {
        self.location_title.clone()
    }

    pub const fn items_opt(&self) -> Option<&Vec<Item>> {
        self.items_opt.as_ref()
    }

    pub const fn items_opt_mut(&mut self) -> Option<&mut Vec<Item>> {
        self.items_opt.as_mut()
    }

    /// Take any icons the cache has learned, and report what it still lacks.
    ///
    /// See [`refresh_icons`]; this is the form the hosts use, which keeps the
    /// borrow of the item list inside the tab.
    pub fn refresh_icons(&mut self, sizes: IconSizes) -> IconWarmup {
        self.items_opt
            .as_deref_mut()
            .map(|items| refresh_icons(items, sizes))
            .unwrap_or_default()
    }

    pub fn set_items(&mut self, mut items: Vec<Item>) {
        let highlighted = self
            .items_opt
            .as_ref()
            .and_then(|items| items.iter().enumerate().find(|i| i.1.highlighted))
            .map(|(i, _)| i);
        let selected = self.selected_locations();
        for item in &mut items {
            item.selected = false;
            if let Some(location) = &item.location_opt
                && selected.contains(location)
            {
                item.selected = true;
            }
        }
        self.items_opt = Some(items);
        if let Some(i) = highlighted
            .zip(self.items_opt.as_mut())
            .and_then(|(h, items)| items.get_mut(h))
        {
            i.highlighted = true;
        }
    }

    pub fn cut_selected(&mut self) {
        if let Some(ref mut items) = self.items_opt {
            for item in items.iter_mut() {
                item.cut = item.selected;
            }
        }
    }

    pub fn refresh_cut(&mut self, locations: &[PathBuf]) {
        if let Some(ref mut items) = self.items_opt {
            for item in items.iter_mut() {
                item.cut = false;
                if let Some(location_path) = item.location_opt.as_ref().and_then(Location::path_opt)
                    && locations.contains(location_path)
                {
                    item.cut = true;
                }
            }
        }
    }

    pub fn selected_locations(&self) -> Vec<Location> {
        if let Some(ref items) = self.items_opt {
            items
                .iter()
                .filter_map(|item| {
                    if item.selected {
                        item.location_opt.clone()
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    pub fn select_all(&mut self) {
        if let Some(ref mut items) = self.items_opt {
            for item in items.iter_mut() {
                if !self.config.show_hidden && item.hidden {
                    item.selected = false;
                    continue;
                }
                item.selected = true;
            }
        }
    }

    pub fn select_none(&mut self) -> bool {
        self.select_focus = None;
        let mut had_selection = false;
        if let Some(ref mut items) = self.items_opt {
            for item in items.iter_mut() {
                if item.selected {
                    item.selected = false;
                    had_selection = true;
                }
            }
        }
        had_selection
    }

    pub fn select_name(&mut self, name: &str) {
        self.select_focus = None;
        if let Some(ref mut items) = self.items_opt {
            for (i, item) in items.iter_mut().enumerate() {
                item.selected = item.name == name;
                if item.selected {
                    self.select_focus = Some(i);
                }
            }
        }
    }

    /// Selects the first item whose name starts with the given prefix (case-insensitive).
    /// Returns true if an item was selected.
    pub fn select_by_prefix(&mut self, prefix: &str) -> bool {
        let prefix_lower = prefix.to_lowercase();
        let focus = self.select_focus.take();

        if let Some(ref mut items) = self.items_opt {
            // First, deselect all items
            for item in items.iter_mut() {
                item.selected = false;
            }

            // Determine the start index of the search. When the index is before the currently focused item, it will be
            // considered first, otherwise last. Consider the focused item last when only a single character has been
            // typed, so we eagerly switch focus on the first character and stay on the same item as long as the prefix
            // matches.
            let single_char = prefix_lower.chars().count() == 1;
            let start = if single_char {
                Self::index_after_focus(focus, self.sort_direction)
            } else {
                Self::index_before_focus(focus, self.sort_direction)
            };
            self.select_focus = Self::select_first_prefix_from_index(
                &prefix_lower,
                items,
                start,
                self.sort_direction,
            );

            if self.select_focus.is_some() || single_char {
                return self.select_focus.is_some();
            }

            let mut chars = prefix_lower.chars();
            let Some(first) = chars.next() else {
                log::error!("search term is empty");
                return self.select_focus.is_some();
            };

            // Check if all entered characters are the same
            if !chars.all(|c| c == first) {
                return self.select_focus.is_some();
            }

            // Search for a single character when all entered characters are the same.
            // This allows cycling through items starting with the same character by repeatedly pressing a key.
            let start = Self::index_after_focus(focus, self.sort_direction);
            self.select_focus = Self::select_first_prefix_from_index(
                &first.to_string(),
                items,
                start,
                self.sort_direction,
            );

            return self.select_focus.is_some();
        }
        false
    }

    fn index_before_focus(current_focus: Option<usize>, forward: bool) -> usize {
        current_focus.map_or(0, |i| if forward { i } else { i + 1 })
    }

    fn index_after_focus(current_focus: Option<usize>, forward: bool) -> usize {
        current_focus.map_or(0, |i| if forward { i + 1 } else { i })
    }

    fn select_first_prefix_from_index(
        prefix_lower: &str,
        items: &mut [Item],
        start: usize,
        forward: bool,
    ) -> Option<usize> {
        // Order the search item so they begin at `start`.
        let Some((until, after)) = items.split_at_mut_checked(start) else {
            log::error!(
                "invalid start index {start} for items of length {}",
                items.len()
            );
            return None;
        };
        let search_items = after
            .iter_mut()
            .enumerate()
            .map(|(i, item)| (i + start, item))
            .chain(until.iter_mut().enumerate());

        if forward {
            Self::select_first_prefix_match(prefix_lower, search_items)
        } else {
            Self::select_first_prefix_match(prefix_lower, search_items.rev())
        }
    }

    /// Selects the first item in the given iterator whose name starts with the given prefix.
    ///
    /// The `prefix` must be lowercase.
    fn select_first_prefix_match<'a>(
        prefix: &str,
        items: impl Iterator<Item = (usize, &'a mut Item)>,
    ) -> Option<usize> {
        for (i, item) in items {
            if item.name.to_lowercase().starts_with(prefix) {
                item.selected = true;
                return Some(i);
            }
        }
        None
    }

    pub fn select_paths(&mut self, paths: Vec<PathBuf>) {
        self.select_focus = None;
        if let Some(ref mut items) = self.items_opt {
            for (i, item) in items.iter_mut().enumerate() {
                item.selected = false;
                if let Some(path) = item.path_opt()
                    && paths.contains(path)
                {
                    item.selected = true;
                    self.select_focus = Some(i);
                }
            }
        }
    }

    fn select_position(&mut self, row: usize, col: usize, mod_shift: bool) -> bool {
        let mut start = (row, col);
        let mut end = (row, col);
        if mod_shift {
            if self.select_focus.is_none() || self.select_range.is_none() {
                // Set select range to initial state if necessary
                self.select_range = self.select_focus.map(|i| (i, i));
            }

            if let Some(pos) = self.select_range_start_pos_opt() {
                if pos.0 < row || (pos.0 == row && pos.1 < col) {
                    start = pos;
                } else {
                    end = pos;
                }
            }
        }

        let mut found = false;
        if let Some(ref mut items) = self.items_opt {
            for (i, item) in items.iter_mut().enumerate() {
                item.selected = false;
                let pos = match item.pos_opt.get() {
                    Some(some) => some,
                    None => continue,
                };
                if pos.0 < start.0 || (pos.0 == start.0 && pos.1 < start.1) {
                    // Before start
                    continue;
                }
                if pos.0 > end.0 || (pos.0 == end.0 && pos.1 > end.1) {
                    // After end
                    continue;
                }
                if pos == (row, col) {
                    // Update focus if this is what we wanted to select
                    self.select_focus = Some(i);
                    self.select_range = if mod_shift {
                        self.select_range.map(|r| (r.0, i))
                    } else {
                        Some((i, i))
                    };
                    found = true;
                }
                item.selected = true;
            }
        }
        found
    }

    pub fn select_rect(&mut self, rect: Rectangle, mod_ctrl: bool, mod_shift: bool) {
        if let Some(ref mut items) = self.items_opt {
            for item in items.iter_mut() {
                let was_overlapped = item.overlaps_drag_rect;
                item.overlaps_drag_rect = item.rect_opt.get().is_some_and(|r| r.intersects(&rect));

                item.selected = if mod_ctrl || mod_shift {
                    if was_overlapped == item.overlaps_drag_rect {
                        item.selected
                    } else {
                        !item.selected
                    }
                } else {
                    item.overlaps_drag_rect
                };
            }
        }
    }

    /// The paths a file drag out of this tab carries: everything selected that
    /// exists on disk, or the item the drag started on when nothing is.
    fn drag_paths(&self, started_on: usize) -> Vec<PathBuf> {
        let Some(items) = self.items_opt.as_ref() else {
            return Vec::new();
        };
        let selected: Vec<_> = items
            .iter()
            .filter(|item| item.selected)
            .filter_map(|item| item.path_opt().cloned())
            .collect();
        if selected.is_empty() {
            items
                .get(started_on)
                .and_then(|item| item.path_opt().cloned())
                .into_iter()
                .collect()
        } else {
            selected
        }
    }

    /// The directory item a drag at `point` would drop into.
    ///
    /// `point` and `Item::rect_opt` are both relative to the `MouseArea` reporting the
    /// drag: `grid_view` and `list_view` lay out their rectangles in that space, so no
    /// conversion is needed. Only directories accept drops; a drag over a file or a gap
    /// between rows drops into the displayed directory.
    fn drop_target(&self, point: Point) -> Option<usize> {
        self.items_opt.as_ref()?.iter().position(|item| {
            item.metadata.is_dir()
                && item.path_opt().is_some()
                && item.rect_opt.get().is_some_and(|rect| rect.contains(point))
        })
    }

    /// Clear the drag indicators and end the drag.
    ///
    /// Each watching widget calls this once. The file list and every breadcrumb segment
    /// see the same `ended` state in one event batch, so repeated calls must be safe.
    /// `ui::dnd::end_drag` supports this and retains the offer until the pending drop
    /// read starts.
    fn end_file_drag(&mut self) {
        if self.dnd_target.is_some() {
            self.highlight_drop_target(None);
        }
        self.dnd_ancestor = None;
        self.dnd_source = false;
        crate::ui::dnd::end_drag();
    }

    /// Draw exactly `target` as the drop target, and nothing else.
    fn highlight_drop_target(&mut self, target: Option<usize>) {
        self.dnd_target = target;
        if let Some(items) = self.items_opt.as_mut() {
            for (i, item) in items.iter_mut().enumerate() {
                item.highlighted = Some(i) == target;
            }
        }
    }

    pub fn select_focus_id(&self) -> Option<widget::Id> {
        let items = self.items_opt.as_ref()?;
        let item = items.get(self.select_focus?)?;
        Some(item.button_id.clone())
    }

    fn select_focus_pos_opt(&self) -> Option<(usize, usize)> {
        let items = self.items_opt.as_ref()?;
        let item = items.get(self.select_focus?)?;
        item.pos_opt.get()
    }

    fn dehighlight_all(&mut self) {
        if let Some(items) = self.items_opt.as_mut() {
            for item in items.iter_mut() {
                item.highlighted = false;
            }
        }
    }

    /// The list-view column geometry for the current view width, or `None`
    /// when the view is too narrow and the list uses its condensed layout.
    ///
    /// The requested widths are shrunk from the right, down to [`COLUMN_MIN`]
    /// each, until Name has [`NAME_MIN`] of room. Only the effective widths
    /// shrink, so widening the window restores what was requested.
    fn list_geometry(&self) -> Option<ListGeometry> {
        let width = self.size_opt.get()?.width;
        let show_type = self.config.show_type_column;
        let mut widths = self.column_widths;
        let mut room = width - widths.fixed_total(show_type);
        let mut shrink = |column: &mut f32| {
            if room < NAME_MIN {
                let shrunk = (*column - (NAME_MIN - room)).max(COLUMN_MIN);
                room += *column - shrunk;
                *column = shrunk;
            }
        };
        if show_type {
            shrink(&mut widths.type_);
        }
        shrink(&mut widths.size);
        shrink(&mut widths.modified);
        (room >= NAME_MIN).then_some(ListGeometry {
            widths,
            show_type,
            name_room: room,
        })
    }

    /// The part of the item view currently on screen, for a view of `size`.
    /// The scroll offset is clamped with the cached content height so a
    /// stale offset after a resize does not point past the end.
    fn visible_rect(&self, size: Size) -> Rectangle {
        let max_scroll_y = self
            .content_height_opt
            .get()
            .map(|ch| (ch - size.height).max(0.0))
            .unwrap_or(f32::MAX);
        let scroll_y = self
            .scroll_opt
            .map(|o| o.y.min(max_scroll_y).max(0.0))
            .unwrap_or(0.0);
        Rectangle::new(Point::new(0.0, scroll_y), size)
    }

    /// Position of the first item on screen, where keyboard navigation starts
    /// when nothing is selected.
    fn first_visible_pos_opt(&self) -> Option<(usize, usize)> {
        let visible_rect = self.visible_rect(self.item_view_size_opt.get().unwrap_or_default());
        self.items_opt
            .as_ref()?
            .iter()
            .filter(|item| {
                item.rect_opt
                    .get()
                    .is_some_and(|rect| rect.intersects(&visible_rect))
            })
            .filter_map(|item| item.pos_opt.get())
            .min()
    }

    pub(crate) fn select_focus_scroll(&mut self) -> Option<AbsoluteOffset> {
        let items = self.items_opt.as_ref()?;
        let item = items.get(self.select_focus?)?;
        let rect = item.rect_opt.get()?;

        let visible_rect = self.visible_rect(self.item_view_size_opt.get().unwrap_or_default());

        if rect.y < visible_rect.y {
            // Scroll up to rect
            self.scroll_opt = Some(AbsoluteOffset { x: 0.0, y: rect.y });
            self.scroll_opt
        } else if (rect.y + rect.height) > (visible_rect.y + visible_rect.height) {
            // Scroll down to rect
            self.scroll_opt = Some(AbsoluteOffset {
                x: 0.0,
                y: rect.y + rect.height - visible_rect.height,
            });
            self.scroll_opt
        } else {
            // Do not scroll
            None
        }
    }

    fn rows_per_page(&self) -> usize {
        let viewport_height = self.item_view_size_opt.get().map_or(0.0, |s| s.height);
        let row_height = self
            .select_focus
            .and_then(|i| self.items_opt.as_ref()?.get(i))
            .and_then(|item| item.rect_opt.get())
            .map_or(0.0, |r| r.height);
        if row_height > 0.0 && viewport_height > 0.0 {
            (viewport_height / row_height).floor() as usize
        } else {
            1
        }
    }

    fn select_range_start_pos_opt(&self) -> Option<(usize, usize)> {
        let items = self.items_opt.as_ref()?;
        let item = items.get(self.select_range.map(|r| r.0)?)?;
        item.pos_opt.get()
    }

    fn select_first_pos_opt(&self) -> Option<(usize, usize)> {
        let items = self.items_opt.as_ref()?;
        let mut first = None;
        for item in items {
            if !item.selected {
                continue;
            }

            let (row, col) = match item.pos_opt.get() {
                Some(some) => some,
                None => continue,
            };

            first = Some(match first {
                Some((first_row, first_col)) => match row.cmp(&first_row) {
                    Ordering::Less => (row, col),
                    Ordering::Equal => (row, col.min(first_col)),
                    Ordering::Greater => (first_row, first_col),
                },
                None => (row, col),
            });
        }
        first
    }

    fn select_last_pos_opt(&self) -> Option<(usize, usize)> {
        let items = self.items_opt.as_ref()?;
        let mut last = None;
        for item in items {
            if !item.selected {
                continue;
            }

            let (row, col) = match item.pos_opt.get() {
                Some(some) => some,
                None => continue,
            };

            last = Some(match last {
                Some((last_row, last_col)) => match row.cmp(&last_row) {
                    Ordering::Greater => (row, col),
                    Ordering::Equal => (row, col.max(last_col)),
                    Ordering::Less => (last_row, last_col),
                },
                None => (row, col),
            });
        }
        last
    }

    fn trigger_async_decode(&mut self) -> Vec<Command> {
        // Only trigger decode in gallery mode for the currently selected image
        if !self.gallery {
            return Vec::new();
        }

        let Some(index) = self.select_focus else {
            return Vec::new();
        };

        let Some(items) = &self.items_opt else {
            return Vec::new();
        };

        let Some(item) = items.get(index) else {
            return Vec::new();
        };

        let Some(ItemThumbnail::Image(_, original_dims)) = &item.thumbnail_opt else {
            return Vec::new();
        };

        if let Some((w, h)) = original_dims
            && !should_use_tiling(*w, *h)
        {
            return Vec::new();
        }

        let Some(path) = item.path_opt() else {
            return Vec::new();
        };

        // Clone path to avoid borrow checker issues
        let path = path.to_path_buf();

        // Get display size for adaptive resolution
        let display_dimensions = self
            .size_opt
            .get()
            .map(|size| (size.width as u32, size.height as u32));

        // Try to decode the image using LargeImageManager with adaptive resolution
        let (should_decode, target_dimensions, generation) = self
            .large_image_manager
            .try_decode(&path, display_dimensions);
        if should_decode {
            vec![Command::Iced(
                crate::ui::iced::Task::perform(
                    decode_large_image(path, target_dimensions),
                    move |result| {
                        result
                            .map(|(path, width, height, pixels)| {
                                Message::ImageDecoded(
                                    path,
                                    width,
                                    height,
                                    pixels,
                                    display_dimensions,
                                    generation,
                                )
                            })
                            .unwrap_or_else(|| Message::AutoScroll(None))
                    },
                )
                .into(),
            )]
        } else {
            Vec::new()
        }
    }

    pub fn change_location(&mut self, location: &Location, history_i_opt: Option<usize>) {
        self.location = location.normalize();
        self.location_ancestors = self.location.ancestors();
        self.location_title = self.location.title();
        self.edit_location = None;
        self.items_opt = None;
        // Remember where this entry was scrolled to before leaving it
        if let Some(saved) = self.history_scroll.get_mut(self.history_i) {
            *saved = self.scroll_opt;
        }
        self.scroll_opt = None;
        self.pending_scroll = None;
        self.select_focus = None;
        self.search_context = None;
        if let Some(history_i) = history_i_opt {
            // Navigating in history
            self.history_i = history_i;
            if let Some(offset) = self.history_scroll.get(history_i).copied().flatten() {
                self.scroll_opt = Some(offset);
                self.pending_scroll = Some(offset);
            }
        } else {
            // Truncate history to remove next entries
            self.history.truncate(self.history_i + 1);
            self.history_scroll.truncate(self.history_i + 1);

            // Compact consecutive matching paths
            {
                let mut remove = false;
                if let Some(last_location) = self.history.last() {
                    if let Location::Network(last_uri, ..) = last_location
                        && let Location::Network(uri, ..) = location
                    {
                        remove = last_uri == uri;
                    } else if let Some(last_path) = last_location.path_opt()
                        && let Some(path) = location.path_opt()
                    {
                        remove = last_path == path;
                    }
                }
                if remove {
                    self.history.pop();
                    self.history_scroll.pop();
                }
            }

            // Push to the front of history
            self.history_i = self.history.len();
            self.history.push(location.clone());
            self.history_scroll.push(None);
        }
    }

    pub fn update(&mut self, message: Message, modifiers: Modifiers) -> Vec<Command> {
        let mut commands = Vec::new();
        let mut cd = None;
        let mut history_i_opt = None;
        let mod_ctrl = modifiers.contains(Modifiers::CTRL) && self.mode.multiple();
        let mod_shift = modifiers.contains(Modifiers::SHIFT) && self.mode.multiple();
        match message {
            Message::AddNetworkDrive => {
                commands.push(Command::AddNetworkDrive);
            }
            Message::AutoScroll(auto_scroll) => {
                commands.push(Command::AutoScroll(auto_scroll));
            }
            Message::ClickRelease(click_i_opt) => {
                // Single click to open, once per click sequence: the release
                // that completes a double click must not open it again
                let already_opened = std::mem::take(&mut self.opened_by_this_click);
                if !mod_ctrl && self.config.single_click && !already_opened {
                    let mut paths_to_open = Vec::new();
                    if let Some(ref mut items) = self.items_opt {
                        for (i, item) in items.iter_mut().enumerate() {
                            if Some(i) == click_i_opt {
                                if let Some(location) = &item.location_opt {
                                    if item.metadata.is_dir() {
                                        cd = Some(location.clone());
                                    } else if let Some(path) = location.path_opt() {
                                        paths_to_open.push(path.clone());
                                    } else {
                                        log::warn!("no path for item {item:?}");
                                    }
                                } else {
                                    log::warn!("no location for item {item:?}");
                                }
                            }
                        }
                    }
                    if !paths_to_open.is_empty() {
                        commands.push(Command::OpenFile(paths_to_open));
                    }
                }

                if click_i_opt != self.clicked.take()
                    && let Some(ref mut items) = self.items_opt
                {
                    for (i, item) in items.iter_mut().enumerate() {
                        if mod_ctrl && Some(i) == click_i_opt && item.selected {
                            item.selected = false;
                        }
                    }
                }
            }
            Message::DragEnd => {
                self.clicked = None;
                self.watch_drag = true;
            }
            Message::DoubleClick(click_i_opt) => {
                // In single-click mode the first release already opened this
                // item, so the double click adds nothing but must still stop
                // the release behind it from opening it a third time
                self.opened_by_this_click = true;
                if !self.config.single_click
                    && let Some(clicked_item) = self
                        .items_opt
                        .as_ref()
                        .and_then(|items| click_i_opt.and_then(|click_i| items.get(click_i)))
                {
                    if let Some(location) = &clicked_item.location_opt {
                        if clicked_item.metadata.is_dir() {
                            cd = Some(location.clone());
                        } else if let Some(path) = location.path_opt() {
                            commands.push(Command::OpenFile(vec![path.clone()]));
                        } else {
                            log::warn!("no path for item {clicked_item:?}");
                        }
                    } else {
                        log::warn!("no location for item {clicked_item:?}");
                    }
                }
            }
            Message::Click(click_i_opt) => {
                self.edit_location = None;
                if click_i_opt.is_none() {
                    self.clicked = click_i_opt;
                }

                if mod_shift {
                    if let Some(click_i) = click_i_opt {
                        self.select_range = self
                            .select_range
                            .map_or(Some((click_i, click_i)), |r| Some((r.0, click_i)));
                        if let Some(range) = self.select_range {
                            let range_min = range.0.min(range.1);
                            let range_max = range.0.max(range.1);
                            // A sorted tab's items can't be linearly selected
                            // Let's say we have:
                            // index | file
                            // 0     | file0
                            // 1     | file1
                            // 2     | file2
                            // This is both the default sort and internal ordering
                            // When sorted it may be displayed as:
                            // 1     | file1
                            // 0     | file0
                            // 2     | file2
                            // However, the internal ordering is still the same thus
                            // linearly selecting items doesn't work. Shift selecting
                            // file0 and file2 would select indices 0 to 2 when it should
                            // select indices 0 AND 2 from items_opt
                            let indices: Vec<_> = self
                                .column_sort()
                                .map(|sorted| sorted.into_iter().map(|(i, _)| i).collect())
                                .unwrap_or_else(|| {
                                    let len = self
                                        .items_opt
                                        .as_deref()
                                        .map(<[Item]>::len)
                                        .unwrap_or_default();
                                    (0..len).collect()
                                });

                            // Find the true indices for the min and max element w.r.t.
                            // a sorted tab.
                            let min = indices
                                .iter()
                                .copied()
                                .position(|offset| offset == range_min)
                                .unwrap_or_default();
                            // We can't skip `min_real` elements here because the index of
                            // `max` may actually be before `min` in a sorted tab
                            let max = indices
                                .iter()
                                .copied()
                                .position(|offset| offset == range_max)
                                .unwrap_or(indices.len());
                            let min_real = min.min(max);
                            let max_real = max.max(min);

                            let in_range: Vec<usize> = indices
                                .into_iter()
                                .skip(min_real)
                                .take(max_real - min_real + 1)
                                .collect();
                            if let Some(ref mut items) = self.items_opt {
                                // Plain shift-click reselects: whatever the
                                // previous, wider range covered is dropped, so
                                // the range can be shrunk as well as grown.
                                // Ctrl with shift keeps the earlier selection
                                // and adds to it.
                                if !mod_ctrl {
                                    for item in items.iter_mut() {
                                        item.selected = false;
                                    }
                                }
                                for index in in_range {
                                    if let Some(item) = items.get_mut(index) {
                                        if item.hidden {
                                            if self.config.show_hidden {
                                                item.selected = true;
                                            }
                                        } else {
                                            item.selected = true;
                                        }
                                    }
                                }
                            }
                        }
                        self.clicked = click_i_opt;
                        self.select_focus = click_i_opt;
                    }
                } else {
                    // In a file chooser a folder replaces the selection, so that
                    // Open enters it instead of being blocked by a mixed selection
                    let clicked_folder_in_file_dialog = matches!(&self.mode, Mode::Dialog(dialog) if !dialog.is_dir())
                        && click_i_opt
                            .and_then(|i| self.items_opt.as_ref()?.get(i))
                            .is_some_and(|item| item.metadata.is_dir());
                    let dont_unset = !clicked_folder_in_file_dialog
                        && (mod_ctrl
                            || self.column_sort().is_some_and(|l| {
                                l.iter()
                                    .any(|&(e_i, e)| Some(e_i) == click_i_opt && e.selected)
                            }));
                    if let Some(ref mut items) = self.items_opt {
                        for (i, item) in items.iter_mut().enumerate() {
                            if Some(i) == click_i_opt {
                                // Filter out selection if it does not match dialog kind
                                if let Mode::Dialog(dialog) = &self.mode {
                                    let item_is_dir = item.metadata.is_dir();
                                    if item_is_dir != dialog.is_dir() {
                                        // Allow selecting folder if dialog is for files to make it
                                        // possible to double click
                                        if !item_is_dir {
                                            continue;
                                        }
                                    }
                                }
                                if !item.selected {
                                    self.clicked = click_i_opt;
                                    item.selected = true;
                                }
                                self.select_range = Some((i, i));
                                self.select_focus = click_i_opt;
                            } else if !dont_unset && item.selected {
                                self.clicked = click_i_opt;
                                item.selected = false;
                            }
                        }
                    }
                }
            }
            Message::Config(config) => {
                // View is preserved for existing tabs
                let view = self.config.view;
                let show_hidden_changed = self.config.show_hidden != config.show_hidden;
                self.config = config;
                self.config.view = view;
                if show_hidden_changed && let Location::Search(path, term, ..) = &self.location {
                    cd = Some(Location::Search(
                        path.clone(),
                        term.clone(),
                        self.config.show_hidden,
                        Instant::now(),
                    ));
                }
                // Unhighlight all items when config changes
                if let Some(ref mut items) = self.items_opt {
                    for item in items.iter_mut() {
                        item.highlighted = false;
                    }
                }
            }
            Message::ContextAction(action) => {
                commands.push(Command::Action(action));
            }
            Message::RunContextAction(action) => {
                commands.push(Command::RunContextAction(action));
            }
            Message::RightClickBackground => {
                self.edit_location = None;

                if self.last_right_click.take().is_none()
                    && let Some(ref mut items) = self.items_opt
                {
                    for item in items.iter_mut() {
                        item.selected = false;
                    }
                }
            }
            Message::Surface(action) => {
                commands.push(Command::Surface(action));
            }
            Message::LocationContextMenuIndex(index) => {
                self.location_context_menu_index = index;
            }
            Message::LocationMenuAction(action) => {
                let path_for_index = |ancestor_index| {
                    self.location
                        .path_opt()
                        .and_then(|path| path.ancestors().nth(ancestor_index))
                        .map(Path::to_path_buf)
                };
                match action {
                    LocationMenuAction::OpenInNewTab(ancestor_index) => {
                        if let Some(path) = path_for_index(ancestor_index) {
                            commands.push(Command::OpenInNewTab(path));
                        }
                    }
                    LocationMenuAction::OpenInNewWindow(ancestor_index) => {
                        if let Some(path) = path_for_index(ancestor_index) {
                            commands.push(Command::OpenInNewWindow(path));
                        }
                    }
                    LocationMenuAction::Preview(ancestor_index) => {
                        if let Some(path) = path_for_index(ancestor_index) {
                            match item_from_path(&path, IconSizes::default()) {
                                Ok(item) => {
                                    commands.push(Command::Preview(PreviewKind::Custom(
                                        PreviewItem(Box::new(item)),
                                    )));
                                }
                                Err(err) => {
                                    log::warn!(
                                        "failed to get item from path {}: {}",
                                        path.display(),
                                        err
                                    );
                                }
                            }
                        }
                    }
                    LocationMenuAction::AddToSidebar(ancestor_index) => {
                        if let Some(path) = path_for_index(ancestor_index) {
                            commands.push(Command::AddToSidebar(path));
                        } else {
                            log::warn!(
                                "no ancestor {ancestor_index} for location {:?}",
                                self.location
                            );
                        }
                    }
                }
            }
            Message::Drag(rect_opt) => {
                self.watch_drag = false;
                if let Some(rect) = rect_opt {
                    if self.mode.multiple() {
                        self.select_rect(rect, mod_ctrl, mod_shift);
                    }
                    if self.select_focus.take().is_some() {
                        // Unfocus currently focused button
                        commands.push(Command::Iced(
                            widget::button::focus(widget::Id::unique()).into(),
                        ));
                    }
                }
            }
            Message::DragFiles(i) => {
                // `on_drag` fires again on every pixel of the gesture; only the
                // first one may start a drag.
                if !self.dnd_source {
                    let paths = self.drag_paths(i);
                    if !paths.is_empty() {
                        // Offer a copy for drops into other applications. For drops
                        // back into this app, the modifier held at the drop determines
                        // whether to move or copy; see `Message::Dnd`.
                        let contents = crate::clipboard::ClipboardCopy::new(
                            crate::clipboard::ClipboardKind::Copy,
                            &paths,
                        );
                        if crate::ui::dnd::start_drag_data(Arc::new(contents)) {
                            self.dnd_source = true;
                            log::debug!("started a file drag of {} path(s)", paths.len());
                        }
                    }
                }
            }
            Message::Dnd(dnd) => {
                let target = dnd.position.and_then(|point| self.drop_target(point));
                if target != self.dnd_target {
                    self.highlight_drop_target(target);
                }
                if dnd.ended {
                    if dnd.dropped
                        && dnd.position.is_some()
                        && self.location.supports_paste()
                        && let Some(to) = target
                            .and_then(|i| self.items_opt.as_ref()?.get(i)?.path_opt().cloned())
                            .or_else(|| self.location.path_opt().cloned())
                    {
                        commands.push(Command::DropFiles(to, mod_ctrl));
                    }
                    self.end_file_drag();
                }
            }
            Message::DndAncestor(index, to, dnd) => {
                // No hit test: this message exists only because the drag is
                // inside that breadcrumb's own `MouseArea`, whose bounds are
                // the segment.
                if dnd.position.is_some() {
                    self.dnd_ancestor = Some(index);
                } else if self.dnd_ancestor == Some(index) {
                    // Every segment reports each change, but only the highlighted
                    // segment may clear its highlight. Otherwise, a segment the drag
                    // just left could clear the highlight set by the segment it entered.
                    self.dnd_ancestor = None;
                }
                if dnd.ended {
                    if dnd.dropped && dnd.position.is_some() && self.location.supports_paste() {
                        commands.push(Command::DropFiles(to, mod_ctrl));
                    }
                    self.end_file_drag();
                }
            }
            Message::EditLocation(edit_location) => {
                self.edit_location = edit_location;
                if self.edit_location.is_some() {
                    commands.push(Command::Iced(
                        widget::text_input::focus(self.edit_location_id.clone()).into(),
                    ));
                }
            }
            Message::EditLocationComplete(selected) => {
                if let Some(mut edit_location) = self.edit_location.take()
                    && !matches!(edit_location.location, Location::Network(..))
                {
                    edit_location.selected = Some(selected);
                    cd = edit_location.resolve();
                }
            }
            Message::EditLocationEnable => {
                commands.push(Command::Iced(
                    widget::text_input::focus(self.edit_location_id.clone()).into(),
                ));
                self.edit_location = Some(self.location.clone().into());
            }
            Message::EditLocationSubmit => {
                if let Some(mut edit_location) = self.edit_location.take() {
                    let typed_opt = match &edit_location.location {
                        Location::Path(path) => path.to_str().map(str::to_string),
                        Location::Network(uri, ..) => Some(uri.clone()),
                        _ => None,
                    };
                    let mut typed_uri = false;
                    if let Some(typed) = typed_opt {
                        match typed.trim().parse::<url::Url>() {
                            Ok(url) if url.scheme() != "file" && url.has_host() => {
                                let uri = url.as_str().to_string();
                                edit_location =
                                    Location::Network(uri.clone(), uri, None).normalize().into();
                                typed_uri = true;
                            }
                            Err(_) if matches!(edit_location.location, Location::Network(..)) => {
                                edit_location =
                                    Location::Path(PathBuf::from(typed)).normalize().into();
                            }
                            _ => {}
                        }
                    }

                    // Select first completion if current location does not exist
                    if edit_location.selected.is_none()
                        && edit_location
                            .completions
                            .as_ref()
                            .is_some_and(|completions| !completions.is_empty())
                        && edit_location
                            .location
                            .path_opt()
                            .is_some_and(|path| !path.exists())
                    {
                        edit_location.selected = Some(0);
                    }

                    cd = edit_location.resolve();
                    if cd.is_none() && typed_uri {
                        cd = Some(edit_location.location);
                    }
                }
            }
            Message::EditLocationTab => {
                if let Some(edit_location) = &mut self.edit_location {
                    edit_location.select(!modifiers.contains(Modifiers::SHIFT));
                }
            }
            Message::OpenInNewTab(path) => {
                commands.push(Command::OpenInNewTab(path));
            }
            Message::ClearRecents => {
                commands.push(Command::ClearRecents);
            }
            Message::EmptyTrash => {
                commands.push(Command::EmptyTrash);
            }
            #[cfg(feature = "desktop")]
            Message::ExecEntryAction(path, action) => {
                let lang_id = crate::localize::LANGUAGE_LOADER.current_language();
                let language = lang_id.language.as_str();
                match path.map_or_else(
                    || {
                        let items = self.items_opt.as_deref()?;
                        items.iter().find(|&item| item.selected).and_then(|item| {
                            let location = item.location_opt.as_ref()?;
                            let path = location.path_opt()?;
                            crate::desktop_entry::load_desktop_file(&[language.into()], path.into())
                        })
                    },
                    |path| crate::desktop_entry::load_desktop_file(&[language.into()], path),
                ) {
                    Some(entry) => commands.push(Command::ExecEntryAction(entry, action)),
                    None => log::warn!("Invalid desktop entry path passed to ExecEntryAction"),
                }
            }
            Message::Gallery(gallery) => {
                self.gallery = gallery;

                if gallery {
                    commands.extend(self.trigger_async_decode());
                }
            }
            Message::GalleryPrevious | Message::GalleryNext => {
                let mut pos_opt = None;
                if let Some(mut indices) = self.column_sort() {
                    if matches!(message, Message::GalleryPrevious) {
                        indices.reverse();
                    }
                    let mut found = false;
                    for (index, item) in indices {
                        if self.select_focus.is_none() {
                            found = true;
                        }
                        if self.select_focus == Some(index) {
                            found = true;
                            continue;
                        }
                        if found && item.can_gallery() {
                            pos_opt = item.pos_opt.get();
                            if pos_opt.is_some() {
                                break;
                            }
                        }
                    }
                }
                if let Some((row, col)) = pos_opt {
                    // Should mod_shift be available?
                    self.select_position(row, col, mod_shift);

                    commands.extend(self.trigger_async_decode());
                }
                if let Some(offset) = self.select_focus_scroll() {
                    commands.push(Command::Iced(
                        scrollable::scroll_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }
                if let Some(id) = self.select_focus_id() {
                    commands.push(Command::Iced(widget::button::focus(id).into()));
                }
            }
            Message::GalleryToggle => {
                if let Some(indices) = self.column_sort() {
                    for (_, item) in &indices {
                        if item.selected && item.can_gallery() {
                            self.gallery = !self.gallery;

                            if self.gallery {
                                commands.extend(self.trigger_async_decode());
                            }
                            break;
                        }
                    }
                }
            }
            Message::GoNext => {
                if let Some(history_i) = self.history_i.checked_add(1)
                    && let Some(location) = self.history.get(history_i)
                {
                    cd = Some(location.clone());
                    history_i_opt = Some(history_i);
                }
            }
            Message::GoPrevious => {
                if let Some(history_i) = self.history_i.checked_sub(1)
                    && let Some(location) = self.history.get(history_i)
                {
                    cd = Some(location.clone());
                    history_i_opt = Some(history_i);
                }
            }
            Message::ItemDown => {
                self.dehighlight_all();
                if let Some(edit_location) = &mut self.edit_location {
                    edit_location.select(true);
                } else if self.gallery {
                    commands.append(&mut self.update(Message::GalleryNext, modifiers));
                } else {
                    if let Some((row, col)) =
                        self.select_focus_pos_opt().or(self.select_last_pos_opt())
                    {
                        if self.select_focus.is_none() {
                            // Select last item in current selection to focus it.
                            self.select_position(row, col, mod_shift);
                        }

                        // Try to select item in next row
                        if !self.select_position(row + 1, col, mod_shift) {
                            // Ensure current item is still selected if there are no other items
                            self.select_position(row, col, mod_shift);
                        }
                    } else {
                        // Select the first item on screen
                        let (row, col) = self.first_visible_pos_opt().unwrap_or((0, 0));
                        self.select_position(row, col, mod_shift);
                    }
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::scroll_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::ItemPageDown => {
                self.dehighlight_all();
                if let Some((row, col)) = self.select_focus_pos_opt().or(self.select_last_pos_opt())
                {
                    if self.select_focus.is_none() {
                        self.select_position(row, col, mod_shift);
                    }

                    let rows_per_page = self.rows_per_page().max(1);
                    let target_row = row.saturating_add(rows_per_page);

                    if !self.select_position(target_row, col, mod_shift) {
                        // Fall back to the last item at or before target_row
                        let best = self.items_opt.as_ref().and_then(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.pos_opt.get())
                                .filter(|(r, _)| *r <= target_row)
                                .max()
                        });
                        if let Some((best_row, best_col)) = best {
                            self.select_position(best_row, best_col, mod_shift);
                        }
                    }
                } else {
                    self.select_position(0, 0, mod_shift);
                }
                if let Some(offset) = self.select_focus_scroll() {
                    commands.push(Command::Iced(
                        scrollable::scroll_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }
                if let Some(id) = self.select_focus_id() {
                    commands.push(Command::Iced(widget::button::focus(id).into()));
                }
            }
            Message::ItemPageUp => {
                self.dehighlight_all();
                if let Some((row, col)) =
                    self.select_focus_pos_opt().or(self.select_first_pos_opt())
                {
                    if self.select_focus.is_none() {
                        self.select_position(row, col, mod_shift);
                    }

                    let rows_per_page = self.rows_per_page().max(1);
                    let target_row = row.saturating_sub(rows_per_page);

                    if !self.select_position(target_row, col, mod_shift) {
                        // Fall back to the first item at or after target_row
                        let best = self.items_opt.as_ref().and_then(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.pos_opt.get())
                                .filter(|(r, _)| *r >= target_row)
                                .min()
                        });
                        if let Some((best_row, best_col)) = best {
                            self.select_position(best_row, best_col, mod_shift);
                        }
                    }
                } else {
                    self.select_position(0, 0, mod_shift);
                }
                if let Some(offset) = self.select_focus_scroll() {
                    commands.push(Command::Iced(
                        scrollable::scroll_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }
                if let Some(id) = self.select_focus_id() {
                    commands.push(Command::Iced(widget::button::focus(id).into()));
                }
            }
            Message::ItemLeft => {
                self.dehighlight_all();
                if self.gallery {
                    commands.append(&mut self.update(Message::GalleryPrevious, modifiers));
                } else {
                    if let Some((row, col)) =
                        self.select_focus_pos_opt().or(self.select_first_pos_opt())
                    {
                        if self.select_focus.is_none() {
                            // Select first item in current selection to focus it.
                            self.select_position(row, col, mod_shift);
                        }

                        // Try to select previous item in current row
                        if !col
                            .checked_sub(1)
                            .is_some_and(|col| self.select_position(row, col, mod_shift))
                        {
                            // Try to select last item in previous row
                            if !row.checked_sub(1).is_some_and(|row| {
                                let mut col = 0;
                                if let Some(ref items) = self.items_opt {
                                    for item in items {
                                        match item.pos_opt.get() {
                                            Some((item_row, item_col)) if item_row == row => {
                                                col = col.max(item_col);
                                            }
                                            _ => continue,
                                        }
                                    }
                                }
                                self.select_position(row, col, mod_shift)
                            }) {
                                // Ensure current item is still selected if there are no other items
                                self.select_position(row, col, mod_shift);
                            }
                        }
                    } else {
                        // Select the first item on screen
                        let (row, col) = self.first_visible_pos_opt().unwrap_or((0, 0));
                        self.select_position(row, col, mod_shift);
                    }
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::scroll_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::ItemRight => {
                self.dehighlight_all();
                if self.gallery {
                    commands.append(&mut self.update(Message::GalleryNext, modifiers));
                } else {
                    if let Some((row, col)) =
                        self.select_focus_pos_opt().or(self.select_last_pos_opt())
                    {
                        if self.select_focus.is_none() {
                            // Select last item in current selection to focus it.
                            self.select_position(row, col, mod_shift);
                        }
                        // Try to select next item in current row
                        if !self.select_position(row, col + 1, mod_shift) {
                            // Try to select first item in next row
                            if !self.select_position(row + 1, 0, mod_shift) {
                                // Ensure current item is still selected if there are no other items
                                self.select_position(row, col, mod_shift);
                            }
                        }
                    } else {
                        // Select the first item on screen
                        let (row, col) = self.first_visible_pos_opt().unwrap_or((0, 0));
                        self.select_position(row, col, mod_shift);
                    }
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::scroll_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::ItemUp => {
                self.dehighlight_all();
                if let Some(edit_location) = &mut self.edit_location {
                    edit_location.select(false);
                } else if self.gallery {
                    commands.append(&mut self.update(Message::GalleryPrevious, modifiers));
                } else {
                    if let Some((row, col)) =
                        self.select_focus_pos_opt().or(self.select_first_pos_opt())
                    {
                        if self.select_focus.is_none() {
                            // Select first item in current selection to focus it.
                            self.select_position(row, col, mod_shift);
                        }

                        // Try to select item in last row
                        if !row
                            .checked_sub(1)
                            .is_some_and(|row| self.select_position(row, col, mod_shift))
                        {
                            // Ensure current item is still selected if there are no other items
                            self.select_position(row, col, mod_shift);
                        }
                    } else {
                        // Select the first item on screen
                        let (row, col) = self.first_visible_pos_opt().unwrap_or((0, 0));
                        self.select_position(row, col, mod_shift);
                    }
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::scroll_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::Location(location) => {
                // Workaround to support favorited files
                match &location {
                    Location::Path(path) => {
                        if path.is_dir() {
                            cd = Some(location);
                        } else {
                            commands.push(Command::OpenFile(vec![path.clone()]));
                        }
                    }
                    _ => {
                        cd = Some(location);
                    }
                }
            }
            Message::LocationUp => {
                // Sets location to the path's parent
                // Does nothing if path is root or location is Trash
                if let Location::Path(ref path) = self.location
                    && let Some(parent) = path.parent()
                {
                    cd = Some(Location::Path(parent.to_owned()));
                }
            }
            Message::Open(path_opt) => {
                match path_opt {
                    Some(path) => {
                        if path.is_dir() {
                            cd = Some(Location::Path(path));
                        } else {
                            commands.push(Command::OpenFile(vec![path]));
                        }
                    }
                    // Open selected items
                    None => {
                        enum ResolveResult {
                            Open(Option<PathBuf>),
                            OpenInTab(Option<PathBuf>),
                            OpenProperties,
                            Cd(Location),
                            Skip,
                        }
                        fn resolve_item(
                            item: &Item,
                            mode: &Mode,
                            is_only_one_selected: bool,
                        ) -> ResolveResult {
                            if !item.selected {
                                return ResolveResult::Skip;
                            }

                            let location = match &item.location_opt {
                                Some(l) => l,
                                None => return ResolveResult::OpenProperties,
                            };

                            let path_opt = location.path_opt();

                            if item.metadata.is_dir() {
                                match mode {
                                    Mode::App => {
                                        if is_only_one_selected {
                                            ResolveResult::Cd(location.clone())
                                        } else {
                                            ResolveResult::OpenInTab(path_opt.cloned())
                                        }
                                    }
                                    Mode::Dialog(_) => {
                                        if is_only_one_selected {
                                            ResolveResult::Cd(location.clone())
                                        } else {
                                            ResolveResult::Skip
                                        }
                                    }
                                }
                            } else {
                                ResolveResult::Open(path_opt.cloned())
                            }
                        }
                        let mut open_files = Vec::new();
                        if let Some(items) = self.items_opt.as_ref() {
                            let selected_count = items.iter().filter(|i| i.selected).count();

                            for item in items.iter() {
                                match resolve_item(item, &self.mode, selected_count == 1) {
                                    ResolveResult::Open(Some(p)) => open_files.push(p),
                                    ResolveResult::OpenInTab(Some(p)) => {
                                        commands.push(Command::OpenInNewTab(p))
                                    }
                                    ResolveResult::Cd(loc) => cd = Some(loc),
                                    ResolveResult::OpenProperties => {}
                                    _ => {}
                                }
                            }
                        }
                        if !open_files.is_empty() {
                            commands.push(Command::OpenFile(open_files));
                        }
                    }
                }
            }
            Message::Reload => {
                let selected_paths = self
                    .selected_locations()
                    .into_iter()
                    .filter_map(Location::into_path_opt)
                    .collect();
                let location = self.location.clone();
                self.change_location(&location, None);
                commands.push(Command::ChangeLocation(
                    self.title(),
                    location,
                    Some(selected_paths),
                ));
            }
            Message::RightClick(_point_opt, click_i_opt) => {
                if mod_ctrl || mod_shift {
                    self.update(Message::Click(click_i_opt), modifiers);
                }
                if let Some(ref mut items) = self.items_opt
                    && !click_i_opt
                        .is_some_and(|click_i| items.get(click_i).is_some_and(|x| x.selected))
                {
                    // If item not selected, clear selection on other items
                    for (i, item) in items.iter_mut().enumerate() {
                        item.selected = Some(i) == click_i_opt;
                    }
                }
                self.last_right_click = click_i_opt;
            }
            Message::MiddleClick(click_i) => {
                if mod_ctrl || mod_shift {
                    self.update(Message::Click(Some(click_i)), modifiers);
                } else {
                    if let Some(ref mut items) = self.items_opt {
                        for (i, item) in items.iter_mut().enumerate() {
                            item.selected = i == click_i;
                        }
                        self.select_range = Some((click_i, click_i));
                    }
                    if let Some(clicked_item) =
                        self.items_opt.as_ref().and_then(|items| items.get(click_i))
                    {
                        if let Some(path) = clicked_item.path_opt() {
                            if clicked_item.metadata.is_dir() {
                                //cd = Some(Location::Path(path.clone()));
                                commands.push(Command::OpenInNewTab(path.clone()));
                            } else {
                                commands.push(Command::OpenFile(vec![path.clone()]));
                            }
                        } else {
                            log::warn!("no path for item {clicked_item:?}");
                        }
                    } else {
                        log::warn!("no item for click index {click_i:?}");
                    }
                }
            }
            Message::HighlightDeactivate(i) => {
                self.watch_drag = true;
                if let Some(item) = self.items_opt.as_mut().and_then(|f| f.get_mut(i)) {
                    item.highlighted = false;
                }
            }
            Message::HighlightActivate(i) => {
                self.watch_drag = true;
                if let Some(item) = self.items_opt.as_mut().and_then(|f| f.get_mut(i)) {
                    item.highlighted = true;
                }
            }
            Message::Resize(viewport) => {
                // Scroll to ensure focused item still in view
                if self.viewport_opt.map(|v| v.size()) != Some(viewport.size())
                    && let Some(offset) = self.select_focus_scroll()
                {
                    commands.push(Command::Iced(
                        scrollable::scroll_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }

                self.viewport_opt = Some(viewport);
            }
            Message::Scroll(viewport) => {
                self.scroll_opt = Some(viewport.absolute_offset());
                self.watch_drag = true;
            }
            Message::ScrollTab(scroll_speed) => {
                commands.push(Command::Iced(
                    scrollable::scroll_by(
                        self.scrollable_id.clone(),
                        AbsoluteOffset {
                            x: 0.0,
                            y: scroll_speed,
                        },
                    )
                    .into(),
                ));
            }
            Message::ScrollRestore => {
                if let Some(offset) = self.pending_scroll.take() {
                    commands.push(Command::Iced(
                        scrollable::scroll_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }
            }
            Message::ScrollToFocused => {
                if let Some(offset) = self.select_focus_scroll() {
                    commands.push(Command::Iced(
                        scrollable::scroll_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }
            }
            Message::SearchContext(location, context) => {
                if location == self.location {
                    self.search_context = context.0;
                } else {
                    log::warn!(
                        "search context provided for {:?} instead of {:?}",
                        location,
                        self.location
                    );
                }
            }
            Message::SearchReady(finished) => {
                let max_results = usize::from(self.config.max_search_results.get());
                let sizes = self.config.icon_sizes;
                if let Some(context) = &mut self.search_context {
                    if let Some(items) = &mut self.items_opt {
                        if finished || context.ready.swap(false, atomic::Ordering::SeqCst) {
                            let duration = Instant::now();
                            while let Ok(search_item) = context.results_rx.try_recv() {
                                // Newest first, which is the sort order search locations are
                                // pinned to in `sort_options`
                                let index =
                                    if let SearchItem::Path(_, _, ref metadata) = search_item {
                                        let item_modified = metadata.modified().ok();
                                        match items.binary_search_by(|other| {
                                            item_modified.cmp(&other.metadata.modified())
                                        }) {
                                            Ok(index) => index,
                                            Err(index) => index,
                                        }
                                    } else {
                                        items.len()
                                    };

                                if index < max_results {
                                    let item = item_from_search_item(search_item, sizes);
                                    items.insert(index, item);
                                }
                                // Ensure that updates make it to the GUI in a timely manner
                                if !finished && duration.elapsed() >= MAX_SEARCH_LATENCY {
                                    break;
                                }
                            }
                        }
                        if items.len() >= max_results {
                            items.truncate(max_results);
                            if let Some(last_modified) =
                                items.last().and_then(|item| item.metadata.modified())
                            {
                                *context.last_modified_opt.write().unwrap() = Some(last_modified);
                            }
                        }
                    } else {
                        log::warn!("search ready but items array is empty");
                    }
                }
                if finished {
                    self.search_context = None;
                }
                // Results arrive item by item rather than through a scan, so
                // their icons need asking for separately or they stay generic
                if let Some(items) = self.items_opt.as_deref_mut() {
                    let warmup = refresh_icons(items, sizes);
                    if !warmup.is_empty() {
                        commands.push(Command::WarmIcons(warmup, sizes));
                    }
                }
            }
            Message::SelectAll => {
                self.select_all();
                if self.select_focus.take().is_some() {
                    // Unfocus currently focused button
                    commands.push(Command::Iced(
                        widget::button::focus(widget::Id::unique()).into(),
                    ));
                }
            }
            Message::SelectFirst => {
                if self.select_position(0, 0, mod_shift) {
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::scroll_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::SelectLast => {
                if let Some(ref items) = self.items_opt
                    && let Some(last_pos) = items.iter().filter_map(|item| item.pos_opt.get()).max()
                    && self.select_position(last_pos.0, last_pos.1, mod_shift)
                {
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::scroll_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::SetOpenWith(mime, id) => {
                commands.push(Command::SetOpenWith(mime, id));
            }
            Message::SetPermissions(path, mode) => {
                commands.push(Command::SetPermissions(path, mode));
            }
            Message::ShiftPermissions(path_mode_opt, shift, bits) => match path_mode_opt {
                Some((path, mode)) => commands.push(Command::SetPermissions(
                    path,
                    set_mode_part(mode, shift, bits),
                )),
                // Shift permissions on all selected items
                None => {
                    let mut permissions = Vec::new();
                    for item in self.items_opt().map_or(Vec::new(), |items| {
                        items.iter().filter(|item| item.selected).collect()
                    }) {
                        if let (Some(path), Some(mode)) = (
                            item.path_opt(),
                            item.file_metadata().map(|metadata| metadata.mode()),
                        ) {
                            permissions.push((path.clone(), set_mode_part(mode, shift, bits)));
                        }
                    }
                    commands.push(Command::SetMultiplePermissions(permissions));
                }
            },
            Message::SetSort(heading_option, dir) => {
                if !matches!(self.location, Location::Search(..)) {
                    self.sort_name = heading_option;
                    self.sort_direction = dir;
                    commands.push(Command::SetSort(
                        self.location.normalize().to_string(),
                        heading_option,
                        self.sort_direction,
                    ));
                }
            }
            Message::TabComplete(path, completions) => {
                if let Some(edit_location) = &mut self.edit_location
                    && edit_location.location.path_opt() == Some(&path)
                {
                    edit_location.completions = Some(completions);
                    commands.push(Command::Iced(
                        widget::text_input::focus(self.edit_location_id.clone()).into(),
                    ));
                }
            }
            Message::IconsReady => {
                let sizes = self.config.icon_sizes;
                if let Some(ref mut items) = self.items_opt {
                    // Refresh only. Asking for more work here could never
                    // settle for a type the icon theme has nothing for.
                    let _ = refresh_icons(items, sizes);
                }
            }
            Message::Thumbnail(path, thumbnail) => {
                if let Some(ref mut items) = self.items_opt {
                    let location = Location::Path(path);
                    for item in items.iter_mut() {
                        if item.location_opt.as_ref() == Some(&location) {
                            let handle_opt = match &thumbnail {
                                ItemThumbnail::NotImage => None,
                                ItemThumbnail::Image(handle, _) => Some(widget::icon::Handle {
                                    symbolic: false,
                                    data: widget::icon::Data::Image(handle.clone()),
                                }),
                                ItemThumbnail::Svg(handle) => Some(widget::icon::Handle {
                                    symbolic: false,
                                    data: widget::icon::Data::Svg(handle.clone()),
                                }),
                                ItemThumbnail::Text(_text) => None,
                            };
                            if let Some(handle) = handle_opt {
                                item.icon_handle_grid.clone_from(&handle);
                                item.icon_handle_list.clone_from(&handle);
                                item.icon_handle_list_condensed = handle;
                            }
                            item.thumbnail_opt = Some(thumbnail);
                            break;
                        }
                    }
                }
            }
            Message::ImageDecoded(path, width, height, pixels, display_size, generation) => {
                // Create handle from pre-decoded RGBA data (fast!)
                let handle = widget::image::Handle::from_rgba(width, height, pixels);

                // Store decoded image handle if generation still matches (not superseded)
                self.large_image_manager.store_decoded_with_generation(
                    path,
                    handle,
                    display_size,
                    generation,
                );
            }
            Message::ColumnResizeStart(divider) => {
                if let Some(geometry) = self.list_geometry() {
                    self.column_resize = Some(ColumnResize {
                        divider,
                        start: geometry.widths,
                        start_name_room: geometry.name_room,
                    });
                }
            }
            Message::ColumnResizeDrag(delta_x) => {
                let Some(resize) = self.column_resize else {
                    return commands;
                };
                if resize.divider == ColumnDivider::SizeType && !self.config.show_type_column {
                    self.column_resize = None;
                    return commands;
                }
                // The divider trades width between its two neighbours, clamped so
                // neither goes below its minimum. Everything else stays put, so
                // the divider follows the pointer.
                let (left_start, left_min, right_start) = match resize.divider {
                    ColumnDivider::NameModified => {
                        (resize.start_name_room, NAME_MIN, resize.start.modified)
                    }
                    ColumnDivider::ModifiedSize => {
                        (resize.start.modified, COLUMN_MIN, resize.start.size)
                    }
                    ColumnDivider::SizeType => (resize.start.size, COLUMN_MIN, resize.start.type_),
                };
                let lo = left_min - left_start;
                let hi = right_start - COLUMN_MIN;
                if lo > hi {
                    return commands;
                }
                let delta_x = delta_x.round().clamp(lo, hi);
                let widths = &mut self.column_widths;
                match resize.divider {
                    ColumnDivider::NameModified => {
                        widths.modified = resize.start.modified - delta_x;
                    }
                    ColumnDivider::ModifiedSize => {
                        widths.modified = resize.start.modified + delta_x;
                        widths.size = resize.start.size - delta_x;
                    }
                    ColumnDivider::SizeType => {
                        widths.size = resize.start.size + delta_x;
                        widths.type_ = resize.start.type_ - delta_x;
                    }
                }
            }
            Message::ColumnResizeEnd => {
                self.column_resize = None;
            }
            Message::ToggleSort(heading_option) => {
                if !matches!(self.location, Location::Search(..)) {
                    let heading_sort = if self.sort_name == heading_option {
                        !self.sort_direction
                    } else {
                        // Default modified to descending, and others to ascending.
                        heading_option != HeadingOptions::Modified
                    };

                    commands.push(Command::SetSort(
                        self.location.normalize().to_string(),
                        heading_option,
                        heading_sort,
                    ));

                    self.sort_direction = heading_sort;
                    self.sort_name = heading_option;
                }
            }
            Message::WindowDrag => {
                commands.push(Command::WindowDrag);
            }
            Message::WindowToggleMaximize => {
                commands.push(Command::WindowToggleMaximize);
            }
            Message::ZoomIn => {
                commands.push(Command::Action(Action::ZoomIn));
            }
            Message::ZoomOut => {
                commands.push(Command::Action(Action::ZoomOut));
            }
            Message::DirectorySize(path, dir_size) => {
                let location = Location::Path(path);
                if let Some(ref mut item) = self.parent_item_opt
                    && item.location_opt.as_ref() == Some(&location)
                {
                    item.dir_size.clone_from(&dir_size);
                }
                if let Some(ref mut items) = self.items_opt {
                    for item in items.iter_mut() {
                        if item.location_opt.as_ref() == Some(&location) {
                            item.dir_size = dir_size;
                            break;
                        }
                    }
                }
            }
            Message::DirectoryChildren(path, children) => {
                if let Some(ref mut items) = self.items_opt {
                    for item in items.iter_mut() {
                        if item.path_opt() == Some(&path) {
                            match &mut item.metadata {
                                ItemMetadata::Path { children_opt, .. } => {
                                    *children_opt = Some(children);
                                }
                                #[cfg(feature = "gvfs")]
                                ItemMetadata::GvfsPath { children_opt, .. } => {
                                    *children_opt = Some(children);
                                }
                                _ => {}
                            }
                            break;
                        }
                    }
                }
            }
            Message::Checksums(path, checksum_state) => {
                let location = Location::Path(path);
                if let Some(ref mut item) = self.parent_item_opt
                    && item.location_opt.as_ref() == Some(&location)
                {
                    item.checksums = checksum_state.clone();
                }
                if let Some(ref mut items) = self.items_opt {
                    for item in items.iter_mut() {
                        if item.location_opt.as_ref() == Some(&location) {
                            item.checksums = checksum_state;
                            break;
                        }
                    }
                }
            }
            Message::CalculateChecksums(path) => {
                let location = Location::Path(path.clone());
                if let Some(ref mut item) = self.parent_item_opt
                    && item.location_opt.as_ref() == Some(&location)
                {
                    item.checksums = ChecksumState::Calculating;
                }
                if let Some(ref mut items) = self.items_opt {
                    for item in items.iter_mut() {
                        if item.location_opt.as_ref() == Some(&location) {
                            item.checksums = ChecksumState::Calculating;
                            break;
                        }
                    }
                }
                commands.push(Command::Iced(
                    crate::ui::Task::future(async move {
                        match calculate_checksums(&path).await {
                            Ok(checksums) => {
                                Message::Checksums(path, ChecksumState::Calculated(checksums))
                            }
                            Err(err) => Message::Checksums(path, ChecksumState::Error(err)),
                        }
                    })
                    .into(),
                ));
            }
            Message::CopyChecksum(value) => {
                commands.push(Command::Iced(
                    crate::ui::iced::clipboard::write(value).into(),
                ));
            }
        }

        // Scroll to top if needed
        if self.scroll_opt.is_none() {
            let offset = AbsoluteOffset { x: 0.0, y: 0.0 };
            self.scroll_opt = Some(offset);
            commands.push(Command::Iced(
                scrollable::scroll_to(
                    self.scrollable_id.clone(),
                    AbsoluteOffset {
                        x: Some(0.0),
                        y: Some(0.0),
                    },
                )
                .into(),
            ));
        }

        // Change directory if requested
        if let Some(mut location) = cd {
            location = location.normalize();
            // Select parent if location is not directory
            let mut selected_paths = None;
            if let Some(path) = location.path_opt()
                && !path.is_dir()
                && let Some(parent) = path.parent()
            {
                selected_paths = Some(vec![path.clone()]);
                location = location.with_path(parent.to_path_buf());
            }
            if location != self.location || selected_paths.is_some() {
                if location.path_opt().is_none_or(|path| path.is_dir()) {
                    if selected_paths.is_none() {
                        selected_paths = self.location.path_opt().map(|path| vec![path.clone()]);
                    }
                    self.change_location(&location, history_i_opt);
                    commands.push(Command::ChangeLocation(
                        self.title(),
                        location,
                        selected_paths,
                    ));
                } else {
                    log::warn!("tried to cd to {location:?} which is not a directory");
                }
            }
        }

        commands
    }

    pub(crate) const fn sort_options(&self) -> (HeadingOptions, bool, bool) {
        match self.location {
            Location::Search(..) => (HeadingOptions::Modified, false, false),
            _ => (
                self.sort_name,
                self.sort_direction,
                self.config.folders_first,
            ),
        }
    }

    /// Names of the selected items, in the order they are displayed
    pub fn selected_names(&self) -> Vec<String> {
        self.column_sort()
            .map(|items| {
                items
                    .into_iter()
                    .filter(|(_, item)| item.selected)
                    .map(|(_, item)| item.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn column_sort(&self) -> Option<Vec<(usize, &Item)>> {
        let check_reverse = |ord: Ordering, sort: bool| {
            if sort { ord } else { ord.reverse() }
        };
        let mut items: Vec<_> = self.items_opt.as_ref()?.iter().enumerate().collect();
        let (sort_name, sort_direction, folders_first) = self.sort_options();
        match sort_name {
            HeadingOptions::Size => {
                items.sort_by(|a, b| {
                    // entries take precedence over size
                    let get_size = |x: &Item| match &x.metadata {
                        ItemMetadata::Path {
                            metadata,
                            children_opt,
                        } => {
                            if metadata.is_dir() {
                                (true, children_opt.unwrap_or_default() as u64)
                            } else {
                                (false, metadata.len())
                            }
                        }
                        ItemMetadata::Trash { metadata, .. } => match metadata.size {
                            trash::TrashItemSize::Entries(entries) => (true, entries as u64),
                            trash::TrashItemSize::Bytes(bytes) => (false, bytes),
                        },
                        ItemMetadata::SimpleDir { entries } => (true, *entries),
                        ItemMetadata::SimpleFile { size } => (false, *size),
                        #[cfg(feature = "gvfs")]
                        ItemMetadata::GvfsPath {
                            size_opt,
                            children_opt,
                            is_dir,
                            ..
                        } => {
                            if *is_dir {
                                (true, children_opt.unwrap_or_default() as u64)
                            } else {
                                (false, size_opt.unwrap_or_default())
                            }
                        }
                    };
                    let (a_is_entry, a_size) = get_size(a.1);
                    let (b_is_entry, b_size) = get_size(b.1);

                    match (a_is_entry, b_is_entry) {
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        _ => check_reverse(a_size.cmp(&b_size), sort_direction),
                    }
                });
            }
            HeadingOptions::Name => items.sort_by(|a, b| {
                if folders_first {
                    match (a.1.metadata.is_dir(), b.1.metadata.is_dir()) {
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        _ => check_reverse(
                            LANGUAGE_SORTER.compare(&a.1.display_name, &b.1.display_name),
                            sort_direction,
                        ),
                    }
                } else {
                    check_reverse(
                        LANGUAGE_SORTER.compare(&a.1.display_name, &b.1.display_name),
                        sort_direction,
                    )
                }
            }),
            HeadingOptions::Type => items.sort_by(|a, b| {
                let by_type = || {
                    check_reverse(
                        crate::file_category::compare_by_type(
                            a.1.metadata.is_dir(),
                            &a.1.mime,
                            &a.1.display_name,
                            b.1.metadata.is_dir(),
                            &b.1.mime,
                            &b.1.display_name,
                        ),
                        sort_direction,
                    )
                };
                if folders_first {
                    match (a.1.metadata.is_dir(), b.1.metadata.is_dir()) {
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        _ => by_type(),
                    }
                } else {
                    by_type()
                }
            }),
            HeadingOptions::Modified => {
                items.sort_by(|a, b| {
                    let a_modified = a.1.metadata.modified();
                    let b_modified = b.1.metadata.modified();
                    if folders_first {
                        match (a.1.metadata.is_dir(), b.1.metadata.is_dir()) {
                            (true, false) => Ordering::Less,
                            (false, true) => Ordering::Greater,
                            _ => check_reverse(a_modified.cmp(&b_modified), sort_direction),
                        }
                    } else {
                        check_reverse(a_modified.cmp(&b_modified), sort_direction)
                    }
                });
            }
            HeadingOptions::TrashedOn => {
                let time_deleted = |x: &Item| match &x.metadata {
                    ItemMetadata::Trash { entry, .. } => Some(entry.time_deleted),
                    _ => None,
                };

                items.sort_by(|a, b| {
                    let a_time_deleted = time_deleted(a.1);
                    let b_time_deleted = time_deleted(b.1);
                    if folders_first {
                        match (a.1.metadata.is_dir(), b.1.metadata.is_dir()) {
                            (true, false) => Ordering::Less,
                            (false, true) => Ordering::Greater,
                            _ => check_reverse(a_time_deleted.cmp(&b_time_deleted), sort_direction),
                        }
                    } else {
                        check_reverse(b_time_deleted.cmp(&a_time_deleted), sort_direction)
                    }
                });
            }
        }
        Some(items)
    }

    pub fn gallery_view(&self) -> Element<'_, Message> {
        let Spacing {
            space_xxs,
            space_xs,
            space_m,
            ..
        } = spacing();

        let mut name_opt = None;
        let mut element_opt: Option<Element<Message>> = None;
        if let Some(index) = self.select_focus
            && let Some(items) = &self.items_opt
            && let Some(item) = items.get(index)
        {
            name_opt = Some(widget::text::heading(&item.display_name));
            match item
                .thumbnail_opt
                .as_ref()
                .unwrap_or(&ItemThumbnail::NotImage)
            {
                ItemThumbnail::NotImage => {}
                ItemThumbnail::Image(handle, original_dims) => {
                    // Determine which image to show based on async decode state
                    let mut is_loading = false;
                    let mut error_msg_opt = None;
                    let image_handle = if let Some(path) = item.path_opt() {
                        if let Some(error_msg) = self.large_image_manager.get_error(path) {
                            error_msg_opt = Some(error_msg.clone());
                            handle.clone()
                        } else if self.large_image_manager.is_decoding(path) {
                            // Currently decoding (initial or re-decode) --> show cached/thumbnail with loading indicator
                            is_loading = true;
                            // Use decoded handle if available (re-decode), otherwise thumbnail (initial decode)
                            self.large_image_manager
                                .get_decoded(path)
                                .cloned()
                                .unwrap_or_else(|| handle.clone())
                        } else if let Some(decoded_handle) =
                            self.large_image_manager.get_decoded(path)
                        {
                            // Decoded and not currently decoding --> use it
                            decoded_handle.clone()
                        } else if let Some((w, h)) = original_dims {
                            // Check if image needs tiling
                            if should_use_tiling(*w, *h) {
                                // Large image --> show thumbnail only
                                handle.clone()
                            } else {
                                // Normal-sized image --> load full resolution directly
                                widget::image::Handle::from_path(path)
                            }
                        } else {
                            // No dimensions available --> show thumbnail
                            handle.clone()
                        }
                    } else {
                        handle.clone()
                    };

                    let content: crate::ui::Element<'_, Message> = if let Some(error_msg) =
                        error_msg_opt
                    {
                        widget::Column::with_capacity(2)
                            .push(widget::image(image_handle))
                            .push(
                                widget::text(fl!("image-load-error", error = error_msg.clone()))
                                    .size(13),
                            )
                            .padding(space_xs)
                            .align_x(crate::ui::iced::Alignment::Center)
                            .into()
                    } else if is_loading {
                        widget::Column::with_capacity(2)
                            .push(widget::image(image_handle))
                            .push(widget::text(fl!("loading-full-image")).size(14))
                            .padding(space_xs)
                            .align_x(crate::ui::iced::Alignment::Center)
                            .into()
                    } else {
                        crate::load_image::loaded_image(image_handle).into()
                    };

                    element_opt = Some(widget::container(content).center(Length::Fill).into());
                }
                ItemThumbnail::Svg(handle) => {
                    element_opt = Some(
                        widget::svg(handle.clone())
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .into(),
                    );
                }
                ItemThumbnail::Text(text) => {
                    element_opt = Some(
                        widget::container(
                            widget::text_editor::text_editor(text)
                                .padding(space_xxs)
                                .style(text_editor_class),
                        )
                        .center(Length::Fill)
                        .into(),
                    );
                }
            }
        }

        let mut column = widget::Column::with_capacity(2);
        column = column.push(widget::space::vertical().height(Length::Fixed(space_m.into())));
        {
            let mut row = widget::Row::with_capacity(5).align_y(Alignment::Center);
            row = row.push(widget::space::horizontal());
            if let Some(name) = name_opt {
                row = row.push(name);
            }
            row = row.push(widget::space::horizontal());
            row = row.push(
                widget::button::icon(widget::icon::from_name("window-close-symbolic"))
                    .class(Button::Standard)
                    .on_press(Message::Gallery(false)),
            );
            row = row.push(widget::space::horizontal().width(Length::Fixed(space_m.into())));
            // This mouse area provides window drag while the header bar is hidden
            let mouse_area = mouse_area::MouseArea::new(row)
                .on_press(|_| Message::WindowDrag)
                .on_double_click(|_| Message::WindowToggleMaximize);
            column = column.push(mouse_area);
        }
        {
            let mut row = widget::Row::with_capacity(7).align_y(Alignment::Center);
            row = row.push(widget::space::horizontal().width(Length::Fixed(space_m.into())));
            row = row.push(
                widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                    .padding(space_xs)
                    .class(Button::Standard)
                    .on_press(Message::GalleryPrevious),
            );
            row = row.push(widget::space::horizontal().width(Length::Fixed(space_xxs.into())));
            if let Some(element) = element_opt {
                row = row.push(element);
            } else {
                row = row.push(space::horizontal().width(Length::Fill));
                row = row.push(space::vertical().height(Length::Fill));
            }
            row = row.push(widget::space::horizontal().width(Length::Fixed(space_xxs.into())));
            row = row.push(
                widget::button::icon(widget::icon::from_name("go-next-symbolic"))
                    .padding(space_xs)
                    .class(Button::Standard)
                    .on_press(Message::GalleryNext),
            );
            row = row.push(widget::space::horizontal().width(Length::Fixed(space_m.into())));
            column = column.push(row);
        }

        widget::container(column)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|theme| {
                let cosmic = theme.cosmic();
                let mut bg = cosmic.bg_color();
                bg.alpha = 0.75;
                widget::container::Style {
                    background: Some(bg.to_color().into()),
                    ..Default::default()
                }
            })
            .into()
    }

    pub fn location_view(&self) -> Element<'_, Message> {
        fn text_width<'a>(
            content: &'a str,
            font: font::Font,
            font_size: f32,
            line_height: f32,
        ) -> f32 {
            let text: text::Text<&'a str, font::Font> = text::Text {
                content,
                bounds: Size::INFINITE,
                size: font_size.into(),
                line_height: text::LineHeight::Absolute(line_height.into()),
                font,
                align_x: text::Alignment::Left,
                align_y: Vertical::Top,
                shaping: text::Shaping::default(),
                wrapping: text::Wrapping::None,
                // Bounds are infinite here, so no ellipsizing can occur: this is a
                // measurement paragraph, not a drawn one.
            };
            graphics::text::Paragraph::with_text(text)
                .min_bounds()
                .width
        }
        fn text_width_body(content: &str) -> f32 {
            text_width(content, font::default(), 14.0, 20.0)
        }
        fn text_width_heading(content: &str) -> f32 {
            text_width(content, font::semibold(), 14.0, 20.0)
        }

        let Spacing {
            space_xxxs,
            space_xxs,
            space_s,
            space_m,
            ..
        } = spacing();

        let size = self.size_opt.get().unwrap_or(Size::new(0.0, 0.0));

        let mut row = widget::Row::with_capacity(5)
            .align_y(Alignment::Center)
            .padding([space_xxxs, 0]);
        let mut w = 0.0;

        let mut prev_button =
            widget::button::custom(widget::icon::from_name("go-previous-symbolic").size(16))
                .padding(space_xxs)
                .class(Button::Icon);
        if self.history_i > 0 && !self.history.is_empty() {
            prev_button = prev_button.on_press(Message::GoPrevious);
        }
        row = row.push(prev_button);
        w += f32::from(space_xxs).mul_add(2.0, 16.0);

        let mut next_button =
            widget::button::custom(widget::icon::from_name("go-next-symbolic").size(16))
                .padding(space_xxs)
                .class(Button::Icon);
        if self.history_i + 1 < self.history.len() {
            next_button = next_button.on_press(Message::GoNext);
        }
        row = row.push(next_button);
        w += f32::from(space_xxs).mul_add(2.0, 16.0);

        row = row.push(widget::space::horizontal().width(Length::Fixed(space_s.into())));
        w += f32::from(space_s);

        let geometry = self.list_geometry();
        let condensed = geometry.is_none();
        // With no geometry the header is not shown; any widths do for the build
        let geometry = geometry.unwrap_or(ListGeometry {
            widths: self.column_widths,
            show_type: self.config.show_type_column,
            name_room: 0.0,
        });

        let (sort_name, sort_direction, _) = self.sort_options();
        let heading =
            |label: String, width: Length, option: HeadingOptions| -> Element<'_, Message> {
                let mut content = widget::Row::with_capacity(2)
                    .align_y(Alignment::Center)
                    .spacing(space_xxxs.to_pixels())
                    .width(width)
                    .height(Length::Fill);
                content = content.push(widget::text::heading(label));
                match (sort_name == option, sort_direction) {
                    (true, true) => {
                        content =
                            content.push(widget::icon::from_name("pan-down-symbolic").size(16));
                    }
                    (true, false) => {
                        content = content.push(widget::icon::from_name("pan-up-symbolic").size(16));
                    }
                    _ => {}
                }
                // A pointer cursor marks the heading as clickable; no hover effect
                mouse_area::MouseArea::new(content)
                    .interaction(crate::ui::iced_core::mouse::Interaction::Pointer)
                    .on_press(move |_| Message::ToggleSort(option))
                    .into()
            };
        // Each divider occupies the gap between two cells plus `DIVIDER_GRAB`
        // of the heading on its left, which is made that much narrower. The
        // fixed widths right of any boundary therefore add up exactly as in
        // the rows, which is what keeps headings over their cells.
        let divider = |divider: ColumnDivider| -> Element<'_, Message> {
            mouse_area::MouseArea::new(
                widget::space::horizontal()
                    .width(Length::Fixed(f32::from(space_xxs) + DIVIDER_GRAB))
                    .height(Length::Fill),
            )
            .interaction(crate::ui::iced_core::mouse::Interaction::ResizingHorizontally)
            .on_press(move |_| Message::ColumnResizeStart(divider))
            .on_drag_delta(|delta| Message::ColumnResizeDrag(delta.x))
            .on_drag_end(|_| Message::ColumnResizeEnd)
            .on_release(|_| Message::ColumnResizeEnd)
            .into()
        };

        let (modified_label, modified_option) = if self.location.is_trash() {
            (fl!("trashed-on"), HeadingOptions::TrashedOn)
        } else {
            (fl!("modified"), HeadingOptions::Modified)
        };
        let widths = geometry.widths;
        let mut headings = vec![
            heading(fl!("name"), Length::Fill, HeadingOptions::Name),
            divider(ColumnDivider::NameModified),
            heading(
                modified_label,
                Length::Fixed(widths.modified - DIVIDER_GRAB),
                modified_option,
            ),
            divider(ColumnDivider::ModifiedSize),
        ];
        if geometry.show_type {
            headings.push(heading(
                fl!("size"),
                Length::Fixed(widths.size - DIVIDER_GRAB),
                HeadingOptions::Size,
            ));
            headings.push(divider(ColumnDivider::SizeType));
            headings.push(heading(
                fl!("type-heading"),
                Length::Fixed(widths.type_),
                HeadingOptions::Type,
            ));
        } else {
            headings.push(heading(
                fl!("size"),
                Length::Fixed(widths.size),
                HeadingOptions::Size,
            ));
        }
        let heading_row = widget::Row::with_children(headings)
            .align_y(Alignment::Center)
            .height(Length::Fixed((space_m + 4).into()))
            .padding([0, space_xxs]);

        let accent_rule = rule::horizontal(1).class(Rule::Custom(Box::new(|theme| rule::Style {
            color: theme.cosmic().accent_color().to_color(),
            radius: 0.0.into(),
            fill_mode: rule::FillMode::Full,
            snap: true,
        })));
        let heading_rule = widget::container(rule::horizontal(1))
            .padding([0, theme::active().cosmic().corner_radii.radius_xs[0] as u16]);

        if let Some(edit_location) = &self.edit_location {
            let mut text_input = None;

            if let Location::Network(ref uri, ..) = edit_location.location {
                let location = edit_location.location.clone();
                text_input = Some(
                    widget::text_input("", uri.clone())
                        .id(self.edit_location_id.clone())
                        .on_input(move |input| {
                            Message::EditLocation(Some(location.with_uri(input).into()))
                        })
                        .on_submit(|_| Message::EditLocationSubmit)
                        .line_height(1.0),
                );
            } else if let Some(resolved_location) = edit_location.resolve()
                && let Some(path) = resolved_location.path_opt().cloned()
            {
                text_input = Some(
                    widget::text_input("", path.to_string_lossy().into_owned())
                        .id(self.edit_location_id.clone())
                        .on_input(move |input| {
                            Message::EditLocation(Some(
                                resolved_location.with_path(PathBuf::from(input)).into(),
                            ))
                        })
                        .on_submit(|_| Message::EditLocationSubmit)
                        .on_tab(Message::EditLocationTab)
                        .on_unfocus(Message::EditLocation(None))
                        .line_height(1.0),
                );
            }
            if let Some(text_input) = text_input {
                row = row.push(
                    widget::button::custom(
                        widget::icon::from_name("window-close-symbolic").size(16),
                    )
                    .on_press(Message::EditLocation(None))
                    .padding(space_xxs)
                    .class(Button::Icon),
                );
                let mut popover =
                    widget::popover(text_input).position(widget::popover::Position::Bottom);
                if let Some(completions) = &edit_location.completions
                    && !completions.is_empty()
                {
                    let mut column =
                        widget::Column::with_capacity(completions.len()).padding(space_xxs);
                    for (i, (name, _path)) in completions.iter().enumerate() {
                        let selected = edit_location.selected == Some(i);
                        column = column.push(
                            widget::button::custom(widget::text::body(name))
                                .class(if selected {
                                    Button::Standard
                                } else {
                                    Button::HeaderBar
                                })
                                .on_press(Message::EditLocationComplete(i))
                                .padding(space_xxs)
                                .width(Length::Fill),
                        );
                    }
                    popover = popover.popup(
                        widget::container(column)
                            .class(Container::Dropdown)
                            .max_width(size.width - 140.0),
                    );
                }
                row = row.push(popover);
                let mut column = widget::Column::with_capacity(4).padding([0, space_s]);
                column = column.push(row);
                column = column.push(accent_rule);
                if self.config.view == View::List && !condensed {
                    column = column.push(heading_row);
                    column = column.push(heading_rule);
                }
                return column.into();
            }
        } else if let Some(path) = self.location.path_opt() {
            row = row.push(
                crate::mouse_area::MouseArea::new(
                    widget::button::custom(widget::icon::from_name("edit-symbolic").size(16))
                        .padding(space_xxs)
                        .class(Button::Icon)
                        .on_press(Message::EditLocation(Some(self.location.clone().into()))),
                )
                .on_middle_press(move |_| Message::OpenInNewTab(path.clone())),
            );
            w += f32::from(space_xxs).mul_add(2.0, 16.0);
        }

        let mut children: Vec<Element<_>> = Vec::new();
        match &self.location {
            Location::Path(path) | Location::Search(SearchLocation::Path(path), ..) => {
                let excess_str = "...";
                let excess_width = text_width_body(excess_str);
                // Read from the list built when the location changed. Naming a
                // segment means stating it, and on a network mount querying
                // it, which must not happen once per segment per frame.
                for (index, (ancestor_location, name)) in self.location_ancestors.iter().enumerate()
                {
                    let Some(ancestor) = ancestor_location.path_opt() else {
                        continue;
                    };
                    let name = name.clone();
                    let (name_width, name_text): (f32, Element<'_, Message>) =
                        if children.is_empty() {
                            (
                                text_width_heading(&name),
                                widget::ellipsize::heading(name, widget::EllipsizeMode::End(1))
                                    .wrapping(text::Wrapping::None)
                                    .into(),
                            )
                        } else {
                            children.push(
                                widget::icon::from_name("go-next-symbolic")
                                    .size(16)
                                    .icon()
                                    .into(),
                            );
                            w += 16.0;
                            (
                                text_width_body(&name),
                                widget::text::body(name)
                                    .wrapping(text::Wrapping::None)
                                    .into(),
                            )
                        };

                    // Add padding for mouse area
                    w += 2.0 * f32::from(space_xxxs);

                    let mut row = widget::Row::with_capacity(2)
                        .align_y(Alignment::Center)
                        .spacing(space_xxxs.to_pixels());
                    let overflow_offset = 64.0;
                    let overflow = w + name_width + overflow_offset > size.width && index > 0;
                    if overflow {
                        row = row.push(widget::text::body(excess_str));
                        w += excess_width;
                    } else {
                        row = row.push(name_text);
                        w += name_width;
                    }

                    let location = ancestor_location.clone();
                    let mouse_area = crate::mouse_area::MouseArea::new(
                        widget::button::custom(row)
                            .padding(space_xxxs)
                            // Reuse `LinkActive`, the highlight for an open breadcrumb
                            // context menu, when a drag hovers over a breadcrumb.
                            .class(
                                if self.location_context_menu_index == Some(index)
                                    || self.dnd_ancestor == Some(index)
                                {
                                    Button::LinkActive
                                } else {
                                    Button::Link
                                },
                            )
                            .on_press(if ancestor == path {
                                Message::EditLocation(Some(self.location.clone().into()))
                            } else {
                                Message::Location(location.clone())
                            }),
                    );

                    let mouse_area = if let Location::Path(_) = &self.location {
                        mouse_area
                            .on_middle_press(move |_| Message::OpenInNewTab(ancestor.to_path_buf()))
                    } else {
                        mouse_area
                    };

                    // The last segment is the displayed directory; clicking it opens
                    // the location editor. Exclude it as a drop target because the
                    // files are already there. All ancestor segments are valid
                    // destinations.
                    let mouse_area = if ancestor == path {
                        mouse_area
                    } else {
                        mouse_area.on_dnd(move |dnd| {
                            Message::DndAncestor(index, ancestor.to_path_buf(), dnd)
                        })
                    };

                    // Each breadcrumb carries the menu for its own ancestor index
                    let mut context_menu = widget::context_menu(
                        mouse_area,
                        Some(menu::location_context_menu(index, &self.mode)),
                    )
                    .on_open(Message::LocationContextMenuIndex(Some(index)))
                    .on_close(Message::LocationContextMenuIndex(None))
                    .on_surface_action(Message::Surface);
                    if let Some(window_id) = self.window_id {
                        context_menu = context_menu.window_id(window_id);
                    }
                    children.push(context_menu.into());

                    // The list already stops at home, so overflow is the
                    // only reason left to stop early
                    if overflow {
                        break;
                    }
                }
                children.reverse();
            }
            Location::Trash | Location::Search(SearchLocation::Trash, ..) => {
                children.push(
                    widget::button::custom(widget::text::heading(fl!("trash")))
                        .padding(space_xxxs)
                        .on_press(Message::Location(Location::Trash))
                        .class(Button::Text)
                        .into(),
                );
            }
            Location::Recents | Location::Search(SearchLocation::Recents, ..) => {
                children.push(
                    widget::button::custom(widget::text::heading(fl!("recents")))
                        .padding(space_xxxs)
                        .on_press(Message::Location(Location::Recents))
                        .class(Button::Text)
                        .into(),
                );
            }
            Location::Network(uri, display_name, path) => {
                children.push(
                    widget::button::custom(widget::text::heading(display_name))
                        .padding(space_xxxs)
                        .on_press(Message::Location(Location::Network(
                            uri.clone(),
                            display_name.clone(),
                            path.clone(),
                        )))
                        .class(Button::Text)
                        .into(),
                );
            }
        }

        row = row.extend(children);
        let mut column = widget::Column::with_capacity(4).padding([0, space_s]);
        column = column.push(row);
        column = column.push(accent_rule);

        if self.config.view == View::List && !condensed {
            column = column.push(heading_row);
            column = column.push(heading_rule);
        }

        column.into()
    }

    pub fn empty_view(&self, has_hidden: bool) -> Element<'_, Message> {
        let Spacing { space_xxs, .. } = spacing();

        mouse_area::MouseArea::new(widget::Column::with_children([widget::container(
            widget::Column::with_children([
                widget::icon::from_name("folder-symbolic")
                    .size(64)
                    .icon()
                    .into(),
                widget::text::body(if has_hidden {
                    fl!("empty-folder-hidden")
                } else if matches!(self.location, Location::Search(..)) {
                    fl!("no-results")
                } else {
                    fl!("empty-folder")
                })
                .into(),
            ])
            .align_x(Alignment::Center)
            .spacing(space_xxs.to_pixels()),
        )
        .center(Length::Fill)
        .into()]))
        // Accept drops into empty folders. Without this handler, the compositor
        // accepted the drop but nobody read it, leaving an external source waiting for
        // `finish`.
        .on_dnd(Message::Dnd)
        .on_press(|_| Message::Click(None))
        .into()
    }

    pub fn grid_view(&self) -> (Element<'_, Message>, bool) {
        let Spacing {
            space_xxs,
            space_xxxs,
            ..
        } = spacing();

        let TabConfig {
            show_hidden,
            icon_sizes,
            ..
        } = self.config;

        // Intentionally the same as space_xxs; named separately to document
        // that this value is the spacing between grid items.
        let grid_spacing = space_xxs;

        let text_height = 3 * 20; // 3 lines of text
        let item_width = (3 * space_xxs + icon_sizes.grid() + 3 * space_xxs) as usize;
        let item_height =
            (space_xxxs + icon_sizes.grid() + space_xxxs + text_height + space_xxxs) as usize;

        let (width, height) = match self.size_opt.get() {
            Some(size) => (
                (size.width.floor() as usize)
                    .saturating_sub(2 * (space_xxs as usize))
                    .max(item_width),
                (size.height.floor() as usize).max(item_height),
            ),
            None => (item_width, item_height),
        };

        let (cols, column_spacing) = {
            let width_m1 = width.saturating_sub(item_width);
            let cols_m1 = width_m1 / (item_width + grid_spacing as usize);
            let cols = cols_m1 + 1;
            let spacing = width_m1
                .checked_div(cols_m1)
                .unwrap_or(0)
                .saturating_sub(item_width);
            (cols, spacing as u16)
        };

        let visible_rect = self.visible_rect(self.size_opt.get().unwrap_or_default());

        let mut grid = widget::grid()
            .column_spacing(column_spacing)
            .row_spacing(grid_spacing)
            .padding(space_xxs.into());

        let mut column = widget::Column::with_capacity(2);
        if let Some(items) = self.column_sort() {
            let mut count = 0;
            let mut col = 0;
            let mut row = 0;
            let mut hidden = 0;
            let mut grid_elements = Vec::new();
            for &(i, item) in &items {
                if !show_hidden && item.hidden {
                    item.pos_opt.set(None);
                    item.rect_opt.set(None);
                    hidden += 1;
                    continue;
                }
                item.pos_opt.set(Some((row, col)));
                let item_rect = Rectangle::new(
                    Point::new(
                        (col * (item_width + column_spacing as usize) + space_xxs as usize) as f32,
                        (row * (item_height + grid_spacing as usize) + space_xxs as usize) as f32,
                    ),
                    Size::new(item_width as f32, item_height as f32),
                );
                item.rect_opt.set(Some(item_rect));

                while grid_elements.len() <= row {
                    grid_elements.push(Vec::new());
                }

                // Only build elements if visible (for performance)
                if item_rect.intersects(&visible_rect) {
                    let buttons: Vec<Element<Message>> = vec![
                        widget::button::custom(
                            widget::icon::icon(item.icon_handle_grid.clone())
                                .content_fit(ContentFit::Contain)
                                .size(icon_sizes.grid()),
                        )
                        .padding(space_xxxs)
                        .class(button_style(
                            item.selected,
                            item.highlighted,
                            item.cut,
                            false,
                            false,
                        ))
                        .into(),
                        widget::tooltip(
                            widget::button::custom(Item::grid_display_name(&item.display_name))
                                .id(item.button_id.clone())
                                .padding([0, space_xxxs])
                                .class(button_style(
                                    item.selected,
                                    item.highlighted,
                                    item.cut,
                                    true,
                                    true,
                                )),
                            widget::text::body(&item.name),
                            widget::tooltip::Position::Bottom,
                        )
                        .into(),
                    ];

                    let mut column = widget::Column::with_capacity(buttons.len())
                        .align_x(Alignment::Center)
                        .height(Length::Fixed(item_height as f32))
                        .width(Length::Fixed(item_width as f32));
                    for button in buttons {
                        column = column.push(
                            mouse_area::MouseArea::new(button)
                                .on_right_press_no_capture()
                                .on_right_press(move |point_opt| {
                                    Message::RightClick(point_opt, Some(i))
                                }),
                        );
                    }

                    let mouse_area = crate::mouse_area::MouseArea::new(column)
                        .on_press(move |_| Message::Click(Some(i)))
                        .on_drag(move |_| Message::DragFiles(i))
                        .on_double_click(move |_| Message::DoubleClick(Some(i)))
                        .on_release(move |_| Message::ClickRelease(Some(i)))
                        .on_middle_press(move |_| Message::MiddleClick(i))
                        .on_enter(move || Message::HighlightActivate(i))
                        .on_exit(move || Message::HighlightDeactivate(i));
                    grid_elements[row].push(Element::from(mouse_area));
                } else {
                    // Add a spacer if the row is empty, so scroll works
                    if grid_elements[row].is_empty() {
                        grid_elements[row].push(Element::from(
                            widget::Column::with_capacity(0)
                                .width(Length::Fill)
                                .height(Length::Fixed(item_height as f32)),
                        ));
                    }
                }

                count += 1;
                col += 1;
                if col >= cols {
                    col = 0;
                    row += 1;
                }
            }

            for row_elements in grid_elements {
                for element in row_elements {
                    grid = grid.push(element);
                }
                grid = grid.insert_row();
            }

            if count == 0 {
                return (self.empty_view(hidden > 0), false);
            }

            column = column.push(grid);

            // Pad the content to the bottom of the view so the whole view is scrollable
            {
                let mut max_bottom = 0;
                for (_, item) in items {
                    if let Some(rect) = item.rect_opt.get() {
                        let bottom = (rect.y + rect.height).ceil() as usize;
                        if bottom > max_bottom {
                            max_bottom = bottom;
                        }
                    }
                }

                // Cache content height for scroll clamping on next frame
                self.content_height_opt.set(Some(max_bottom as f32));

                let top_deduct = 7 * (space_xxs as usize);

                self.item_view_size_opt
                    .set(self.size_opt.get().map(|s| Size {
                        width: s.width,
                        height: s.height - top_deduct as f32,
                    }));

                let spacer_height = height.saturating_sub(max_bottom + top_deduct);
                if spacer_height > 0 {
                    column = column.push(widget::container(
                        space::vertical().height(Length::Fixed(spacer_height as f32)),
                    ));
                }
            }
        }

        let mut mouse_area = mouse_area::MouseArea::new(column.width(Length::Fill))
            .on_dnd(Message::Dnd)
            .on_press(|_| Message::Click(None))
            .on_auto_scroll(Message::AutoScroll)
            .on_drag_end(|_| Message::DragEnd)
            .show_drag_rect(self.mode.multiple())
            .on_release(|_| Message::ClickRelease(None));
        if self.watch_drag {
            mouse_area = mouse_area.on_drag(Message::Drag);
        }

        (mouse_area.into(), true)
    }

    pub fn list_view(&self) -> (Element<'_, Message>, bool) {
        let Spacing {
            space_s, space_xxs, ..
        } = spacing();

        let TabConfig {
            show_hidden,
            icon_sizes,
            ..
        } = self.config;

        let size = self.size_opt.get().unwrap_or_else(|| Size::new(0.0, 0.0));
        let geometry = self.list_geometry();
        let condensed = geometry.is_none();
        let show_type_column = self.config.show_type_column;
        // Only read in the non-condensed layouts
        let widths = geometry.map(|g| g.widths).unwrap_or(self.column_widths);
        let modified_width = widths.modified;
        let size_width = widths.size;
        let type_width = if show_type_column { widths.type_ } else { 0.0 };
        let is_search = matches!(self.location, Location::Search(..));
        let icon_size = if condensed || is_search {
            icon_sizes.list_condensed()
        } else {
            icon_sizes.list()
        };
        let row_height = icon_size + 2 * space_xxs;

        let mut column = widget::Column::with_capacity(3);
        let mut y: f32 = 0.0;

        let rule_padding = theme::active().cosmic().corner_radii.radius_xs[0] as u16;

        let visible_rect = self.visible_rect(self.size_opt.get().unwrap_or_default());

        if let Some(items) = self.column_sort() {
            let mut count = 0;
            let mut hidden = 0;
            for (i, item) in items {
                if item.hidden && !show_hidden {
                    item.pos_opt.set(None);
                    item.rect_opt.set(None);
                    hidden += 1;
                    continue;
                }

                if count > 0 {
                    column = column
                        .push(widget::container(rule::horizontal(1)).padding([0, rule_padding]));
                    y += 1.0;
                }

                item.pos_opt.set(Some((count, 0)));
                let item_rect = Rectangle::new(
                    Point::new(f32::from(space_s), y),
                    Size::new(size.width - f32::from(2 * space_s), f32::from(row_height)),
                );
                item.rect_opt.set(Some(item_rect));

                // Only build elements if visible (for performance)
                let button_row = if item_rect.intersects(&visible_rect) {
                    let modified_text = match &item.metadata {
                        ItemMetadata::Path { metadata, .. } => match metadata.modified() {
                            Ok(time) => self.format_time(time).to_string(),
                            Err(_) => String::new(),
                        },
                        ItemMetadata::Trash { entry, .. } => FormatTime::from_secs(
                            entry.time_deleted,
                            &self.date_time_formatter,
                            &self.time_formatter,
                        )
                        .map(|t| t.to_string())
                        .unwrap_or_default(),
                        #[cfg(feature = "gvfs")]
                        ItemMetadata::GvfsPath { .. } => match item.metadata.modified() {
                            Some(mtime) => self.format_time(mtime).to_string(),
                            None => String::new(),
                        },
                        _ => String::new(),
                    };

                    let size_text = match &item.metadata {
                        ItemMetadata::Path {
                            metadata,
                            children_opt,
                        } => {
                            if metadata.is_dir() {
                                children_opt.map_or_else(String::new, |children| {
                                    fl!("item-count", count = children)
                                })
                            } else {
                                format_size(metadata.len())
                            }
                        }
                        ItemMetadata::Trash { metadata, .. } => match metadata.size {
                            trash::TrashItemSize::Entries(entries) => {
                                fl!("item-count", count = entries)
                            }
                            trash::TrashItemSize::Bytes(bytes) => format_size(bytes),
                        },
                        ItemMetadata::SimpleDir { entries } => fl!("item-count", count = entries),
                        ItemMetadata::SimpleFile { size } => format_size(*size),
                        #[cfg(feature = "gvfs")]
                        ItemMetadata::GvfsPath {
                            size_opt,
                            children_opt,
                            is_dir,
                            ..
                        } => {
                            if *is_dir {
                                // Children are not counted on remote filesystems
                                children_opt.map_or_else(String::new, |children| {
                                    fl!("item-count", count = children)
                                })
                            } else {
                                format_size(size_opt.unwrap_or_default())
                            }
                        }
                    };

                    let type_cell = || -> Element<'_, Message> {
                        widget::text::body(
                            crate::file_category::FileCategory::of(
                                item.metadata.is_dir(),
                                &item.mime,
                            )
                            .to_string(),
                        )
                        .width(Length::Fixed(type_width))
                        .into()
                    };

                    let row = if condensed {
                        widget::Row::with_children([
                            widget::icon::icon(item.icon_handle_list_condensed.clone())
                                .content_fit(ContentFit::Contain)
                                .size(icon_size)
                                .into(),
                            widget::Column::with_children([
                                Item::list_display_name(item.display_name.clone()).into(),
                                widget::text::caption(format!("{modified_text} - {size_text}"))
                                    .into(),
                            ])
                            .into(),
                        ])
                        .height(Length::Fixed(f32::from(row_height)))
                        .align_y(Alignment::Center)
                        .spacing(space_xxs.to_pixels())
                    } else if is_search {
                        let mut cells: Vec<Element<'_, Message>> = vec![
                            widget::icon::icon(item.icon_handle_list_condensed.clone())
                                .content_fit(ContentFit::Contain)
                                .size(icon_size)
                                .into(),
                            widget::Column::with_children([
                                Item::list_display_name(item.display_name.clone()).into(),
                                widget::text::caption(match item.path_opt() {
                                    Some(path) => path.display().to_string(),
                                    None => String::new(),
                                })
                                .into(),
                            ])
                            .width(Length::Fill)
                            .into(),
                            widget::text::body(modified_text.clone())
                                .width(Length::Fixed(modified_width))
                                .into(),
                            widget::text::body(size_text.clone())
                                .width(Length::Fixed(size_width))
                                .into(),
                        ];
                        if show_type_column {
                            cells.push(type_cell());
                        }
                        widget::Row::with_children(cells)
                            .height(Length::Fixed(f32::from(row_height)))
                            .align_y(Alignment::Center)
                            .spacing(space_xxs.to_pixels())
                    } else {
                        let mut cells: Vec<Element<'_, Message>> = vec![
                            widget::icon::icon(item.icon_handle_list.clone())
                                .content_fit(ContentFit::Contain)
                                .size(icon_size)
                                .into(),
                            Item::list_display_name(item.display_name.clone())
                                .width(Length::Fill)
                                .into(),
                            widget::text::body(modified_text.clone())
                                .width(Length::Fixed(modified_width))
                                .into(),
                            widget::text::body(size_text.clone())
                                .width(Length::Fixed(size_width))
                                .into(),
                        ];
                        if show_type_column {
                            cells.push(type_cell());
                        }
                        widget::Row::with_children(cells)
                            .height(Length::Fixed(f32::from(row_height)))
                            .align_y(Alignment::Center)
                            .spacing(space_xxs.to_pixels())
                    };

                    let button =
                        |row| {
                            let mouse_area = crate::mouse_area::MouseArea::new(
                                widget::button::custom(row)
                                    .width(Length::Fill)
                                    .id(item.button_id.clone())
                                    .padding([0, space_xxs])
                                    .class(button_style(
                                        item.selected,
                                        item.highlighted,
                                        item.cut,
                                        true,
                                        true,
                                    )),
                            )
                            .on_press(move |_| Message::Click(Some(i)))
                            .on_drag(move |_| Message::DragFiles(i))
                            .on_double_click(move |_| Message::DoubleClick(Some(i)))
                            .on_release(move |_| Message::ClickRelease(Some(i)))
                            .on_middle_press(move |_| Message::MiddleClick(i))
                            .on_enter(move || Message::HighlightActivate(i))
                            .on_exit(move || Message::HighlightDeactivate(i));

                            mouse_area.on_right_press_no_capture().on_right_press(
                                move |point_opt| Message::RightClick(point_opt, Some(i)),
                            )
                        };

                    let button_row: Element<_> = button(row).into();

                    button_row
                } else {
                    widget::Column::with_capacity(0)
                        .width(Length::Fill)
                        .height(Length::Fixed(f32::from(row_height)))
                        .into()
                };

                count += 1;
                y += f32::from(row_height);
                column = column.push(button_row);
            }

            if count == 0 {
                return (self.empty_view(hidden > 0), false);
            }

            // Cache content height for scroll clamping on next frame
            self.content_height_opt.set(Some(y));
        }
        // Pad the content to the bottom of the view so the whole view is scrollable
        {
            let top_deduct = (if condensed || is_search { 6 } else { 9 }) * space_xxs;

            self.item_view_size_opt
                .set(self.size_opt.get().map(|s| Size {
                    width: s.width,
                    height: s.height - f32::from(top_deduct),
                }));

            let spacer_height = size.height - y - f32::from(top_deduct);
            if spacer_height > 0. {
                column = column.push(widget::container(space::vertical().height(spacer_height)));
            }
        }
        let mut mouse_area = mouse_area::MouseArea::new(column.padding([0, space_s]))
            .with_id(Id::new("list-view"))
            .on_dnd(Message::Dnd)
            .on_press(|_| Message::Click(None))
            .on_auto_scroll(Message::AutoScroll)
            .on_drag_end(|_| Message::DragEnd)
            .show_drag_rect(self.mode.multiple())
            .on_release(|_| Message::ClickRelease(None));
        if self.watch_drag {
            mouse_area = mouse_area.on_drag(Message::Drag);
        }

        (mouse_area.into(), true)
    }

    pub fn view_responsive<'a>(
        &'a self,
        key_binds: &'a HashMap<KeyBind, Action>,
        modifiers: &'a Modifiers,
        size: Size,
        clipboard_paste_available: bool,
        context_actions: &'a [ContextActionPreset],
    ) -> Element<'a, Message> {
        // Update cached size
        self.size_opt.set(Some(size));

        let Spacing {
            space_xxs,
            space_xs,
            ..
        } = spacing();

        let location_view = self.location_view();
        let (item_view, can_scroll) = match self.config.view {
            View::Grid => self.grid_view(),
            View::List => self.list_view(),
        };
        let item_view: Element<'a, Message> =
            widget::container(item_view).width(Length::Fill).into();
        let mouse_area = mouse_area::MouseArea::new(item_view)
            .on_press(move |_point_opt| Message::Click(None))
            .on_release(|_| Message::ClickRelease(None))
            .on_resize(Message::Resize)
            .on_back_press(move |_point_opt| Message::GoPrevious)
            .on_forward_press(move |_point_opt| Message::GoNext)
            .on_scroll(|delta| respond_to_scroll_direction(delta, modifiers))
            .on_right_press(|_| Message::RightClickBackground);

        let items_area: Element<'_, Message> = if can_scroll {
            // FIXME: new responsive widget will remove the state from the scrollable
            // id_container with custom id forces the state to be extracted in a diff
            // pre-processing step
            widget::id_container(
                widget::scrollable(mouse_area)
                    .id(self.scrollable_id.clone())
                    .on_scroll(Message::Scroll)
                    .width(Length::Fill)
                    .height(Length::Fill),
                widget::Id::from(format!("{}-scrollable", self.scrollable_name)),
            )
            .into()
        } else {
            mouse_area.into()
        };

        // Wrap the scrollable, not its content, so the popup anchors in window coordinates
        let mut context_menu = widget::context_menu(
            items_area,
            Some(menu::context_menu(
                self,
                key_binds,
                modifiers,
                clipboard_paste_available,
                context_actions,
            )),
        )
        .item_width(crate::ui::widget::menu::ItemWidth::Uniform(360))
        .on_surface_action(Message::Surface);
        if let Some(window_id) = self.window_id {
            context_menu = context_menu.window_id(window_id);
        }

        let mut tab_column = widget::Column::with_capacity(3);
        tab_column = tab_column.push(location_view);
        tab_column = tab_column.push(context_menu);
        match &self.location {
            Location::Trash | Location::Search(SearchLocation::Trash, ..) => {
                if let Some(items) = self.items_opt()
                    && !items.is_empty()
                {
                    tab_column = tab_column.push(
                        widget::layer_container(widget::Row::with_children([
                            widget::space::horizontal().into(),
                            widget::button::standard(fl!("empty-trash"))
                                .on_press(Message::EmptyTrash)
                                .into(),
                        ]))
                        .padding([space_xxs, space_xs])
                        .layer(Layer::Primary)
                        .apply(widget::container)
                        .padding(([0, 0, 7, 0]).to_padding()),
                    );
                }
            }
            Location::Recents | Location::Search(SearchLocation::Recents, ..) => {
                if let Some(items) = self.items_opt()
                    && !items.is_empty()
                {
                    tab_column = tab_column.push(
                        widget::layer_container(widget::Row::with_children([
                            widget::space::horizontal().into(),
                            widget::button::standard(fl!("clear-recents-history"))
                                .on_press(Message::ClearRecents)
                                .into(),
                        ]))
                        .padding([space_xxs, space_xs])
                        .layer(Layer::Primary)
                        .apply(widget::container)
                        .padding(([0, 0, 7, 0]).to_padding()),
                    );
                }
            }
            Location::Network(uri, _display_name, _path) if uri == "network:///" => {
                tab_column = tab_column.push(
                    widget::layer_container(widget::Row::with_children([
                        widget::space::horizontal().into(),
                        widget::button::standard(fl!("add-network-drive"))
                            .on_press(Message::AddNetworkDrive)
                            .into(),
                    ]))
                    .padding([space_xxs, space_xs])
                    .layer(Layer::Primary)
                    .apply(widget::container)
                    .padding(([0, 0, 7, 0]).to_padding()),
                );
            }
            _ => {}
        }
        let tab_view = widget::container(tab_column)
            .height(Length::Fill)
            .width(Length::Fill);

        tab_view.into()
    }
    pub fn multi_preview_view<'a>(
        &'a self,
        mime_app_cache_opt: Option<&'a mime_app::MimeAppCache>,
    ) -> Element<'a, Message> {
        let Spacing {
            space_xxxs,
            space_m,
            ..
        } = spacing();

        let mut column = widget::Column::with_capacity(4).spacing(space_m.to_pixels());

        let handle = widget::icon::from_name("text-x-generic")
            .size(IconSizes::default().grid())
            .handle();

        let icon = widget::icon::icon(handle.clone())
            .content_fit(ContentFit::Contain)
            .size(IconSizes::default().grid());

        let icon_container1 = widget::container(icon.clone()).padding(padding::bottom(10).left(10));
        let icon_container2 =
            widget::container(icon.clone()).padding(padding::top(5).bottom(5).left(5).right(5));
        let icon_container3 = widget::container(icon).padding(padding::top(10).right(10));
        let stack = stack![icon_container1, icon_container2, icon_container3];

        column = column.push(
            widget::container(stack)
                .center_x(Length::Fill)
                .max_height(THUMBNAIL_SIZE as f32),
        );

        let selected_items: Vec<&Item> = self.items_opt().map_or(Vec::new(), |items| {
            items
                .iter()
                .filter(|item| {
                    if item.selected {
                        item.location_opt
                            .as_ref()
                            .and_then(Location::path_opt)
                            .is_some()
                    } else {
                        false
                    }
                })
                .collect()
        });

        let mut details = widget::Column::with_capacity(3).spacing(space_xxxs.to_pixels());
        details = details.push(widget::text::body(fl!(
            "items",
            items = selected_items.len()
        )));

        let mut total_size: u64 = 0;
        let mut mime_type_counts: BTreeMap<String, u64> = BTreeMap::new();
        let mut user_name: BTreeSet<String> = BTreeSet::new();
        let mut mode_user: BTreeSet<u32> = BTreeSet::new();
        let mut group_name: BTreeSet<String> = BTreeSet::new();
        let mut mode_group: BTreeSet<u32> = BTreeSet::new();
        let mut mode_other: BTreeSet<u32> = BTreeSet::new();
        let mut calculating_dir_size = false;
        let mut dir_size_error: Option<String> = None;

        for item in selected_items.iter() {
            *mime_type_counts.entry(item.mime.to_string()).or_insert(0) += 1;

            if let Some(metadata) = item.file_metadata() {
                if metadata.is_dir() {
                    match &item.dir_size {
                        DirSize::Calculating(_) => {
                            calculating_dir_size = true;
                        }
                        DirSize::Directory(size) => {
                            total_size = total_size.saturating_add(*size);
                        }
                        DirSize::NotDirectory => (),
                        DirSize::Error(err) => {
                            dir_size_error = Some(err.clone());
                        }
                    };
                } else {
                    total_size = total_size.saturating_add(metadata.len());
                }
                let mode = metadata.mode();
                user_name.insert(crate::tab::user_name(metadata.uid()));
                mode_user.insert(get_mode_part(mode, MODE_SHIFT_USER));
                group_name.insert(crate::tab::group_name(metadata.gid()));
                mode_group.insert(get_mode_part(mode, MODE_SHIFT_GROUP));
                mode_other.insert(get_mode_part(mode, MODE_SHIFT_OTHER));
            }
        }
        let mut mime_types: Vec<(String, u64)> = mime_type_counts.into_iter().collect();
        mime_types.sort_by(|(_, v1), (_, v2)| v2.cmp(v1));

        // Limit the number of displayed mime types
        let limit = usize::min(10, mime_types.len());

        let mut mime_type_strings: Vec<String> = mime_types[..limit]
            .iter()
            .map(|(mime, count)| format!("{} ({})", mime, count))
            .collect();

        if mime_types.len() > limit {
            mime_type_strings.push("...".to_string());
        }

        details = details.push(widget::text::body(fl!(
            "type",
            mime = mime_type_strings.join(", ")
        )));

        let size = {
            if calculating_dir_size {
                fl!("calculating")
            } else if let Some(error) = dir_size_error {
                error
            } else {
                format_size(total_size)
            }
        };

        details = details.push(widget::text::body(fl!("item-size", size = size)));

        column = column.push(details);

        column = column.push(widget::button::standard(fl!("open")).on_press(Message::Open(None)));

        let mut settings = Vec::new();
        // Only allow modifying open-with if all mime types are the same
        if mime_types.len() == 1
            && let Some(mime) = mime_types
                .first()
                .and_then(|(mime, _)| mime.parse::<Mime>().ok())
            && let Some(mime_app_cache) = mime_app_cache_opt
        {
            let mime_apps = mime_app_cache.get_apps_for_mime(&mime, false);
            if !mime_apps.is_empty() {
                let mime_closure = mime.clone();
                let (names, icons) = mime_apps
                    .iter()
                    .map(|(app, _)| (Cow::Owned(app.name.clone()), app.icon()))
                    .collect::<(Vec<_>, Vec<_>)>();
                settings.push(
                    widget::settings::item::builder(fl!("open-with")).control(
                        Element::from(
                            widget::dropdown(
                                names,
                                mime_apps.iter().position(|(x, _)| x.is_default(&mime)),
                                move |index| (index, mime_closure.clone()),
                            )
                            .icons(Cow::Owned(icons)),
                        )
                        .map(move |(index, mime)| {
                            let mime_app = &mime_apps[index].0;
                            Message::SetOpenWith(mime, mime_app.id.clone())
                        }),
                    ),
                );
            }
        }

        // Only return mode part if it's the only one
        fn selected_mode_part(mut modes: BTreeSet<u32>) -> Option<usize> {
            match (modes.pop_first(), modes.pop_first()) {
                (Some(mode), None) => Some(mode.try_into().unwrap()),
                _ => None,
            }
        }

        // Convert a limited number of values from a set into a comma separated list
        fn join_set(set: BTreeSet<String>) -> String {
            let limit = 5;
            let mut title = set.into_iter().collect::<Vec<String>>();
            if title.len() > limit {
                title.truncate(limit);
                title.push("...".to_string());
            }
            title.join(", ")
        }

        let mode_part_user = selected_mode_part(mode_user);
        settings.push(
            widget::settings::item::builder(join_set(user_name))
                .description(fl!("owner"))
                .control(
                    widget::dropdown(
                        Cow::Borrowed(MODE_NAMES.as_slice()),
                        mode_part_user,
                        move |selected| {
                            Message::ShiftPermissions(
                                None,
                                MODE_SHIFT_USER,
                                selected.try_into().unwrap(),
                            )
                        },
                    )
                    .placeholder(fl!("mixed")),
                ),
        );

        let mode_part_group = selected_mode_part(mode_group);
        settings.push(
            widget::settings::item::builder(join_set(group_name))
                .description(fl!("group"))
                .control(
                    widget::dropdown(
                        Cow::Borrowed(MODE_NAMES.as_slice()),
                        mode_part_group,
                        move |selected| {
                            Message::ShiftPermissions(
                                None,
                                MODE_SHIFT_GROUP,
                                selected.try_into().unwrap(),
                            )
                        },
                    )
                    .placeholder(fl!("mixed")),
                ),
        );

        let mode_part_other = selected_mode_part(mode_other);
        settings.push(
            widget::settings::item::builder(fl!("other")).control(
                widget::dropdown(
                    Cow::Borrowed(MODE_NAMES.as_slice()),
                    mode_part_other,
                    move |selected| {
                        Message::ShiftPermissions(
                            None,
                            MODE_SHIFT_OTHER,
                            selected.try_into().unwrap(),
                        )
                    },
                )
                .placeholder(fl!("mixed")),
            ),
        );

        if !settings.is_empty() {
            let mut section = widget::settings::section();
            section = section.extend(settings);
            column = column.push(section);
        }

        column.into()
    }
    pub fn view<'a>(
        &'a self,
        key_binds: &'a HashMap<KeyBind, Action>,
        modifiers: &'a Modifiers,
        clipboard_paste_available: bool,
        context_actions: &'a [ContextActionPreset],
    ) -> Element<'a, Message> {
        widget::responsive(move |size| {
            widget::id_container(
                self.view_responsive(
                    key_binds,
                    modifiers,
                    size,
                    clipboard_paste_available,
                    context_actions,
                ),
                Id::from(format!(
                    "tab-{}-{}",
                    self.scrollable_name, self.location_title
                )),
            )
            .into()
        })
        .into()
    }

    pub fn subscription(&self, preview: bool) -> Subscription<Message> {
        let jobs = self.thumb_config.jobs.get() as usize;
        let mut subscriptions = Vec::with_capacity(jobs + 3);

        if let Some(items) = &self.items_opt {
            let visible_rect = self.visible_rect(self.size_opt.get().unwrap_or_default());

            // Count the children of visible directories in the background. Doing it while
            // scanning costs one directory listing per entry, which stalls remote filesystems.
            for item in items {
                let uncounted_dir = match &item.metadata {
                    ItemMetadata::Path {
                        metadata,
                        children_opt: None,
                    } => metadata.is_dir(),
                    #[cfg(feature = "gvfs")]
                    ItemMetadata::GvfsPath {
                        children_opt: None,
                        is_dir: true,
                        ..
                    } => true,
                    _ => false,
                };
                if !uncounted_dir {
                    continue;
                }

                // Skip items that are not visible, or have no determined rect
                match item.rect_opt.get() {
                    Some(rect) if rect.intersects(&visible_rect) => {}
                    _ => continue,
                }

                let Some(path) = item.path_opt().cloned() else {
                    continue;
                };

                struct ChildrenWrapper(PathBuf);
                impl Hash for ChildrenWrapper {
                    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                        self.0.hash(state);
                    }
                }

                subscriptions.push(Subscription::run_with(
                    ChildrenWrapper(path),
                    |ChildrenWrapper(path)| {
                        let path = path.clone();
                        stream::channel(
                            1,
                            move |mut output: futures::channel::mpsc::Sender<_>| async move {
                                let message = {
                                    let path = path.clone();
                                    tokio::task::spawn_blocking(move || {
                                        let children = match fs::read_dir(&path) {
                                            Ok(entries) => entries.count(),
                                            Err(err) => {
                                                log::warn!(
                                                    "failed to read directory {}: {}",
                                                    path.display(),
                                                    err
                                                );
                                                0
                                            }
                                        };
                                        Message::DirectoryChildren(path, children)
                                    })
                                    .await
                                    .unwrap()
                                };

                                if let Err(err) = output.send(message).await {
                                    log::warn!("failed to send directory children: {err}");
                                }

                                std::future::pending().await
                            },
                        )
                    },
                ));
            }

            for item in items {
                if item.thumbnail_opt.is_some() {
                    // Skip items that already have a mime type and thumbnail
                    continue;
                }

                match item.rect_opt.get() {
                    Some(rect) => {
                        if !rect.intersects(&visible_rect) {
                            // Skip items that are not visible
                            continue;
                        }
                    }
                    None => {
                        // Skip items with no determined rect (this should include hidden items)
                        continue;
                    }
                }

                let Some(path) = item.path_opt().cloned() else {
                    continue;
                };

                let metadata = item.metadata.clone();
                let can_thumbnail = match metadata {
                    ItemMetadata::Path { .. } => true,
                    #[cfg(feature = "gvfs")]
                    ItemMetadata::GvfsPath { .. } => true,
                    _ => false,
                };
                if can_thumbnail {
                    let mime = item.mime.clone();
                    let max_jobs = jobs;
                    let max_mb = u64::from(self.thumb_config.max_mem_mb.get());
                    let max_size = u64::from(self.thumb_config.max_size_mb.get());

                    // Determine effective memory budget based on image size
                    let (effective_max_mb, effective_jobs) = if mime.type_() == mime::IMAGE {
                        match item.image_dimensions.get().copied().flatten() {
                            Some((width, height)) => {
                                let (_use_dedicated, eff_mb, eff_jobs) =
                                    should_use_dedicated_worker(width, height, max_mb, max_jobs);
                                (eff_mb, eff_jobs)
                            }
                            None => (max_mb, max_jobs),
                        }
                    } else {
                        (max_mb, max_jobs)
                    };

                    #[derive(Clone)]
                    struct Wrapper {
                        path: PathBuf,
                        metadata: ItemMetadata,
                        mime: mime::Mime,
                        effective_max_mb: u64,
                        effective_jobs: usize,
                        max_size: u64,
                    }

                    impl Hash for Wrapper {
                        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                            self.path.hash(state);
                        }
                    }

                    subscriptions.push(Subscription::run_with(
                        Wrapper {
                            path: path.clone(),
                            metadata,
                            mime,
                            effective_max_mb,
                            effective_jobs,
                            max_size,
                        },
                        |wrapper| {
                            let Wrapper {
                                path,
                                metadata,
                                mime,
                                effective_max_mb,
                                effective_jobs,
                                max_size,
                            } = wrapper.clone();
                            stream::channel(
                                1,
                                move |mut output: futures::channel::mpsc::Sender<_>| async move {
                                    while crate::operation::is_actively_writing_to(&path) {
                                        crate::operation::actively_writing_tick().await;
                                    }

                                    let message = {
                                        let path = path.clone();

                                        // Acquire semaphore permit
                                        let _permit = THUMB_SEMAPHORE.acquire().await.unwrap();

                                        tokio::task::spawn_blocking(move || {
                                            let start = Instant::now();
                                            let thumbnail = ItemThumbnail::new(
                                                &path,
                                                metadata,
                                                mime,
                                                THUMBNAIL_SIZE,
                                                effective_max_mb,
                                                effective_jobs,
                                                max_size,
                                            );
                                            log::debug!(
                                                "thumbnailed {} in {:?}",
                                                path.display(),
                                                start.elapsed()
                                            );
                                            Message::Thumbnail(path, thumbnail)
                                        })
                                        .await
                                        .unwrap()
                                    };

                                    match output.send(message).await {
                                        Ok(()) => {}
                                        Err(err) => {
                                            log::warn!(
                                                "failed to send thumbnail for {}: {}",
                                                path.display(),
                                                err
                                            );
                                        }
                                    }

                                    std::future::pending().await
                                },
                            )
                        },
                    ));
                }

                if subscriptions.len() >= jobs {
                    break;
                }
            }

            if preview {
                // Load directory size for selected items

                let mut selected_items: Vec<&Item> =
                    items.iter().filter(|item| item.selected).collect();

                if selected_items.is_empty()
                    && let Some(p) = self.parent_item_opt.as_ref()
                {
                    selected_items.push(p)
                }
                for item in selected_items {
                    // Item must have a path
                    if let Some(path) = item.path_opt().cloned() {
                        // Item must be calculating directory size
                        if let DirSize::Calculating(controller) = &item.dir_size {
                            struct Wrapper {
                                path: PathBuf,
                                controller: Controller,
                            }
                            impl Hash for Wrapper {
                                fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                                    self.path.hash(state);
                                }
                            }
                            subscriptions.push(Subscription::run_with(
                                Wrapper { path: path.clone(), controller: controller.clone() },
                                |Wrapper { path, controller }| {
                                    let path = path.clone();
                                    let controller = controller.clone();
                                    stream::channel(1, |mut output: futures::channel::mpsc::Sender<_>| async move {
                                        let message = {
                                            let start = Instant::now();
                                            match calculate_dir_size(&path, controller).await {
                                                Ok(size) => {
                                                    log::debug!(
                                                        "calculated directory size of {} in {:?}",
                                                        path.display(),
                                                        start.elapsed()
                                                    );
                                                    Message::DirectorySize(
                                                        path.clone(),
                                                        DirSize::Directory(size),
                                                    )
                                                }
                                                Err(err) => {
                                                    log::warn!(
                                                        "failed to calculate directory size of {}: {}",
                                                        path.display(),
                                                        err
                                                    );
                                                    Message::DirectorySize(
                                                        path.clone(),
                                                        DirSize::Error(err.to_string()),
                                                    )
                                                }
                                            }
                                        };

                                        match output.send(message).await {
                                            Ok(()) => {}
                                            Err(err) => {
                                                log::warn!(
                                                    "failed to send directory size for {}: {}",
                                                    path.display(),
                                                    err
                                                );
                                            }
                                        }

                                        std::future::pending().await
                                    })
                                }
                            ));
                        }
                    }
                }
            }
        }

        // Load search items incrementally
        if let Location::Search(search_location, term, show_hidden, start) = &self.location {
            let location = self.location.clone();
            let search_location = search_location.clone();
            let term = term.clone();
            let show_hidden = *show_hidden;
            let start = *start;
            #[derive(Debug, Hash, Clone)]
            struct Wrapper {
                location: Location,
                search_location: SearchLocation,
                term: String,
                show_hidden: bool,
                start: Instant,
            }

            subscriptions.push(Subscription::run_with(
                Wrapper {
                    location: location.clone(),
                    search_location: search_location.clone(),
                    term: term.clone(),
                    show_hidden,
                    start,
                },
                |wrapper| {
                    let wrapper = wrapper.clone();
                    stream::channel(
                        2,
                        move |mut output: futures::channel::mpsc::Sender<Message>| async move {
                            let Wrapper {
                                location,
                                search_location,
                                term,
                                show_hidden,
                                start,
                            } = wrapper;
                            let (results_tx, results_rx) = mpsc::channel(65536);

                            let ready = Arc::new(atomic::AtomicBool::new(false));
                            let last_modified_opt = Arc::new(RwLock::new(None));
                            output
                                .send(Message::SearchContext(
                                    location.clone(),
                                    SearchContextWrapper(Some(SearchContext {
                                        results_rx,
                                        ready: ready.clone(),
                                        last_modified_opt: last_modified_opt.clone(),
                                    })),
                                ))
                                .await
                                .unwrap();

                            let (watch_tx, mut watch_rx) = tokio::sync::watch::channel(true);
                            {
                                tokio::task::spawn_blocking(move || {
                                    scan_search(
                                        &search_location,
                                        &term,
                                        show_hidden,
                                        move |search_item| -> bool {
                                            // Don't send if the result is too old
                                            if let Some(last_modified) =
                                                *last_modified_opt.read().unwrap()
                                                && let SearchItem::Path(_, _, ref metadata) =
                                                    search_item
                                            {
                                                if let Ok(modified) = metadata.modified() {
                                                    if modified < last_modified {
                                                        return true;
                                                    }
                                                } else {
                                                    return true;
                                                }
                                            }

                                            match results_tx.blocking_send(search_item) {
                                                Ok(()) => {
                                                    if ready.swap(true, atomic::Ordering::SeqCst) {
                                                        true
                                                    } else {
                                                        // Wake up update method
                                                        watch_tx.send(false).is_ok()
                                                    }
                                                }
                                                Err(_) => false,
                                            }
                                        },
                                    );
                                    log::info!(
                                        "searched for {:?} in {} in {:?}",
                                        term,
                                        search_location,
                                        start.elapsed(),
                                    );
                                });
                            }

                            while watch_rx.changed().await.is_ok() {
                                let is_ready = *watch_rx.borrow_and_update();
                                let _ = output.send(Message::SearchReady(is_ready)).await;
                            }

                            // Send final ready
                            let _ = output.send(Message::SearchReady(true)).await;

                            std::future::pending().await
                        },
                    )
                },
            ));
        }

        if let Some(path) = self
            .edit_location
            .as_ref()
            .and_then(|x| x.location.path_opt())
            .cloned()
        {
            subscriptions.push(Subscription::run_with(
                ("tab_complete", path.clone()),
                |(_, path)| {
                    let path = path.clone();
                    stream::channel(
                        1,
                        |mut output: futures::channel::mpsc::Sender<_>| async move {
                            let message = {
                                let path = path.clone();
                                tokio::task::spawn_blocking(move || {
                                    let start = Instant::now();
                                    match tab_complete(&path) {
                                        Ok(completions) => {
                                            log::info!(
                                                "tab completed {} in {:?}",
                                                path.display(),
                                                start.elapsed()
                                            );
                                            Message::TabComplete(path.clone(), completions)
                                        }
                                        Err(err) => {
                                            log::warn!(
                                                "failed to tab complete {}: {}",
                                                path.display(),
                                                err
                                            );
                                            Message::TabComplete(path.clone(), Vec::new())
                                        }
                                    }
                                })
                                .await
                                .unwrap()
                            };

                            match output.send(message).await {
                                Ok(()) => {}
                                Err(err) => {
                                    log::warn!(
                                        "failed to send tab completion for {}: {}",
                                        path.display(),
                                        err
                                    );
                                }
                            }

                            std::future::pending().await
                        },
                    )
                },
            ));
        }

        Subscription::batch(subscriptions)
    }

    const fn format_time(&self, time: SystemTime) -> FormatTime<'_> {
        format_time(time, &self.date_time_formatter, &self.time_formatter)
    }
}

pub fn respond_to_scroll_direction(delta: ScrollDelta, modifiers: &Modifiers) -> Option<Message> {
    if !modifiers.control() {
        return None;
    }

    let delta_y = match delta {
        ScrollDelta::Lines { y, .. } => y,
        ScrollDelta::Pixels { y, .. } => y,
    };

    if delta_y > 0.0 {
        return Some(Message::ZoomIn);
    }

    if delta_y < 0.0 {
        return Some(Message::ZoomOut);
    }

    None
}

fn text_editor_class(
    theme: &crate::ui::Theme,
    status: crate::ui::widget::text_editor::Status,
) -> crate::ui::iced::widget::text_editor::Style {
    let cosmic = theme.cosmic();
    let container = theme.current_container();

    let mut background: crate::ui::iced::Color = container.component.base.to_color();
    background.a = 0.25;
    let selection = cosmic.accent.base.to_color();
    let value = cosmic.palette.neutral_9.to_color();
    let mut placeholder = cosmic.palette.neutral_9;
    placeholder.alpha = 0.7;
    let placeholder = placeholder.to_color();

    match status {
        crate::ui::iced::widget::text_editor::Status::Active
        | crate::ui::iced::widget::text_editor::Status::Disabled => {
            crate::ui::iced::widget::text_editor::Style {
                background: background.into(),
                border: crate::ui::iced::Border {
                    radius: cosmic.corner_radii.radius_m.to_radius(),
                    width: 2.0,
                    color: container.component.divider.to_color(),
                },
                placeholder,
                value,
                selection,
            }
        }
        crate::ui::iced::widget::text_editor::Status::Hovered
        | crate::ui::iced::widget::text_editor::Status::Focused { .. } => {
            crate::ui::iced::widget::text_editor::Style {
                background: background.into(),
                border: crate::ui::iced::Border {
                    radius: cosmic.corner_radii.radius_m.to_radius(),
                    width: 2.0,
                    color: cosmic.accent.base.to_color(),
                },
                placeholder,
                value,
                selection,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::{fs, io};

    use crate::ui::iced::mouse::ScrollDelta;
    use crate::ui::iced_runtime::keyboard::Modifiers;
    use log::{debug, trace};
    use mime_guess::mime;
    use tempfile::TempDir;
    use test_log::test;

    use super::{
        ItemMetadata, ItemThumbnail, Location, Message, Tab, respond_to_scroll_direction, scan_path,
    };
    use crate::app::test_utils::{
        NAME_LEN, NUM_DIRS, NUM_FILES, NUM_HIDDEN, NUM_NESTED, assert_eq_tab_path, empty_fs,
        eq_path_item, filter_dirs, read_dir_sorted, simple_fs, tab_click_new,
    };
    use crate::config::{IconSizes, TabConfig, ThumbCfg};

    // Boilerplate for tab tests. Checks if simulated clicks selected items.
    fn tab_selects_item(
        clicks: &[usize],
        modifiers: Modifiers,
        expected_selected: &[bool],
    ) -> io::Result<()> {
        let (_fs, mut tab) = tab_click_new(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;

        // Simulate clicks by triggering Message::Click
        for &click in clicks {
            debug!("Emitting Message::Click(Some({click})) with modifiers: {modifiers:?}");
            tab.update(Message::Click(Some(click)), modifiers);
        }

        let items = tab
            .items_opt
            .as_deref()
            .expect("tab should be populated with items");

        for (i, (&expected, actual)) in expected_selected.iter().zip(items).enumerate() {
            assert_eq!(
                expected,
                actual.selected,
                "expected index {i} to be {}",
                if expected {
                    "selected but it was deselected"
                } else {
                    "deselected but it was selected"
                }
            );
        }

        Ok(())
    }

    fn tab_history() -> io::Result<(TempDir, Tab, Vec<PathBuf>)> {
        let fs = simple_fs(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();
        let mut tab = Tab::new(
            Location::Path(path.into()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );

        // All directories (simple_fs only produces one nested layer)
        let dirs: Vec<PathBuf> = {
            let top_level = filter_dirs(path)?;
            let mut result = Vec::new();
            for dir in top_level {
                let nested_dirs = filter_dirs(&dir)?;
                result.push(dir);
                result.extend(nested_dirs);
            }
            result
        };
        assert!(
            dirs.len() == NUM_DIRS + NUM_DIRS * NUM_NESTED,
            "Sanity check: Have {} dirs instead of {}",
            dirs.len(),
            NUM_DIRS + NUM_DIRS * NUM_NESTED
        );

        debug!("Building history by emitting Message::Location");
        for dir in &dirs {
            debug!(
                "Emitting Message::Location(Location::Path(\"{}\"))",
                dir.display()
            );
            tab.update(
                Message::Location(Location::Path(dir.clone())),
                Modifiers::empty(),
            );
        }
        trace!("Tab history: {:?}", tab.history);

        Ok((fs, tab, dirs))
    }

    #[test]
    fn scan_path_succeeds_on_valid_path() -> io::Result<()> {
        let fs = simple_fs(NUM_FILES, NUM_HIDDEN, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();

        // Read directory entries and sort as earth-files does
        let entries = read_dir_sorted(path)?;

        debug!("Calling scan_path(\"{}\")", path.display());
        let actual = scan_path(&path.to_owned(), IconSizes::default());

        // scan_path shouldn't skip any entries
        assert_eq!(entries.len(), actual.len());

        // Correct files should be scanned
        assert!(
            entries
                .into_iter()
                .zip(actual.into_iter())
                .all(|(path, item)| eq_path_item(&path, &item))
        );

        Ok(())
    }

    #[test]
    fn scan_path_returns_empty_vec_for_invalid_path() -> io::Result<()> {
        let fs = simple_fs(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();

        // A nonexisting path within the temp dir
        let invalid_path = path.join("ferris");
        assert!(!invalid_path.exists());

        debug!("Calling scan_path(\"{}\")", invalid_path.display());
        let actual = scan_path(&invalid_path, IconSizes::default());

        assert!(actual.is_empty());

        Ok(())
    }

    #[test]
    fn scan_path_empty_dir_returns_empty_vec() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();

        debug!("Calling scan_path(\"{}\")", path.display());
        let actual = scan_path(&path.to_owned(), IconSizes::default());

        assert_eq!(0, path.read_dir()?.count());
        assert!(actual.is_empty());

        Ok(())
    }

    #[test]
    fn tab_location_changes_location() -> io::Result<()> {
        let fs = simple_fs(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();

        // Next directory in temp directory
        // This does not have to be sorted
        let next_dir = filter_dirs(path)?
            .next()
            .expect("temp directory should have at least one directory");

        let mut tab = Tab::new(
            Location::Path(path.to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        debug!(
            "Emitting Message::Location(Location::Path(\"{}\"))",
            next_dir.display()
        );
        tab.update(
            Message::Location(Location::Path(next_dir.clone())),
            Modifiers::empty(),
        );

        // Validate that the tab's path updated
        // NOTE: `items_opt` is set to None with Message::Location so this ONLY checks for equal paths
        // If item contents are NOT None then this needs to be reevaluated for correctness
        assert_eq_tab_path(&tab, &next_dir);
        assert!(
            tab.items_opt.is_none(),
            "Tab's `items` is not None which means this test needs to be updated"
        );

        Ok(())
    }

    #[test]
    fn tab_click_single_selects_item() -> io::Result<()> {
        // Select the second directory with no keys held down
        tab_selects_item(&[1], Modifiers::empty(), &[false, true])
    }

    #[test]
    fn tab_click_double_opens_folder() -> io::Result<()> {
        let (fs, mut tab) = tab_click_new(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();

        // Simulate double clicking second directory
        debug!("Emitting double click Message::DoubleClick(Some(1))");
        tab.update(Message::DoubleClick(Some(1)), Modifiers::empty());

        // Path to second directory
        let second_dir = read_dir_sorted(path)?
            .into_iter()
            .filter(|p| p.is_dir())
            .nth(1)
            .expect("should be at least two directories");

        // Location should have changed to second_dir
        assert_eq_tab_path(&tab, &second_dir);

        Ok(())
    }

    #[test]
    fn tab_click_ctrl_selects_multiple() -> io::Result<()> {
        // Select the first and second directory by holding down ctrl
        tab_selects_item(&[0, 1], Modifiers::CTRL, &[true, true])
    }

    /// Scanning must not resolve icons. Each one searches the icon theme on
    /// disk for tens of milliseconds, and a directory holds enough distinct
    /// types that doing it inline delays the listing by seconds.
    #[test]
    fn scanning_leaves_icons_for_the_worker() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        fs::write(path.join("notes.txt"), b"x")?;
        fs::create_dir(path.join("sub"))?;
        let sizes = IconSizes::default();

        let mut items = super::scan_path(&path.to_path_buf(), sizes);
        // One pass both takes what the cache knows and reports what it lacks,
        // so there is no window in which another tab's worker can fill the
        // cache between the two and leave these placeholders stranded
        let warmup = super::refresh_icons(&mut items, sizes);
        assert!(
            !warmup.is_empty(),
            "the scan should have left the icons to be resolved"
        );

        // What the worker does, then the tab takes the results
        super::warm_icons(&warmup, sizes);
        assert!(
            super::refresh_icons(&mut items, sizes).is_empty(),
            "warming should satisfy everything the scan left"
        );

        // A second visit to the same types asks for nothing
        let mut again = super::scan_path(&path.to_path_buf(), sizes);
        assert!(super::refresh_icons(&mut again, sizes).is_empty());

        // And a type the icon theme has nothing for is not asked for twice
        assert!(
            super::refresh_icons(&mut again, sizes).is_empty(),
            "a type with no icon must not be requeued on every pass"
        );
        Ok(())
    }

    /// Dimensions come from the scan, never from drawing. A scanned image
    /// carries its size; an item built any other way carries none, rather
    /// than opening the file while a frame is being drawn.
    #[test]
    fn image_dimensions_are_filled_by_the_scan_only() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        image::RgbaImage::new(7, 3)
            .save(path.join("pic.png"))
            .expect("the test image should be written");
        fs::write(path.join("plain.txt"), b"x")?;

        let (_parent, items) = Location::Path(path.to_owned()).scan(IconSizes::default());
        let by_name = |name: &str| {
            items
                .iter()
                .find(|item| item.name == name)
                .expect("the scan should list it")
        };
        assert_eq!(
            by_name("pic.png").image_dimensions.get().copied().flatten(),
            Some((7, 3))
        );
        assert_eq!(
            by_name("plain.txt")
                .image_dimensions
                .get()
                .copied()
                .flatten(),
            None,
            "a file that is not an image is never opened for its size"
        );

        // Built outside a scan, as search results and refreshes are
        let metadata = fs::metadata(path.join("pic.png"))?;
        let fresh = super::item_from_entry(
            path.join("pic.png"),
            "pic.png".to_string(),
            metadata,
            IconSizes::default(),
        );
        assert!(
            fresh.image_dimensions.get().is_none(),
            "constructing an item must not read the file"
        );
        Ok(())
    }

    /// A double click is two presses and two releases. In single-click mode
    /// the first release opens the item and nothing after it may open it
    /// again; in double-click mode only the double click opens it.
    #[test]
    fn a_click_sequence_opens_an_item_exactly_once() -> io::Result<()> {
        let opens = |single_click: bool| -> io::Result<usize> {
            let fs = empty_fs()?;
            fs::write(fs.path().join("a.txt"), b"x")?;
            let location = Location::Path(fs.path().to_owned());
            let (_parent, items) = location.scan(IconSizes::default());
            let mut tab = Tab::new(
                location,
                TabConfig::default(),
                ThumbCfg::default(),
                None,
                std::borrow::Cow::Borrowed("Undefined"),
                None,
            );
            tab.set_items(items);
            tab.config.single_click = single_click;
            let file = tab
                .items_opt
                .as_deref()
                .expect("tab should be populated")
                .iter()
                .position(|item| !item.metadata.is_dir())
                .expect("the fixture has files");

            let mut commands = Vec::new();
            for message in [
                Message::Click(Some(file)),
                Message::ClickRelease(Some(file)),
                Message::DoubleClick(Some(file)),
                Message::ClickRelease(Some(file)),
            ] {
                commands.extend(tab.update(message, Modifiers::empty()));
            }
            Ok(commands
                .iter()
                .filter(|command| matches!(command, super::Command::OpenFile(_)))
                .count())
        };

        assert_eq!(
            opens(true)?,
            1,
            "single-click mode opened it more than once"
        );
        assert_eq!(
            opens(false)?,
            1,
            "double-click mode opened it more than once"
        );
        Ok(())
    }

    #[test]
    fn tab_gonext_moves_forward_in_history() -> io::Result<()> {
        let (fs, mut tab, dirs) = tab_history()?;
        let path = fs.path();

        // Rewind to the start
        for _ in 0..dirs.len() {
            debug!("Emitting Message::GoPrevious to rewind to the start",);
            tab.update(Message::GoPrevious, Modifiers::empty());
        }
        assert_eq_tab_path(&tab, path);

        // Back to the future. Directories should be in the order they were opened.
        for dir in dirs {
            debug!("Emitting Message::GoNext",);
            tab.update(Message::GoNext, Modifiers::empty());
            assert_eq_tab_path(&tab, &dir);
        }

        Ok(())
    }

    #[test]
    fn tab_goprev_moves_backward_in_history() -> io::Result<()> {
        let (fs, mut tab, dirs) = tab_history()?;
        let path = fs.path();

        for dir in dirs.into_iter().rev() {
            assert_eq_tab_path(&tab, &dir);
            debug!("Emitting Message::GoPrevious",);
            tab.update(Message::GoPrevious, Modifiers::empty());
        }
        assert_eq_tab_path(&tab, path);

        Ok(())
    }

    #[test]
    fn tab_scroll_up_with_ctrl_modifier_zooms() -> io::Result<()> {
        let message_maybe =
            respond_to_scroll_direction(ScrollDelta::Pixels { x: 0.0, y: 1.0 }, &Modifiers::CTRL);
        assert!(message_maybe.is_some());
        assert!(matches!(message_maybe.unwrap(), Message::ZoomIn));
        Ok(())
    }

    #[test]
    fn tab_scroll_up_without_ctrl_modifier_does_not_zoom() -> io::Result<()> {
        let message_maybe = respond_to_scroll_direction(
            ScrollDelta::Pixels { x: 0.0, y: 1.0 },
            &Modifiers::empty(),
        );
        assert!(message_maybe.is_none());
        Ok(())
    }

    #[test]
    fn tab_scroll_down_with_ctrl_modifier_zooms() -> io::Result<()> {
        let message_maybe =
            respond_to_scroll_direction(ScrollDelta::Pixels { x: 0.0, y: -1.0 }, &Modifiers::CTRL);
        assert!(message_maybe.is_some());
        assert!(matches!(message_maybe.unwrap(), Message::ZoomOut));
        Ok(())
    }

    #[test]
    fn tab_scroll_down_without_ctrl_modifier_does_not_zoom() -> io::Result<()> {
        let message_maybe = respond_to_scroll_direction(
            ScrollDelta::Pixels { x: 0.0, y: -1.0 },
            &Modifiers::empty(),
        );
        assert!(message_maybe.is_none());
        Ok(())
    }
    #[test]
    fn tab_empty_history_does_nothing_on_prev_next() -> io::Result<()> {
        let fs = simple_fs(0, NUM_NESTED, NUM_DIRS, 0, NAME_LEN)?;
        let path = fs.path();
        let mut tab = Tab::new(
            Location::Path(path.into()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );

        // Tab's location shouldn't change if GoPrev or GoNext is triggered
        debug!("Emitting Message::GoPrevious",);
        tab.update(Message::GoPrevious, Modifiers::empty());
        assert_eq_tab_path(&tab, path);

        debug!("Emitting Message::GoNext",);
        tab.update(Message::GoNext, Modifiers::empty());
        assert_eq_tab_path(&tab, path);

        Ok(())
    }

    #[test]
    fn tab_locationup_moves_up_hierarchy() -> io::Result<()> {
        let fs = simple_fs(0, NUM_NESTED, NUM_DIRS, 0, NAME_LEN)?;
        let path = fs.path();
        let mut next_dir = filter_dirs(path)?
            .next()
            .expect("should be at least one directory");

        let mut tab = Tab::new(
            Location::Path(next_dir.clone()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        // This will eventually yield false once root is hit
        while next_dir.pop() {
            debug!("Emitting Message::LocationUp",);
            tab.update(Message::LocationUp, Modifiers::empty());
            assert_eq_tab_path(&tab, &next_dir);
        }

        Ok(())
    }

    #[test]
    fn sort_long_number_file_names() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();

        // Create files with names 255 characters long that only contain a single number
        // Example: 0000...0 for 255 characters
        // https://en.wikipedia.org/wiki/Filename#Comparison_of_filename_limitations
        let mut base_nums: Vec<_> = ('0'..='9').collect();
        fastrand::shuffle(&mut base_nums);
        debug!("Shuffled numbers for paths: {base_nums:?}");
        let paths: Vec<_> = base_nums
            .iter()
            .copied()
            .map(|base| path.join(std::iter::repeat_n(base, 255).collect::<String>()))
            .collect();

        for (file, base) in paths.iter().zip(base_nums) {
            trace!("Creating long file name for {base}");
            fs::File::create(file)?;
        }

        debug!("Creating tab for directory of long file names");
        Tab::new(
            Location::Path(path.into()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );

        Ok(())
    }

    #[test]
    fn sort_by_type_groups_mime_then_name() -> io::Result<()> {
        use super::HeadingOptions;

        let fs = empty_fs()?;
        let path = fs.path();
        // Two images, one text file and one folder; names chosen so a plain
        // name sort would interleave the two mime groups.
        fs::create_dir(path.join("a-folder"))?;
        fs::write(path.join("b.txt"), b"x")?;
        fs::write(path.join("c.png"), b"x")?;
        fs::write(path.join("a.png"), b"x")?;

        let location = Location::Path(path.to_owned());
        let (parent_item_opt, items) = location.scan(IconSizes::default());
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.parent_item_opt = parent_item_opt;
        tab.set_items(items);

        let names = |tab: &Tab| -> Vec<String> {
            tab.column_sort()
                .expect("tab should have items")
                .into_iter()
                .map(|(_, item)| item.name.clone())
                .collect()
        };

        tab.sort_name = HeadingOptions::Type;
        tab.sort_direction = true;
        assert_eq!(names(&tab), ["a-folder", "a.png", "c.png", "b.txt"]);

        tab.sort_direction = false;
        assert_eq!(names(&tab), ["a-folder", "b.txt", "c.png", "a.png"]);

        // Without folders first, folders still group first as the Folder category
        tab.config.folders_first = false;
        tab.sort_direction = true;
        assert_eq!(names(&tab), ["a-folder", "a.png", "c.png", "b.txt"]);
        tab.sort_direction = false;
        assert_eq!(names(&tab), ["b.txt", "c.png", "a.png", "a-folder"]);

        Ok(())
    }

    #[test]
    fn mode_calculations() {
        use super::{
            MODE_SHIFT_GROUP, MODE_SHIFT_OTHER, MODE_SHIFT_USER, get_mode_part, set_mode_part,
        };
        for user in 0..=7 {
            for group in 0..=7 {
                for other in 0..=7 {
                    let mode = (user << MODE_SHIFT_USER)
                        | (group << MODE_SHIFT_GROUP)
                        | (other << MODE_SHIFT_OTHER);
                    assert_eq!(format!("{mode:03o}"), format!("{user:o}{group:o}{other:o}"),);
                    assert_eq!(get_mode_part(mode, MODE_SHIFT_USER), user);
                    assert_eq!(get_mode_part(mode, MODE_SHIFT_GROUP), group);
                    assert_eq!(get_mode_part(mode, MODE_SHIFT_OTHER), other);

                    let mode_no_user = (group << MODE_SHIFT_GROUP) | (other << MODE_SHIFT_OTHER);
                    assert_eq!(
                        format!("{mode_no_user:03o}"),
                        format!("0{group:o}{other:o}")
                    );
                    assert_eq!(set_mode_part(mode_no_user, MODE_SHIFT_USER, user), mode);

                    let mode_no_group = (user << MODE_SHIFT_USER) | (other << MODE_SHIFT_OTHER);
                    assert_eq!(
                        format!("{mode_no_group:03o}"),
                        format!("{user:o}0{other:o}")
                    );
                    assert_eq!(set_mode_part(mode_no_group, MODE_SHIFT_GROUP, group), mode);

                    let mode_no_other = (user << MODE_SHIFT_USER) | (group << MODE_SHIFT_GROUP);
                    assert_eq!(
                        format!("{mode_no_other:03o}"),
                        format!("{user:o}{group:o}0")
                    );
                    assert_eq!(set_mode_part(mode_no_other, MODE_SHIFT_OTHER, other), mode);
                }
            }
        }
    }

    #[test]
    fn item_thumbnail_text_preview_small_utf8_returns_text() -> io::Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().join("preview.txt");
        fs::write(&path, "Hello, world!")?;
        let metadata = fs::metadata(&path)?;
        let item_metadata = ItemMetadata::Path {
            metadata,
            children_opt: None,
        };
        let thumb = ItemThumbnail::new(
            &path,
            item_metadata,
            mime::TEXT_PLAIN,
            128,
            100 * 1024 * 1024,
            1,
            8,
        );
        assert!(
            matches!(thumb, ItemThumbnail::Text(_)),
            "small text file should produce Text thumbnail"
        );
        Ok(())
    }

    #[test]
    fn item_thumbnail_text_preview_empty_file_returns_not_image() -> io::Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().join("empty.txt");
        fs::File::create(&path)?;
        let metadata = fs::metadata(&path)?;
        let item_metadata = ItemMetadata::Path {
            metadata,
            children_opt: None,
        };
        let thumb = ItemThumbnail::new(
            &path,
            item_metadata,
            mime::TEXT_PLAIN,
            128,
            100 * 1024 * 1024,
            1,
            8,
        );
        assert!(
            matches!(thumb, ItemThumbnail::NotImage),
            "empty text file should produce NotImage (no read)"
        );
        Ok(())
    }

    #[test]
    fn item_thumbnail_text_preview_invalid_utf8_uses_valid_prefix() -> io::Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().join("invalid_utf8.txt");
        // Valid UTF-8 "ab" then invalid byte sequence then "c"
        fs::write(&path, b"ab\xff\xfe\xfdc")?;
        let metadata = fs::metadata(&path)?;
        let item_metadata = ItemMetadata::Path {
            metadata,
            children_opt: None,
        };
        let thumb = ItemThumbnail::new(
            &path,
            item_metadata,
            mime::TEXT_PLAIN,
            128,
            100 * 1024 * 1024,
            1,
            8,
        );
        match &thumb {
            ItemThumbnail::Text(content) => {
                // Text editor content may add a trailing newline
                assert_eq!(content.text().trim_end(), "ab");
            }
            _ => panic!(
                "expected Text thumbnail with valid prefix only, got {:?}",
                thumb
            ),
        }
        Ok(())
    }
}
