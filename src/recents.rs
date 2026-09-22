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

use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::mpsc::{self, Sender};

/// A change to the recently-used list.
enum Job {
    /// Record that `path` was opened by the application `id` runs.
    Record {
        path: PathBuf,
        id: String,
        exec: String,
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
                        if let Err(err) =
                            recently_used_xbel::update_recently_used(&path, id, exec, None)
                        {
                            log::warn!("failed to record {} as recent: {err}", path.display());
                        }
                    }
                    Job::Clear => {
                        if let Err(err) = recently_used_xbel::clear_recently_used() {
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
