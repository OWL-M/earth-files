use std::sync::Arc;
use std::sync::atomic::{self, AtomicBool, AtomicU16, AtomicU32, AtomicU64};
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum ControllerState {
    Cancelled,
    Failed,
    Paused,
    Running,
}

impl From<ControllerState> for u16 {
    fn from(state: ControllerState) -> Self {
        state as u16
    }
}

impl TryFrom<u16> for ControllerState {
    type Error = u16;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Cancelled),
            1 => Ok(Self::Failed),
            2 => Ok(Self::Paused),
            3 => Ok(Self::Running),
            other => Err(other),
        }
    }
}

#[derive(Debug)]
struct ControllerInner {
    state: AtomicU16,
    /// `f32` progress stored as its bit pattern.
    progress: AtomicU32,
    /// Checking the selection before anything is touched.
    checking: AtomicBool,
    /// Cancelled by Abort: stopped where it was, nothing put back.
    aborted: AtomicBool,
    /// A cancel was asked for: kept through what the state becomes after.
    cancel_asked: AtomicBool,
    /// What the checks have walked so far: files, and their size in bytes.
    checked_files: AtomicU64,
    checked_bytes: AtomicU64,
    notify: Notify,
}

/// A handle on one operation's state. Clones control the same operation;
/// dropping one changes nothing: what is to stop when its work is dropped
/// holds a [`CancelOnDrop`].
#[derive(Clone, Debug)]
pub struct Controller {
    inner: Arc<ControllerInner>,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            inner: Arc::new(ControllerInner {
                state: AtomicU16::new(ControllerState::Running.into()),
                progress: AtomicU32::new(0.0f32.to_bits()),
                checking: AtomicBool::new(false),
                aborted: AtomicBool::new(false),
                cancel_asked: AtomicBool::new(false),
                checked_files: AtomicU64::new(0),
                checked_bytes: AtomicU64::new(0),
                notify: Notify::new(),
            }),
        }
    }
}

impl Controller {
    pub async fn check(&self) -> Result<(), ControllerState> {
        loop {
            // Listening before looking, as in `until_aborted`
            let notified = self.inner.notify.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();
            match self.state() {
                ControllerState::Cancelled => return Err(ControllerState::Cancelled),
                ControllerState::Failed => return Err(ControllerState::Failed),
                ControllerState::Paused => (),
                ControllerState::Running => return Ok(()),
            }

            notified.await;
        }
    }

    pub fn progress(&self) -> f32 {
        f32::from_bits(self.inner.progress.load(atomic::Ordering::Relaxed))
    }

    pub fn set_progress(&self, progress: f32) {
        self.inner
            .progress
            .swap(progress.to_bits(), atomic::Ordering::Relaxed);
    }

    /// Whether the operation is still checking its selection, before
    /// anything is touched.
    pub fn is_checking(&self) -> bool {
        self.inner.checking.load(atomic::Ordering::Relaxed)
    }

    pub fn set_checking(&self, checking: bool) {
        self.inner
            .checking
            .store(checking, atomic::Ordering::Relaxed);
    }

    /// The checks walked `files` more files, of `bytes` in all.
    pub fn add_checked(&self, files: u64, bytes: u64) {
        self.inner
            .checked_files
            .fetch_add(files, atomic::Ordering::Relaxed);
        self.inner
            .checked_bytes
            .fetch_add(bytes, atomic::Ordering::Relaxed);
    }

    /// What the checks have walked so far: files, and their size in bytes.
    pub fn checked(&self) -> (u64, u64) {
        (
            self.inner.checked_files.load(atomic::Ordering::Relaxed),
            self.inner.checked_bytes.load(atomic::Ordering::Relaxed),
        )
    }

    pub fn state(&self) -> ControllerState {
        ControllerState::try_from(self.inner.state.load(atomic::Ordering::Relaxed))
            .unwrap_or(ControllerState::Failed)
    }

    /// Ends the operation as failed: set by what runs it, from how it
    /// ended, over a cancel too, as when putting back what a cancel undoes
    /// failed.
    pub fn fail(&self) {
        self.set_state(ControllerState::Failed);
    }

    fn set_state(&self, state: ControllerState) {
        self.inner
            .state
            .store(state.into(), atomic::Ordering::Relaxed);
        self.inner.notify.notify_waiters();
    }

    /// Whether `other` controls the same operation as this one.
    pub fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self.state(), ControllerState::Cancelled)
    }

    pub fn cancel(&self) {
        self.inner
            .cancel_asked
            .store(true, atomic::Ordering::Relaxed);
        self.set_state(ControllerState::Cancelled);
    }

    /// Whether a cancel was asked for, though the state has moved on since:
    /// a cancel whose putting back failed ends as failed, and what it
    /// still can put back is put back all the same.
    pub fn cancel_asked(&self) -> bool {
        self.inner.cancel_asked.load(atomic::Ordering::Relaxed)
    }

    /// Stops the operation where it is and keeps what it did: a cancel that
    /// puts nothing back.
    pub fn abort(&self) {
        self.inner.aborted.store(true, atomic::Ordering::Relaxed);
        self.cancel();
    }

    /// Returns once the operation is aborted.
    pub async fn until_aborted(&self) {
        loop {
            // Listening before looking: `notify_waiters` wakes only the
            // waiters already listening, so an abort between a look and a
            // listen would be missed and the wait would never end
            let notified = self.inner.notify.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();
            if self.is_aborted() {
                return;
            }
            notified.await;
        }
    }

    /// Whether it was stopped by [`Self::abort`] rather than cancelled.
    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(atomic::Ordering::Relaxed)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self.state(), ControllerState::Failed)
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.state(), ControllerState::Paused)
    }

    /// Pauses a running operation. One cancelled or failed stays so: the
    /// look and the change are one step, so a cancel in between is not
    /// overwritten.
    pub fn pause(&self) {
        self.change(ControllerState::Running, ControllerState::Paused);
    }

    /// Takes the state from `from` to `to`, if it still is `from`.
    fn change(&self, from: ControllerState, to: ControllerState) {
        if self
            .inner
            .state
            .compare_exchange(
                from.into(),
                to.into(),
                atomic::Ordering::Relaxed,
                atomic::Ordering::Relaxed,
            )
            .is_ok()
        {
            self.inner.notify.notify_waiters();
        }
    }

    /// Returns when the state is paused.
    ///
    /// Use this to pause futures.
    pub async fn until_paused(&self) {
        loop {
            // Listening before looking, as in `until_aborted`
            let notified = self.inner.notify.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();
            if matches!(self.state(), ControllerState::Paused) {
                return;
            }

            notified.await;
        }
    }

    /// Returns when the state is no longer paused: running again, or
    /// cancelled or failed, which no resume can undo.
    ///
    /// Use this to resume futures.
    pub async fn until_unpaused(&self) {
        loop {
            // Listening before looking, as in `until_aborted`
            let notified = self.inner.notify.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();
            if !self.is_paused() {
                return;
            }

            notified.await;
        }
    }

    /// Resumes a paused operation, as [`Self::pause`] does: one cancelled
    /// or failed meanwhile stays so.
    pub fn unpause(&self) {
        self.change(ControllerState::Paused, ControllerState::Running);
    }
}

/// Cancels an operation when dropped, unless [`Self::disarm`]ed first: held
/// by what runs it, so that work dropped part-way, such as a task the
/// runtime let go, stops the blocking jobs it started, which notice nothing
/// else.
#[must_use]
pub struct CancelOnDrop(Option<Controller>);

impl CancelOnDrop {
    pub fn new(controller: Controller) -> Self {
        Self(Some(controller))
    }

    /// The work ended on its own: nothing to cancel.
    pub fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(controller) = self.0.take() {
            controller.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpause_does_not_revive_a_cancelled_or_failed_controller() {
        let controller = Controller::default();
        controller.cancel();
        controller.unpause();
        assert!(controller.is_cancelled());

        controller.fail();
        controller.unpause();
        assert!(controller.is_failed());

        let controller = Controller::default();
        controller.pause();
        controller.unpause();
        assert_eq!(controller.state(), ControllerState::Running);
    }

    #[test]
    fn pause_does_not_overwrite_a_cancelled_or_failed_controller() {
        let controller = Controller::default();
        controller.cancel();
        controller.pause();
        assert!(controller.is_cancelled());

        controller.fail();
        controller.pause();
        assert!(controller.is_failed());
    }

    /// A cancel while paused ends the wait for a resume, which can never
    /// come
    #[test]
    fn a_cancel_while_paused_ends_the_wait_to_resume() {
        let controller = Controller::default();
        controller.pause();
        let mut waiting = std::pin::pin!(controller.until_unpaused());
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        assert!(waiting.as_mut().poll(&mut cx).is_pending());
        controller.cancel();
        assert!(waiting.as_mut().poll(&mut cx).is_ready());
    }

    #[test]
    fn a_cancel_asked_for_is_remembered_past_a_failure() {
        let controller = Controller::default();
        assert!(!controller.cancel_asked());
        controller.cancel();
        controller.fail();
        assert!(controller.cancel_asked() && controller.clone().cancel_asked());
    }

    #[test]
    fn abort_is_a_cancel_that_says_so() {
        let controller = Controller::default();
        controller.cancel();
        assert!(!controller.is_aborted());
        let controller = Controller::default();
        controller.abort();
        assert!(controller.is_cancelled() && controller.is_aborted());
        assert!(controller.clone().is_aborted());
    }

    #[test]
    fn only_a_guard_dropped_armed_cancels() {
        let controller = Controller::default();
        drop(controller.clone());
        CancelOnDrop::new(controller.clone()).disarm();
        assert_eq!(controller.state(), ControllerState::Running);
        drop(CancelOnDrop::new(controller.clone()));
        assert!(controller.is_cancelled());
    }

    #[test]
    fn progress_round_trips_through_bits() {
        let controller = Controller::default();
        assert_eq!(controller.progress(), 0.0);
        controller.set_progress(0.375);
        assert_eq!(controller.progress(), 0.375);
        assert_eq!(controller.clone().progress(), 0.375);
    }

    #[test]
    fn state_round_trips_through_u16() {
        for state in [
            ControllerState::Cancelled,
            ControllerState::Failed,
            ControllerState::Paused,
            ControllerState::Running,
        ] {
            assert_eq!(ControllerState::try_from(u16::from(state)), Ok(state));
        }
        assert_eq!(ControllerState::try_from(4), Err(4));
    }
}
