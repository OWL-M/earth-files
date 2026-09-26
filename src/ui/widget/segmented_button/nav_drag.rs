// SPDX-License-Identifier: GPL-3.0-only

//! Dragging sidebar entries: where the gap opens, and how the rows move.
//!
//! The vertical [`SegmentedButton`](super::SegmentedButton) is the sidebar.
//! Some of its rows can be rearranged (the app says which), and while one of
//! them, or a folder that can be pinned, is dragged over it, a gap half a
//! row tall opens where it would land. The rows below the gap are drawn that
//! much lower, each on a spring of its own, so they glide aside as the gap
//! moves and settle when it closes.
//!
//! Everything here works on row positions in the model's order, and on each
//! row's top as laid out with no gap: `tops`, relative to the widget's top.
//! A gap is named by the row it opens above, `gap`; rows from there on are
//! drawn lower. The gap after the last row that can move names the row after
//! it, which may be one past the end.
//!
//! When a drop rearranges the sidebar, the app rebuilds the model, and the
//! rows would jump to their new places. [`NavState::expect`] records where
//! each row is on screen at the drop; once the rebuilt model is laid out,
//! [`NavState::sync`] starts each row there and springs it home.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::Rectangle;
use iced_texture_cache::iced_animate::{Spring, SpringParams};

/// How the rows move: the path-completion dropdown's spring.
const SPRING: SpringParams = SpringParams::new(0.25, Duration::from_millis(300));

/// How close to its place a row has to be to stop, in pixels.
const SETTLED: f32 = 0.5;

/// A frame longer than this is advanced as this, so a stall resumes the
/// motion where it stopped instead of landing it in one step.
const MAX_FRAME: f32 = 1.0 / 15.0;

/// How long a drop's rearrangement waits for the rebuilt model. The app
/// rebuilds in the same update that handles the drop, so anything later is a
/// drop it refused, and the next unrelated rebuild must not be animated as
/// if it were that one.
const EXPECT_FOR: Duration = Duration::from_secs(1);

/// The sidebar's drag state, kept in the widget's tree state.
#[derive(Default)]
pub(crate) struct NavState {
    /// Per row, in model order, how far below its place it is drawn.
    offsets: Vec<Spring>,
    /// When the last frame advanced the springs; `None` while they rest.
    last_frame: Option<Instant>,
    /// The row the gap opens above, while there is one.
    gap: Option<usize>,
    /// How tall the gap is.
    gap_height: f32,
    /// The drag whose folder has been checked for pinning, by
    /// [`Drag::id`](crate::ui::dnd::Drag::id), and the folder if it can be.
    pub(super) pin_checked: Option<(u64, Option<PathBuf>)>,
    /// The region the sidebar is drawn in, as last handed to `update`.
    pub(super) viewport: Rectangle,
    /// Where the rows are to start once the model has been rebuilt.
    expected: Option<Expected>,
}

/// A rearrangement a drop asked for, waiting for the model to be rebuilt.
struct Expected {
    /// Per row of the rebuilt model, the row it was before, or `None` for a
    /// row the drop added.
    from: Vec<Option<usize>>,
    /// Where each row was drawn at the drop, relative to the widget's top.
    tops: Vec<f32>,
    /// Where a row the drop added starts: the gap's top.
    added_top: f32,
    /// The labels at the drop, to tell the rebuilt model from the old one.
    labels: u64,
    at: Instant,
}

/// How a drop rearranges the rows, for [`NavState::expect`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Change {
    /// The row `from` moves to the gap above row `gap`.
    Move { from: usize, gap: usize },
    /// A row is added in the gap above row `gap`.
    Add { gap: usize },
    /// The row `from` goes.
    Remove { from: usize },
}

impl NavState {
    /// The row the gap opens above, while there is one.
    pub(super) fn gap(&self) -> Option<usize> {
        self.gap
    }

    /// How much taller the sidebar is laid out for the gap.
    pub(super) fn extra_height(&self) -> f32 {
        if self.gap.is_some() {
            self.gap_height
        } else {
            0.0
        }
    }

    /// How far below its place row `nth` is drawn now.
    pub(super) fn offset(&self, nth: usize) -> f32 {
        self.offsets.get(nth).map_or(0.0, Spring::position)
    }

    /// Open the gap above row `gap`, `height` tall, or close it. Returns
    /// whether that changed, and so the sidebar's height.
    pub(super) fn set_gap(&mut self, gap: Option<usize>, height: f32) -> bool {
        if gap == self.gap && (gap.is_none() || height == self.gap_height) {
            return false;
        }
        self.gap = gap;
        self.gap_height = height;
        self.retarget();
        true
    }

    fn retarget(&mut self) {
        for (nth, spring) in self.offsets.iter_mut().enumerate() {
            spring.set_target(match self.gap {
                Some(gap) if nth >= gap => self.gap_height,
                _ => 0.0,
            });
        }
    }

    /// Whether a row is still on its way.
    pub(super) fn is_moving(&self) -> bool {
        self.offsets
            .iter()
            .any(|spring| !spring.is_settled_within(SETTLED))
    }

    /// Advance the rows to `now`. Returns whether any is still moving, and so
    /// whether another frame is wanted.
    pub(super) fn tick(&mut self, now: Instant) -> bool {
        if !self.is_moving() {
            self.last_frame = None;
            return false;
        }
        let dt = self.last_frame.map_or(0.0, |last_frame| {
            now.saturating_duration_since(last_frame)
                .as_secs_f32()
                .min(MAX_FRAME)
        });
        for spring in &mut self.offsets {
            spring.tick(dt);
            if spring.is_settled_within(SETTLED) {
                spring.snap();
            }
        }
        let moving = self.is_moving();
        self.last_frame = moving.then_some(now);
        moving
    }

    /// Remember where the rows are drawn now, laid out at `tops`, so the
    /// model a drop is about to rebuild with `change` can start its rows
    /// there. `labels` fingerprints the model as it is at the drop; see
    /// [`labels`].
    pub(super) fn expect(&mut self, change: Change, tops: &[f32], labels: u64, now: Instant) {
        let rows = tops.len();
        let mut from: Vec<Option<usize>> = (0..rows).map(Some).collect();
        match change {
            Change::Move { from: moved, gap } => {
                if moved < rows {
                    let row = from.remove(moved);
                    let to = if gap > moved { gap - 1 } else { gap };
                    from.insert(to.min(from.len()), row);
                }
            }
            Change::Add { gap } => from.insert(gap.min(rows), None),
            Change::Remove { from: removed } => {
                if removed < rows {
                    from.remove(removed);
                }
            }
        }
        let drawn: Vec<f32> = tops
            .iter()
            .enumerate()
            .map(|(nth, top)| top + self.offset(nth))
            .collect();
        let added_top = match change {
            Change::Add { gap } => tops.get(gap).copied().unwrap_or_else(|| {
                // The gap after the last row: below it, as the next top would be.
                tops.last().map_or(0.0, |last| last + self.gap_height)
            }),
            Change::Move { .. } | Change::Remove { .. } => 0.0,
        };
        self.expected = Some(Expected {
            from,
            tops: drawn,
            added_top,
            labels,
            at: now,
        });
    }

    /// Bring the springs in line with the rows as laid out now, at `tops`,
    /// with labels fingerprinting to `labels`.
    ///
    /// A model rebuilt the way a drop said it would be starts each row where
    /// it was drawn at the drop; any other change of rows puts them straight
    /// in their places.
    pub(super) fn sync(&mut self, tops: &[f32], labels: u64, now: Instant) {
        let expected = self.expected.take();
        let expected = match expected {
            Some(expected) if now.saturating_duration_since(expected.at) > EXPECT_FOR => None,
            // Not rebuilt yet: keep waiting.
            Some(expected) if expected.labels == labels => {
                self.expected = Some(expected);
                None
            }
            expected => expected,
        };

        if let Some(expected) = expected.filter(|expected| expected.from.len() == tops.len()) {
            self.offsets = tops
                .iter()
                .zip(&expected.from)
                .map(|(top, from)| {
                    let start = from
                        .and_then(|from| expected.tops.get(from).copied())
                        .unwrap_or(expected.added_top);
                    let mut spring = Spring::new(SPRING, 0.0);
                    spring.set_target(start - top);
                    spring.snap();
                    spring
                })
                .collect();
            self.gap = None;
            self.retarget();
            return;
        }

        if self.offsets.len() != tops.len() {
            self.offsets = vec![Spring::new(SPRING, 0.0); tops.len()];
            self.retarget();
        }
    }
}

/// A fingerprint of the rows' labels, in order, which a rebuild that
/// rearranges them changes.
pub(super) fn labels<'a>(labels: impl Iterator<Item = Option<&'a str>>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for label in labels {
        label.hash(&mut hasher);
    }
    hasher.finish()
}

/// Where the gap goes for a sidebar entry dragged to `y`: the nearest gap to
/// it among the rows `run` that can move, laid out at `tops`, `height` tall.
///
/// `y` is measured as the rows are drawn, with the gap `open` pushing the
/// rows below it `gap_height` lower. Taking that back out first keeps the gap
/// from chasing the pointer: over the gap, it stays where it is.
pub(super) fn nearest_gap(
    run: &[usize],
    tops: &[f32],
    height: f32,
    open: Option<usize>,
    gap_height: f32,
    y: f32,
) -> Option<usize> {
    let y = unshift(tops, open, gap_height, y);
    gaps(run, tops, height)
        .min_by(|(_, a), (_, b)| (a - y).abs().total_cmp(&(b - y).abs()))
        .map(|(gap, _)| gap)
}

/// Where the gap goes for a folder dragged to `y` to be pinned: above a row
/// that can move when `y` is in its top quarter, below it in its bottom
/// quarter, and where it already is while `y` is inside it. `None` over the
/// middle of a row, where a drop moves the folder into it instead.
pub(super) fn pin_gap(
    run: &[usize],
    tops: &[f32],
    height: f32,
    open: Option<usize>,
    gap_height: f32,
    y: f32,
) -> Option<usize> {
    if let Some(gap) = open
        && let Some(top) = gap_top(tops, height, gap)
        && (top..top + gap_height).contains(&y)
    {
        return Some(gap);
    }
    let y = unshift(tops, open, gap_height, y);
    run.iter().find_map(|&row| {
        let top = *tops.get(row)?;
        let into = (y - top) / height;
        if (0.0..0.25).contains(&into) {
            Some(row)
        } else if (0.75..1.0).contains(&into) {
            Some(row + 1)
        } else {
            None
        }
    })
}

/// The gaps among the rows `run`, each with the `y` it sits at: above each
/// row, and below the last.
fn gaps<'a>(
    run: &'a [usize],
    tops: &'a [f32],
    height: f32,
) -> impl Iterator<Item = (usize, f32)> + 'a {
    let after = run
        .last()
        .and_then(|&last| Some((last + 1, tops.get(last)? + height)));
    run.iter()
        .filter_map(|&row| Some((row, *tops.get(row)?)))
        .chain(after)
}

/// Where the gap above row `gap` starts: at that row's place, or below the
/// last row for the gap after it.
fn gap_top(tops: &[f32], height: f32, gap: usize) -> Option<f32> {
    tops.get(gap).copied().or_else(|| {
        gap.checked_sub(1)
            .and_then(|last| Some(tops.get(last)? + height))
    })
}

/// `y` on the rows as laid out with no gap, from `y` as drawn with the gap
/// `open`: past the gap's middle, the rows are `gap_height` lower.
fn unshift(tops: &[f32], open: Option<usize>, gap_height: f32, y: f32) -> f32 {
    match open.and_then(|gap| tops.get(gap)) {
        Some(top) if y >= top + gap_height / 2.0 => y - gap_height,
        _ => y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows 32 tall, 4 apart: Recents, three pinned, and Trash below a
    /// divider. Rows 0 to 3 can move.
    const TOPS: [f32; 5] = [0.0, 36.0, 72.0, 108.0, 149.0];
    const RUN: [usize; 4] = [0, 1, 2, 3];
    const HEIGHT: f32 = 32.0;
    const GAP: f32 = 16.0;

    fn nearest(open: Option<usize>, y: f32) -> Option<usize> {
        nearest_gap(&RUN, &TOPS, HEIGHT, open, GAP, y)
    }

    fn pin(open: Option<usize>, y: f32) -> Option<usize> {
        pin_gap(&RUN, &TOPS, HEIGHT, open, GAP, y)
    }

    #[test]
    fn a_dragged_entry_takes_the_nearest_gap() {
        assert_eq!(nearest(None, 5.0), Some(0));
        assert_eq!(nearest(None, 30.0), Some(1));
        assert_eq!(nearest(None, 80.0), Some(2));
        // Below the last row that can move, and anywhere further down, the
        // gap is the one above Trash, never below it.
        assert_eq!(nearest(None, 135.0), Some(4));
        assert_eq!(nearest(None, 500.0), Some(4));
        assert_eq!(nearest(None, -50.0), Some(0));
    }

    #[test]
    fn the_gap_stays_put_under_the_pointer() {
        // The gap above row 2 spans 72..88, and row 2 is drawn at 88..120.
        assert_eq!(nearest(Some(2), 75.0), Some(2));
        assert_eq!(nearest(Some(2), 95.0), Some(2));
        // Past row 2's middle as drawn, the gap goes below it.
        assert_eq!(nearest(Some(2), 110.0), Some(3));
        // And stays there over the same spot.
        assert_eq!(nearest(Some(3), 110.0), Some(3));
    }

    #[test]
    fn a_folder_opens_the_gap_only_near_an_edge() {
        // Top and bottom quarter of row 1 (36..68).
        assert_eq!(pin(None, 40.0), Some(1));
        assert_eq!(pin(None, 64.0), Some(2));
        // Its middle is for moving the folder into it.
        assert_eq!(pin(None, 52.0), None);
        // Trash cannot be pinned around.
        assert_eq!(pin(None, 150.0), None);
        assert_eq!(pin(None, 178.0), None);
        // Below the last pinned row: the gap above Trash.
        assert_eq!(pin(None, 136.0), Some(4));
    }

    #[test]
    fn a_folder_over_the_open_gap_keeps_it() {
        // The gap above row 2 spans 72..88.
        assert_eq!(pin(Some(2), 80.0), Some(2));
        // Row 2 is drawn at 88..120: its top quarter is 88..96.
        assert_eq!(pin(Some(2), 90.0), Some(2));
        assert_eq!(pin(Some(2), 104.0), None);
    }

    fn state_with_rows(rows: usize) -> NavState {
        let mut state = NavState::default();
        state.sync(&TOPS[..rows], 0, Instant::now());
        state
    }

    fn settle(state: &mut NavState) {
        let start = Instant::now();
        for frame in 1..=120 {
            if !state.tick(start + Duration::from_millis(frame * 16)) {
                return;
            }
        }
        panic!("the rows never settled");
    }

    #[test]
    fn the_gap_springs_open_and_shut() {
        let mut state = state_with_rows(5);
        assert!(state.set_gap(Some(2), GAP));
        assert_eq!(state.extra_height(), GAP);
        assert_eq!(state.offset(2), 0.0, "it opens from shut");
        settle(&mut state);
        assert_eq!(
            (0..5).map(|nth| state.offset(nth)).collect::<Vec<_>>(),
            [0.0, 0.0, GAP, GAP, GAP]
        );

        assert!(state.set_gap(None, GAP));
        assert_eq!(state.extra_height(), 0.0);
        settle(&mut state);
        assert!((0..5).all(|nth| state.offset(nth) == 0.0));
    }

    #[test]
    fn rows_start_where_they_were_drawn_at_the_drop() {
        let mut state = state_with_rows(5);
        state.set_gap(Some(1), GAP);
        settle(&mut state);
        // Row 3 dropped in the gap above row 1: it becomes row 1.
        let now = Instant::now();
        state.expect(Change::Move { from: 3, gap: 1 }, &TOPS, 1, now);

        // Not rebuilt yet: nothing happens.
        state.sync(&TOPS, 1, now);
        assert_eq!(state.offset(1), GAP);

        state.sync(&TOPS, 2, now);
        // The moved row was drawn at 108 + 16 and now sits at 36.
        assert_eq!(state.offset(1), 108.0 + GAP - 36.0);
        // Row 1 went down to 2: it was drawn at 36 + 16 and sits at 72.
        assert_eq!(state.offset(2), 36.0 + GAP - 72.0);
        // Trash did not move, but it was drawn in the gap's shadow.
        assert_eq!(state.offset(4), GAP);
        assert_eq!(state.gap(), None);
        settle(&mut state);
        assert!((0..5).all(|nth| state.offset(nth) == 0.0));
    }

    #[test]
    fn a_pinned_folder_starts_in_the_gap() {
        let mut state = state_with_rows(4);
        state.set_gap(Some(2), GAP);
        settle(&mut state);
        let now = Instant::now();
        state.expect(Change::Add { gap: 2 }, &TOPS[..4], 1, now);
        state.sync(&TOPS, 2, now);
        // The new row 2 starts at the gap's top, which is its place.
        assert_eq!(state.offset(2), 0.0);
        // The old row 2, now 3, was drawn at 72 + 16.
        assert_eq!(state.offset(3), 72.0 + GAP - 108.0);
    }

    #[test]
    fn an_unpinned_row_closes_up() {
        let mut state = state_with_rows(5);
        let now = Instant::now();
        state.expect(Change::Remove { from: 1 }, &TOPS, 1, now);
        state.sync(&TOPS[..4], 2, now);
        // What was row 2, drawn at 72, now sits at 36.
        assert_eq!(state.offset(1), 72.0 - 36.0);
    }

    #[test]
    fn a_refused_drop_is_not_animated_by_a_later_rebuild() {
        let mut state = state_with_rows(5);
        let now = Instant::now();
        state.expect(Change::Remove { from: 1 }, &TOPS, 1, now);
        // The app kept the row: the labels never change, and the next
        // rebuild comes well after.
        state.sync(&TOPS, 1, now);
        state.sync(&TOPS[..4], 2, now + EXPECT_FOR * 2);
        assert!((0..4).all(|nth| state.offset(nth) == 0.0));
    }
}
