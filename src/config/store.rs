// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! On-disk configuration.
//!
//! Values live in RON under `$XDG_CONFIG_HOME/earth-files/`. A missing or
//! unreadable file yields `Default` rather than an error: a corrupt config must
//! never stop the app from starting.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Whether each config file is currently unsafe to write, keyed by path.
///
/// The seal has to be shared rather than held per `Store`: the app writes
/// through one store while the change watcher reads through another built
/// from the same path, and a seal only one of them can see would not stop
/// the other from overwriting a file it failed to rescue.
static SEALS: LazyLock<Mutex<HashMap<PathBuf, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn seal_for(path: &Path) -> Arc<AtomicBool> {
    let mut seals = SEALS.lock().unwrap_or_else(|err| err.into_inner());
    Arc::clone(seals.entry(path.to_path_buf()).or_default())
}

/// A single configuration file.
#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
    /// Set when a malformed file could not be copied aside. Saving is then
    /// refused, because overwriting it would destroy settings the user could
    /// otherwise still recover by hand. Shared by every store on this path.
    sealed: Arc<AtomicBool>,
}

impl Store {
    /// A store backed by an exact path. Used by tests.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let sealed = seal_for(&path);
        Self { path, sealed }
    }

    /// A store for `name` under the user's config directory.
    pub fn named(name: &str) -> Self {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| crate::home_dir().join(".config"))
            .join("earth-files");
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
                if err.kind() == io::ErrorKind::NotFound {
                    // Nothing is there to protect any more, so a seal from an
                    // earlier failed rescue must not keep blocking saves: the
                    // user may have moved the bad file aside by hand
                    self.sealed.store(false, Ordering::Relaxed);
                } else {
                    log::warn!("failed to read {}: {err}", self.path.display());
                }
                return T::default();
            }
        };

        match ron::from_str(&text) {
            Ok(value) => {
                // The file reads cleanly, so whatever made it unsafe to write
                // is behind us and saving may resume
                self.sealed.store(false, Ordering::Relaxed);
                value
            }
            Err(err) => {
                log::error!("failed to parse {}: {err}", self.path.display());
                match self.preserve_malformed() {
                    Ok(backup) => {
                        // The bad file is out of the way, so writing a fresh
                        // one destroys nothing
                        self.sealed.store(false, Ordering::Relaxed);
                        log::error!(
                            "kept the unreadable {} as {}; starting from defaults",
                            self.path.display(),
                            backup.display()
                        );
                    }
                    Err(err) => {
                        log::error!(
                            "failed to keep a copy of {}: {err}. Settings will not be saved \
                             over it, so it can still be recovered by hand",
                            self.path.display()
                        );
                        self.sealed.store(true, Ordering::Relaxed);
                    }
                }
                T::default()
            }
        }
    }

    /// Move a malformed file aside under a name that is not already taken, so
    /// a second bad start cannot overwrite the first rescue copy.
    fn preserve_malformed(&self) -> io::Result<PathBuf> {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config".to_string());
        let parent = self.path.parent().unwrap_or(Path::new("."));
        for n in 0.. {
            let backup = parent.join(if n == 0 {
                format!("{name}.bak")
            } else {
                format!("{name}.bak.{n}")
            });
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&backup)
            {
                Ok(_) => {
                    fs::rename(&self.path, &backup)?;
                    return Ok(backup);
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }
        unreachable!("the loop only ends by returning")
    }

    /// Write the value, creating the parent directory as needed.
    ///
    /// This flushes the value all the way to the disk before returning, so it
    /// must not be called where a person is waiting: use
    /// [`Self::save_in_background`] from anything driven by the event loop.
    pub fn save<T: Serialize>(&self, value: &T) -> io::Result<()> {
        let text = self.prepare(value)?;
        write_text(&self.path, &self.sealed, &text)
    }

    /// Queue the value to be written, and return without waiting for the disk.
    ///
    /// The value is serialized here, so what is queued is a snapshot of it
    /// taken now rather than a reference that could change before it lands.
    /// Everything after that happens on one shared writer thread, which is
    /// what keeps an older snapshot from overwriting a newer one. A save
    /// queued for a path that already has one waiting replaces it: these are
    /// whole files, so only the newest is worth writing.
    ///
    /// Failures are reported through the log rather than to the caller, which
    /// has long since returned. The seal is the exception: it is checked here
    /// as well, so a caller still learns immediately that its settings are
    /// being refused.
    pub fn save_in_background<T: Serialize>(&self, value: &T) -> io::Result<()> {
        let text = self.prepare(value)?;
        queue_write(self.path.clone(), Arc::clone(&self.sealed), text);
        Ok(())
    }

    /// The seal check and serialization both saves share.
    fn prepare<T: Serialize>(&self, value: &T) -> io::Result<String> {
        if self.sealed.load(Ordering::Relaxed) {
            return Err(io::Error::other(format!(
                "refusing to overwrite the unreadable {}",
                self.path.display()
            )));
        }
        ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::default())
            .map_err(io::Error::other)
    }
}

/// Put `text` at `path`, durably, unless the path is sealed.
///
/// The seal is read again here and not only when the write was asked for: a
/// queued write can be picked up after a failed rescue has sealed the path,
/// and it must not go through then either.
fn write_text(path: &Path, sealed: &AtomicBool, text: &str) -> io::Result<()> {
    if sealed.load(Ordering::Relaxed) {
        return Err(io::Error::other(format!(
            "refusing to overwrite the unreadable {}",
            path.display()
        )));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }

    // Write to a temporary file in the same directory and rename over the
    // target, so a reader never observes a half-written file. Writing in
    // place would truncate first, and this app watches its own config
    // directory: another instance could read the gap.
    let parent = parent.unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    io::Write::write_all(&mut file, text.as_bytes())?;
    // Get the contents down before the rename publishes the name, and the
    // directory entry down after it, so a crash leaves either the old file
    // or the new one rather than an empty one
    file.as_file().sync_all()?;
    file.persist(path).map_err(|err| err.error)?;
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// A write waiting for the writer thread.
struct Queued {
    sealed: Arc<AtomicBool>,
    text: String,
}

#[derive(Default)]
struct WriteQueue {
    /// The newest queued text per path.
    by_path: HashMap<PathBuf, Queued>,
    /// Whether a write is on its way to the disk right now. [`flush`] waits
    /// for this as well as for the queue to empty, because a write that has
    /// left the queue is not yet a write that has landed.
    writing: bool,
}

/// The queue, and the signal that it changed.
static WRITES: LazyLock<Arc<(Mutex<WriteQueue>, Condvar)>> = LazyLock::new(|| {
    let queue = Arc::new((Mutex::new(WriteQueue::default()), Condvar::new()));
    let worker = Arc::clone(&queue);
    if let Err(err) = std::thread::Builder::new()
        .name("config-writer".to_owned())
        .spawn(move || run_writes(&worker))
    {
        // Without the thread nothing would ever drain the queue, so saves go
        // back to happening where they are asked for. Slow is better than lost.
        log::error!("failed to start the config writer: {err}; saving inline instead");
        WRITER_RUNNING.store(false, Ordering::Relaxed);
    }
    queue
});

/// Whether the writer thread exists. Only ever set false, once, at startup.
static WRITER_RUNNING: AtomicBool = AtomicBool::new(true);

fn queue_write(path: PathBuf, sealed: Arc<AtomicBool>, text: String) {
    let (lock, change) = &**WRITES;
    if !WRITER_RUNNING.load(Ordering::Relaxed) {
        if let Err(err) = write_text(&path, &sealed, &text) {
            log::warn!("failed to save {}: {err}", path.display());
        }
        return;
    }

    let mut queue = lock.lock().unwrap_or_else(|err| err.into_inner());
    queue.by_path.insert(path, Queued { sealed, text });
    change.notify_all();
}

fn run_writes(queue: &(Mutex<WriteQueue>, Condvar)) {
    let (lock, change) = queue;
    let mut state = lock.lock().unwrap_or_else(|err| err.into_inner());
    loop {
        let next = state
            .by_path
            .keys()
            .next()
            .cloned()
            .and_then(|path| state.by_path.remove_entry(&path));

        let Some((path, queued)) = next else {
            // Nothing left: say so, so a flush waiting on this can return,
            // and sleep until something is queued.
            state.writing = false;
            change.notify_all();
            state = change.wait(state).unwrap_or_else(|err| err.into_inner());
            continue;
        };

        state.writing = true;
        drop(state);

        if let Err(err) = write_text(&path, &queued.sealed, &queued.text) {
            log::warn!("failed to save {}: {err}", path.display());
        }

        state = lock.lock().unwrap_or_else(|err| err.into_inner());
    }
}

/// Wait until every queued save has reached the disk.
///
/// Called before the application exits, so settings changed in the last moment
/// before quitting are not lost with the process.
pub fn flush() {
    if !WRITER_RUNNING.load(Ordering::Relaxed) {
        return;
    }
    let (lock, change) = &**WRITES;
    let mut queue = lock.lock().unwrap_or_else(|err| err.into_inner());
    while !queue.by_path.is_empty() || queue.writing {
        queue = change.wait(queue).unwrap_or_else(|err| err.into_inner());
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
                        // Read on a blocking worker. This loop is a task on a
                        // shared runtime, and reading and parsing the file is
                        // ordinary blocking I/O however short the file is.
                        let store = store.clone();
                        let Ok(value) =
                            tokio::task::spawn_blocking(move || store.load::<T>()).await
                        else {
                            break;
                        };
                        if output.send(value).await.is_err() {
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

    #[test]
    fn a_malformed_file_is_kept_aside_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.ron");
        std::fs::write(&path, b"this is not ron").unwrap();

        let store = Store::at(&path);
        let config: Sample = store.load();
        assert_eq!(
            config,
            Sample::default(),
            "a bad file falls back to defaults"
        );

        let backup = dir.path().join("config.ron.bak");
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            b"this is not ron",
            "the unreadable file is kept"
        );
        assert!(!path.exists(), "the unreadable file is moved, not copied");

        // A second bad file does not overwrite the first rescue copy
        std::fs::write(&path, b"also not ron").unwrap();
        let _: Sample = Store::at(&path).load();
        assert_eq!(std::fs::read(&backup).unwrap(), b"this is not ron");
        assert_eq!(
            std::fs::read(dir.path().join("config.ron.bak.1")).unwrap(),
            b"also not ron"
        );

        // Saving works again once the bad file is out of the way
        store.save(&Sample::default()).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn removing_the_malformed_file_by_hand_unblocks_saving() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.ron");
        std::fs::create_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not ron").unwrap();

        let store = Store::at(&path);
        let mut perms = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
        std::fs::set_permissions(path.parent().unwrap(), perms.clone()).unwrap();
        let _: Sample = store.load();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        std::fs::set_permissions(path.parent().unwrap(), perms).unwrap();
        assert!(store.save(&Sample::default()).is_err(), "sealed");

        // The user takes the bad file away themselves
        std::fs::remove_file(&path).unwrap();
        let _: Sample = store.load();
        store
            .save(&Sample::default())
            .expect("an absent file leaves nothing to protect");
    }

    #[test]
    fn the_seal_is_shared_between_stores_and_clears_once_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.ron");
        std::fs::create_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not ron").unwrap();

        let writer = Store::at(&path);
        let mut perms = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
        std::fs::set_permissions(path.parent().unwrap(), perms.clone()).unwrap();

        // The watcher builds its own store for the same path; the seal it
        // sets must stop the writer too
        let _: Sample = Store::at(&path).load();
        assert!(
            writer.save(&Sample::default()).is_err(),
            "a seal set through one store must stop every store on that path"
        );

        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        std::fs::set_permissions(path.parent().unwrap(), perms).unwrap();

        // Once the bad file can be kept aside, saving works again
        let _: Sample = Store::at(&path).load();
        writer
            .save(&Sample::default())
            .expect("the seal should clear once the bad file is preserved");
        assert!(path.parent().unwrap().join("config.ron.bak").exists());
    }

    #[test]
    fn saving_is_refused_while_a_malformed_file_could_not_be_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.ron");
        std::fs::create_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not ron").unwrap();

        let store = Store::at(&path);
        // Make the rescue copy impossible by taking away write access
        let mut perms = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
        std::fs::set_permissions(path.parent().unwrap(), perms.clone()).unwrap();

        let _: Sample = store.load();
        let refused = store.save(&Sample::default());

        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        std::fs::set_permissions(path.parent().unwrap(), perms).unwrap();

        assert!(
            refused.is_err(),
            "must not overwrite what it could not keep"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"not ron");
    }

    #[derive(Debug, Default, Deserialize, PartialEq, Serialize)]
    struct Sample {
        value: u32,
        name: String,
    }

    #[test]
    fn a_background_save_lands_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("sample.ron"));

        store
            .save_in_background(&Sample {
                value: 7,
                name: "seven".to_owned(),
            })
            .expect("queueing should not fail");
        flush();

        assert_eq!(
            store.load::<Sample>(),
            Sample {
                value: 7,
                name: "seven".to_owned(),
            },
            "the queued value reached the file"
        );
    }

    #[test]
    fn the_newest_background_save_is_the_one_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("sample.ron"));

        // Queued back to back, as a drag that changes one setting many times
        // does. Whether the writer coalesces them or writes each in turn, the
        // file must end up holding the last one and never an earlier one.
        for value in 0..50 {
            store
                .save_in_background(&Sample {
                    value,
                    name: value.to_string(),
                })
                .expect("queueing should not fail");
        }
        flush();

        assert_eq!(
            store.load::<Sample>(),
            Sample {
                value: 49,
                name: "49".to_owned(),
            },
            "an older snapshot overwrote a newer one"
        );
    }

    #[test]
    fn a_background_save_does_not_wait_for_the_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("sample.ron"));

        // Not a timing assertion about the disk: queueing must not do the
        // work at all, so even a pathologically slow filesystem cannot make
        // this exceed the time it takes to serialize fifty small values.
        let started = std::time::Instant::now();
        for value in 0..50 {
            store
                .save_in_background(&Sample {
                    value,
                    name: String::new(),
                })
                .expect("queueing should not fail");
        }
        let queueing = started.elapsed();
        flush();

        assert!(
            queueing < Duration::from_secs(2),
            "queueing fifty saves took {queueing:?}, so it is still writing inline"
        );
    }

    #[test]
    fn a_sealed_path_refuses_a_background_save_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.ron");
        std::fs::write(&path, b"not ron").expect("write");

        // Make the rescue copy impossible, which is what seals the path
        let mut perms = std::fs::metadata(dir.path())
            .expect("metadata")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
        std::fs::set_permissions(dir.path(), perms.clone()).expect("set permissions");

        let store = Store::at(&path);
        let _: Sample = store.load();

        let refused = store.save_in_background(&Sample::default());

        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        std::fs::set_permissions(dir.path(), perms).expect("restore permissions");

        assert!(refused.is_err(), "a sealed path must refuse a queued save");
        flush();
        assert_eq!(
            std::fs::read(&path).expect("read"),
            b"not ron",
            "the unreadable file was overwritten anyway"
        );
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
        let sample = Sample {
            value: 7,
            name: "seven".to_string(),
        };
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
        store
            .save(&Sample {
                value: 1,
                name: "one".to_string(),
            })
            .expect("first save");
        store
            .save(&Sample {
                value: 2,
                name: "two".to_string(),
            })
            .expect("second save");

        // The rename must leave exactly the target file behind, with no
        // leftover temporary alongside it.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("sample.ron")]);
        assert_eq!(
            store.load::<Sample>(),
            Sample {
                value: 2,
                name: "two".to_string()
            }
        );
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

        let mut config = Config {
            show_details: true,
            show_recents: false,
            ..Config::default()
        };
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
                (
                    "/home/user/Downloads".to_string(),
                    (HeadingOptions::Modified, false),
                ),
                ("/home/user/Music".to_string(), (HeadingOptions::Name, true)),
                (
                    "/home/user/Pictures".to_string(),
                    (HeadingOptions::Size, false),
                ),
            ]),
        };

        store.save(&state).expect("save");
        assert_eq!(store.load::<State>(), state);
    }
}
