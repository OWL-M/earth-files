// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! On-disk configuration, replacing `cosmic_config`.
//!
//! Values live in RON under `$XDG_CONFIG_HOME/cosmic-files/`. A missing or
//! unreadable file yields `Default` rather than an error: a corrupt config must
//! never stop the app from starting.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// A single configuration file.
#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// A store backed by an exact path. Used by tests.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// A store for `name` under the user's config directory.
    pub fn named(name: &str) -> Self {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| crate::home_dir().join(".config"))
            .join("cosmic-files");
        Self::at(dir.join(format!("{name}.ron")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the stored value, falling back to `Default` when the file is
    /// absent, unreadable or malformed.
    pub fn load<T: DeserializeOwned + Default>(&self) -> T {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) => {
                if err.kind() != io::ErrorKind::NotFound {
                    log::warn!("failed to read {}: {err}", self.path.display());
                }
                return T::default();
            }
        };

        match ron::from_str(&text) {
            Ok(value) => value,
            Err(err) => {
                log::warn!("failed to parse {}: {err}", self.path.display());
                T::default()
            }
        }
    }

    /// Write the value, creating the parent directory as needed.
    pub fn save<T: Serialize>(&self, value: &T) -> io::Result<()> {
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        if let Some(parent) = parent {
            fs::create_dir_all(parent)?;
        }
        let text = ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::default())
            .map_err(io::Error::other)?;

        // Write to a temporary file in the same directory and rename over the
        // target, so a reader never observes a half-written file. Writing in
        // place would truncate first, and this app watches its own config
        // directory: another instance could read the gap.
        let mut file = tempfile::NamedTempFile::new_in(parent.unwrap_or(Path::new(".")))?;
        io::Write::write_all(&mut file, text.as_bytes())?;
        file.persist(&self.path).map_err(|err| err.error)?;
        Ok(())
    }
}

use std::any::TypeId;
use std::time::Duration;

use crate::ui::iced::Subscription;
use crate::ui::iced::futures::SinkExt;
use crate::ui::iced::futures::channel::mpsc::Sender;
use notify_debouncer_full::{new_debouncer, notify};

impl Store {
    /// Emit the reloaded value whenever the backing file changes.
    ///
    /// `Subscription::run_with` takes a plain function pointer, so the store
    /// path travels as the subscription's identifying data rather than as a
    /// captured variable.
    pub fn subscription<T>(&self) -> Subscription<T>
    where
        T: DeserializeOwned + Default + Send + 'static,
    {
        Subscription::run_with(
            (TypeId::of::<T>(), self.path.clone()),
            |(_, path): &(TypeId, PathBuf)| {
                let store = Store::at(path.clone());
                crate::ui::iced::stream::channel(1, move |mut output: Sender<T>| async move {
                    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

                    let watcher_res = new_debouncer(
                        Duration::from_millis(250),
                        None,
                        move |result: notify_debouncer_full::DebounceEventResult| {
                            if result.is_ok() {
                                let _ = tx.blocking_send(());
                            }
                        },
                    );

                    let Ok(mut debouncer) = watcher_res else {
                        log::warn!("failed to create config watcher");
                        std::future::pending::<()>().await;
                        unreachable!();
                    };

                    // Watch the directory, not the file: saves replace the file
                    // rather than writing in place, which drops a file-level watch.
                    let dir = store
                        .path()
                        .parent()
                        .map_or_else(|| store.path().to_path_buf(), Path::to_path_buf);
                    if let Err(err) = fs::create_dir_all(&dir) {
                        log::warn!("failed to create {}: {err}", dir.display());
                    }
                    if let Err(err) = debouncer.watch(&dir, notify::RecursiveMode::NonRecursive) {
                        log::warn!("failed to watch {}: {err:?}", dir.display());
                    }

                    while rx.recv().await.is_some() {
                        if output.send(store.load::<T>()).await.is_err() {
                            break;
                        }
                    }

                    drop(debouncer);
                })
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Default, Deserialize, PartialEq, Serialize)]
    struct Sample {
        value: u32,
        name: String,
    }

    #[test]
    fn missing_file_yields_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("sample.ron"));
        assert_eq!(store.load::<Sample>(), Sample::default());
    }

    #[test]
    fn saved_value_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("sample.ron"));
        let sample = Sample { value: 7, name: "seven".to_string() };
        store.save(&sample).expect("save");
        assert_eq!(store.load::<Sample>(), sample);
    }

    #[test]
    fn corrupt_file_yields_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.ron");
        std::fs::write(&path, b"this is not ron").expect("write");
        assert_eq!(Store::at(path).load::<Sample>(), Sample::default());
    }

    #[test]
    fn save_replaces_atomically_leaving_no_partial_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("sample.ron"));
        store.save(&Sample { value: 1, name: "one".to_string() }).expect("first save");
        store.save(&Sample { value: 2, name: "two".to_string() }).expect("second save");

        // The rename must leave exactly the target file behind, with no
        // leftover temporary alongside it.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("sample.ron")]);
        assert_eq!(store.load::<Sample>(), Sample { value: 2, name: "two".to_string() });
    }

    #[test]
    fn save_creates_missing_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("nested").join("sample.ron"));
        store.save(&Sample::default()).expect("save");
        assert!(store.path().exists());
    }

    #[test]
    fn real_config_round_trips() {
        use crate::config::{Config, ContextActionPreset, ContextActionSelection, Favorite};
        use std::path::PathBuf;

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("config.ron"));

        let mut config = Config::default();
        config.favorites = vec![
            Favorite::Home,
            Favorite::Path(PathBuf::from("/some/dir")),
            Favorite::Network {
                uri: "sftp://example.com/".to_string(),
                name: "example.com".to_string(),
                path: PathBuf::from("/run/mount/example"),
            },
            Favorite::Named {
                path: PathBuf::from("/some/other"),
                name: "Custom".to_string(),
            },
        ];
        config.context_actions = vec![
            ContextActionPreset {
                name: "Open in terminal".to_string(),
                confirm: false,
                selection: ContextActionSelection::Folders,
                steps: vec!["kgx --path %f".to_string()],
            },
            ContextActionPreset {
                name: "Checksum".to_string(),
                confirm: true,
                selection: ContextActionSelection::Files,
                steps: vec!["sha256sum %F".to_string(), "echo done".to_string()],
            },
        ];
        config.show_details = true;
        config.show_recents = false;

        store.save(&config).expect("save");
        assert_eq!(store.load::<Config>(), config);
    }

    #[test]
    fn real_state_round_trips() {
        use crate::FxOrderMap;
        use crate::config::State;
        use crate::tab::HeadingOptions;

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("state.ron"));

        let state = State {
            sort_names: FxOrderMap::from_iter([
                ("/home/user/Downloads".to_string(), (HeadingOptions::Modified, false)),
                ("/home/user/Music".to_string(), (HeadingOptions::Name, true)),
                ("/home/user/Pictures".to_string(), (HeadingOptions::Size, false)),
            ]),
        };

        store.save(&state).expect("save");
        assert_eq!(store.load::<State>(), state);
    }
}
