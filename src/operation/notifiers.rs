// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::Notify;

/// Monitor files which are being written to.
pub struct FileWritingNotifier {
    data: Vec<PathBuf>,
    notify: Arc<Notify>,
}

static ACTIVELY_WRITING: LazyLock<Mutex<FileWritingNotifier>> = LazyLock::new(|| {
    Mutex::new(FileWritingNotifier {
        data: Vec::new(),
        notify: Arc::new(Notify::new()),
    })
});

/// Append path that is being written to.
pub fn actively_writing_add(path: PathBuf) {
    ACTIVELY_WRITING.lock().unwrap().data.push(path);
}

/// Remove path to file that has finished writing and notify waiters.
pub fn actively_writing_remove(path: &Path) {
    let mut guard = ACTIVELY_WRITING.lock().unwrap();
    guard.data.retain(|p| p != path);
    guard.notify.notify_waiters();
}

/// Wait until the actively-writing queue is empty or a file has been removed.
pub async fn actively_writing_tick() {
    let notify = Arc::clone(&ACTIVELY_WRITING.lock().unwrap().notify);
    // Listening before looking, under the same lock a removal takes:
    // `notify_waiters` wakes only those already listening, so a removal
    // between the look and the listen would leave this waiting for another
    let notified = notify.notified();
    {
        let guard = ACTIVELY_WRITING.lock().unwrap();
        if guard.data.is_empty() {
            return;
        }
    }
    notified.await;
}

/// Check if a file is being written to. Avoid thumbnail generation until after it is finished.
pub fn is_actively_writing_to(path: &Path) -> bool {
    ACTIVELY_WRITING
        .lock()
        .unwrap()
        .data
        .iter()
        .any(|p| p == path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Another file finishing ends the wait, though one is still being
    /// written
    #[test]
    fn a_removal_ends_the_wait() {
        let path = PathBuf::from("/notifiers-test/a");
        let other = PathBuf::from("/notifiers-test/b");
        actively_writing_add(path.clone());
        actively_writing_add(other.clone());
        let mut tick = std::pin::pin!(actively_writing_tick());
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        assert!(tick.as_mut().poll(&mut cx).is_pending());
        actively_writing_remove(&path);
        assert!(tick.as_mut().poll(&mut cx).is_ready());
        actively_writing_remove(&other);
    }
}
