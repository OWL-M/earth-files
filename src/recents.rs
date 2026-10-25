//! Recent-file bookkeeping, done on one ordered background thread.
//!
//! `recently-used.xbel` is read, edited and written back whole on every
//! change, so two updates that overlap lose one of their entries. That rules
//! out spawning a task per file. It is also a disk write, which rules out
//! doing it where it used to be done: inline in the handlers that open files,
//! once per file in the selection, with the event loop waiting.
//!
//! One worker answers both. Callers hand over a job and return immediately,
//! and the jobs are applied in the order they were made.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::mpsc::{self, Sender};
use std::time::SystemTime;

use xdg_kit::basedir::Durability;
use xdg_kit::bookmarks::{BookmarkFile, EditError, Opened, WriteError};

/// How many times a change is tried again when another program wrote the
/// list between this worker's read and its write.
const WRITE_TRIES: usize = 4;

/// Where the list is kept: `$XDG_DATA_HOME/recently-used.xbel`.
pub fn path() -> Option<PathBuf> {
    crate::xdg_dirs::data_home().map(|data| data.join("recently-used.xbel"))
}

/// The list as it is on disk now; an empty one when there is no file yet.
pub fn load() -> Result<BookmarkFile, String> {
    let base_dirs = crate::xdg_dirs::BASE_DIRS
        .as_ref()
        .ok_or_else(|| "no base directories".to_owned())?;
    BookmarkFile::load_recently_used(base_dirs).map_err(|err| err.to_string())
}

/// A watcher that calls `changed` whenever the list at `list` changes.
///
/// It watches the folder the list is in, not the list itself: the list is
/// replaced whole by each write, ours and other programs' alike, and a watch
/// on the file ends with the file it was on. A folder watch also sees the
/// list appear when there was none yet.
pub fn watch(
    list: PathBuf,
    mut changed: impl FnMut() + Send + 'static,
) -> Result<
    notify_debouncer_full::Debouncer<
        notify_debouncer_full::notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
    String,
> {
    use notify_debouncer_full::notify::RecursiveMode;

    let folder = list
        .parent()
        .ok_or_else(|| format!("{} has no folder", list.display()))?
        .to_path_buf();
    let name = list.file_name().map(ToOwned::to_owned);
    let mut watcher = notify_debouncer_full::new_debouncer(
        std::time::Duration::from_millis(250),
        Some(std::time::Duration::from_millis(250)),
        move |events: notify_debouncer_full::DebounceEventResult| match events {
            Ok(events) => {
                // Programs differ in how they change the list, so anything
                // but a read of it counts
                if events.iter().any(|event| {
                    let kind = event.kind;
                    (kind.is_create() || kind.is_modify() || kind.is_remove() || kind.is_other())
                        && event
                            .paths
                            .iter()
                            .any(|path| path.file_name() == name.as_deref())
                }) {
                    changed();
                }
            }
            Err(errors) => log::warn!("failed to watch the recent files list: {errors:?}"),
        },
    )
    .map_err(|err| err.to_string())?;
    watcher
        .watch(&folder, RecursiveMode::NonRecursive)
        .map_err(|err| format!("failed to watch {}: {err}", folder.display()))?;
    Ok(watcher)
}

/// The URI the list names `path` by.
fn uri(path: &Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

/// Loads the list, applies `change` and writes it back. When another program
/// wrote it in between, the write is refused rather than overwriting that
/// change, and this starts again from the new file.
fn edit(change: impl Fn(&mut BookmarkFile) -> Result<(), EditError>) -> Result<(), String> {
    let list = path().ok_or_else(|| "no data folder".to_owned())?;
    edit_at(&list, change)
}

/// [`edit`] on the list at `list`.
fn edit_at(
    list: &Path,
    change: impl Fn(&mut BookmarkFile) -> Result<(), EditError>,
) -> Result<(), String> {
    for _ in 0..WRITE_TRIES {
        let mut file = BookmarkFile::read(list).map_err(|err| err.to_string())?;
        change(&mut file).map_err(|err| err.to_string())?;
        match file.write() {
            Ok(Durability::Synced) => return Ok(()),
            // Written; only syncing its folder failed. Writing again could
            // undo a change made since, so it counts as done.
            Ok(Durability::NotSynced(err)) => {
                log::warn!("wrote the recent files list but could not sync it: {err}");
                return Ok(());
            }
            Err(WriteError::Conflict) => {}
            Err(err) => return Err(err.to_string()),
        }
    }
    Err("another program kept changing the list".to_owned())
}

/// A change to the recently-used list.
enum Job {
    /// Record that `path` was opened by the application `id` runs.
    Record {
        path: PathBuf,
        id: String,
        exec: String,
    },
    /// Forget these paths.
    Remove {
        paths: Box<[PathBuf]>,
        done: mpsc::SyncSender<Result<(), String>>,
    },
    /// Forget everything.
    Clear,
    /// Answer once every job queued before this one has been applied.
    Barrier(mpsc::SyncSender<()>),
}

/// The one thread that touches the recently-used file.
///
/// `None` when the thread could not be started, in which case recents simply
/// stop being recorded: it is bookkeeping, and losing it is better than
/// failing to open the file the user asked for.
static WORKER: LazyLock<Option<Sender<Job>>> = LazyLock::new(|| {
    let (tx, rx) = mpsc::channel::<Job>();
    let spawned = std::thread::Builder::new()
        .name("recents".to_owned())
        .spawn(move || {
            // Ends when every sender is dropped, which is at exit.
            for job in rx {
                match job {
                    Job::Record { path, id, exec } => {
                        if let Err(err) = edit(record_change(&path, id, &exec)) {
                            log::warn!("failed to record {} as recent: {err}", path.display());
                        }
                    }
                    Job::Remove { paths, done } => {
                        let result = edit(remove_change(&paths));
                        // Reported back, unlike the rest: this one is an
                        // operation the user started and can see fail.
                        let _ = done.send(result);
                    }
                    Job::Clear => {
                        if let Err(err) = edit(BookmarkFile::clear) {
                            log::warn!("failed to clear recents history: {err}");
                        }
                    }
                    Job::Barrier(done) => {
                        // The queue is in order, so reaching this means every
                        // job queued before it is on disk.
                        let _ = done.send(());
                    }
                }
            }
        });

    match spawned {
        Ok(_) => Some(tx),
        Err(err) => {
            log::error!("failed to start the recent files worker: {err}");
            None
        }
    }
});

/// The change that records `path` as opened by the application `id`, which
/// runs as `exec`. A path with no URI changes nothing.
fn record_change(
    path: &Path,
    id: String,
    exec: &str,
) -> impl Fn(&mut BookmarkFile) -> Result<(), EditError> {
    let uri = uri(path);
    if uri.is_none() {
        log::warn!("cannot record {} as recent: no URI", path.display());
    }
    let opened = Opened {
        name: id,
        // Quoted, with `%u`, as other programs write it
        exec: format!("'{exec} %u'"),
        mime: crate::mime_icon::mime_for_path(path, false),
        groups: Vec::new(),
        private: false,
        when: SystemTime::now(),
    };
    move |file| match &uri {
        Some(uri) => file.open(uri, &opened),
        None => Ok(()),
    }
}

/// The change that forgets `paths`.
fn remove_change(paths: &[PathBuf]) -> impl Fn(&mut BookmarkFile) -> Result<(), EditError> {
    let uris: Vec<String> = paths.iter().filter_map(|path| uri(path)).collect();
    move |file| {
        for uri in &uris {
            file.remove(uri)?;
        }
        Ok(())
    }
}

fn submit(job: Job) {
    let Some(tx) = WORKER.as_ref() else {
        return;
    };
    if tx.send(job).is_err() {
        log::warn!("the recent files worker stopped; recents are no longer recorded");
    }
}

/// Records that `path` was opened, without waiting for the write.
pub fn record(path: PathBuf, id: String, exec: String) {
    submit(Job::Record { path, id, exec });
}

/// Forgets every recent file, without waiting for the write.
pub fn clear() {
    submit(Job::Clear);
}

/// Forgets `paths`, waiting for the write and reporting whether it worked.
///
/// Blocking, so it belongs on a worker rather than the event loop. It goes
/// through the same queue as everything else here, because the file is read,
/// edited and written back whole: a removal running beside a recording would
/// undo one of them.
pub fn remove(paths: Box<[PathBuf]>) -> Result<(), String> {
    let Some(tx) = WORKER.as_ref() else {
        return Err("the recent files worker is not running".to_owned());
    };
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    if tx
        .send(Job::Remove {
            paths,
            done: done_tx,
        })
        .is_err()
    {
        return Err("the recent files worker stopped".to_owned());
    }
    done_rx
        .recv()
        .unwrap_or_else(|_| Err("the recent files worker stopped".to_owned()))
}

/// Waits until every queued change has been applied.
///
/// Called before the process exits. The worker is an ordinary thread, which
/// the process would otherwise take down mid-write along with everything
/// still queued behind it.
pub fn flush() {
    let Some(tx) = WORKER.as_ref() else {
        return;
    };
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    if tx.send(Job::Barrier(done_tx)).is_err() {
        return;
    }
    // The worker is gone if this fails, so there is nothing left to wait for.
    let _ = done_rx.recv();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One entry: URI, type, and each application's name and command.
    type Entry = (String, Option<String>, Vec<(String, String)>);

    /// The entries of the list at `list`.
    fn entries(list: &Path) -> Vec<Entry> {
        BookmarkFile::read(list)
            .unwrap()
            .bookmarks()
            .map(|bookmark| {
                (
                    bookmark.href().to_owned(),
                    bookmark.mime_type().map(str::to_owned),
                    bookmark
                        .applications()
                        .iter()
                        .map(|app| {
                            (
                                app.name().unwrap_or_default().to_owned(),
                                app.exec().unwrap_or_default().to_owned(),
                            )
                        })
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn an_opened_file_is_recorded_with_its_detected_type() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("recently-used.xbel");
        // No extension: only its contents say it is text
        let notes = dir.path().join("notes");
        std::fs::write(&notes, "plain words\n").unwrap();

        edit_at(
            &list,
            record_change(&notes, "Earth Files".into(), "earth-files"),
        )
        .unwrap();

        let uri = uri(&notes).unwrap();
        assert_eq!(
            entries(&list),
            [(
                uri,
                Some("text/plain".to_owned()),
                vec![("Earth Files".to_owned(), "'earth-files %u'".to_owned())]
            )]
        );
    }

    /// Every write replaces the list whole, so a watch must outlast the file
    /// it saw: two writes in a row are both noticed.
    #[test]
    fn the_watch_sees_every_write_of_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("recently-used.xbel");
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, "x").unwrap();
        // The list is there before the watch starts, as it usually is
        edit_at(
            &list,
            record_change(&notes, "Earth Files".into(), "earth-files"),
        )
        .unwrap();
        let (tx, rx) = mpsc::channel();
        let _watcher = watch(list.clone(), move || {
            let _ = tx.send(());
        })
        .unwrap();

        for _ in 0..2 {
            edit_at(
                &list,
                record_change(&notes, "Earth Files".into(), "earth-files"),
            )
            .unwrap();
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .expect("the write is seen");
            // The debouncer may report one write as several events
            while rx
                .recv_timeout(std::time::Duration::from_millis(600))
                .is_ok()
            {}
        }
        // A file beside the list is not the list
        std::fs::write(dir.path().join("other.txt"), "y").unwrap();
        assert!(rx.recv_timeout(std::time::Duration::from_secs(1)).is_err());
    }

    #[test]
    fn forgetting_files_leaves_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("recently-used.xbel");
        let [a, b] = ["a.txt", "b.txt"].map(|name| {
            let path = dir.path().join(name);
            std::fs::write(&path, "x").unwrap();
            path
        });
        for path in [&a, &b] {
            edit_at(
                &list,
                record_change(path, "Earth Files".into(), "earth-files"),
            )
            .unwrap();
        }

        edit_at(&list, remove_change(std::slice::from_ref(&a))).unwrap();
        let left: Vec<String> = entries(&list).into_iter().map(|(href, ..)| href).collect();
        assert_eq!(left, [uri(&b).unwrap()]);

        edit_at(&list, BookmarkFile::clear).unwrap();
        assert!(entries(&list).is_empty());
    }
}
