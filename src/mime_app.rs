// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::ui::widget;
use notify_debouncer_full::notify;
use rustc_hash::{FxHashMap, FxHashSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{self, Instant};
use xdg_kit::desktop_entry::{
    AppEntry, DesktopEntry, Launch, LaunchOptions, LaunchPlan, Locales, Target, prepare_launch,
};
pub use xdg_kit::mime::MimeType as Mime;
use xdg_kit::mime_apps::MimeApps;

/// The type whose default application is the user's terminal.
const TERMINAL_MIME: Mime = Mime::from_static("x-scheme-handler/terminal");

pub async fn watch(mut emitter: impl FnMut() + 'static + Send) {
    let Some(base_dirs) = crate::xdg_dirs::BASE_DIRS.as_ref() else {
        return;
    };
    // Only the places of the `mimeapps.list` files are wanted, so no
    // desktop entries are read for this
    let lists = MimeApps::load(
        base_dirs,
        &xdg_kit::desktop_entry::Desktops::from_env(),
        Arc::new(xdg_kit::desktop_entry::AppIndex::from_dirs(
            std::iter::empty::<PathBuf>(),
        )),
        crate::mime_icon::SHARED_MIME_INFO.clone(),
    );
    let files: FxHashSet<PathBuf> = lists.files().into_iter().map(Path::to_path_buf).collect();
    let watched = files.clone();

    let watcher_result = notify_debouncer_full::new_debouncer(
        time::Duration::from_millis(250),
        Some(time::Duration::from_millis(250)),
        move |event_res: notify_debouncer_full::DebounceEventResult| {
            let Ok(events) = event_res else {
                return;
            };

            if events.iter().any(|event| {
                (event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove())
                    && touches_a_list(&event.paths, &watched)
            }) {
                emitter();
            }
        },
    );

    if let Ok(mut watcher) = watcher_result {
        for folder in list_folders(&files) {
            // A folder that does not exist has no list to change
            _ = watcher.watch(folder, notify::RecursiveMode::NonRecursive);
        }
        std::future::pending().await
    }
}

/// The folders to watch for changes to `files`, the `mimeapps.list` files.
///
/// The folders, not the files: a list is saved by writing a new file and
/// renaming it over the old one, which ends a watch on the old file, so a
/// watch on the file would see only the first save.
fn list_folders(files: &FxHashSet<PathBuf>) -> FxHashSet<&Path> {
    files.iter().filter_map(|file| file.parent()).collect()
}

/// Whether an event on `paths` concerns one of the `mimeapps.list` files,
/// rather than something else in their folders.
fn touches_a_list(paths: &[PathBuf], files: &FxHashSet<PathBuf>) -> bool {
    paths.iter().any(|path| files.contains(path))
}

pub fn exec_to_command(
    exec: &str,
    entry_name: &str,
    entry_path: Option<&Path>,
    path_opt: &[impl AsRef<OsStr>],
) -> Option<Vec<process::Command>> {
    let arguments = shlex::split(exec)?;

    if arguments.is_empty() {
        tracing::error!("command does not contain any arguments");
        return None;
    }

    let mut commands = Vec::new();

    let paths = path_opt
        .iter()
        .map(AsRef::as_ref)
        .map(Some)
        // Add a single `None` if no path was given.
        .chain(std::iter::repeat_n(
            None,
            if path_opt.is_empty() { 1 } else { 0 },
        ));

    for path in paths {
        let mut batch_process = false;
        let mut args = Vec::with_capacity(arguments.len());
        let mut field_code_used = false;

        for argument in arguments.iter().skip(1) {
            let mut new_argument: Vec<u8> = Vec::with_capacity(argument.capacity());
            let mut chars = argument.chars();
            while let Some(char) = chars.next() {
                // https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html
                if char == '%' {
                    match chars.next() {
                        Some('%') => new_argument.push(b'%'),
                        Some('c') => new_argument.extend_from_slice(entry_name.as_bytes()),
                        Some('k') => {
                            if let Some(path) = entry_path {
                                new_argument.extend_from_slice(path.as_os_str().as_bytes());
                            }
                        }

                        // %f and %u behave the same in a file manager.
                        Some('f' | 'u') => {
                            if let Some(path) = path
                                && !field_code_used
                            {
                                batch_process = true;
                                field_code_used = true;
                                new_argument.extend_from_slice(path.as_bytes());
                            }
                        }

                        // %F and %U behave the same in a file manager.
                        Some('F') | Some('U') if !field_code_used && new_argument.is_empty() => {
                            field_code_used = true;
                            for path in path_opt.iter().map(AsRef::as_ref) {
                                args.push(path.as_bytes().to_owned());
                            }
                        }

                        _ => (),
                    }
                } else {
                    new_argument.extend_from_slice(char.encode_utf8(&mut [0; 4]).as_bytes());
                }
            }

            if !new_argument.is_empty() {
                args.push(new_argument);
            }
        }

        let mut command = process::Command::new(&arguments[0]);

        for arg in args {
            command.arg(OsStr::from_bytes(&arg));
        }

        commands.push(command);

        if !batch_process {
            break;
        }
    }

    #[cfg(debug_assertions)]
    for command in &commands {
        log::debug!(
            "Parsed program {} with args: {:?}",
            command.get_program().to_string_lossy(),
            command.get_args()
        );
    }

    Some(commands)
}

/// The programs a launch plan starts, as commands to spawn.
///
/// A plan made without D-Bus activation, which
/// [`MimeAppCache::launch_options`] turns off, only ever holds commands.
pub fn plan_commands(plan: LaunchPlan) -> Vec<process::Command> {
    match plan.launch {
        Launch::Commands(planned) => planned
            .into_iter()
            .map(|planned| {
                let mut command = process::Command::new(planned.program);
                command.args(planned.args).envs(planned.env);
                for name in planned.env_remove {
                    command.env_remove(name);
                }
                if let Some(dir) = planned.working_dir {
                    command.current_dir(dir);
                }
                command
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The commands that launch `entry` with `targets`, or `None`, logged, when
/// it cannot be launched.
pub fn entry_commands(
    entry: &DesktopEntry,
    targets: &[Target],
    options: &LaunchOptions,
) -> Option<Vec<process::Command>> {
    match prepare_launch(entry, targets, options) {
        Ok(plan) => Some(plan_commands(plan)),
        Err(err) => {
            let path = entry.path().unwrap_or(Path::new(""));
            log::warn!("cannot launch {}: {err}", path.display());
            None
        }
    }
}

/// Opens a file or URI with its default application, as `xdg-open` does,
/// D-Bus activation included. It blocks, reading the associations or calling
/// D-Bus, so it runs on a worker. Made by [`MimeAppCache::opener`].
#[derive(Clone, Debug)]
pub struct Opener {
    /// The cache's associations; none while it is not loaded.
    associations: Option<MimeApps>,
    /// The cache's launch options, its terminal among them.
    options: LaunchOptions,
}

impl Opener {
    /// Opens `target`: a file whose type is `mime`, or a URI.
    pub fn open(&self, target: &Target, mime: Option<&Mime>) -> Result<(), String> {
        let plan = self.plan(target, mime)?;
        if matches!(plan.launch, Launch::Commands(_)) {
            // Commands run detached, as every launch of this app does
            plan_commands(plan).into_iter().try_for_each(|mut command| {
                crate::spawn_detached::spawn_detached(&mut command).map_err(|err| err.to_string())
            })
        } else {
            plan.spawn().map(drop).map_err(|err| err.to_string())
        }
    }

    /// How [`Self::open`] opens `target`. A file opens with the default for
    /// `mime`, the type already found for it, or for one of its parents; a
    /// URI with the default for its scheme.
    fn plan(&self, target: &Target, mime: Option<&Mime>) -> Result<LaunchPlan, String> {
        let (associations, options) = (self.associations()?, &self.options);
        match (target, mime) {
            (Target::Path(_), Some(mime)) => {
                let app = std::iter::once(mime.clone())
                    .chain(crate::mime_icon::parent_mime_types(mime))
                    .find_map(|mime| associations.default_for(&mime))
                    .ok_or_else(|| format!("no application opens {mime}"))?;
                prepare_launch(app.entry(), std::slice::from_ref(target), options)
                    .map_err(|err| err.to_string())
            }
            _ => xdg_kit::mime_apps::open_plan(target, associations, options)
                .map_err(|err| err.to_string()),
        }
    }

    /// The cache's associations, once it has loaded them.
    fn associations(&self) -> Result<&MimeApps, String> {
        self.associations
            .as_ref()
            .ok_or_else(|| "the applications are not loaded yet".to_owned())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MimeAppMatch {
    Exact,
    Related,
    Other,
}

#[derive(Clone, Debug)]
pub struct MimeApp {
    /// The desktop file ID without `.desktop`
    pub id: String,
    pub name: String,
    pub exec: Option<String>,
    entry: AppEntry,
    icon_name: Box<str>,
    icon: std::sync::OnceLock<widget::icon::Handle>,
    /// Not associated with any type, so not offered among other
    /// applications.
    no_display: bool,
}

impl MimeApp {
    /// The commands that open `targets` with this application, or `None`,
    /// logged, when it cannot open them.
    pub fn commands(
        &self,
        targets: &[Target],
        options: &LaunchOptions,
    ) -> Option<Vec<process::Command>> {
        entry_commands(self.entry.entry(), targets, options)
    }

    pub fn path(&self) -> &Path {
        self.entry.path()
    }

    pub fn no_display(&self) -> bool {
        self.no_display
    }

    pub fn icon(&self) -> widget::icon::Handle {
        self.icon
            .get_or_init(|| {
                let name = &*self.icon_name;
                if name.starts_with('/') {
                    crate::ui::widget::icon::from_path(PathBuf::from(name))
                } else {
                    crate::ui::widget::icon::from_name(name).size(32).handle()
                }
            })
            .clone()
    }
}

// This allows usage of MimeApp in a dropdown
impl AsRef<str> for MimeApp {
    fn as_ref(&self) -> &str {
        &self.name
    }
}

/// The one thread that edits `mimeapps.list`.
///
/// It is read, changed and written back whole, so the changes have to be
/// applied one at a time and in the order they were made.
pub enum AssociationJob {
    Write(Mime, String, std::sync::mpsc::Sender<bool>),
    /// Answer once every write queued before this one is on disk.
    Barrier(std::sync::mpsc::SyncSender<()>),
}

/// What a queued association write answers with once it has landed.
pub type AssociationAck = std::sync::mpsc::Receiver<bool>;

static ASSOCIATION_WRITER: LazyLock<Option<std::sync::mpsc::Sender<AssociationJob>>> =
    LazyLock::new(|| {
        let (tx, rx) = std::sync::mpsc::channel::<AssociationJob>();
        let spawned = std::thread::Builder::new()
            .name("mime-associations".to_owned())
            .spawn(move || {
                // Ends when every sender is dropped, which is at exit.
                for job in rx {
                    match job {
                        AssociationJob::Write(mime, id, done) => {
                            let _ = done.send(MimeAppCache::set_default(mime, id));
                        }
                        AssociationJob::Barrier(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            });
        match spawned {
            Ok(_) => Some(tx),
            Err(err) => {
                log::error!("failed to start the mime association writer: {err}");
                None
            }
        }
    });

/// Wait until every queued association write has reached the disk.
///
/// Called before the process exits: choosing a default application and closing
/// the window straight after must not lose the choice.
pub fn flush_associations() {
    let Some(tx) = ASSOCIATION_WRITER.as_ref() else {
        return;
    };
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(0);
    if tx.send(AssociationJob::Barrier(done_tx)).is_err() {
        return;
    }
    let _ = done_rx.recv();
}

/// The applications for one type, worked out once.
struct TypeApps {
    /// The type's own applications, then those offered with them (players
    /// for audio, text editors for text), then those of the types it is a
    /// kind of and of wildcards; each group by name, except that the default
    /// leads the group it is in.
    apps: Vec<(Arc<MimeApp>, MimeAppMatch)>,
    /// The default application's ID, without `.desktop`.
    default: Option<String>,
    /// The order to try applications in when opening: the default, then
    /// the type's own, then those of the types it is a kind of, of its
    /// media wildcard, of any data and of `*/*`.
    launch: Vec<Arc<MimeApp>>,
}

pub struct MimeAppCache {
    apps: Vec<Arc<MimeApp>>,
    by_id: FxHashMap<String, Arc<MimeApp>>,
    terminals: Vec<Arc<MimeApp>>,
    associations: Option<MimeApps>,
    /// Answers already worked out, by type. Asked from view code, so each
    /// type is resolved once per cache.
    by_type: Mutex<FxHashMap<Mime, Arc<TypeApps>>>,
    /// The `mimeapps.list` default for `x-scheme-handler/terminal`.
    default_terminal: Option<String>,
    locales: Locales,
    /// Whether this holds a real answer. See [`Self::empty`].
    loaded: bool,
}

impl MimeAppCache {
    /// A cache with nothing in it.
    ///
    /// What the application holds until the real one has been built on a
    /// worker. Every question it is asked answers "nothing", which is why
    /// callers check [`Self::is_loaded`] rather than trusting the answer.
    pub fn empty() -> Self {
        Self {
            apps: Vec::new(),
            by_id: FxHashMap::default(),
            terminals: Vec::new(),
            associations: None,
            by_type: Mutex::default(),
            default_terminal: None,
            locales: Locales::untranslated(),
            loaded: false,
        }
    }

    /// Whether this cache has been built, as opposed to standing in for one
    /// that is still being built.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn new() -> Self {
        let mut mime_app_cache = Self::empty();
        mime_app_cache.reload();
        mime_app_cache.loaded = true;
        mime_app_cache
    }

    /// How this app launches applications: in the user's languages, with
    /// the terminal it opens folders in for those that need one, and always
    /// through `Exec`, as [`plan_commands`] runs only commands.
    pub fn launch_options(&self) -> LaunchOptions {
        let mut options = LaunchOptions::default();
        options.locales = self.locales.clone();
        options.terminal = self.terminal_command().unwrap_or_default();
        options.dbus = false;
        options
    }

    /// What a worker needs to open a file or URI with its default
    /// application once the usual launch has found nothing that runs; see
    /// [`Opener`].
    pub fn opener(&self) -> Opener {
        let mut options = self.launch_options();
        // The usual launch runs only commands; this is where an application
        // started only through D-Bus is reached
        options.dbus = true;
        Opener {
            associations: self.associations.clone(),
            options,
        }
    }

    /// The command that runs a program in the terminal: the terminal's own
    /// command, then `-e`, which most terminals take.
    fn terminal_command(&self) -> Option<Vec<std::ffi::OsString>> {
        let terminal = self.terminal()?;
        let mut options = LaunchOptions::default();
        options.dbus = false;
        let plan = prepare_launch(terminal.entry.entry(), &[], &options).ok()?;
        let Launch::Commands(commands) = plan.launch else {
            return None;
        };
        let command = commands.into_iter().next()?;
        let mut parts = vec![command.program];
        parts.extend(command.args);
        parts.push("-e".into());
        Some(parts)
    }

    /// What the cache knows about `mime`, worked out the first time.
    fn type_apps(&self, mime: &Mime) -> Arc<TypeApps> {
        let mut by_type = self.by_type.lock().unwrap();
        by_type
            .entry(mime.clone())
            .or_insert_with(|| Arc::new(self.resolve(mime)))
            .clone()
    }

    fn resolve(&self, mime: &Mime) -> TypeApps {
        use crate::localize::LANGUAGE_SORTER;
        use xdg_kit::mime_apps::Matched;

        let Some(associations) = &self.associations else {
            return TypeApps {
                apps: Vec::new(),
                default: None,
                launch: Vec::new(),
            };
        };
        let app_of = |entry: &AppEntry| self.by_id.get(app_id(entry.id())).cloned();
        let default = associations
            .default_for(mime)
            .and_then(|entry| app_of(&entry));

        let mut seen = FxHashSet::default();
        let mut exact = Vec::new();
        let mut related = Vec::new();
        let matches = associations.apps_for(mime);
        for found in &matches {
            if found.matched == Matched::Exact
                && let Some(app) = app_of(&found.app)
                && seen.insert(app.id.clone())
            {
                exact.push(app);
            }
        }
        // Players are offered for audio, and text editors for any text
        let include_mime = match mime.media().to_ascii_lowercase().as_str() {
            "audio" => Some(Mime::from_static("video/mp4")),
            "text" => Some(Mime::TEXT_PLAIN),
            _ => None,
        };
        if let Some(include) = include_mime {
            for found in associations.apps_for(&include) {
                if found.matched == Matched::Exact
                    && let Some(app) = app_of(&found.app)
                    && seen.insert(app.id.clone())
                {
                    exact.push(app);
                }
            }
        }
        for found in &matches {
            if found.matched != Matched::Exact
                && let Some(app) = app_of(&found.app)
                && seen.insert(app.id.clone())
            {
                related.push(app);
            }
        }

        // By name, with the default leading the group it belongs to
        let by_name =
            |a: &Arc<MimeApp>, b: &Arc<MimeApp>| LANGUAGE_SORTER.compare(&a.name, &b.name);
        exact.sort_by(by_name);
        related.sort_by(by_name);
        if let Some(app) = &default {
            let group = if related.iter().any(|other| other.id == app.id) {
                &mut related
            } else {
                &mut exact
            };
            group.retain(|other| other.id != app.id);
            group.insert(0, app.clone());
        }
        let mut launch: Vec<Arc<MimeApp>> = default.iter().cloned().collect();
        for found in &matches {
            if let Some(app) = app_of(&found.app)
                && launch.iter().all(|other| other.id != app.id)
            {
                launch.push(app);
            }
        }
        let apps = exact
            .into_iter()
            .map(|app| (app, MimeAppMatch::Exact))
            .chain(related.into_iter().map(|app| (app, MimeAppMatch::Related)))
            .collect();

        TypeApps {
            apps,
            default: default.map(|app| app.id.clone()),
            launch,
        }
    }

    pub fn get_apps_for_mime(
        &self,
        mime_type: &Mime,
        include_other: bool,
    ) -> Vec<(Arc<MimeApp>, MimeAppMatch)> {
        let mut results = self.type_apps(mime_type).apps.clone();

        if include_other {
            let dedupe: FxHashSet<String> = results.iter().map(|(app, _)| app.id.clone()).collect();
            let mut apps = self
                .apps()
                .iter()
                .filter(|mime_app| !mime_app.no_display())
                .filter(|mime_app| !dedupe.contains(&mime_app.id))
                .map(|mime_app| (mime_app.clone(), MimeAppMatch::Other))
                .collect::<Vec<_>>();
            apps.sort_by(|(a, _), (b, _)| {
                crate::localize::LANGUAGE_SORTER.compare(&a.name, &b.name)
            });
            results.extend(apps);
        }

        results
    }

    /// Whether `app` is the default application for `mime`.
    pub fn is_default(&self, app: &MimeApp, mime: &Mime) -> bool {
        self.type_apps(mime).default.as_deref() == Some(app.id.as_str())
    }

    /// Reload the installed applications and their associations and
    /// defaults.
    pub fn reload(&mut self) {
        match crate::xdg_dirs::BASE_DIRS.as_ref() {
            Some(base_dirs) => self.reload_from(base_dirs),
            None => {
                *self = Self {
                    loaded: self.loaded,
                    ..Self::empty()
                }
            }
        }
    }

    /// [`Self::reload`] from the applications and lists of `base_dirs`.
    fn reload_from(&mut self, base_dirs: &xdg_kit::basedir::BaseDirs) {
        use crate::localize::LANGUAGE_SORTER;
        use xdg_kit::desktop_entry::{AppIndex, Desktops, EntryType};

        let start = Instant::now();

        self.apps.clear();
        self.by_id.clear();
        self.terminals.clear();
        self.by_type.get_mut().unwrap().clear();
        self.associations = None;
        self.default_terminal = None;
        self.locales = Locales::from_env();

        let index = Arc::new(AppIndex::load(base_dirs));
        let associations = MimeApps::load(
            base_dirs,
            &Desktops::from_env(),
            index.clone(),
            crate::mime_icon::SHARED_MIME_INFO.clone(),
        );
        // Offered among other applications only when they open some type
        let associated: FxHashSet<String> = associations
            .associated_apps()
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect();

        for found in index.iter() {
            let entry = found.entry();
            if entry.entry_type() != Some(EntryType::Application) {
                continue;
            }
            let id = app_id(found.id()).to_owned();
            let app = Arc::new(MimeApp {
                name: entry.name(&self.locales).unwrap_or_else(|| id.clone()),
                exec: entry.exec(),
                icon_name: entry.icon(&self.locales).unwrap_or_default().into(),
                icon: std::sync::OnceLock::new(),
                no_display: !associated.contains(found.id()),
                entry: found.clone(),
                id,
            });

            tracing::info!(target: "mime-apps", id = app.id, "detected desktop entry");

            if entry.categories().iter().any(|c| c == "TerminalEmulator") {
                self.terminals.push(app.clone());
            }
            self.by_id.insert(app.id.clone(), app.clone());
            self.apps.push(app);
        }
        self.apps
            .sort_by(|a, b| LANGUAGE_SORTER.compare(&a.name, &b.name));

        self.default_terminal = self.configured_terminal(&associations);
        self.associations = Some(associations);

        let elapsed = start.elapsed();
        tracing::info!(target: "mime-apps", "loaded mime app cache in {elapsed:?}");
    }

    /// The terminal the lists name as the default for
    /// `x-scheme-handler/terminal`: the first installed terminal emulator
    /// named, in the order the lists are read, else the type's default.
    ///
    /// Terminals do not list that type in their desktop files, and
    /// `MimeApps::default_for` takes a default only for an application
    /// associated with the type, so a terminal chosen as the default would
    /// be passed over there.
    fn configured_terminal(&self, associations: &MimeApps) -> Option<String> {
        use xdg_kit::mime_apps::{Group, MimeAppsList, TypePattern};

        let pattern = TypePattern::from(TERMINAL_MIME);
        for file in associations.files() {
            let Ok(list) = MimeAppsList::open(file) else {
                continue;
            };
            for id in list.ids(Group::Defaults, &pattern) {
                let id = app_id(&id);
                if self.terminals.iter().any(|terminal| terminal.id == id) {
                    return Some(id.to_owned());
                }
            }
        }
        associations
            .default_for(&TERMINAL_MIME)
            .map(|entry| app_id(entry.id()).to_owned())
    }

    pub fn apps(&self) -> &[Arc<MimeApp>] {
        &self.apps
    }

    /// The applications to open `mime` with, in the order to try them: the
    /// default, then those of the type itself, of the types it is a kind
    /// of, of its media wildcard, of any data and of `*/*`.
    pub fn get(&self, key: &Mime) -> Vec<Arc<MimeApp>> {
        self.type_apps(key).launch.clone()
    }

    pub fn icons(&self, key: &Mime) -> Vec<widget::icon::Handle> {
        self.get(key).iter().map(|app| app.icon()).collect()
    }

    /// The terminal to open folders in: the mimeapps default for
    /// `x-scheme-handler/terminal`, then `$TERMINAL`, then a list of common
    /// terminals, then whatever terminal emulator was found first.
    pub fn terminal(&self) -> Option<&Arc<MimeApp>> {
        const COMMON_TERMINALS: &[&str] = &[
            "org.gnome.Ptyxis",
            "org.gnome.Console",
            "org.gnome.Terminal",
            "org.kde.konsole",
            "kitty",
            "Alacritty",
            "foot",
            "org.wezfurlong.wezterm",
            "com.mitchellh.ghostty",
            "xterm",
        ];

        let by_id = |id: &str| self.terminals.iter().find(|terminal| terminal.id == id);

        if let Some(terminal) = self.default_terminal.as_deref().and_then(by_id) {
            return Some(terminal);
        }

        // `$TERMINAL` names a binary, so match it against the desktop id or the
        // basename of the program each entry executes
        if let Some(name) = std::env::var_os("TERMINAL")
            && let Some(name) = Path::new(&name).file_name().and_then(OsStr::to_str)
            && let Some(terminal) = self.terminals.iter().find(|terminal| {
                terminal.id == name
                    || terminal.exec.as_deref().is_some_and(|exec| {
                        exec.split_whitespace()
                            .next()
                            .and_then(|program| Path::new(program).file_name())
                            .and_then(OsStr::to_str)
                            == Some(name)
                    })
            })
        {
            return Some(terminal);
        }

        COMMON_TERMINALS
            .iter()
            .find_map(|id| by_id(id))
            .or_else(|| self.terminals.first())
    }

    /// Queue `id` as the default application for `mime`, and return a handle
    /// to wait on.
    ///
    /// Queued here, synchronously, and deliberately so. The write itself is
    /// serialized through one worker because it is a read-modify-write of the
    /// whole `mimeapps.list`, and two overlapping changes would each write
    /// back a file that did not know about the other. But a single writer only
    /// fixes overlap; it cannot fix the order things reach it in. Handing this
    /// to a worker to enqueue would mean two choices for the same type racing
    /// to get into the queue, and the one the user made first could land
    /// second and win. Enqueueing where the choice is made keeps the order the
    /// user made them in.
    pub fn queue_default(mime: Mime, id: String) -> Option<AssociationAck> {
        let tx = ASSOCIATION_WRITER.as_ref().or_else(|| {
            log::error!("the mime association writer is not running");
            None
        })?;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        if tx.send(AssociationJob::Write(mime, id, done_tx)).is_err() {
            log::warn!("the mime association writer stopped");
            return None;
        }
        Some(done_rx)
    }

    /// Wait for a queued association write to finish. Blocking, so it belongs
    /// on a worker.
    pub fn await_association(ack: AssociationAck) -> bool {
        ack.recv().unwrap_or(false)
    }

    /// Records `id` as the default application for `mime` in the user's
    /// `mimeapps.list`, keeping the rest of the file as it was.
    ///
    /// Returns whether the associations changed, in which case the caller
    /// reloads the cache. Reloading here would mean rebuilding it inline, and
    /// it is built by walking every desktop entry on the system.
    ///
    /// Reads and writes `mimeapps.list`, so it belongs on a worker. It takes
    /// nothing from the cache, which is why it is an associated function: the
    /// caller can hand it to a worker without the cache going with it.
    pub fn set_default(mime: Mime, id: String) -> bool {
        let Some(base_dirs) = crate::xdg_dirs::BASE_DIRS.as_ref() else {
            log::warn!("failed to find mimeapps.list path");
            return false;
        };
        Self::set_default_in(
            base_dirs,
            &xdg_kit::desktop_entry::Desktops::from_env(),
            mime,
            id,
        )
    }

    /// [`Self::set_default`] in the user's list of `base_dirs`, for the
    /// desktops `desktops`.
    ///
    /// The list written is the one read first: the user's list for the
    /// first of `desktops` that has one, else their `mimeapps.list`. A
    /// default written anywhere else would lose to one that list names.
    fn set_default_in(
        base_dirs: &xdg_kit::basedir::BaseDirs,
        desktops: &xdg_kit::desktop_entry::Desktops,
        mime: Mime,
        mut id: String,
    ) -> bool {
        use xdg_kit::mime_apps::{MIMEAPPS_LIST, TypePattern};

        let config_home = base_dirs.config_home();
        let plain = config_home.join(MIMEAPPS_LIST);
        let path = desktops
            .names()
            .iter()
            .filter(|name| !name.is_empty() && !name.contains(['/', '\0']))
            .map(|name| config_home.join(format!("{}-{MIMEAPPS_LIST}", name.to_ascii_lowercase())))
            .find(|path| path.exists())
            .unwrap_or_else(|| plain.clone());

        let suffix = ".desktop";
        if !id.ends_with(suffix) {
            id.push_str(suffix);
        }
        let pattern = TypePattern::from(mime);

        // A default counts only for an application associated with the type,
        // and a desktop's own list holds no associations, so the user's
        // plain list associates it first. Writing the default there does
        // both.
        if path != plain && !edit_list(&plain, |list| list.add_association(&pattern, &id)) {
            return false;
        }
        edit_list(&path, |list| list.set_default(&pattern, &id))
    }
}

/// Reads the list at `path`, applies `edit` and writes it back; whether all
/// of it worked. Problems are logged.
fn edit_list(
    path: &Path,
    edit: impl FnOnce(
        &mut xdg_kit::mime_apps::MimeAppsList,
    ) -> Result<(), xdg_kit::mime_apps::EditError>,
) -> bool {
    let mut list = match xdg_kit::mime_apps::MimeAppsList::open(path) {
        // An entry already under an alias of the type is changed, not left
        // beside the new one
        Ok(list) => list.resolving_aliases(crate::mime_icon::SHARED_MIME_INFO.clone()),
        Err(err) => {
            log::warn!("failed to read {}: {err}", path.display());
            return false;
        }
    };
    if let Err(err) = edit(&mut list) {
        log::warn!("failed to edit {}: {err}", path.display());
        return false;
    }
    match list.write() {
        Ok(xdg_kit::basedir::Durability::Synced) => true,
        // The list was replaced; only syncing its folder failed. Writing it
        // again could overwrite a change made since, so it counts as written.
        Ok(xdg_kit::basedir::Durability::NotSynced(err)) => {
            log::warn!("wrote {} but could not sync it: {err}", path.display());
            true
        }
        Err(err) => {
            log::warn!("failed to write {}: {}", path.display(), err);
            false
        }
    }
}

impl Default for MimeAppCache {
    fn default() -> Self {
        Self::new()
    }
}

/// A desktop file ID without its `.desktop`, as this app names applications.
fn app_id(id: &str) -> &str {
    id.strip_suffix(".desktop").unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::exec_to_command;
    use std::path::Path;

    /// The base directories of a home at `home`: its own `config` and
    /// `data` folders, and no system ones.
    fn base_dirs(home: &Path) -> xdg_kit::basedir::BaseDirs {
        xdg_kit::basedir::BaseDirs::from_vars(|name| {
            let path = match name {
                "HOME" => home.to_owned(),
                "XDG_DATA_HOME" => home.join("data"),
                "XDG_CONFIG_HOME" => home.join("config"),
                "XDG_DATA_DIRS" | "XDG_CONFIG_DIRS" => home.join("none"),
                _ => return None,
            };
            Some(path.into())
        })
        .unwrap()
    }

    /// Writes the desktop entry `id` into `home`'s applications.
    fn install(home: &Path, id: &str, extra: &str) {
        let apps = home.join("data/applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join(format!("{id}.desktop")),
            format!("[Desktop Entry]\nType=Application\nName={id}\nExec={id} %f\n{extra}"),
        )
        .unwrap();
    }

    /// Writes `text` as `home`'s list `name`.
    fn write_list(home: &Path, name: &str, text: &str) {
        let config = home.join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join(name), text).unwrap();
    }

    /// A default chosen in the app lands in the list read first, the
    /// desktop's own when there is one, so it wins over the default that
    /// list already names.
    #[test]
    fn a_chosen_default_goes_to_the_list_read_first() {
        use xdg_kit::desktop_entry::{AppIndex, Desktops};
        use xdg_kit::mime_apps::MimeApps;

        let home = tempfile::tempdir().unwrap();
        install(home.path(), "old", "MimeType=text/plain;\n");
        install(home.path(), "new", "MimeType=text/plain;\n");
        install(home.path(), "player", "MimeType=video/mp4;\n");
        write_list(
            home.path(),
            "gnome-mimeapps.list",
            "[Default Applications]\ntext/plain=old.desktop;\n",
        );
        let base_dirs = base_dirs(home.path());
        let desktops = Desktops::new(["ubuntu", "GNOME"]);
        let default = || {
            MimeApps::load(
                &base_dirs,
                &desktops,
                std::sync::Arc::new(AppIndex::load(&base_dirs)),
                crate::mime_icon::SHARED_MIME_INFO.clone(),
            )
            .default_for(&super::Mime::TEXT_PLAIN)
            .map(|app| app.id().to_owned())
        };
        assert_eq!(default().as_deref(), Some("old.desktop"));

        assert!(super::MimeAppCache::set_default_in(
            &base_dirs,
            &desktops,
            super::Mime::TEXT_PLAIN,
            "new".to_owned(),
        ));
        assert_eq!(default().as_deref(), Some("new.desktop"));

        // An application the type does not list is associated with it too,
        // in the plain list, or the default would not count
        assert!(super::MimeAppCache::set_default_in(
            &base_dirs,
            &desktops,
            super::Mime::TEXT_PLAIN,
            "player".to_owned(),
        ));
        assert_eq!(default().as_deref(), Some("player.desktop"));

        // With no list of the desktop's own, the user's plain list
        let desktops = Desktops::new(["KDE"]);
        assert!(super::MimeAppCache::set_default_in(
            &base_dirs,
            &desktops,
            super::Mime::from_static("image/png"),
            "new".to_owned(),
        ));
        assert!(home.path().join("config/mimeapps.list").exists());
    }

    /// A terminal chosen as the default for `x-scheme-handler/terminal`
    /// is used, though terminals do not list that type themselves.
    #[test]
    fn the_chosen_terminal_is_used() {
        let home = tempfile::tempdir().unwrap();
        install(home.path(), "kitty", "Categories=TerminalEmulator;\n");
        install(home.path(), "foot", "Categories=TerminalEmulator;\n");
        write_list(
            home.path(),
            "mimeapps.list",
            "[Default Applications]\nx-scheme-handler/terminal=no-such-terminal.desktop;foot.desktop;\n",
        );
        let mut cache = super::MimeAppCache::empty();
        cache.reload_from(&base_dirs(home.path()));
        assert_eq!(cache.default_terminal.as_deref(), Some("foot"));
        assert_eq!(cache.terminal().map(|t| t.id.as_str()), Some("foot"));
    }

    /// The program and arguments of each command `plan` runs.
    fn command_lines(plan: xdg_kit::desktop_entry::LaunchPlan) -> Vec<Vec<String>> {
        super::plan_commands(plan)
            .iter()
            .map(|command| {
                std::iter::once(command.get_program())
                    .chain(command.get_args())
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect()
            })
            .collect()
    }

    /// A home with a browser, the default for `https`, and an editor, the
    /// default for `text/plain`, loaded into a cache.
    fn opening_home() -> (tempfile::TempDir, super::MimeAppCache) {
        let home = tempfile::tempdir().unwrap();
        let apps = home.path().join("data/applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join("browser.desktop"),
            "[Desktop Entry]\nType=Application\nName=browser\nExec=browser %u\n\
             MimeType=x-scheme-handler/https;\n",
        )
        .unwrap();
        install(home.path(), "editor", "MimeType=text/plain;\n");
        write_list(
            home.path(),
            "mimeapps.list",
            "[Default Applications]\nx-scheme-handler/https=browser.desktop\ntext/plain=editor.desktop\n",
        );
        let mut cache = super::MimeAppCache::empty();
        cache.reload_from(&base_dirs(home.path()));
        (home, cache)
    }

    /// A URI opens with the default for its scheme, a file with the default
    /// for the type already found for it, without looking at it again.
    #[test]
    fn a_uri_or_a_file_opens_with_its_default() {
        use xdg_kit::desktop_entry::Target;

        let (home, cache) = opening_home();
        let opener = cache.opener();

        let url = "https://example.org/a";
        let plan = opener.plan(&Target::Uri(url.to_owned()), None).unwrap();
        assert_eq!(command_lines(plan), [["browser", url]]);

        // Never read: the type given is the one used
        let file = home.path().join("no such file");
        let plan = opener
            .plan(&Target::Path(file.clone()), Some(&super::Mime::TEXT_PLAIN))
            .unwrap();
        let file = file.to_string_lossy().into_owned();
        assert_eq!(command_lines(plan), [["editor".to_owned(), file]]);

        assert!(
            opener
                .plan(&Target::Uri("gopher://example.org".to_owned()), None)
                .is_err()
        );
    }

    /// A default started only through D-Bus is opened that way.
    #[test]
    fn a_dbus_only_default_opens_through_dbus() {
        use xdg_kit::desktop_entry::{Launch, Target};

        let home = tempfile::tempdir().unwrap();
        let apps = home.path().join("data/applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join("org.example.Viewer.desktop"),
            "[Desktop Entry]\nType=Application\nName=Viewer\nDBusActivatable=true\n\
             MimeType=text/plain;\n",
        )
        .unwrap();
        write_list(
            home.path(),
            "mimeapps.list",
            "[Default Applications]\ntext/plain=org.example.Viewer.desktop\n",
        );
        let mut cache = super::MimeAppCache::empty();
        cache.reload_from(&base_dirs(home.path()));

        // The usual launch has no command for it
        let target = Target::Path(home.path().join("a.txt"));
        let viewer = cache.get(&super::Mime::TEXT_PLAIN);
        assert!(
            viewer[0]
                .commands(std::slice::from_ref(&target), &cache.launch_options())
                .is_none()
        );

        let plan = cache
            .opener()
            .plan(&target, Some(&super::Mime::TEXT_PLAIN))
            .unwrap();
        assert!(matches!(plan.launch, Launch::DBus(_)), "{plan:?}");
    }

    /// A default that runs in a terminal opens in the user's terminal.
    #[test]
    fn a_terminal_default_opens_in_the_terminal() {
        use xdg_kit::desktop_entry::Target;

        let home = tempfile::tempdir().unwrap();
        install(home.path(), "kitty", "Categories=TerminalEmulator;\n");
        install(home.path(), "vim", "Terminal=true\nMimeType=text/plain;\n");
        write_list(
            home.path(),
            "mimeapps.list",
            "[Default Applications]\ntext/plain=vim.desktop\n\
             x-scheme-handler/terminal=kitty.desktop\n",
        );
        let mut cache = super::MimeAppCache::empty();
        cache.reload_from(&base_dirs(home.path()));
        let opener = cache.opener();

        let file = home.path().join("notes.txt");
        let plan = opener
            .plan(&Target::Path(file.clone()), Some(&super::Mime::TEXT_PLAIN))
            .unwrap();
        let file = file.to_string_lossy().into_owned();
        assert_eq!(command_lines(plan), [["kitty", "-e", "vim", file.as_str()]]);
    }

    /// Before the applications are loaded, nothing opens.
    #[test]
    fn an_empty_cache_opens_nothing() {
        let target = xdg_kit::desktop_entry::Target::Uri("https://example.org".to_owned());
        assert!(
            super::MimeAppCache::empty()
                .opener()
                .plan(&target, None)
                .is_err()
        );
    }

    /// Changes to a list are seen after the app has saved it, too: a save
    /// renames a new file over the old one, which a watch on the file
    /// itself would not survive.
    #[test]
    fn list_changes_are_seen_after_a_save() {
        use notify_debouncer_full::notify::{self, Watcher as _};
        use std::time::Duration;

        let home = tempfile::tempdir().unwrap();
        write_list(home.path(), "mimeapps.list", "[Default Applications]\n");
        let list = home.path().join("config/mimeapps.list");
        let files: rustc_hash::FxHashSet<std::path::PathBuf> =
            std::iter::once(list.clone()).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx).unwrap();
        for folder in super::list_folders(&files) {
            watcher
                .watch(folder, notify::RecursiveMode::NonRecursive)
                .unwrap();
        }
        let seen = || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(Ok(event)) if super::touches_a_list(&event.paths, &files) => {
                        // Let the rest of this change's events arrive
                        std::thread::sleep(Duration::from_millis(100));
                        while rx.try_recv().is_ok() {}
                        return true;
                    }
                    Ok(_) => {}
                    Err(_) => return false,
                }
            }
            false
        };

        let base_dirs = base_dirs(home.path());
        let desktops = xdg_kit::desktop_entry::Desktops::new(Vec::<String>::new());
        assert!(super::MimeAppCache::set_default_in(
            &base_dirs,
            &desktops,
            super::Mime::TEXT_PLAIN,
            "a".to_owned(),
        ));
        assert!(seen(), "the save is seen");
        std::fs::write(&list, "[Default Applications]\ntext/plain=b.desktop;\n").unwrap();
        assert!(seen(), "a change after the save is seen");
        std::fs::write(home.path().join("config/other.txt"), "x").unwrap();
        assert!(!seen(), "other files in the folder are not");
    }

    /// The gates in `app` decide whether to act or wait by asking
    /// `is_loaded`, so an empty cache has to say no to that and to everything
    /// else. Saying "no applications" while looking loaded would make every
    /// file fall through to `xdg-open`.
    #[test]
    fn an_empty_cache_admits_it_knows_nothing() {
        let cache = super::MimeAppCache::empty();
        assert!(!cache.is_loaded(), "an empty cache is not a built one");
        assert!(cache.apps().is_empty());
        assert!(
            cache.terminal().is_none(),
            "an empty cache must not name a terminal it does not have"
        );
    }

    /// A default reached only through a broader type, here any data, is
    /// still the default: opening tries it before the type's own
    /// applications.
    #[test]
    fn a_default_through_a_broader_type_opens_first() {
        let home = tempfile::tempdir().unwrap();
        let apps = home.path().join("data/applications");
        std::fs::create_dir_all(&apps).unwrap();
        for (id, types) in [
            ("viewer", "application/pdf;"),
            ("hexer", "application/octet-stream;"),
        ] {
            std::fs::write(
                apps.join(format!("{id}.desktop")),
                format!(
                    "[Desktop Entry]\nType=Application\nName={id}\nExec={id} %f\nMimeType={types}\n"
                ),
            )
            .unwrap();
        }
        let config = home.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("mimeapps.list"),
            "[Default Applications]\napplication/pdf=hexer.desktop\n",
        )
        .unwrap();
        let base_dirs = base_dirs(home.path());

        let mut cache = super::MimeAppCache::empty();
        cache.reload_from(&base_dirs);
        let pdf = super::Mime::from_static("application/pdf");
        let order: Vec<String> = cache.get(&pdf).iter().map(|app| app.id.clone()).collect();
        assert_eq!(order, ["hexer", "viewer"]);
        let hexer = cache.get(&pdf).remove(0);
        assert!(cache.is_default(&hexer, &pdf));
    }

    /// An application counts as opening some type when a `mimeapps.list`
    /// adds it, though its desktop file names no type. One named only as a
    /// default does not, as the default is not taken either.
    #[test]
    fn other_applications_are_those_some_type_lists() {
        let home = tempfile::tempdir().unwrap();
        let apps = home.path().join("data/applications");
        std::fs::create_dir_all(&apps).unwrap();
        for (id, types) in [
            ("viewer", "application/pdf;"),
            ("added", ""),
            ("named", ""),
            ("plain", ""),
        ] {
            std::fs::write(
                apps.join(format!("{id}.desktop")),
                format!(
                    "[Desktop Entry]\nType=Application\nName={id}\nExec={id} %f\nMimeType={types}\n"
                ),
            )
            .unwrap();
        }
        let config = home.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("mimeapps.list"),
            "[Added Associations]\ntext/plain=added.desktop;\n[Default Applications]\ntext/plain=named.desktop;\n",
        )
        .unwrap();
        let base_dirs = base_dirs(home.path());

        let mut cache = super::MimeAppCache::empty();
        cache.reload_from(&base_dirs);
        let mut shown: Vec<&str> = cache
            .apps()
            .iter()
            .filter(|app| !app.no_display())
            .map(|app| app.id.as_str())
            .collect();
        shown.sort_unstable();
        assert_eq!(shown, ["added", "viewer"]);
    }

    /// And a real one says yes, so the gates open.
    #[test]
    fn a_built_cache_reports_itself_loaded() {
        assert!(super::MimeAppCache::new().is_loaded());
    }

    #[test]
    fn keys_within_words() {
        let exec = "/usr/bin/foo --option=%f";
        let paths = ["file1"];
        let commands = exec_to_command(exec, "keys_within_words", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(1, commands.len());
        let command = commands.first().unwrap();

        assert_eq!("/usr/bin/foo", command.get_program().to_str().unwrap());
        assert_eq!(
            "--option=file1",
            command.get_args().next().unwrap().to_str().unwrap()
        );
    }

    #[test]
    fn no_path_f_field_code() {
        let exec = "/usr/bin/foo %f";
        let paths: [&str; 0] = [];
        let commands = exec_to_command(exec, "no_path_f_field_code", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(1, commands.len());
        let command = commands.first().unwrap();

        assert_eq!("/usr/bin/foo", command.get_program().to_str().unwrap());
        assert_eq!(0, command.get_args().len());
    }

    #[test]
    fn one_path_f_field_code() {
        let exec = "/usr/bin/foo %f";
        let paths = ["file1"];
        let commands = exec_to_command(exec, "one_path_f_field_code", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(1, commands.len());
        let command = commands.first().unwrap();

        assert_eq!("/usr/bin/foo", command.get_program().to_str().unwrap());
        assert_eq!(
            "file1",
            command.get_args().next().unwrap().to_str().unwrap()
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn one_path_F_field_code() {
        let exec = "/usr/bin/cosmic-term -w %F";
        let paths = ["/home/user"];
        let commands = exec_to_command(exec, "one_path_F_field_code", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(1, commands.len());
        let command = commands.first().unwrap();
        let mut args = command.get_args();

        assert_eq!(
            "/usr/bin/cosmic-term",
            command.get_program().to_str().unwrap()
        );
        assert_eq!("-w", args.next().unwrap().to_str().unwrap());
        assert_eq!(paths[0], args.next().unwrap().to_str().unwrap());
    }

    #[test]
    fn one_path_u_field_code() {
        let exec = "/usr/bin/cosmic-term -w %u";
        let paths = ["/home/user"];
        let commands = exec_to_command(exec, "one_path_u_field_code", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(1, commands.len());
        let command = commands.first().unwrap();
        let mut args = command.get_args();

        assert_eq!(
            "/usr/bin/cosmic-term",
            command.get_program().to_str().unwrap()
        );
        assert_eq!("-w", args.next().unwrap().to_str().unwrap());
        assert_eq!(paths[0], args.next().unwrap().to_str().unwrap());
    }

    #[test]
    #[allow(non_snake_case)]
    fn one_path_U_field_code() {
        let exec = "/usr/bin/rmrfbye %U";
        let paths = ["/"];
        let commands = exec_to_command(exec, "one_path_U_field_code", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(1, commands.len());
        let command = commands.first().unwrap();

        assert_eq!("/usr/bin/rmrfbye", command.get_program().to_str().unwrap());
        assert_eq!("/", command.get_args().next().unwrap().to_str().unwrap());
    }

    #[test]
    fn mult_path_f_field_code() {
        let exec = "/usr/games/ppsspp %f";
        let paths = [
            "/usr/share/games/psp/miku.iso",
            "/usr/share/games/psp/eternia.iso",
        ];
        let commands = exec_to_command(exec, "mult_path_f_field_code", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(paths.len(), commands.len());
        for (command, path) in commands.into_iter().zip(paths.iter()) {
            assert_eq!("/usr/games/ppsspp", command.get_program().to_str().unwrap());

            assert_eq!(1, command.get_args().len());
            let command_path = command.get_args().next().unwrap();
            assert_eq!(*path, command_path.to_str().unwrap());
        }
    }

    #[test]
    #[allow(non_snake_case)]
    fn mult_path_F_field_code() {
        let exec = "/usr/games/gzdoom %F";
        let paths = [
            "/usr/share/games/doom2/hr.wad",
            "/usr/share/games/doom2/hrmus.wad",
        ];
        let commands = exec_to_command(exec, "mult_path_F_field_code", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(1, commands.len());
        let command = commands.first().unwrap();

        assert_eq!("/usr/games/gzdoom", command.get_program().to_str().unwrap());
        assert!(
            paths
                .iter()
                .zip(command.get_args())
                .all(|(&expected, actual)| expected == actual.to_string_lossy())
        );
    }

    #[test]
    fn mult_path_u_field_code() {
        let exec = "/usr/bin/cosmic_browser %u";
        let paths = [
            "file:///home/josh/Books/osstep.pdf",
            "https://redox-os.org/",
            "https://system76.com/",
        ];
        let commands = exec_to_command(exec, "mult_path_u_field_code", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(paths.len(), commands.len());
        for (command, path) in commands.into_iter().zip(paths.iter()) {
            assert_eq!(
                "/usr/bin/cosmic_browser",
                command.get_program().to_str().unwrap()
            );

            assert_eq!(1, command.get_args().len());
            let command_path = command.get_args().next().unwrap();
            assert_eq!(*path, command_path.to_str().unwrap());
        }
    }

    #[test]
    #[allow(non_snake_case)]
    fn mult_path_U_field_code() {
        let exec = "/usr/bin/mpv %U";
        let paths = [
            "frieren01.mkv",
            "rtmp://example.org/this/video/doesnt/exist.avi",
        ];
        let commands = exec_to_command(exec, "mult_path_U_field_code", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(1, commands.len());
        let command = commands.first().unwrap();
        assert_eq!(paths.len(), command.get_args().count());

        assert_eq!("/usr/bin/mpv", command.get_program().to_str().unwrap());
        assert!(
            paths
                .iter()
                .zip(command.get_args())
                .all(|(&expected, actual)| expected == actual.to_string_lossy())
        );
    }

    #[test]
    fn flatpak_style_exec() {
        // Tests args before field codes
        let exec = "/usr/bin/flatpak run --branch=stable --command=ferris --file-forwarding org.joshfake.ferris @@u %U";
        let args = [
            "run",
            "--branch=stable",
            "--command=ferris",
            "--file-forwarding",
            "org.joshfake.ferris",
            "@@u",
        ];
        let paths = ["file1.rs", "file2.rs"];
        let commands = exec_to_command(exec, "flatpak_style_exec", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(1, commands.len());
        let command = commands.first().unwrap();
        assert_eq!(args.len() + paths.len(), command.get_args().count());

        assert_eq!("/usr/bin/flatpak", command.get_program().to_str().unwrap());
        assert!(
            args.iter()
                .chain(paths.iter())
                .zip(command.get_args())
                .all(|(&expected, actual)| expected == actual.to_string_lossy())
        );
    }

    #[test]
    fn multiple_field_codes() {
        // Tests that only one field code is used rather than passing paths to each field code
        let exec = "/usr/games/roguelike %U %f";
        let paths = [
            "file:///usr/share/games/roguelike/mods/mod1",
            "file:///usr/share/games/roguelike/mods/mod2",
        ];
        let commands = exec_to_command(exec, "multiple_field_codes", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(1, commands.len());
        let command = commands.first().unwrap();

        assert_eq!(
            "/usr/games/roguelike",
            command.get_program().to_str().unwrap()
        );
        assert!(
            paths
                .iter()
                .zip(command.get_args())
                .all(|(&expected, actual)| expected == actual.to_string_lossy())
        );
    }

    #[test]
    fn sandwiched_field_code() {
        // Tests that arguments before and after the field code works
        // (Borrowed from KDE because someone had this exact line in an issue)
        let exec = "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=okular --file-forwarding org.kde.okular @@u %U @@";
        let args_leading = [
            "run",
            "--branch=stable",
            "--arch=x86_64",
            "--command=okular",
            "--file-forwarding",
            "org.kde.okular",
            "@@u",
        ];
        let paths = ["rust_game_dev.pdf", "superhero_ferris.epub"];
        let args_trailing = ["@@"];
        let commands = exec_to_command(exec, "sandwiched_field_code", None, &paths)
            .expect("Should parse valid exec");

        assert_eq!(1, commands.len());
        let command = commands.first().unwrap();
        assert_eq!(
            args_leading.len() + paths.len() + args_trailing.len(),
            command.get_args().len()
        );

        assert_eq!("/usr/bin/flatpak", command.get_program().to_str().unwrap());
        assert!(
            args_leading
                .iter()
                .chain(paths.iter())
                .chain(args_trailing.iter())
                .zip(command.get_args())
                .all(|(&expected, actual)| expected == actual.to_string_lossy())
        );
    }
}
