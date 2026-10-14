// SPDX-License-Identifier: GPL-3.0-only

//! Other programs' file actions, read from Dolphin's service menus.
//!
//! A service menu is a `.desktop` file a program installs so file managers
//! offer its actions on files: PeaZip's "Extract here", for example. They
//! are read and matched as KIO's `KFileItemActions` does, so a program
//! shows the same actions here as in Dolphin.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use xdg_kit::desktop_entry::{DesktopEntry, Locales};

use crate::mime_app::{self, Mime};
use crate::spawn_detached::spawn_detached;

/// The id that stands for a separator in a service menu's `Actions`.
const SEPARATOR: &str = "_SEPARATOR_";

/// The type a legacy file in `kservices5` names to be a service menu.
const LEGACY_TYPE: &str = "KonqPopupMenu/Plugin";

/// One action a service menu offers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServiceAction {
    pub name: String,
    pub icon: Option<String>,
    pub exec: String,
    /// The service menu file, for the `%k` field code.
    pub file: PathBuf,
}

/// A row of a program's submenu.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ServiceItem {
    Action(ServiceAction),
    Separator,
}

/// What the menu shows for other programs.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ServiceEntry {
    /// The actions sharing an `X-KDE-Submenu`, under that name.
    Program {
        name: String,
        icon: Option<String>,
        items: Vec<ServiceItem>,
    },
    /// An action of a file without a submenu.
    Action(ServiceAction),
}

impl ServiceAction {
    /// The commands that run the action on `paths`.
    fn commands(&self, paths: &[PathBuf]) -> Option<Vec<std::process::Command>> {
        mime_app::exec_to_command(&self.exec, &self.name, Some(&self.file), paths)
    }

    /// Runs the action on `paths`.
    pub fn run(&self, paths: &[PathBuf]) {
        let Some(commands) = self.commands(paths) else {
            log::warn!(
                "service menu action {:?}: invalid Exec {:?}",
                self.name,
                self.exec
            );
            return;
        };
        for mut command in commands {
            if let Err(err) = spawn_detached(&mut command) {
                log::warn!("failed to run service menu action {:?}: {err}", self.name);
                return;
            }
        }
    }
}

impl ServiceEntry {
    fn name(&self) -> &str {
        match self {
            Self::Program { name, .. } => name,
            Self::Action(action) => &action.name,
        }
    }
}

/// A selected item, as a service menu's types are matched against it.
#[derive(Clone, Copy, Debug)]
pub struct Target<'a> {
    pub mime: &'a Mime,
    pub is_dir: bool,
}

/// Which locations a service menu is for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Protocols {
    /// Any but Trash: nothing said.
    Unsaid,
    /// `X-KDE-Protocol`: only this one.
    Only(String),
    /// `X-KDE-Protocol=!…`: any but this one.
    Not(String),
    /// `X-KDE-Protocols`: only these.
    Any(Vec<String>),
}

/// One service menu file, read.
#[derive(Clone, Debug)]
struct ServiceFile {
    protocols: Protocols,
    required: Vec<usize>,
    min: Option<usize>,
    max: Option<usize>,
    types: Vec<String>,
    exclude: Vec<String>,
    submenu: Option<String>,
    items: Vec<ServiceItem>,
}

/// Every service menu installed.
#[derive(Clone, Debug, Default)]
pub struct ServiceMenus {
    files: Vec<ServiceFile>,
}

/// The data directories in the order they are searched: `XDG_DATA_HOME`
/// first, then each of `XDG_DATA_DIRS`.
pub fn data_dirs(base: &xdg_kit::basedir::BaseDirs) -> Vec<PathBuf> {
    std::iter::once(base.data_home().to_path_buf())
        .chain(base.data_dirs().iter().cloned())
        .collect()
}

/// The folders service menus are read from, under each of `dirs`.
fn menu_folders(dirs: &[PathBuf]) -> Vec<PathBuf> {
    dirs.iter()
        .flat_map(|dir| [dir.join("kio/servicemenus"), dir.join("kservices5")])
        .collect()
}

/// The folders to watch for service menus coming and going under `dirs`:
/// each menu folder that exists, and for one that does not, the nearest
/// folder above it that does, so that its making is seen. Which these are
/// changes as folders are made, so the watch is started again when it does.
pub fn watched_folders(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = Vec::new();
    for folder in menu_folders(dirs) {
        if let Some(existing) = folder.ancestors().find(|a| a.is_dir())
            && !folders.iter().any(|f| f == existing)
        {
            folders.push(existing.to_path_buf());
        }
    }
    folders
}

/// Whether a change at `path` concerns service menus under `dirs`: a menu
/// file, or one of the menu folders or a folder on the way to one.
#[cfg(any(feature = "desktop", test))]
fn concerns_menus(path: &Path, dirs: &[PathBuf]) -> bool {
    path.extension().is_some_and(|ext| ext == "desktop")
        || menu_folders(dirs)
            .iter()
            .any(|folder| folder.starts_with(path))
}

/// Calls `emitter` whenever a service menu under `dirs` is added, changed
/// or removed, or a menu folder is made, watching `folders` (see
/// [`watched_folders`]).
#[cfg(feature = "desktop")]
pub async fn watch(
    dirs: Vec<PathBuf>,
    folders: Vec<PathBuf>,
    mut emitter: impl FnMut() + 'static + Send,
) {
    use notify_debouncer_full::notify;
    let watcher_result = notify_debouncer_full::new_debouncer(
        std::time::Duration::from_millis(250),
        Some(std::time::Duration::from_millis(250)),
        move |event_res: notify_debouncer_full::DebounceEventResult| {
            let Ok(events) = event_res else {
                return;
            };
            if events.iter().any(|event| {
                (event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove())
                    && event.paths.iter().any(|path| concerns_menus(path, &dirs))
            }) {
                emitter();
            }
        },
    );
    if let Ok(mut watcher) = watcher_result {
        for folder in folders {
            _ = watcher.watch(&folder, notify::RecursiveMode::NonRecursive);
        }
        std::future::pending().await
    }
}

/// The `*.desktop` files directly in `dir`, by name, so the order does not
/// depend on the file system's.
fn desktop_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| !kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "desktop"))
        .collect();
    files.sort();
    files
}

/// The files in `dirs`' `sub` folders, a file name seen in an earlier one
/// hiding later files of that name.
fn unique_files(dirs: &[PathBuf], sub: &str) -> Vec<PathBuf> {
    let mut seen = HashSet::<OsString>::new();
    dirs.iter()
        .flat_map(|dir| desktop_files(&dir.join(sub)))
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| seen.insert(name.to_owned()))
        })
        .collect()
}

/// A KDE list: comma-separated, as KConfig reads a list.
fn kde_list(entry: &DesktopEntry, key: &str) -> Vec<String> {
    entry
        .string(key)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The service menu files, in the order KIO reads them: `kio/servicemenus`
/// in each data directory, then the legacy `kservices5` files that name
/// [`LEGACY_TYPE`]. A file name seen earlier hides later ones.
fn discover(dirs: &[PathBuf]) -> Vec<(PathBuf, DesktopEntry)> {
    let current = unique_files(dirs, "kio/servicemenus")
        .into_iter()
        .filter_map(|path| Some((DesktopEntry::read(&path).ok()?, path)));
    let legacy = unique_files(dirs, "kservices5")
        .into_iter()
        .filter_map(|path| Some((DesktopEntry::read(&path).ok()?, path)))
        .filter(|(entry, _)| {
            kde_list(entry, "ServiceTypes")
                .iter()
                .any(|t| t == LEGACY_TYPE)
        });
    let mut seen = HashSet::<OsString>::new();
    current
        .chain(legacy)
        .filter(|(_, path)| {
            path.file_name()
                .is_some_and(|name| seen.insert(name.to_owned()))
        })
        .map(|(entry, path)| (path, entry))
        .collect()
}

impl ServiceFile {
    /// `None` for a file that cannot be used: one asking for a KIOSK
    /// authorization, which is not supported.
    fn read(path: &Path, entry: &DesktopEntry, locales: &Locales) -> Option<Self> {
        if !kde_list(entry, "X-KDE-AuthorizeAction").is_empty() {
            return None;
        }
        let protocols = if let Some(protocol) = entry.string("X-KDE-Protocol") {
            match protocol.strip_prefix('!') {
                Some(not) => Protocols::Not(not.to_owned()),
                None => Protocols::Only(protocol),
            }
        } else if entry.string("X-KDE-Protocols").is_some() {
            Protocols::Any(kde_list(entry, "X-KDE-Protocols"))
        } else {
            Protocols::Unsaid
        };
        let number = |key| {
            entry
                .string(key)
                .and_then(|value| value.trim().parse().ok())
        };
        let mut types = entry.mime_types();
        if types.is_empty() {
            types = kde_list(entry, "ServiceTypes");
            types.retain(|t| t != LEGACY_TYPE);
        }
        let items = entry
            .list("Actions")
            .unwrap_or_default()
            .iter()
            .filter_map(|id| {
                if id == SEPARATOR {
                    return Some(ServiceItem::Separator);
                }
                let action = entry.action(id)?;
                Some(ServiceItem::Action(ServiceAction {
                    name: action.name(locales)?,
                    icon: action.icon(locales),
                    exec: action.exec()?,
                    file: path.to_path_buf(),
                }))
            })
            .collect();
        Some(Self {
            protocols,
            required: kde_list(entry, "X-KDE-RequiredNumberOfUrls")
                .iter()
                .filter_map(|n| n.parse().ok())
                .collect(),
            min: number("X-KDE-MinNumberOfUrls"),
            max: number("X-KDE-MaxNumberOfUrls"),
            types,
            exclude: kde_list(entry, "ExcludeServiceTypes"),
            submenu: entry
                .key_file()
                .locale_string(entry.main_group(), "X-KDE-Submenu", locales)
                .filter(|name| !name.is_empty()),
            items,
        })
    }

    /// Whether the file is for a location of `scheme` and `count` items.
    fn shown_for(&self, scheme: &str, count: usize) -> bool {
        let protocol = match &self.protocols {
            Protocols::Only(only) => only == scheme,
            Protocols::Not(not) => not != scheme,
            Protocols::Any(list) => list.iter().any(|p| p == scheme),
            // Trashed files are not for other programs' actions, unless a
            // file asks for Trash
            Protocols::Unsaid => scheme != "trash",
        };
        protocol
            && (self.required.is_empty() || self.required.contains(&count))
            && self.min.is_none_or(|min| count >= min)
            && self.max.is_none_or(|max| count <= max)
    }

    /// Whether every target is of the file's types and of none it excludes.
    fn types_match(&self, targets: &[Target], is_a: &impl Fn(&Mime, &Mime) -> bool) -> bool {
        !self.types.is_empty()
            && targets.iter().all(|target| {
                list_contains(&self.types, *target, is_a)
                    && !list_contains(&self.exclude, *target, is_a)
            })
    }
}

/// Whether `target` is of one of `types`, as KIO matches them.
fn list_contains(types: &[String], target: Target, is_a: &impl Fn(&Mime, &Mime) -> bool) -> bool {
    let mime = target.mime.as_str();
    types.iter().any(|t| {
        t == mime
            || t == "all/all"
            || (!target.is_dir
                && matches!(
                    t.as_str(),
                    "allfiles" | "all/allfiles" | "application/octet-stream"
                ))
            || Mime::new(t).is_ok_and(|base| is_a(target.mime, &base))
            || t.strip_suffix("/*")
                .is_some_and(|group| mime.starts_with(group))
    })
}

impl ServiceMenus {
    /// Every service menu under `dirs` (see [`data_dirs`]), named in
    /// `locales`.
    pub fn load(dirs: &[PathBuf], locales: &Locales) -> Self {
        Self {
            files: discover(dirs)
                .iter()
                .filter_map(|(path, entry)| ServiceFile::read(path, entry, locales))
                .collect(),
        }
    }

    /// What to show for `count` selected items of the kinds `targets` (each
    /// kind once is enough) in a location of `scheme` (`file` for a local
    /// folder, `trash` in Trash). Actions sharing a submenu name are
    /// gathered into one program, files in the order read and actions in the
    /// order each file lists them; programs and lone actions are ordered by
    /// name. `is_a` says whether a type is another or a kind of it.
    pub fn entries(
        &self,
        scheme: &str,
        count: usize,
        targets: &[Target],
        is_a: impl Fn(&Mime, &Mime) -> bool,
    ) -> Vec<ServiceEntry> {
        if targets.is_empty() || count == 0 {
            return Vec::new();
        }
        let mut entries: Vec<ServiceEntry> = Vec::new();
        for file in &self.files {
            if !file.shown_for(scheme, count) || !file.types_match(targets, &is_a) {
                continue;
            }
            match &file.submenu {
                Some(submenu) => {
                    let found = entries.iter_mut().find_map(|entry| match entry {
                        ServiceEntry::Program { name, items, .. } if name == submenu => Some(items),
                        _ => None,
                    });
                    match found {
                        Some(items) => items.extend(file.items.iter().cloned()),
                        None => entries.push(ServiceEntry::Program {
                            name: submenu.clone(),
                            icon: None,
                            items: file.items.clone(),
                        }),
                    }
                }
                None => entries.extend(file.items.iter().filter_map(|item| match item {
                    ServiceItem::Action(action) => Some(ServiceEntry::Action(action.clone())),
                    ServiceItem::Separator => None,
                })),
            }
        }
        entries.retain_mut(|entry| match entry {
            // A program takes its first action's icon; one with no action
            // left is not shown
            ServiceEntry::Program { icon, items, .. } => {
                tidy_separators(items);
                let first = items.iter().find_map(|item| match item {
                    ServiceItem::Action(action) => Some(action),
                    ServiceItem::Separator => None,
                });
                first.map(|action| *icon = action.icon.clone()).is_some()
            }
            ServiceEntry::Action(_) => true,
        });
        entries.sort_by_cached_key(|entry| entry.name().to_lowercase());
        entries
    }
}

/// No separator first, last, or after another.
fn tidy_separators(items: &mut Vec<ServiceItem>) {
    let mut last_was_separator = true;
    items.retain(|item| {
        let separator = *item == ServiceItem::Separator;
        let keep = !(separator && last_was_separator);
        last_was_separator = separator;
        keep
    });
    if items.last() == Some(&ServiceItem::Separator) {
        items.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data directory holding `files`, each `(path under it, contents)`.
    fn data_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (path, contents) in files {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(path, contents).expect("write");
        }
        dir
    }

    /// The types these tests know: a ZIP file is an archive is data.
    fn is_a(a: &Mime, b: &Mime) -> bool {
        let chain: &[&str] = match a.as_str() {
            "application/zip" => &["application/zip", "application/octet-stream"],
            "text/markdown" => &["text/markdown", "text/plain", "application/octet-stream"],
            "image/png" => &["image/png", "application/octet-stream"],
            other => return other == b.as_str(),
        };
        chain.contains(&b.as_str())
    }

    fn mime(text: &str) -> Mime {
        Mime::new(text).expect("type")
    }

    /// PeaZip's KDE 6 service menu, cut to three actions.
    const PEAZIP: &str = "[Desktop Entry]
Type=Service
MimeType=application/octet-stream;inode/directory
Actions=peazipadd;peazipexthere;peazipopen;
X-KDE-Submenu=PeaZip
X-KDE-Priority=TopLevel

[Desktop Action peazipadd]
Name=Archive...
Icon=peazip
Exec=peazip -add2archive %F

[Desktop Action peazipexthere]
Name=Extract here
Icon=peazip_extract
Exec=peazip -ext2here %F

[Desktop Action peazipopen]
Name=Open with PeaZip...
Icon=peazip
Exec=peazip -ext2browse %F
";

    fn one_action(name: &str, extra: &str) -> String {
        format!(
            "[Desktop Entry]\nType=Service\nActions=go;\n{extra}\n\n[Desktop Action go]\nName={name}\nExec=run %F\n"
        )
    }

    fn names(entries: &[ServiceEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                ServiceEntry::Program { name, items, .. } => format!(
                    "{name}: {}",
                    items
                        .iter()
                        .map(|item| match item {
                            ServiceItem::Action(action) => action.name.as_str(),
                            ServiceItem::Separator => "--",
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                ServiceEntry::Action(action) => action.name.clone(),
            })
            .collect()
    }

    fn entries_for(dirs: &[PathBuf], scheme: &str, targets: &[Target]) -> Vec<String> {
        names(&ServiceMenus::load(dirs, &Locales::untranslated()).entries(
            scheme,
            targets.len(),
            targets,
            is_a,
        ))
    }

    #[test]
    fn a_program_with_several_actions_is_one_row_in_its_own_order() {
        let dir = data_dir(&[("kio/servicemenus/peazip-kde6.desktop", PEAZIP)]);
        let zip = mime("application/zip");
        let menus = ServiceMenus::load(&[dir.path().to_path_buf()], &Locales::untranslated());
        let entries = menus.entries(
            "file",
            1,
            &[Target {
                mime: &zip,
                is_dir: false,
            }],
            is_a,
        );
        assert_eq!(
            names(&entries),
            ["PeaZip: Archive..., Extract here, Open with PeaZip..."]
        );
        let ServiceEntry::Program { icon, items, .. } = &entries[0] else {
            panic!("not a program");
        };
        assert_eq!(icon.as_deref(), Some("peazip"), "the first action's icon");
        let ServiceItem::Action(action) = &items[1] else {
            panic!("not an action");
        };
        assert_eq!(action.exec, "peazip -ext2here %F");
        assert!(action.file.ends_with("peazip-kde6.desktop"));
    }

    /// `application/octet-stream` is every file, as declared: a Markdown
    /// file gets PeaZip's extract action too, as in Dolphin.
    #[test]
    fn data_types_match_every_file_and_folders_match_only_folders() {
        let dir = data_dir(&[("kio/servicemenus/peazip-kde6.desktop", PEAZIP)]);
        let dirs = [dir.path().to_path_buf()];
        let md = mime("text/markdown");
        let folder = mime("inode/directory");
        assert_eq!(
            entries_for(
                &dirs,
                "file",
                &[Target {
                    mime: &md,
                    is_dir: false
                }]
            )
            .len(),
            1
        );
        assert_eq!(
            entries_for(
                &dirs,
                "file",
                &[Target {
                    mime: &folder,
                    is_dir: true
                }]
            )
            .len(),
            1
        );

        let only_files = data_dir(&[(
            "kio/servicemenus/f.desktop",
            &one_action("Files only", "MimeType=application/octet-stream;"),
        )]);
        let dirs = [only_files.path().to_path_buf()];
        assert!(
            entries_for(
                &dirs,
                "file",
                &[Target {
                    mime: &folder,
                    is_dir: true
                }]
            )
            .is_empty()
        );
    }

    #[test]
    fn special_and_wildcard_types_match_as_kio_matches_them() {
        let png = mime("image/png");
        let folder = mime("inode/directory");
        let file = Target {
            mime: &png,
            is_dir: false,
        };
        let dir_target = Target {
            mime: &folder,
            is_dir: true,
        };
        let shown = |types: &str, target: Target| {
            let dir = data_dir(&[(
                "kio/servicemenus/a.desktop",
                &one_action("Act", &format!("MimeType={types}")),
            )]);
            !entries_for(&[dir.path().to_path_buf()], "file", &[target]).is_empty()
        };
        assert!(shown("all/all;", file));
        assert!(shown("all/all;", dir_target));
        assert!(shown("all/allfiles;", file));
        assert!(!shown("all/allfiles;", dir_target));
        assert!(shown("allfiles;", file));
        assert!(shown("image/*;", file));
        assert!(!shown("text/*;", file));
        assert!(!shown("application/zip;", file));
    }

    #[test]
    fn every_selected_item_must_match_and_none_be_excluded() {
        let dir = data_dir(&[(
            "kio/servicemenus/a.desktop",
            &one_action(
                "Act",
                "MimeType=application/octet-stream;\nExcludeServiceTypes=image/png",
            ),
        )]);
        let dirs = [dir.path().to_path_buf()];
        let zip = mime("application/zip");
        let png = mime("image/png");
        let zip_t = Target {
            mime: &zip,
            is_dir: false,
        };
        let png_t = Target {
            mime: &png,
            is_dir: false,
        };
        assert_eq!(entries_for(&dirs, "file", &[zip_t]), ["Act"]);
        assert!(entries_for(&dirs, "file", &[zip_t, png_t]).is_empty());
    }

    #[test]
    fn the_number_of_selected_items_is_checked() {
        let zip = mime("application/zip");
        let t = Target {
            mime: &zip,
            is_dir: false,
        };
        let shown = |extra: &str, count: usize| {
            let dir = data_dir(&[(
                "kio/servicemenus/a.desktop",
                &one_action("Act", &format!("MimeType=all/all;\n{extra}")),
            )]);
            !entries_for(&[dir.path().to_path_buf()], "file", &vec![t; count]).is_empty()
        };
        assert!(shown("X-KDE-RequiredNumberOfUrls=1,3", 3));
        assert!(!shown("X-KDE-RequiredNumberOfUrls=1,3", 2));
        assert!(shown("X-KDE-MinNumberOfUrls=2", 2));
        assert!(!shown("X-KDE-MinNumberOfUrls=2", 1));
        assert!(shown("X-KDE-MaxNumberOfUrls=2", 2));
        assert!(!shown("X-KDE-MaxNumberOfUrls=2", 3));
    }

    #[test]
    fn protocols_are_checked_and_trash_needs_asking_for() {
        let zip = mime("application/zip");
        let t = Target {
            mime: &zip,
            is_dir: false,
        };
        let shown = |extra: &str, scheme: &str| {
            let dir = data_dir(&[(
                "kio/servicemenus/a.desktop",
                &one_action("Act", &format!("MimeType=all/all;\n{extra}")),
            )]);
            !entries_for(&[dir.path().to_path_buf()], scheme, &[t]).is_empty()
        };
        assert!(shown("", "file"));
        assert!(!shown("", "trash"));
        assert!(shown("X-KDE-Protocol=trash", "trash"));
        assert!(!shown("X-KDE-Protocol=trash", "file"));
        assert!(!shown("X-KDE-Protocol=!sftp", "sftp"));
        assert!(shown("X-KDE-Protocol=!sftp", "file"));
        assert!(shown("X-KDE-Protocols=file,trash", "trash"));
        assert!(!shown("X-KDE-Protocols=file,smb", "sftp"));
    }

    #[test]
    fn a_file_asking_for_an_authorization_is_skipped() {
        let dir = data_dir(&[(
            "kio/servicemenus/a.desktop",
            &one_action(
                "Act",
                "MimeType=all/all;\nX-KDE-AuthorizeAction=shell_access",
            ),
        )]);
        let zip = mime("application/zip");
        assert!(
            entries_for(
                &[dir.path().to_path_buf()],
                "file",
                &[Target {
                    mime: &zip,
                    is_dir: false
                }]
            )
            .is_empty()
        );
    }

    /// Files are found in each data directory in turn; the user's file of a
    /// name hides the system's. Legacy `kservices5` files count only when
    /// they name the plugin type, and only directly in that folder.
    #[test]
    fn files_are_found_as_kio_finds_them() {
        let user = data_dir(&[
            (
                "kio/servicemenus/same.desktop",
                &one_action("User's", "MimeType=all/all;"),
            ),
            (
                "kservices5/old.desktop",
                &one_action("Legacy", "ServiceTypes=KonqPopupMenu/Plugin,all/all"),
            ),
            (
                "kservices5/notmenu.desktop",
                &one_action("Not a menu", "ServiceTypes=Something"),
            ),
            (
                "kservices5/ServiceMenus/deep.desktop",
                &one_action("Too deep", "ServiceTypes=KonqPopupMenu/Plugin,all/all"),
            ),
        ]);
        let system = data_dir(&[
            (
                "kio/servicemenus/same.desktop",
                &one_action("System's", "MimeType=all/all;"),
            ),
            (
                "kio/servicemenus/other.desktop",
                &one_action("Other", "MimeType=all/all;"),
            ),
        ]);
        let dirs = [user.path().to_path_buf(), system.path().to_path_buf()];
        let zip = mime("application/zip");
        assert_eq!(
            entries_for(
                &dirs,
                "file",
                &[Target {
                    mime: &zip,
                    is_dir: false
                }]
            ),
            ["Legacy", "Other", "User's"]
        );
    }

    /// Files naming the same submenu are one program, in the order the files
    /// are read (by name: `-minimal` before `.desktop`); actions repeated
    /// across files are shown each time, as in Dolphin. `_SEPARATOR_`
    /// divides a program's actions, repeats of it count once.
    #[test]
    fn files_sharing_a_submenu_are_one_program_repeats_kept() {
        let minimal = "[Desktop Entry]
Type=Service
MimeType=all/all;
Actions=peazipexthere;_SEPARATOR_;_SEPARATOR_;
X-KDE-Submenu=PeaZip

[Desktop Action peazipexthere]
Name=Extract here
Exec=peazip -ext2here %F
";
        let dir = data_dir(&[
            ("kio/servicemenus/peazip-kde6.desktop", PEAZIP),
            ("kio/servicemenus/peazip-kde6-minimal.desktop", minimal),
            (
                "kio/servicemenus/zzz.desktop",
                &one_action("Alone", "MimeType=all/all;"),
            ),
        ]);
        let zip = mime("application/zip");
        assert_eq!(
            entries_for(
                &[dir.path().to_path_buf()],
                "file",
                &[Target {
                    mime: &zip,
                    is_dir: false
                }]
            ),
            [
                "Alone",
                "PeaZip: Extract here, --, Archive..., Extract here, Open with PeaZip...",
            ]
        );
    }

    #[test]
    fn an_action_runs_its_exec_on_the_selected_paths() {
        let action = ServiceAction {
            name: "Extract here".into(),
            icon: None,
            exec: "peazip -ext2here %F".into(),
            file: PathBuf::from("/menus/peazip.desktop"),
        };
        let commands = action
            .commands(&[PathBuf::from("/a.zip"), PathBuf::from("/b b.zip")])
            .expect("commands");
        assert_eq!(commands.len(), 1, "%F takes every path at once");
        let command = &commands[0];
        assert_eq!(command.get_program(), "peazip");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["-ext2here", "/a.zip", "/b b.zip"]);
    }

    #[test]
    fn menus_are_read_from_two_folders_in_each_data_directory() {
        assert_eq!(
            menu_folders(&[
                PathBuf::from("/home/u/.local/share"),
                PathBuf::from("/usr/share")
            ]),
            [
                PathBuf::from("/home/u/.local/share/kio/servicemenus"),
                PathBuf::from("/home/u/.local/share/kservices5"),
                PathBuf::from("/usr/share/kio/servicemenus"),
                PathBuf::from("/usr/share/kservices5"),
            ]
        );
    }

    #[test]
    fn a_missing_menu_folder_is_watched_through_the_folder_above_it() {
        let dir = data_dir(&[("kio/servicemenus/a.desktop", "")]);
        let data = dir.path().to_path_buf();
        assert_eq!(
            watched_folders(std::slice::from_ref(&data)),
            [data.join("kio/servicemenus"), data.clone()],
            "kservices5 is missing, so the data folder itself is watched"
        );
        let dirs = std::slice::from_ref(&data);
        assert!(concerns_menus(&data.join("kservices5"), dirs));
        assert!(concerns_menus(&data.join("kio"), dirs));
        assert!(concerns_menus(
            &data.join("kio/servicemenus/b.desktop"),
            dirs
        ));
        assert!(!concerns_menus(&data.join("fonts"), dirs));
    }

    #[test]
    fn nothing_selected_has_no_actions() {
        let dir = data_dir(&[("kio/servicemenus/peazip-kde6.desktop", PEAZIP)]);
        assert!(entries_for(&[dir.path().to_path_buf()], "file", &[]).is_empty());
    }
}
