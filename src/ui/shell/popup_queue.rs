// SPDX-License-Identifier: GPL-3.0-only

//! Holds new popups back until the popups being removed are gone.
//!
//! exwlshell creates the popups asked for in a loop pass before it destroys
//! the ones asked to go, so a popup replacing another would take its grab
//! while the old one is still there. Hyprland then dismisses the new popup
//! along with the old grab, and Mutter and Smithay reject a grabbing popup
//! that is not on the topmost one. Waiting for the old surface's `Closed`
//! puts the destroy on the wire first.
//!
//! One queue serves every shell on the thread, as [`crate::ui::surface::chain`]
//! does: a file chooser runs a shell of its own inside its host's, and a popup
//! of one replaces a popup of the other.

use iced_core::window;
use iced_exwlshell::actions::IcedNewPopupSettings;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// How long a popup waits for the removals ahead of it before it is sent
/// anyway: a removal the toolkit never reports closed must not keep every
/// later popup away.
pub(crate) const WAIT_LIMIT: Duration = Duration::from_millis(250);

/// A popup's creation, as sent to exwlshell.
pub(crate) type Request = (window::Id, IcedNewPopupSettings);

thread_local! {
    static QUEUE: RefCell<PopupQueue<Request>> = RefCell::new(PopupQueue::new());
    static OWNERS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The shell a popup belongs to. Every shell hears every surface close, so
/// the queue takes a close only from the popup's own shell: once per surface,
/// though a text context menu's reopened popup carries the same id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Owner(u64);

impl Owner {
    pub(crate) fn unique() -> Self {
        Self(OWNERS.with(|next| {
            let id = next.get();
            next.set(id + 1);
            id
        }))
    }
}

/// Runs `f` on the thread's queue.
pub(crate) fn with<R>(f: impl FnOnce(&mut PopupQueue<Request>) -> R) -> R {
    QUEUE.with(|queue| f(&mut queue.borrow_mut()))
}

pub(crate) struct PopupQueue<T> {
    /// Popups sent to exwlshell whose surfaces are not reported closed yet,
    /// with the shells they belong to.
    live: HashMap<window::Id, Owner>,
    /// Live popups asked to go.
    closing: HashSet<window::Id>,
    /// Popups held back, with their parents and shells, in the order they
    /// were asked for.
    held: Vec<(window::Id, window::Id, Owner, T)>,
    /// Popups cancelled while held by a shell other than their own, for
    /// their own to forget.
    orphans: Vec<window::Id>,
    /// Counts the times the queue was emptied, so a wait limit set for an
    /// earlier batch cannot release a later one early.
    batch: u64,
}

/// What [`PopupQueue::open`] decided.
#[derive(Debug, PartialEq)]
pub(crate) enum Open<T> {
    /// Nothing is in the way: send it now.
    Send(T),
    /// Held back. `Some(batch)` when it is the first of its batch, which is
    /// when the wait limit for [`PopupQueue::expired`] has to be started.
    Held(Option<u64>),
}

impl<T> PopupQueue<T> {
    pub(crate) fn new() -> Self {
        Self {
            live: HashMap::new(),
            closing: HashSet::new(),
            held: Vec::new(),
            orphans: Vec::new(),
            batch: 0,
        }
    }

    /// The removal of popup `id` was asked for. Only one that was sent is
    /// waited for: nothing else will be reported closed.
    pub(crate) fn closing(&mut self, id: window::Id) {
        if self.live.contains_key(&id) {
            self.closing.insert(id);
        }
    }

    /// Popup `id` of shell `owner` is to be created on `parent` with
    /// `request`.
    pub(crate) fn open(
        &mut self,
        id: window::Id,
        parent: window::Id,
        owner: Owner,
        request: T,
    ) -> Open<T> {
        if self.closing.is_empty() && self.held.is_empty() {
            self.live.insert(id, owner);
            return Open::Send(request);
        }
        let first = self.held.is_empty();
        self.held.push((id, parent, owner, request));
        Open::Held(first.then_some(self.batch))
    }

    /// Whether popup `id` is held back.
    pub(crate) fn holds(&self, id: window::Id) -> bool {
        self.held.iter().any(|(held, ..)| *held == id)
    }

    /// Popup `id` is no longer asked for. Whether it was still held back,
    /// in which case it never reached the compositor.
    pub(crate) fn cancel(&mut self, id: window::Id) -> bool {
        let Some(at) = self.held.iter().position(|(held, ..)| *held == id) else {
            return false;
        };
        drop(self.held.remove(at));
        self.emptied();
        true
    }

    /// Surface `parent` is going: the popups held back to open on it, or on
    /// one of those, which are dropped, since exwlshell skips a popup whose
    /// parent it cannot find without a word.
    pub(crate) fn cancel_children(&mut self, parent: window::Id) -> Vec<window::Id> {
        let mut gone = vec![parent];
        let mut cancelled = Vec::new();
        while let Some(at) = self.held.iter().position(|(_, on, ..)| gone.contains(on)) {
            let (id, ..) = self.held.remove(at);
            gone.push(id);
            cancelled.push(id);
        }
        if !cancelled.is_empty() {
            self.emptied();
        }
        cancelled
    }

    /// Popup `id`, cancelled while held, belongs to another shell, which
    /// forgets it when it takes it with [`PopupQueue::take_orphans`].
    pub(crate) fn orphan(&mut self, id: window::Id) {
        self.orphans.push(id);
    }

    /// The orphans that `owns` claims.
    pub(crate) fn take_orphans(&mut self, owns: impl Fn(window::Id) -> bool) -> Vec<window::Id> {
        let (mine, others) = self.orphans.drain(..).partition(|id| owns(*id));
        self.orphans = others;
        mine
    }

    /// A new batch starts once nothing is held.
    fn emptied(&mut self) {
        if self.held.is_empty() {
            self.batch += 1;
        }
    }

    /// Shell `by` heard that surface `id` is gone: the popups that may be
    /// sent now. Only the popup's own shell's word counts.
    pub(crate) fn closed(&mut self, id: window::Id, by: Owner) -> Vec<T> {
        if self.live.get(&id) != Some(&by) {
            return Vec::new();
        }
        self.live.remove(&id);
        self.closing.remove(&id);
        if self.closing.is_empty() {
            self.release()
        } else {
            Vec::new()
        }
    }

    /// The wait limit started for `batch` ran out: whatever is still held
    /// goes now, and the removals it waited on are no longer waited for.
    pub(crate) fn expired(&mut self, batch: u64) -> Vec<T> {
        if batch != self.batch {
            return Vec::new();
        }
        self.closing.clear();
        self.release()
    }

    fn release(&mut self) -> Vec<T> {
        if self.held.is_empty() {
            return Vec::new();
        }
        self.batch += 1;
        self.held
            .drain(..)
            .map(|(id, _, owner, request)| {
                self.live.insert(id, owner);
                request
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The main application's shell, and a file chooser's nested in it.
    const HOST: Owner = Owner(u64::MAX);
    const CHOOSER: Owner = Owner(u64::MAX - 1);

    /// The main window every popup here opens on, unless it is a submenu.
    fn window() -> window::Id {
        crate::ui::window::reserved()
    }

    /// A popup already on screen, and now asked to go.
    fn going(queue: &mut PopupQueue<i32>) -> window::Id {
        let id = window::Id::unique();
        assert_eq!(queue.open(id, window(), HOST, 0), Open::Send(0));
        queue.closing(id);
        id
    }

    #[test]
    fn a_popup_with_nothing_closing_goes_at_once() {
        let mut queue = PopupQueue::new();
        assert_eq!(
            queue.open(window::Id::unique(), window(), HOST, 1),
            Open::Send(1)
        );
    }

    #[test]
    fn a_popup_waits_for_the_one_it_replaces() {
        let mut queue = PopupQueue::new();
        let old = going(&mut queue);
        assert!(matches!(
            queue.open(window::Id::unique(), window(), HOST, 1),
            Open::Held(Some(_))
        ));
        assert_eq!(queue.closed(old, HOST), [1]);
    }

    /// A removal of a popup never sent would never be reported closed.
    #[test]
    fn a_removal_of_a_popup_never_sent_is_not_waited_for() {
        let mut queue = PopupQueue::new();
        queue.closing(window::Id::unique());
        assert_eq!(
            queue.open(window::Id::unique(), window(), HOST, 1),
            Open::Send(1)
        );
    }

    /// A popup released after waiting is on screen like any other, so its
    /// own removal is waited for in turn.
    #[test]
    fn a_released_popup_is_waited_for_when_it_goes() {
        let mut queue = PopupQueue::new();
        let old = going(&mut queue);
        let released = window::Id::unique();
        let _ = queue.open(released, window(), HOST, 1);
        assert_eq!(queue.closed(old, HOST), [1]);

        queue.closing(released);
        assert!(matches!(
            queue.open(window::Id::unique(), window(), HOST, 2),
            Open::Held(_)
        ));
    }

    #[test]
    fn a_popup_waits_for_every_removal_ahead_of_it() {
        let mut queue = PopupQueue::new();
        let (menu, submenu) = (window::Id::unique(), window::Id::unique());
        assert_eq!(queue.open(menu, window(), HOST, 0), Open::Send(0));
        assert_eq!(queue.open(submenu, menu, HOST, 0), Open::Send(0));
        queue.closing(menu);
        queue.closing(submenu);
        let _ = queue.open(window::Id::unique(), window(), HOST, 1);
        assert_eq!(queue.closed(submenu, HOST), Vec::<i32>::new());
        assert_eq!(queue.closed(menu, HOST), [1]);
    }

    #[test]
    fn held_popups_go_in_the_order_asked_for() {
        let mut queue = PopupQueue::new();
        let old = going(&mut queue);
        assert!(matches!(
            queue.open(window::Id::unique(), window(), HOST, 1),
            Open::Held(Some(_))
        ));
        assert_eq!(
            queue.open(window::Id::unique(), window(), HOST, 2),
            Open::Held(None)
        );
        assert_eq!(queue.closed(old, HOST), [1, 2]);
    }

    #[test]
    fn a_popup_after_a_held_one_waits_behind_it() {
        let mut queue = PopupQueue::new();
        let _old = going(&mut queue);
        let _ = queue.open(window::Id::unique(), window(), HOST, 1);
        // Nothing new is closing, but the first is still held.
        assert!(matches!(
            queue.open(window::Id::unique(), window(), HOST, 2),
            Open::Held(_)
        ));
    }

    #[test]
    fn a_popup_cancelled_while_held_is_never_sent() {
        let mut queue = PopupQueue::new();
        let old = going(&mut queue);
        let new = window::Id::unique();
        let _ = queue.open(new, window(), HOST, 1);
        assert!(queue.cancel(new));
        assert_eq!(queue.closed(old, HOST), Vec::<i32>::new());
    }

    #[test]
    fn cancelling_a_popup_that_was_sent_is_not_a_cancel() {
        let mut queue = PopupQueue::new();
        let id = window::Id::unique();
        let _ = queue.open(id, window(), HOST, 1);
        assert!(!queue.cancel(id));
    }

    #[test]
    fn the_wait_limit_releases_what_is_held() {
        let mut queue = PopupQueue::new();
        let _old = going(&mut queue);
        let Open::Held(Some(batch)) = queue.open(window::Id::unique(), window(), HOST, 1) else {
            panic!("held as the first of its batch");
        };
        assert_eq!(queue.expired(batch), [1]);
        // The removal it gave up on no longer holds the next one back.
        assert_eq!(
            queue.open(window::Id::unique(), window(), HOST, 2),
            Open::Send(2)
        );
    }

    #[test]
    fn a_wait_limit_of_an_earlier_batch_releases_nothing() {
        let mut queue = PopupQueue::new();
        let first = going(&mut queue);
        let Open::Held(Some(old_batch)) = queue.open(window::Id::unique(), window(), HOST, 1)
        else {
            panic!("held as the first of its batch");
        };
        assert_eq!(queue.closed(first, HOST), [1]);

        let second = going(&mut queue);
        let Open::Held(Some(batch)) = queue.open(window::Id::unique(), window(), HOST, 2) else {
            panic!("held as the first of its batch");
        };
        assert_ne!(batch, old_batch);
        assert_eq!(queue.expired(old_batch), Vec::<i32>::new());
        assert_eq!(queue.closed(second, HOST), [2]);
    }

    /// The root menu is dismissed while the submenu replacing another waits:
    /// that submenu has nowhere to open.
    #[test]
    fn a_popup_held_on_a_parent_that_goes_is_dropped() {
        let mut queue = PopupQueue::new();
        let root = window::Id::unique();
        let old_sub = going(&mut queue);
        let new_sub = window::Id::unique();
        let _ = queue.open(new_sub, root, HOST, 1);
        assert_eq!(queue.cancel_children(root), [new_sub]);
        assert!(!queue.holds(new_sub));
        assert_eq!(queue.closed(old_sub, HOST), Vec::<i32>::new());
    }

    #[test]
    fn a_popup_held_on_a_held_popup_whose_parent_goes_is_dropped_too() {
        let mut queue = PopupQueue::new();
        let (root, sub, subsub) = (
            window::Id::unique(),
            window::Id::unique(),
            window::Id::unique(),
        );
        let _old = going(&mut queue);
        let _ = queue.open(sub, root, HOST, 1);
        let _ = queue.open(subsub, sub, HOST, 2);
        assert_eq!(queue.cancel_children(root), [sub, subsub]);
    }

    #[test]
    fn popups_held_on_another_parent_stay() {
        let mut queue = PopupQueue::new();
        let (root, other) = (window::Id::unique(), window::Id::unique());
        let old = going(&mut queue);
        let _ = queue.open(window::Id::unique(), window(), HOST, 1);
        assert_eq!(queue.cancel_children(root), Vec::<window::Id>::new());
        assert_eq!(queue.cancel_children(other), Vec::<window::Id>::new());
        assert_eq!(queue.closed(old, HOST), [1]);
    }

    /// Two shells, the host and a chooser, share one queue. The chooser's
    /// popup replaces the host's, which is removed through the chooser; the
    /// chooser's popup still waits for it.
    #[test]
    fn a_popup_of_one_shell_waits_for_the_removal_of_another_shells() {
        let host_menu = window::Id::unique();
        let host_window = window::Id::unique();
        let chooser_menu = window::Id::unique();
        let chooser_window = window::Id::unique();

        // The host opens its menu; the chooser, sharing the queue, asks it
        // to go and opens its own.
        with(|queue| {
            assert!(matches!(
                queue.open(host_menu, host_window, HOST, (host_menu, settings())),
                Open::Send(_)
            ));
        });
        with(|queue| {
            queue.closing(host_menu);
            assert!(matches!(
                queue.open(
                    chooser_menu,
                    chooser_window,
                    CHOOSER,
                    (chooser_menu, settings())
                ),
                Open::Held(_)
            ));
        });
        // Every shell hears of every close; the host's word releases it.
        assert!(with(|queue| queue.closed(host_menu, CHOOSER)).is_empty());
        let released = with(|queue| queue.closed(host_menu, HOST));
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].0, chooser_menu);
    }

    /// A popup held for one shell and cancelled by another is forgotten by
    /// its own shell, not by the one that cancelled it.
    #[test]
    fn a_popup_cancelled_by_another_shell_goes_back_to_its_own() {
        let mut queue = PopupQueue::new();
        let _old = going(&mut queue);
        let host_menu = window::Id::unique();
        let _ = queue.open(host_menu, window(), HOST, 1);

        // The chooser cancels it, but does not own it.
        assert!(queue.cancel(host_menu));
        queue.orphan(host_menu);
        assert_eq!(
            queue.take_orphans(|_| false),
            Vec::<window::Id>::new(),
            "the chooser does not take it"
        );
        assert_eq!(queue.take_orphans(|id| id == host_menu), [host_menu]);
        assert_eq!(queue.take_orphans(|_| true), Vec::<window::Id>::new());
    }

    fn settings() -> IcedNewPopupSettings {
        crate::ui::surface::PopupSettings {
            id: window::Id::unique(),
            parent: window(),
            positioner: crate::ui::surface::Positioner::default(),
            animate: false,
        }
        .to_exwlshell()
    }

    /// A text context menu reopens under its own id, and both shells hear
    /// the old one close: the second hearing must not forget the new one,
    /// whose own removal is then waited for like any other.
    #[test]
    fn a_close_heard_by_every_shell_counts_once_for_a_reused_id() {
        for chooser_hears_first in [false, true] {
            let mut queue = PopupQueue::new();
            let menu = window::Id::unique();
            assert_eq!(queue.open(menu, window(), HOST, 0), Open::Send(0));
            queue.closing(menu);
            assert!(matches!(queue.open(menu, window(), HOST, 1), Open::Held(_)));

            let released = if chooser_hears_first {
                assert_eq!(queue.closed(menu, CHOOSER), Vec::<i32>::new());
                queue.closed(menu, HOST)
            } else {
                let released = queue.closed(menu, HOST);
                assert_eq!(queue.closed(menu, CHOOSER), Vec::<i32>::new());
                released
            };
            assert_eq!(released, [1]);

            queue.closing(menu);
            assert!(
                matches!(
                    queue.open(window::Id::unique(), window(), HOST, 2),
                    Open::Held(_)
                ),
                "chooser heard first: {chooser_hears_first}"
            );
        }
    }
}
