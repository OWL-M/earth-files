// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::ui::widget;
use bstr::{BString, ByteSlice, ByteVec};
pub use mime_guess::Mime;
#[cfg(feature = "desktop")]
use notify_debouncer_full::notify;
use rustc_hash::{FxHashMap, FxHashSet};
use std::ffi::OsStr;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock, RwLock, atomic};
use std::time::{self, Duration, Instant};
use std::{fs, io, process};

#[cfg(feature = "desktop")]
pub async fn watch(mut emitter: impl FnMut() + 'static + Send) {
    let watcher_result = notify_debouncer_full::new_debouncer(
        time::Duration::from_millis(250),
        Some(time::Duration::from_millis(250)),
        move |event_res: notify_debouncer_full::DebounceEventResult| {
            let Ok(events) = event_res else {
                return;
            };

            if events.iter().any(|event| {
                event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove()
            }) {
                emitter();
            }
        },
    );

    if let Ok(mut watcher) = watcher_result {
        let system_paths = cosmic_mime_apps::list_paths();
        let local_paths = (|| {
            let base_dirs = xdg::BaseDirectories::new();
            let Some(home) = base_dirs.get_config_home() else {
                return Err(std::io::Error::other("XDG config home not set"));
            };

            let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") else {
                return Err(std::io::Error::other("XDG_CURRENT_DESKTOP unset"));
            };

            let default_mimeapps = home.join("mimeapps.list");
            let desktop_mimeapps =
                home.join([&desktop.to_ascii_lowercase(), "-mimeapps.list"].concat());

            Ok([desktop_mimeapps, default_mimeapps])
        })()
        .ok();

        for path in system_paths
            .iter()
            .chain(local_paths.as_ref().into_iter().flatten())
        {
            _ = watcher.watch(path.as_path(), notify::RecursiveMode::NonRecursive);
        }

        std::future::pending().await
    }
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
            let mut new_argument = BString::new(Vec::with_capacity(argument.capacity()));
            let mut chars = argument.chars();
            while let Some(char) = chars.next() {
                // https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html
                if char == '%' {
                    match chars.next() {
                        Some('%') => new_argument.push_char(char),
                        Some('c') => new_argument.push_str(entry_name),
                        Some('k') => {
                            if let Some(path) = entry_path {
                                new_argument.push_str(path.as_os_str().as_bytes());
                            }
                        }

                        // %f and %u behave the same in a file manager.
                        Some('f' | 'u') => {
                            if let Some(path) = path
                                && !field_code_used
                            {
                                batch_process = true;
                                field_code_used = true;
                                new_argument.push_str(path.as_bytes());
                            }
                        }

                        // %F and %U behave the same in a file manager.
                        Some('F') | Some('U') if !field_code_used && new_argument.is_empty() => {
                            field_code_used = true;
                            for path in path_opt.iter().map(AsRef::as_ref) {
                                args.push(BString::new(path.as_bytes().to_owned()));
                            }
                        }

                        _ => (),
                    }
                } else {
                    new_argument.push_char(char);
                }
            }

            if !new_argument.is_empty() {
                args.push(new_argument);
            }
        }

        let mut command = process::Command::new(&arguments[0]);

        for arg in args {
            match arg.to_os_str() {
                Ok(arg) => {
                    command.arg(arg);
                }
                Err(_) => {
                    tracing::error!("invalid string encoding in command");
                    return None;
                }
            }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MimeAppMatch {
    Exact,
    Related,
    Other,
}

#[derive(Clone, Debug)]
pub struct MimeApp {
    pub id: String,
    pub path: Option<PathBuf>,
    pub name: String,
    pub exec: Option<String>,
    icon_name: Box<str>,
    icon: std::sync::OnceLock<widget::icon::Handle>,
    is_default: Arc<RwLock<FxHashSet<Box<str>>>>,
    no_display: Arc<AtomicBool>,
}

impl MimeApp {
    pub fn command<O: AsRef<OsStr>>(&self, path_opt: &[O]) -> Option<Vec<process::Command>> {
        exec_to_command(
            self.exec.as_deref()?,
            &self.name,
            self.path.as_deref(),
            path_opt,
        )
    }

    pub fn is_default(&self, mime: &Mime) -> bool {
        self.is_default.read().unwrap().contains(mime.essence_str())
    }

    pub fn no_display(&self) -> bool {
        self.no_display.load(atomic::Ordering::Relaxed)
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

pub struct MimeAppCache {
    apps: Vec<Arc<MimeApp>>,
    cache: FxHashMap<Mime, Vec<Arc<MimeApp>>>,
    terminals: Vec<Arc<MimeApp>>,
    /// The mimeapps default for `x-scheme-handler/terminal`, resolved at most
    /// once. Answering it costs a whole `xdg-mime` process, and
    /// [`Self::terminal`] is asked every time a menu that offers "open in
    /// terminal" is built.
    default_terminal: OnceLock<Option<String>>,
}

impl MimeAppCache {
    pub fn new() -> Self {
        let mut mime_app_cache = Self {
            apps: Vec::new(),
            cache: FxHashMap::default(),
            terminals: Vec::new(),
            default_terminal: OnceLock::new(),
        };
        mime_app_cache.reload();
        mime_app_cache
    }

    pub fn get_apps_for_mime(
        &self,
        mime_type: &Mime,
        include_other: bool,
    ) -> Vec<(&Arc<MimeApp>, MimeAppMatch)> {
        let mut results = Vec::new();
        let mut dedupe = FxHashSet::default();

        // start with exact matches
        results.extend(
            self.get(mime_type)
                .iter()
                .filter(|&mime_app| dedupe.insert(&mime_app.id))
                .map(|mime_app| (mime_app, MimeAppMatch::Exact)),
        );

        let include_mime = match mime_type.type_().as_str() {
            "audio" => Some("video/mp4".parse::<Mime>().expect("video/mp4 mime")),
            "text" => Some(mime_guess::mime::TEXT_PLAIN),
            _ => None,
        };

        if let Some(mime) = include_mime {
            results.extend(
                self.get(&mime)
                    .iter()
                    .filter(|&mime_app| dedupe.insert(&mime_app.id))
                    .map(|mime_app| (mime_app, MimeAppMatch::Exact)),
            );
        }

        // grab matches based off of subclass / parent mime type
        if let Some(parent_types) = crate::mime_icon::parent_mime_types(mime_type) {
            for parent_type in parent_types {
                results.extend(
                    self.get(&parent_type)
                        .iter()
                        .filter(|&mime_app| dedupe.insert(&mime_app.id))
                        .map(|mime_app| (mime_app, MimeAppMatch::Related)),
                );
            }
        }

        if include_other {
            results.extend({
                let mut apps = self
                    .apps()
                    .iter()
                    .filter(|mime_app| !mime_app.no_display())
                    .filter(|&mime_app| dedupe.insert(&mime_app.id))
                    .map(|mime_app| (mime_app, MimeAppMatch::Other))
                    .collect::<Vec<_>>();
                apps.sort_by(|(a, _), (b, _)| {
                    crate::localize::LANGUAGE_SORTER.compare(&a.name, &b.name)
                });
                apps
            });
        }

        results
    }

    #[cfg(not(feature = "desktop"))]
    pub fn reload(&mut self) {}

    /// Reload mime types and their known app associations and defaults.
    #[cfg(feature = "desktop")]
    pub fn reload(&mut self) {
        use crate::localize::LANGUAGE_SORTER;
        use crate::mime_icon;
        use freedesktop_desktop_entry as fde;
        use std::borrow::Cow;

        let start = Instant::now();

        self.apps.clear();
        self.cache.clear();
        self.terminals.clear();
        // The associations being reloaded are where the default terminal
        // comes from, so the remembered answer is out of date too.
        self.default_terminal = OnceLock::new();

        let mut list = cosmic_mime_apps::List::default();
        let paths = cosmic_mime_apps::list_paths();
        list.load_from_paths(&paths);
        let locales = fde::get_languages_from_env();
        let desktop_entries = fde::Iter::new(fde::default_paths()).entries(Some(&locales));
        let shared_mime_info = &*mime_icon::SHARED_MIME_INFO;
        let mut aliased_mimes = FxHashMap::default();

        for desktop_entry in desktop_entries {
            let name = desktop_entry
                .name(&locales)
                .unwrap_or_else(|| Cow::Borrowed(desktop_entry.id()));

            let app = Arc::new(MimeApp {
                id: desktop_entry.appid.clone(),
                path: Some(desktop_entry.path.clone()),
                name: name.into(),
                exec: desktop_entry.exec().map(String::from),
                icon_name: desktop_entry.icon().unwrap_or_default().into(),
                icon: std::sync::OnceLock::new(),
                is_default: Arc::new(RwLock::default()),
                no_display: Arc::new(AtomicBool::new(false)),
            });

            tracing::info!(target: "mime-apps", id = app.id, "detected desktop entry");

            self.apps.push(app.clone());

            if desktop_entry
                .categories()
                .into_iter()
                .flatten()
                .any(|c| c == "TerminalEmulator")
            {
                self.terminals.push(app.clone());
            }

            // Cache associations defined by the desktop entry.
            let mime_types = desktop_entry.mime_type().unwrap_or_else(Vec::new);
            let associated_mime_types = mime_types.iter().filter_map(|m| {
                m.parse::<Mime>().ok().map(|mime| {
                    if let Some(unaliased) = shared_mime_info.unalias_mime_type(&mime) {
                        aliased_mimes.insert(unaliased.clone(), mime);
                        return unaliased;
                    }

                    mime
                })
            });

            for mime in associated_mime_types {
                let apps = self.cache.entry(mime.clone()).or_default();
                if apps.iter().all(|cached_app| cached_app.id != app.id) {
                    apps.push(app.clone());
                }
            }
        }

        // Cache added associations from mimeapps lists.
        for (mut added_mime, added_apps) in &list.added_associations {
            let _unaliased;
            if let Some(unaliased) = shared_mime_info.unalias_mime_type(added_mime) {
                aliased_mimes.insert(unaliased.clone(), added_mime.clone());
                _unaliased = unaliased;
                added_mime = &_unaliased;
            }

            for added_app in added_apps {
                if let Some(app) = self
                    .apps
                    .iter()
                    .find(|cached| cached.id.as_str() == added_app.as_ref())
                {
                    let apps = self.cache.entry(added_mime.clone()).or_default();
                    if apps.iter().all(|cached_app| cached_app.id != app.id) {
                        apps.push(app.clone());
                    }
                }
            }
        }

        // Remove associations
        for (mut removed_mime, removed_apps) in &list.removed_associations {
            let _unaliased;
            if let Some(unaliased) = shared_mime_info.unalias_mime_type(removed_mime) {
                aliased_mimes.insert(unaliased.clone(), removed_mime.clone());
                _unaliased = unaliased;
                removed_mime = &_unaliased;
            }

            for removed_app in removed_apps {
                if let Some(app) = self
                    .apps
                    .iter()
                    .find(|cached| cached.id.as_str() == removed_app.as_ref())
                    && let Some(apps) = self.cache.get_mut(removed_mime)
                {
                    apps.retain(|cached_app| cached_app.id != app.id);
                }
            }
        }

        // Fetch defaults and sort apps by their default precedence.
        for (mime, mut apps) in std::mem::take(&mut self.cache).into_iter() {
            let defaults = list.default_app_for(&mime);
            let aliased_defaults = aliased_mimes
                .get(&mime)
                .and_then(|mime| list.default_app_for(mime));

            let cache = self
                .cache
                .entry(mime.clone())
                .or_insert_with(|| Vec::with_capacity(apps.len()));

            // Sort cached apps for this mime by default precedence.
            for default in defaults
                .into_iter()
                .flatten()
                .chain(aliased_defaults.into_iter().flatten())
            {
                let default = default.strip_suffix(".desktop").unwrap_or(default.as_ref());
                let mut found_any = false;
                apps.retain(|app| {
                    let found = app.id.as_str() == default;
                    if found {
                        app.is_default
                            .write()
                            .unwrap()
                            .insert(mime.essence_str().into());
                        cache.push(app.clone());
                        found_any = true;
                    }

                    !found
                });

                if !found_any && let Some(app) = self.apps.iter().find(|app| app.id == default) {
                    app.is_default
                        .write()
                        .unwrap()
                        .insert(mime.essence_str().into());
                    cache.push(app.clone());
                }
            }

            // Sort remaining apps by name
            apps.sort_by(|a, b| LANGUAGE_SORTER.compare(&a.name, &b.name));
            cache.extend_from_slice(&apps);

            tracing::debug!(target: "mime-apps", mime = mime.essence_str(), apps = ?(cache.iter().map(|app| &*app.id).collect::<Vec<&str>>()), "mime defaults found")
        }

        let associated: rustc_hash::FxHashSet<&str> = self
            .cache
            .values()
            .flatten()
            .map(|app| app.id.as_str())
            .collect();
        for app in &self.apps {
            app.no_display.store(
                !associated.contains(app.id.as_str()),
                atomic::Ordering::Relaxed,
            );
        }

        let elapsed = start.elapsed();
        tracing::info!(target: "mime-apps", "loaded mime app cache in {elapsed:?}");
    }

    pub fn apps(&self) -> &[Arc<MimeApp>] {
        &self.apps
    }

    pub fn get(&self, key: &Mime) -> &[Arc<MimeApp>] {
        self.cache.get(key).map_or(&[], Vec::as_slice)
    }

    pub fn icons(&self, key: &Mime) -> Vec<widget::icon::Handle> {
        self.cache
            .get(key)
            .map_or_else(Vec::new, |apps| apps.iter().map(|app| app.icon()).collect())
    }

    /// How long `xdg-mime` may take to name the default terminal.
    ///
    /// It normally answers in milliseconds, but it is a shell script that
    /// consults other tools, and any of them can hang. This runs where a menu
    /// is being built, so waiting on it indefinitely means the menu never
    /// opens.
    const TERMINAL_QUERY_TIMEOUT: Duration = Duration::from_secs(2);

    /// Work out the default terminal now, so the first menu that asks for it
    /// does not have to wait for `xdg-mime`.
    ///
    /// Called on the worker that builds the cache. Without it that query --
    /// a whole process, with a two second deadline -- lands on whichever
    /// handler first asks, which is one the user is waiting on.
    pub fn prime_terminal(&self) {
        let _ = self.get_default_terminal();
    }

    fn get_default_terminal(&self) -> Option<&str> {
        self.default_terminal
            .get_or_init(|| {
                let mut child = process::Command::new("xdg-mime")
                    .args(["query", "default", "x-scheme-handler/terminal"])
                    .stdin(process::Stdio::null())
                    .stdout(process::Stdio::piped())
                    .stderr(process::Stdio::null())
                    .spawn()
                    .ok()?;

                // Safe to wait without draining: the answer is one desktop
                // file id, nowhere near a pipe buffer.
                match crate::child::wait_with_timeout(&mut child, Self::TERMINAL_QUERY_TIMEOUT) {
                    Ok(Some(status)) if status.success() => {}
                    Ok(Some(_)) => return None,
                    Ok(None) => {
                        log::warn!(
                            "killed `xdg-mime query default x-scheme-handler/terminal`: no answer in {:?}",
                            Self::TERMINAL_QUERY_TIMEOUT
                        );
                        return None;
                    }
                    Err(err) => {
                        log::warn!("failed to ask xdg-mime for the default terminal: {err}");
                        return None;
                    }
                }

                let mut output = String::new();
                child
                    .stdout
                    .as_mut()?
                    .read_to_string(&mut output)
                    .ok()?;
                Some(output.trim().replace(".desktop", ""))
            })
            .as_deref()
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

        if let Some(terminal) = self.get_default_terminal().and_then(by_id) {
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

    #[cfg(not(feature = "desktop"))]
    pub fn set_default(&mut self, mime: Mime, id: String) -> bool {
        log::warn!(
            "failed to set default handler for {mime:?} to {id:?}: desktop feature not enabled"
        );
        false
    }

    /// Records `id` as the default application for `mime`.
    ///
    /// Returns whether the associations changed, in which case the caller
    /// reloads the cache. Reloading here would mean rebuilding it inline, and
    /// it is built by walking every desktop entry on the system.
    #[cfg(feature = "desktop")]
    pub fn set_default(&mut self, mime: Mime, mut id: String) -> bool {
        let Some(path) = cosmic_mime_apps::local_list_path() else {
            log::warn!("failed to find mimeapps.list path");
            return false;
        };

        let mut list = cosmic_mime_apps::List::default();
        match fs::read_to_string(&path) {
            Ok(string) => {
                list.load_from(&string);
            }
            Err(err) => {
                if err.kind() != io::ErrorKind::NotFound {
                    log::warn!("failed to read {}: {}", path.display(), err);
                    return false;
                }
            }
        }

        let suffix = ".desktop";
        if !id.ends_with(suffix) {
            id.push_str(suffix);
        }
        list.set_default_app(mime, id);

        let mut string = list.to_string();
        string.push('\n');
        match fs::write(&path, string) {
            Ok(()) => true,
            Err(err) => {
                log::warn!("failed to write {}: {}", path.display(), err);
                false
            }
        }
    }
}

impl Default for MimeAppCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::exec_to_command;

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
