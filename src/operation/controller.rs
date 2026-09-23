use std::sync::Arc;
use std::sync::atomic::{self, AtomicU16, AtomicU32};
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
    notify: Notify,
}

#[derive(Debug)]
pub struct Controller {
    primary: bool,
    inner: Arc<ControllerInner>,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            primary: true,
            inner: Arc::new(ControllerInner {
                state: AtomicU16::new(ControllerState::Running.into()),
                progress: AtomicU32::new(0.0f32.to_bits()),
                notify: Notify::new(),
            }),
        }
    }
}

impl Controller {
    pub async fn check(&self) -> Result<(), ControllerState> {
        loop {
            match self.state() {
                ControllerState::Cancelled => return Err(ControllerState::Cancelled),
                ControllerState::Failed => return Err(ControllerState::Failed),
                ControllerState::Paused => (),
                ControllerState::Running => return Ok(()),
            }

            self.inner.notify.notified().await;
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

    pub fn state(&self) -> ControllerState {
        ControllerState::try_from(self.inner.state.load(atomic::Ordering::Relaxed))
            .unwrap_or(ControllerState::Failed)
    }

    pub fn set_state(&self, state: ControllerState) {
        self.inner
            .state
            .store(state.into(), atomic::Ordering::Relaxed);
        self.inner.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self.state(), ControllerState::Cancelled)
    }

    pub fn cancel(&self) {
        self.set_state(ControllerState::Cancelled);
    }

    pub fn is_failed(&self) -> bool {
        matches!(self.state(), ControllerState::Failed)
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.state(), ControllerState::Paused)
    }

    pub fn pause(&self) {
        self.set_state(ControllerState::Paused);
    }

    /// Returns when the state is paused.
    ///
    /// Use this to pause futures.
    pub async fn until_paused(&self) {
        loop {
            if matches!(self.state(), ControllerState::Paused) {
                return;
            }

            self.inner.notify.notified().await;
        }
    }

    /// Returns when state is neither paused, cancelled, nor failed.
    ///
    /// Use this to resume futures.
    pub async fn until_unpaused(&self) {
        loop {
            if !matches!(
                self.state(),
                ControllerState::Paused | ControllerState::Cancelled | ControllerState::Failed
            ) {
                return;
            }

            self.inner.notify.notified().await;
        }
    }

    pub fn unpause(&self) {
        if !self.is_cancelled() && !self.is_failed() {
            self.set_state(ControllerState::Running);
        }
    }
}

impl Clone for Controller {
    fn clone(&self) -> Self {
        Self {
            primary: false,
            inner: self.inner.clone(),
        }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        // Cancel operations if primary controller is dropped and controller is still running
        if self.primary && self.state() != ControllerState::Failed {
            self.cancel();
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

        controller.set_state(ControllerState::Failed);
        controller.unpause();
        assert!(controller.is_failed());

        controller.pause();
        controller.unpause();
        assert_eq!(controller.state(), ControllerState::Running);
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
