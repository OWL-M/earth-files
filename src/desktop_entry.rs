// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Desktop entry actions.

use std::path::PathBuf;

use xdg_kit::desktop_entry::{DesktopEntry, Locales};

/// One `[Desktop Action ...]` group.
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopAction {
    /// What the entry's `Actions` key calls it.
    pub id: String,
    pub name: String,
}

/// The parts of a desktop entry this app acts on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesktopEntryData {
    /// The file, read again to run an action.
    pub path: PathBuf,
    pub name: String,
    pub desktop_actions: Vec<DesktopAction>,
}

/// Read a `.desktop` file, keeping only the fields this app uses. An action
/// without a name or a command line is left out.
pub fn load_desktop_file(locales: &[String], path: PathBuf) -> Option<DesktopEntryData> {
    let entry = DesktopEntry::read(&path).ok()?;
    let locales = Locales::new(locales);

    let name = entry.name(&locales).unwrap_or_else(|| {
        // The desktop file ID's last part, as the entry is known when unnamed
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();
        file_name
            .strip_suffix(".desktop")
            .unwrap_or(&file_name)
            .to_owned()
    });

    let desktop_actions = entry
        .actions()
        .into_iter()
        .filter(|action| action.exec().is_some())
        .filter_map(|action| {
            Some(DesktopAction {
                name: action.name(&locales)?,
                id: action.id().to_owned(),
            })
        })
        .collect();

    Some(DesktopEntryData {
        path,
        name,
        desktop_actions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_desktop_file(dir: &std::path::Path, filename: &str, contents: &str) -> PathBuf {
        let path = dir.join(filename);
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn name_falls_back_to_appid_when_unnamed() {
        let dir = tempdir().unwrap();
        let path = write_desktop_file(
            dir.path(),
            "org.example.Foo.desktop",
            "[Desktop Entry]\nType=Application\n",
        );

        let data = load_desktop_file(&[], path).expect("entry should parse");
        assert_eq!(data.name, "org.example.Foo");
    }

    #[test]
    fn action_without_name_is_skipped() {
        let dir = tempdir().unwrap();
        let path = write_desktop_file(
            dir.path(),
            "no-name-action.desktop",
            "[Desktop Entry]\nType=Application\nName=Example\nExec=example\nActions=Foo;\n\n[Desktop Action Foo]\nExec=example --thing\n",
        );

        let data = load_desktop_file(&[], path).expect("entry should parse");
        assert_eq!(data.name, "Example");
        assert!(data.desktop_actions.is_empty());
    }

    #[test]
    fn action_without_exec_is_skipped() {
        let dir = tempdir().unwrap();
        let path = write_desktop_file(
            dir.path(),
            "no-exec-action.desktop",
            "[Desktop Entry]\nType=Application\nName=Example\nExec=example\nActions=Foo;\n\n[Desktop Action Foo]\nName=Do The Thing\n",
        );

        let data = load_desktop_file(&[], path).expect("entry should parse");
        assert_eq!(data.name, "Example");
        assert!(data.desktop_actions.is_empty());
    }

    #[test]
    fn action_with_name_and_exec_is_kept() {
        let dir = tempdir().unwrap();
        let path = write_desktop_file(
            dir.path(),
            "full-action.desktop",
            "[Desktop Entry]\nType=Application\nName=Example\nExec=example\nActions=Foo;\n\n[Desktop Action Foo]\nName=Do The Thing\nExec=example --thing\n",
        );

        let data = load_desktop_file(&[], path).expect("entry should parse");
        assert_eq!(data.desktop_actions.len(), 1);
        assert_eq!(
            data.desktop_actions[0],
            DesktopAction {
                id: "Foo".to_string(),
                name: "Do The Thing".to_string(),
            }
        );
    }

    #[test]
    fn actions_preserve_file_order() {
        let dir = tempdir().unwrap();
        let path = write_desktop_file(
            dir.path(),
            "ordered-actions.desktop",
            "[Desktop Entry]\nType=Application\nName=Example\nExec=example\nActions=First;Second;\n\n[Desktop Action First]\nName=First Action\nExec=example --first\n\n[Desktop Action Second]\nName=Second Action\nExec=example --second\n",
        );

        let data = load_desktop_file(&[], path).expect("entry should parse");
        assert_eq!(
            data.desktop_actions,
            vec![
                DesktopAction {
                    id: "First".to_string(),
                    name: "First Action".to_string(),
                },
                DesktopAction {
                    id: "Second".to_string(),
                    name: "Second Action".to_string(),
                },
            ]
        );
    }

    #[test]
    fn unparsable_file_yields_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.desktop");

        assert!(load_desktop_file(&[], path).is_none());
    }
}
