// SPDX-License-Identifier: GPL-3.0-only

//! The long-running tasks the progress card in the bottom-right corner shows.
//!
//! File operations are shown from the moment they start; folder loads and
//! mounts only once they have run for [`SLOW`], so the quick ones never
//! flash a row. A shown task that ends shows "done" for [`DONE_LINGER`] or
//! "failed" for the longer [`FAILED_LINGER`], then takes [`LEAVE`] to slide
//! out before it is forgotten.
//!
//! An operation waiting on the user about a path it may not touch is marked
//! on its row, one question at a time: the oldest is asking, the rest wait.
//! One that failed stays until it is dismissed.
//!
//! A row that ended is held while the pointer is over it; when the pointer
//! leaves, it lingers half its time again before it slides out. A right
//! click dismisses any row that ended.
//!
//! Pure: the caller passes the time, so it can be tested without a clock.

use std::time::{Duration, Instant};

use crate::ui::widget::segmented_button::Entity;

/// How long a folder load, a mount or an unmount runs before it gets a row.
pub(crate) const SLOW: Duration = Duration::from_millis(600);

/// How long a row that succeeded stays before it slides out.
pub(crate) const DONE_LINGER: Duration = Duration::from_millis(1300);

/// How long a row that failed stays before it slides out: longer than one
/// that succeeded, so a failure is not missed.
pub(crate) const FAILED_LINGER: Duration = Duration::from_secs(3);

/// How long a row that ended stays, by whether it succeeded.
fn linger(ok: bool) -> Duration {
    if ok { DONE_LINGER } else { FAILED_LINGER }
}

/// How long a row takes to slide out.
pub(crate) const LEAVE: Duration = Duration::from_millis(300);

/// The most rows the card shows; the rest are counted instead.
pub(crate) const MAX_ROWS: usize = 4;

/// Which task a row is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// A file operation, by its id in `App::pending_operations`.
    Operation(u64),
    /// A tab's folder being read.
    Load(Entity),
    /// A drive or network location being mounted, by the mounter item's id,
    /// or for a network drive the URI typed. Two mounts of the same one at
    /// once share one row, which the first to answer ends.
    Mount(String),
    /// A drive being unmounted or ejected, by the mounter item's id.
    Unmount(String),
}

/// Where a shown task is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Running,
    /// Waiting on the user, whose question this row shows.
    Asking,
    /// Waiting on the user behind another row's question.
    Waiting,
    Done,
    Failed,
}

#[derive(Debug)]
struct Entry {
    id: u64,
    key: Key,
    label: String,
    /// When its row may appear.
    shows_at: Instant,
    /// When it ended, and whether it succeeded.
    ended: Option<(Instant, bool)>,
    /// While it waits on the user: in which turn it asked, lowest first.
    blocked: Option<u64>,
    /// Its ended row stays until it is dismissed: it failed and can be
    /// tried again from there.
    kept: bool,
    /// When its row was dismissed.
    dismissed: Option<Instant>,
    /// The pointer is over its row, which holds it.
    held: bool,
    /// When the pointer last left its row.
    released: Option<Instant>,
}

impl Entry {
    fn is_shown(&self, now: Instant) -> bool {
        now >= self.shows_at && !self.is_gone(now)
    }

    /// When it starts to slide out, if it does on its own or was dismissed.
    fn leaves_at(&self) -> Option<Instant> {
        if let Some(at) = self.dismissed {
            return Some(at);
        }
        if self.kept || self.held {
            return None;
        }
        let (ended, ok) = self.ended?;
        // Left after it ended: half its time again from then
        Some(match self.released.filter(|&at| at >= ended) {
            Some(at) => at + linger(ok) / 2,
            None => ended + linger(ok),
        })
    }

    fn is_leaving(&self, now: Instant) -> bool {
        self.leaves_at().is_some_and(|at| now >= at)
    }

    fn is_gone(&self, now: Instant) -> bool {
        self.leaves_at().is_some_and(|at| now >= at + LEAVE)
    }
}

/// A row of the card.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Row<'a> {
    pub(crate) key: &'a Key,
    /// Stays the same for as long as the row is on screen, including
    /// across a restart in place: what the view keys the row's widget
    /// state by.
    pub(crate) id: u64,
    pub(crate) label: &'a str,
    pub(crate) state: State,
    /// Past its linger: sliding out.
    pub(crate) leaving: bool,
}

/// What the card shows now.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Visible<'a> {
    /// The oldest [`MAX_ROWS`] rows, oldest first.
    pub(crate) rows: Vec<Row<'a>>,
    /// The shown rows left out. One of them can finish and spend its linger
    /// unseen, appearing part-way through it once rows above it leave.
    pub(crate) more: usize,
}

impl Visible<'_> {
    /// Every row, those left out included.
    pub(crate) fn total(&self) -> usize {
        self.rows.len() + self.more
    }

    /// Whether every row is sliding out, so the card goes with them. True
    /// when there are no rows; the caller shows no card then.
    pub(crate) fn leaving(&self) -> bool {
        self.more == 0 && self.rows.iter().all(|row| row.leaving)
    }
}

/// The tasks being tracked, in the order they started.
#[derive(Debug, Default)]
pub(crate) struct Tasks {
    entries: Vec<Entry>,
    next_id: u64,
    /// The turn the next task to ask gets.
    next_turn: u64,
}

impl Tasks {
    /// Task `key` started now, to be shown after `delay`. One already
    /// tracked under `key` and currently shown keeps its row and place,
    /// relabelled and shown at once, so restarting it (e.g. a folder load
    /// on a re-navigate) does not hide it then bring it back at the end;
    /// otherwise it is replaced, to be shown after `delay` as usual.
    pub(crate) fn start(&mut self, key: Key, label: String, delay: Duration, now: Instant) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.key == key && entry.is_shown(now))
        {
            entry.label = label;
            entry.shows_at = now;
            entry.ended = None;
            entry.blocked = None;
            entry.kept = false;
            entry.dismissed = None;
            entry.held = false;
            entry.released = None;
            return;
        }
        self.entries.retain(|entry| entry.key != key);
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(Entry {
            id,
            key,
            label,
            shows_at: now + delay,
            ended: None,
            blocked: None,
            kept: false,
            dismissed: None,
            held: false,
            released: None,
        });
    }

    /// Task `key` waits on the user. It asks once every task that asked
    /// before it has had its answer.
    pub(crate) fn block(&mut self, key: &Key) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.key == *key && entry.ended.is_none())
        {
            entry.blocked = Some(self.next_turn);
            self.next_turn += 1;
        }
    }

    /// Task `key` has its answer and runs on.
    pub(crate) fn unblock(&mut self, key: &Key) {
        for entry in self.entries.iter_mut().filter(|entry| entry.key == *key) {
            entry.blocked = None;
        }
    }

    /// Task `key` failed, and its row stays until [`Self::dismiss`], so its
    /// reason can be read and it can be tried again.
    pub(crate) fn finish_failed_kept(&mut self, key: &Key, label: Option<String>, now: Instant) {
        self.finish_kept(key, false, true, label, now);
    }

    /// Task `key` failed: its row says so and stays until closed. A task
    /// without a row showing gets one: one that never showed progress, or
    /// one whose row the user closed with Cancel before its failure came.
    pub(crate) fn fail_kept(&mut self, key: &Key, label: String, now: Instant) {
        let shown = self
            .entries
            .iter()
            .any(|entry| entry.key == *key && entry.ended.is_none() && entry.is_shown(now));
        if !shown {
            self.start(key.clone(), label.clone(), Duration::ZERO, now);
        }
        self.finish_failed_kept(key, Some(label), now);
    }

    fn finish_kept(
        &mut self,
        key: &Key,
        ok: bool,
        kept: bool,
        label: Option<String>,
        now: Instant,
    ) {
        self.finish(key, ok, label, now);
        if let Some(entry) = self.ended_now(key, now) {
            entry.kept = kept;
        }
    }

    /// The entry for task `key` that [`Self::finish`] just ended at `now`.
    fn ended_now(&mut self, key: &Key, now: Instant) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|entry| entry.key == *key && entry.ended.is_some_and(|(at, _)| at == now))
    }

    /// The user closed row `id`, which had ended: it slides out now.
    pub(crate) fn dismiss(&mut self, id: u64, now: Instant) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == id && entry.ended.is_some())
        {
            entry.dismissed.get_or_insert(now);
        }
    }

    /// The pointer came over row `id` (`over`), or left it, at `now`.
    pub(crate) fn hover(&mut self, id: u64, over: bool, now: Instant) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            entry.held = over;
            if !over {
                entry.released = Some(now);
            }
        }
    }

    /// Task `key` ended. One not shown yet is forgotten at once; a shown
    /// one lingers, relabelled with `label` if given.
    pub(crate) fn finish(&mut self, key: &Key, ok: bool, label: Option<String>, now: Instant) {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.key == *key && entry.ended.is_none())
        else {
            return;
        };
        self.entries[index].blocked = None;
        if now < self.entries[index].shows_at {
            self.entries.remove(index);
            return;
        }
        let entry = &mut self.entries[index];
        entry.ended = Some((now, ok));
        if let Some(label) = label {
            entry.label = label;
        }
    }

    /// The task asking the user now: of those waiting on the user, the
    /// first to ask.
    pub(crate) fn asking(&self) -> Option<&Key> {
        self.entries
            .iter()
            .filter(|entry| entry.ended.is_none())
            .filter_map(|entry| entry.blocked.map(|turn| (turn, &entry.key)))
            .min_by_key(|(turn, _)| *turn)
            .map(|(_, key)| key)
    }

    /// Forgets task `key` at once, shown or not, without the slide: for
    /// when the user asked for it to go (a cancel, a closed tab).
    pub(crate) fn remove(&mut self, key: &Key) {
        self.entries.retain(|entry| entry.key != *key);
    }

    /// Forgets the tasks whose rows have slid out.
    pub(crate) fn prune(&mut self, now: Instant) {
        self.entries.retain(|entry| !entry.is_gone(now));
    }

    /// The next moment after `now` at which what the card shows changes on
    /// its own: a waiting task's row appearing, a finished row starting to
    /// slide out, or one having slid out. `None` when nothing will change
    /// until a task starts or ends, as while every shown task is running.
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.entries
            .iter()
            .flat_map(|entry| {
                let waiting = (entry.ended.is_none()).then_some(entry.shows_at);
                let leaving = entry
                    .leaves_at()
                    .map(|at| [at, at + LEAVE])
                    .into_iter()
                    .flatten();
                waiting.into_iter().chain(leaving)
            })
            .filter(|&at| at > now)
            .min()
    }

    /// Whether anything is tracked, shown or waiting to be. Only the tests
    /// ask: the app asks [`Self::next_deadline`] instead.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The rows to show at `now`. The row asking the user is never left
    /// out: counted under `more`, it goes first instead.
    pub(crate) fn visible(&self, now: Instant) -> Visible<'_> {
        let mut shown: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| entry.is_shown(now))
            .collect();
        let asking = shown
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.blocked.map(|turn| (turn, index)))
            .min()
            .map(|(_, index)| index);
        if let Some(index) = asking
            && index >= MAX_ROWS
        {
            let entry = shown.remove(index);
            shown.insert(0, entry);
        }
        let asking_id = asking.map(|index| {
            if index >= MAX_ROWS {
                shown[0].id
            } else {
                shown[index].id
            }
        });
        let more = shown.len().saturating_sub(MAX_ROWS);
        let rows = shown
            .into_iter()
            .take(MAX_ROWS)
            .map(|entry| Row {
                key: &entry.key,
                id: entry.id,
                label: &entry.label,
                state: match (entry.ended, entry.blocked) {
                    (None, Some(_)) if Some(entry.id) == asking_id => State::Asking,
                    (None, Some(_)) => State::Waiting,
                    (None, None) => State::Running,
                    (Some((_, true)), _) => State::Done,
                    (Some((_, false)), _) => State::Failed,
                },
                leaving: entry.is_leaving(now),
            })
            .collect();
        Visible { rows, more }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(n: u64) -> Key {
        Key::Operation(n)
    }

    fn labels(tasks: &Tasks, now: Instant) -> Vec<&str> {
        tasks
            .visible(now)
            .rows
            .iter()
            .map(|row| row.label)
            .collect()
    }

    fn states(tasks: &Tasks, now: Instant) -> Vec<(&str, State)> {
        tasks
            .visible(now)
            .rows
            .iter()
            .map(|row| (row.label, row.state))
            .collect()
    }

    #[test]
    fn the_first_to_ask_asks_and_the_others_wait() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        for (n, label) in [(1, "a"), (2, "b"), (3, "c")] {
            tasks.start(op(n), label.into(), Duration::ZERO, now);
        }
        tasks.block(&op(3));
        tasks.block(&op(1));
        assert_eq!(
            states(&tasks, now),
            [
                ("a", State::Waiting),
                ("b", State::Running),
                ("c", State::Asking)
            ]
        );

        tasks.unblock(&op(3));
        assert_eq!(
            states(&tasks, now),
            [
                ("a", State::Asking),
                ("b", State::Running),
                ("c", State::Running)
            ]
        );
    }

    #[test]
    fn a_row_asking_from_under_more_goes_first() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        for n in 0..=MAX_ROWS as u64 {
            tasks.start(op(n), n.to_string(), Duration::ZERO, now);
        }
        let last = MAX_ROWS as u64;
        tasks.block(&op(last));

        let visible = tasks.visible(now);
        assert_eq!(visible.more, 1);
        assert_eq!(visible.rows.len(), MAX_ROWS);
        assert_eq!(visible.rows[0].label, last.to_string());
        assert_eq!(visible.rows[0].state, State::Asking);
        assert_eq!(visible.rows[1].label, "0", "the others keep their order");
    }

    #[test]
    fn a_held_row_waits_and_then_lingers_half_its_time() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "copying".into(), Duration::ZERO, now);
        let id = tasks.visible(now).rows[0].id;
        tasks.finish(&op(1), true, Some("copied".into()), now);

        // Held well past its time: still there, not leaving
        tasks.hover(id, true, now);
        let later = now + DONE_LINGER * 5;
        assert!(!tasks.visible(later).rows[0].leaving);
        assert_eq!(tasks.next_deadline(later), None);

        // Let go: half its time again, then it slides out
        tasks.hover(id, false, later);
        assert_eq!(tasks.next_deadline(later), Some(later + DONE_LINGER / 2));
        assert!(tasks.visible(later + DONE_LINGER / 2).rows[0].leaving);
    }

    #[test]
    fn a_right_click_dismisses_an_ended_row_only() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "running".into(), Duration::ZERO, now);
        let id = tasks.visible(now).rows[0].id;
        tasks.dismiss(id, now);
        assert!(!tasks.visible(now).rows[0].leaving, "a running row stays");

        tasks.finish(&op(1), true, None, now);
        tasks.dismiss(id, now);
        assert!(tasks.visible(now).rows[0].leaving);
    }

    #[test]
    fn the_task_asking_is_the_first_to_ask() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "a".into(), Duration::ZERO, now);
        tasks.start(op(2), "b".into(), Duration::ZERO, now);
        assert_eq!(tasks.asking(), None);
        tasks.block(&op(2));
        tasks.block(&op(1));
        assert_eq!(tasks.asking(), Some(&op(2)));
        tasks.finish(&op(2), true, None, now);
        assert_eq!(tasks.asking(), Some(&op(1)));
    }

    /// A failure always has a row: here for a task that never had one
    #[test]
    fn a_failure_without_a_row_gets_one() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.fail_kept(&op(1), "rename".into(), now);
        assert_eq!(
            states(&tasks, now + FAILED_LINGER * 10),
            [("rename", State::Failed)]
        );
    }

    #[test]
    fn a_kept_failed_row_stays_until_dismissed() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "copying".into(), Duration::ZERO, now);
        tasks.finish_failed_kept(&op(1), Some("copy".into()), now);

        let later = now + FAILED_LINGER * 10;
        let row = |tasks: &Tasks, at| {
            let visible = tasks.visible(at);
            visible
                .rows
                .first()
                .map(|row| (row.id, row.state, row.leaving))
        };
        let (id, state, leaving) = row(&tasks, later).expect("still shown");
        assert_eq!((state, leaving), (State::Failed, false));
        assert_eq!(tasks.next_deadline(now), None);
        tasks.dismiss(id, later);
        assert_eq!(
            row(&tasks, later).map(|(_, _, leaving)| leaving),
            Some(true)
        );
    }

    #[test]
    fn a_finish_ends_the_asking() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "a".into(), Duration::ZERO, now);
        tasks.block(&op(1));
        tasks.finish(&op(1), false, None, now);
        assert_eq!(states(&tasks, now), [("a", State::Failed)]);
    }

    #[test]
    fn a_task_without_a_delay_shows_at_once() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "copy".into(), Duration::ZERO, now);
        assert_eq!(labels(&tasks, now), ["copy"]);
    }

    #[test]
    fn a_slow_task_shows_only_after_its_delay() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(Key::Mount("usb".into()), "mount".into(), SLOW, now);
        assert!(labels(&tasks, now + SLOW / 2).is_empty());
        assert_eq!(labels(&tasks, now + SLOW), ["mount"]);
    }

    #[test]
    fn a_task_that_ends_before_its_delay_never_shows() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(Key::Mount("usb".into()), "mount".into(), SLOW, now);
        tasks.finish(&Key::Mount("usb".into()), true, None, now + SLOW / 2);
        assert!(tasks.is_empty());
    }

    #[test]
    fn a_finished_row_lingers_then_slides_out_then_goes() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "copying".into(), Duration::ZERO, now);
        tasks.finish(&op(1), true, Some("copied".into()), now);

        let row = |at| {
            let visible = tasks.visible(at);
            visible
                .rows
                .first()
                .map(|row| (row.label.to_owned(), row.state, row.leaving))
        };
        assert_eq!(row(now), Some(("copied".into(), State::Done, false)));
        assert_eq!(
            row(now + DONE_LINGER),
            Some(("copied".into(), State::Done, true))
        );
        assert_eq!(row(now + DONE_LINGER + LEAVE), None);

        tasks.prune(now + DONE_LINGER + LEAVE);
        assert!(tasks.is_empty());
    }

    #[test]
    fn a_failed_task_is_failed() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "copying".into(), Duration::ZERO, now);
        tasks.finish(&op(1), false, None, now);
        assert_eq!(
            tasks.visible(now).rows.first().map(|row| row.state),
            Some(State::Failed)
        );
    }

    #[test]
    fn prune_keeps_what_is_still_shown() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "a".into(), Duration::ZERO, now);
        tasks.finish(&op(1), true, None, now);
        tasks.prune(now + DONE_LINGER);
        assert_eq!(labels(&tasks, now + DONE_LINGER), ["a"]);
    }

    #[test]
    fn rows_are_the_oldest_first_and_the_rest_are_counted() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        for n in 0..6 {
            tasks.start(op(n), format!("{n}"), Duration::ZERO, now);
        }
        let visible = tasks.visible(now);
        assert_eq!(
            visible.rows.iter().map(|row| row.label).collect::<Vec<_>>(),
            ["0", "1", "2", "3"]
        );
        assert_eq!(visible.more, 2);
        assert_eq!(visible.total(), 6);
    }

    #[test]
    fn a_task_started_again_replaces_itself() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        let key = Key::Load(Entity::default());
        tasks.start(key.clone(), "a".into(), SLOW, now);
        tasks.start(key, "b".into(), SLOW, now + SLOW / 2);
        assert!(labels(&tasks, now + SLOW).is_empty());
        assert_eq!(labels(&tasks, now + SLOW / 2 + SLOW), ["b"]);
    }

    #[test]
    fn a_removed_task_is_gone_at_once() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "a".into(), Duration::ZERO, now);
        tasks.remove(&op(1));
        assert!(tasks.is_empty());
    }

    #[test]
    fn the_card_leaves_only_when_every_row_does() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "a".into(), Duration::ZERO, now);
        tasks.start(op(2), "b".into(), Duration::ZERO, now);
        tasks.finish(&op(1), true, None, now);
        assert!(!tasks.visible(now + DONE_LINGER).leaving());
        tasks.finish(&op(2), true, None, now);
        assert!(tasks.visible(now + DONE_LINGER).leaving());
    }

    #[test]
    fn a_shown_task_started_again_stays_shown_in_place() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        let key = Key::Load(Entity::default());
        tasks.start(key.clone(), "a".into(), SLOW, now);
        tasks.start(op(2), "b".into(), Duration::ZERO, now);
        let later = now + SLOW;
        assert_eq!(labels(&tasks, later), ["a", "b"]);

        tasks.start(key, "a2".into(), SLOW, later);
        assert_eq!(labels(&tasks, later), ["a2", "b"]);
    }

    #[test]
    fn the_card_stays_while_rows_are_hidden_under_more() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        for n in 0..5 {
            tasks.start(op(n), format!("{n}"), Duration::ZERO, now);
        }
        for n in 0..4 {
            tasks.finish(&op(n), true, None, now);
        }
        assert!(!tasks.visible(now + DONE_LINGER).leaving());
    }

    #[test]
    fn a_second_finish_changes_nothing() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "copying".into(), Duration::ZERO, now);
        tasks.finish(&op(1), true, Some("copied".into()), now);
        tasks.finish(
            &op(1),
            false,
            Some("x".into()),
            now + Duration::from_secs(1),
        );

        assert_eq!(
            tasks
                .visible(now)
                .rows
                .first()
                .map(|row| (row.label.to_owned(), row.state)),
            Some(("copied".into(), State::Done))
        );
        assert!(tasks.visible(now + DONE_LINGER + LEAVE).rows.is_empty());
    }

    #[test]
    fn a_task_still_waiting_is_not_counted_in_more() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        for n in 0..4 {
            tasks.start(op(n), format!("{n}"), Duration::ZERO, now);
        }
        tasks.start(Key::Mount("usb".into()), "mount".into(), SLOW, now);
        let visible = tasks.visible(now);
        assert_eq!(visible.rows.len(), 4);
        assert_eq!(visible.more, 0);
    }

    #[test]
    fn a_finished_row_started_again_runs_again_in_place() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "a".into(), Duration::ZERO, now);
        tasks.finish(&op(1), true, None, now);

        let later = now + DONE_LINGER;
        tasks.start(op(1), "a again".into(), Duration::ZERO, later);

        let row = |at| {
            let visible = tasks.visible(at);
            visible.rows.first().map(|row| (row.state, row.leaving))
        };
        assert_eq!(row(later), Some((State::Running, false)));
        assert_eq!(row(later + LEAVE), Some((State::Running, false)));
    }

    #[test]
    fn a_row_keeps_its_id_when_started_again_in_place() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "a".into(), Duration::ZERO, now);
        tasks.start(op(2), "b".into(), Duration::ZERO, now);
        let rows = tasks.visible(now).rows;
        let (id_a, id_b) = (rows[0].id, rows[1].id);
        assert_ne!(id_a, id_b);

        tasks.start(op(1), "a again".into(), Duration::ZERO, now);
        let rows = tasks.visible(now).rows;
        assert_eq!(rows[0].id, id_a);
        assert_eq!(rows[1].id, id_b);
    }

    #[test]
    fn the_next_deadline_of_a_waiting_task_is_when_it_shows() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(Key::Mount("usb".into()), "mount".into(), SLOW, now);
        assert_eq!(tasks.next_deadline(now), Some(now + SLOW));
    }

    #[test]
    fn a_shown_running_task_has_no_next_deadline() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "copy".into(), Duration::ZERO, now);
        assert_eq!(tasks.next_deadline(now), None);
        tasks.start(Key::Mount("usb".into()), "mount".into(), SLOW, now);
        assert_eq!(tasks.next_deadline(now + SLOW), None);
    }

    #[test]
    fn an_ended_task_next_starts_leaving_then_is_gone() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "copy".into(), Duration::ZERO, now);
        tasks.finish(&op(1), true, None, now);
        assert_eq!(tasks.next_deadline(now), Some(now + DONE_LINGER));
        assert_eq!(
            tasks.next_deadline(now + DONE_LINGER),
            Some(now + DONE_LINGER + LEAVE)
        );
        assert_eq!(tasks.next_deadline(now + DONE_LINGER + LEAVE), None);
    }

    #[test]
    fn the_next_deadline_is_the_earliest_of_several() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "copy".into(), Duration::ZERO, now);
        tasks.finish(&op(1), true, None, now);
        tasks.start(Key::Mount("usb".into()), "mount".into(), SLOW, now);
        tasks.start(Key::Load(Entity::default()), "load".into(), SLOW * 2, now);
        assert_eq!(tasks.next_deadline(now), Some(now + SLOW));
    }

    #[test]
    fn a_replaced_task_gets_a_new_id() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "a".into(), Duration::ZERO, now);
        let first_id = tasks.visible(now).rows.first().unwrap().id;

        tasks.remove(&op(1));
        tasks.start(op(1), "a again".into(), Duration::ZERO, now);
        let second_id = tasks.visible(now).rows.first().unwrap().id;

        assert_ne!(first_id, second_id);
    }

    #[test]
    fn a_failed_row_stays_longer_than_a_done_one() {
        let now = Instant::now();
        let mut tasks = Tasks::default();
        tasks.start(op(1), "a".into(), Duration::ZERO, now);
        tasks.start(op(2), "b".into(), Duration::ZERO, now);
        tasks.finish(&op(1), true, None, now);
        tasks.finish(&op(2), false, None, now);

        let at = now + DONE_LINGER + LEAVE;
        assert_eq!(labels(&tasks, at), ["b"]);
        assert!(!tasks.visible(at).leaving());
        assert_eq!(
            tasks.next_deadline(at),
            Some(now + FAILED_LINGER),
            "the failed row starts leaving at its own linger"
        );
        assert!(labels(&tasks, now + FAILED_LINGER + LEAVE).is_empty());
    }
}
