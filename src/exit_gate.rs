// SPDX-License-Identifier: GPL-3.0-only

//! When a file manager whose window is closed may exit.
//!
//! Closing the window does not stop copies and moves in flight: the process
//! stays, shows an "in progress" notification, and exits once they are done.
//! That notification never times out, so the process must not exit while one
//! is on its way to the screen or still being taken down, or it stays up for
//! good. [`ExitGate`] holds where the notification is, and every exit check
//! goes through it.
//!
//! It is generic over the notification handle so it can be tested without a
//! notification daemon.

/// What to do next, with the window closed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step<H> {
    /// Something is in flight; a later event asks again.
    Wait,
    /// Show the notification, then report it with [`ExitGate::shown`].
    Show,
    /// Close this notification, then report it with [`ExitGate::closed`].
    Close(H),
    /// Nothing left to wait for.
    Exit,
}

// Without notifications nothing is ever shown, so neither is it closed.
#[cfg_attr(not(feature = "notify"), allow(dead_code))]
#[derive(Debug)]
enum Notice<H> {
    None,
    /// Showing it failed; not tried again.
    Unavailable,
    Opening,
    Open(H),
    Closing,
}

#[derive(Debug)]
pub(crate) struct ExitGate<H> {
    notice: Notice<H>,
}

impl<H> Default for ExitGate<H> {
    fn default() -> Self {
        Self {
            notice: Notice::None,
        }
    }
}

impl<H> ExitGate<H> {
    /// Decides the next step while the window is closed. `pending` is whether
    /// operations are still running, `can_notify` whether notifications can
    /// be shown at all.
    pub(crate) fn step(&mut self, pending: bool, can_notify: bool) -> Step<H> {
        match std::mem::replace(&mut self.notice, Notice::None) {
            Notice::None if pending && can_notify => {
                self.notice = Notice::Opening;
                Step::Show
            }
            notice @ (Notice::None | Notice::Unavailable) if pending => {
                self.notice = notice;
                Step::Wait
            }
            Notice::None | Notice::Unavailable => Step::Exit,
            Notice::Open(handle) if !pending => {
                self.notice = Notice::Closing;
                Step::Close(handle)
            }
            notice => {
                self.notice = notice;
                Step::Wait
            }
        }
    }

    /// The notification asked for by [`Step::Show`] is up, or could not be
    /// shown (`None`).
    #[cfg_attr(not(feature = "notify"), allow(dead_code))]
    pub(crate) fn shown(&mut self, handle: Option<H>) {
        if matches!(self.notice, Notice::Opening) {
            self.notice = handle.map_or(Notice::Unavailable, Notice::Open);
        }
    }

    /// The notification handed out by [`Step::Close`] is gone.
    #[cfg_attr(not(feature = "notify"), allow(dead_code))]
    pub(crate) fn closed(&mut self) {
        if matches!(self.notice, Notice::Closing) {
            self.notice = Notice::None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_pending_exits_at_once() {
        let mut gate = ExitGate::<u8>::default();
        assert_eq!(gate.step(false, true), Step::Exit);
    }

    #[test]
    fn a_running_operation_shows_the_notification_once_and_waits() {
        let mut gate = ExitGate::<u8>::default();
        assert_eq!(gate.step(true, true), Step::Show);
        assert_eq!(gate.step(true, true), Step::Wait, "already on its way");
        gate.shown(Some(7));
        assert_eq!(gate.step(true, true), Step::Wait, "still running");
    }

    #[test]
    fn operations_finishing_while_it_is_shown_wait_for_the_handle() {
        let mut gate = ExitGate::<u8>::default();
        assert_eq!(gate.step(true, true), Step::Show);
        // The last operation finishes before the notification is up.
        assert_eq!(gate.step(false, true), Step::Wait);
        gate.shown(Some(7));
        assert_eq!(gate.step(false, true), Step::Close(7));
    }

    #[test]
    fn a_slow_close_holds_off_every_other_exit_check() {
        let mut gate = ExitGate::<u8>::default();
        let _ = gate.step(true, true);
        gate.shown(Some(7));
        assert_eq!(gate.step(false, true), Step::Close(7));
        // Completions and failures of one batch, and `MaybeExit`s from
        // anywhere, all ask again while the close is still under way.
        for _ in 0..3 {
            assert_eq!(gate.step(false, true), Step::Wait);
        }
        gate.closed();
        assert_eq!(gate.step(false, true), Step::Exit);
    }

    #[test]
    fn a_notification_that_could_not_be_shown_does_not_hold_up_the_exit() {
        let mut gate = ExitGate::<u8>::default();
        let _ = gate.step(true, true);
        gate.shown(None);
        assert_eq!(gate.step(true, true), Step::Wait, "not tried again");
        assert_eq!(gate.step(false, true), Step::Exit);
    }

    #[test]
    fn without_notifications_it_waits_for_the_operations_alone() {
        let mut gate = ExitGate::<u8>::default();
        assert_eq!(gate.step(true, false), Step::Wait);
        assert_eq!(gate.step(false, false), Step::Exit);
    }
}
