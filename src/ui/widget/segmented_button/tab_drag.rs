// SPDX-License-Identifier: GPL-3.0-only

//! Dragging tabs: the gap a dragged tab opens, and how the tabs move.
//!
//! The horizontal [`SegmentedButton`](super::SegmentedButton) is the tab bar.
//! Its tabs share the bar's width. A dragged tab leaves the row, and the rest
//! spread over the bar. While it is over the bar, a gap half a tab wide opens
//! where it would land, and the other tabs narrow so the gap fits. Each slot
//! between tabs has a gap spring of its own, so as the pointer moves, the gap
//! closes in one slot while it opens in the next and the tabs glide.
//!
//! Everything here is along the bar, measured from where the row of tabs
//! starts. The tabs on screen are the `order` the widget passes in, less the
//! dragged tab: the shown tabs. Slot `k` is before the `k`-th shown tab, and
//! the slot after the last is the shown count.
//!
//! Whenever the row changes all at once, the tabs would jump: when the dragged
//! tab leaves it or comes back, and when a drop reorders the tab model in
//! place. [`TabState::start_from`] starts each tab, found by its entity, where
//! it was drawn, and springs it to its new place. For a reorder that waits
//! until the new order is laid out: [`TabState::expect`] records where the
//! tabs are drawn at the drop, and [`TabState::sync`] starts them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced_texture_cache::iced_animate::Spring;

use super::model::Entity;
use super::nav_drag::{MAX_FRAME, SETTLED, SPRING};

/// How long a drop's reorder waits for the tab model to change; see
/// `nav_drag::EXPECT_FOR`.
const EXPECT_FOR: Duration = Duration::from_secs(1);

/// The tab bar's drag state, kept in the widget's tree state.
#[derive(Default)]
pub(crate) struct TabState {
    /// The dragged tab, left out of the row.
    hidden: Option<Entity>,
    /// Per slot, how wide the gap there is.
    gaps: Vec<Spring>,
    /// The slot the gap is opening in, while there is one.
    open: Option<usize>,
    /// How wide the open gap is to be.
    gap_width: f32,
    /// Tabs on their way to a new place: how far right of it, and how much
    /// wider than it, each is drawn.
    moved: HashMap<Entity, (Spring, Spring)>,
    /// When the last frame advanced the springs; `None` while they rest.
    last_frame: Option<Instant>,
    /// Where the tabs are to start once the new order is laid out.
    expected: Option<Expected>,
}

/// A reorder a drop asked for, waiting for the tab model to change.
struct Expected {
    /// The tabs on screen at the drop, the dragged one included, in order.
    order: Vec<Entity>,
    /// Where each was drawn at the drop, and how wide; the dragged one as
    /// the gap it was dropped in.
    drawn: HashMap<Entity, (f32, f32)>,
    at: Instant,
}

impl TabState {
    /// Whether the tabs are laid out by this state rather than evenly: while
    /// a tab is dragged, a gap is open or closing, or a tab is on its way to a
    /// new place.
    pub(super) fn is_active(&self) -> bool {
        self.hidden.is_some()
            || self.open.is_some()
            || !self.moved.is_empty()
            || self
                .gaps
                .iter()
                .any(|gap| gap.position() != 0.0 || gap.target() != 0.0)
    }

    /// The slot the gap is opening in, while there is one.
    pub(super) fn open(&self) -> Option<usize> {
        self.open
    }

    /// The dragged tab, left out of the row.
    pub(super) fn hidden(&self) -> Option<Entity> {
        self.hidden
    }

    /// The tabs of `order` that are in the row: all but the dragged one.
    pub(super) fn shown(&self, order: &[Entity]) -> Vec<Entity> {
        order
            .iter()
            .copied()
            .filter(|entity| Some(*entity) != self.hidden)
            .collect()
    }

    /// Open the gap in `slot` among `count` shown tabs, `width` wide, or close
    /// it with `None`. Returns whether anything changed.
    pub(super) fn set_open(&mut self, slot: Option<usize>, count: usize, width: f32) -> bool {
        let resized = self.gaps.len() != count + 1;
        if resized {
            self.gaps = vec![Spring::new(SPRING, 0.0); count + 1];
        }
        if !resized && slot == self.open && (slot.is_none() || width == self.gap_width) {
            return false;
        }
        self.open = slot;
        self.gap_width = width;
        for (k, gap) in self.gaps.iter_mut().enumerate() {
            gap.set_target(if Some(k) == slot { width } else { 0.0 });
        }
        true
    }

    /// Take `dragged` out of the row of `order`, `width` wide, and let the
    /// others spread from where they are drawn.
    pub(super) fn hide(&mut self, dragged: Entity, order: &[Entity], width: f32, spacing: f32) {
        let drawn = self.drawn(order, width, spacing);
        self.hidden = Some(dragged);
        self.open = None;
        self.gaps = vec![Spring::new(SPRING, 0.0); order.len()];
        self.start_from(&drawn, order, width, spacing);
    }

    /// Put the dragged tab back in the row of `order` where it was, growing
    /// from the gap if there is one, and from nothing otherwise.
    pub(super) fn unhide(&mut self, order: &[Entity], width: f32, spacing: f32) {
        let drawn = self.drawn(order, width, spacing);
        self.forget_drag(order.len());
        self.start_from(&drawn, order, width, spacing);
    }

    /// Leave the drag behind: no tab hidden, no gap.
    fn forget_drag(&mut self, count: usize) {
        self.hidden = None;
        self.open = None;
        self.gaps = vec![Spring::new(SPRING, 0.0); count + 1];
    }

    /// Where each tab of `order` is drawn now, and how wide. The dragged tab
    /// is drawn as the open gap, or as nothing at its own slot.
    fn drawn(&self, order: &[Entity], width: f32, spacing: f32) -> HashMap<Entity, (f32, f32)> {
        let shown = self.shown(order);
        let placed = self.layout(order, width, spacing);
        let mut drawn: HashMap<Entity, (f32, f32)> = placed
            .iter()
            .map(|(entity, x, tab)| (*entity, (*x, *tab)))
            .collect();
        if let Some(hidden) = self.hidden {
            let slot = self.open.unwrap_or_else(|| {
                order
                    .iter()
                    .position(|entity| *entity == hidden)
                    .unwrap_or(shown.len())
            });
            let gap = self
                .gaps
                .get(slot)
                .map_or(0.0, |gap| gap.position().max(0.0));
            // The gap starts where the shown tab after it, pushed right by
            // the gap, would start less the gap: right after the one before.
            let x = match placed.get(slot) {
                Some((_, x, _)) => x - gap,
                None => placed.last().map_or(0.0, |(_, x, tab)| x + tab + spacing),
            };
            drawn.insert(hidden, (x, gap));
        }
        drawn
    }

    /// Start each tab of `order` where `drawn` says it is, headed for its
    /// place in the row as laid out now.
    fn start_from(
        &mut self,
        drawn: &HashMap<Entity, (f32, f32)>,
        order: &[Entity],
        width: f32,
        spacing: f32,
    ) {
        self.moved.clear();
        let placed = self.layout(order, width, spacing);
        self.moved = placed
            .into_iter()
            .filter_map(|(entity, x, tab)| {
                let (drawn_x, drawn_width) = drawn.get(&entity)?;
                Some((
                    entity,
                    (spring_from(drawn_x - x), spring_from(drawn_width - tab)),
                ))
            })
            .collect();
    }

    /// Whether a gap or a tab is still on its way.
    pub(super) fn is_moving(&self) -> bool {
        self.gaps.iter().any(|gap| !gap.is_settled_within(SETTLED))
            || self.moved.values().any(|(x, width)| {
                !x.is_settled_within(SETTLED) || !width.is_settled_within(SETTLED)
            })
    }

    /// Advance the springs to `now`. Returns whether any is still moving, and
    /// so whether another frame is wanted.
    pub(super) fn tick(&mut self, now: Instant) -> bool {
        if !self.is_moving() {
            self.last_frame = None;
            self.moved.clear();
            return false;
        }
        let dt = self.last_frame.map_or(0.0, |last_frame| {
            now.saturating_duration_since(last_frame)
                .as_secs_f32()
                .min(MAX_FRAME)
        });
        let advance = |spring: &mut Spring| {
            spring.tick(dt);
            if spring.is_settled_within(SETTLED) {
                spring.snap();
            }
        };
        self.gaps.iter_mut().for_each(advance);
        for (x, width) in self.moved.values_mut() {
            advance(x);
            advance(width);
        }
        self.moved
            .retain(|_, (x, width)| x.position() != 0.0 || width.position() != 0.0);
        let moving = self.is_moving();
        self.last_frame = moving.then_some(now);
        moving
    }

    /// The shown tabs of `order`, each with where it is drawn now and how
    /// wide, in a row `width` wide with `spacing` between tabs.
    pub(super) fn layout(
        &self,
        order: &[Entity],
        width: f32,
        spacing: f32,
    ) -> Vec<(Entity, f32, f32)> {
        let shown = self.shown(order);
        let gap = |slot: usize| {
            self.gaps
                .get(slot)
                .map_or(0.0, |gap| gap.position().max(0.0))
        };
        let gaps: f32 = (0..=shown.len()).map(gap).sum();
        let tab = tab_width(width, spacing, shown.len(), gaps);
        let mut shift = 0.0;
        shown
            .into_iter()
            .enumerate()
            .map(|(nth, entity)| {
                shift += gap(nth);
                let mut x = (nth as f32).mul_add(tab + spacing, shift);
                let mut drawn = tab;
                if let Some((dx, dw)) = self.moved.get(&entity) {
                    x += dx.position();
                    drawn += dw.position();
                }
                (entity, x, drawn)
            })
            .collect()
    }

    /// The slot for a tab dragged to `x` among `count` shown tabs: as many as
    /// have their middle left of `x`, with the tabs laid out as the open gap
    /// will leave them. Laid out that way, the gap stays put while `x` is
    /// inside it or over either half of the tabs beside it that keeps it there.
    pub(super) fn slot_for(&self, count: usize, width: f32, spacing: f32, x: f32) -> usize {
        let open = if self.open.is_some() {
            self.gap_width
        } else {
            0.0
        };
        let tab = tab_width(width, spacing, count, open);
        (0..count)
            .filter(|&nth| {
                let shift = if self.open.is_some_and(|slot| nth >= slot) {
                    open
                } else {
                    0.0
                };
                (nth as f32).mul_add(tab + spacing, shift) + tab / 2.0 < x
            })
            .count()
    }

    /// Remember where the tabs of `order` are drawn now, the dragged one as
    /// the gap it is dropped in, for the reorder the drop is about to ask for.
    pub(super) fn expect(&mut self, order: Vec<Entity>, width: f32, spacing: f32, now: Instant) {
        let drawn = self.drawn(&order, width, spacing);
        self.expected = Some(Expected {
            order,
            drawn,
            at: now,
        });
    }

    /// Bring the tabs in line with `order`, as laid out now in a row `width`
    /// wide. Once a drop's reorder has happened, the drag is over: each tab
    /// starts where it was drawn at the drop. A reorder that never came puts
    /// the dragged tab back.
    pub(super) fn sync(&mut self, order: &[Entity], width: f32, spacing: f32, now: Instant) {
        let Some(expected) = self.expected.take() else {
            return;
        };
        if now.saturating_duration_since(expected.at) > EXPECT_FOR {
            self.unhide(order, width, spacing);
            return;
        }
        if expected.order == order {
            // Not reordered yet: keep waiting.
            self.expected = Some(expected);
            return;
        }
        self.forget_drag(order.len());
        self.start_from(&expected.drawn, order, width, spacing);
    }
}

/// A spring starting `offset` away from 0 and headed there.
fn spring_from(offset: f32) -> Spring {
    let mut spring = Spring::new(SPRING, 0.0);
    spring.set_target(offset);
    spring.snap();
    spring.set_target(0.0);
    spring
}

/// How wide each of `count` tabs is in a row `width` wide, `spacing` apart,
/// with `gaps` of it taken by gaps.
#[allow(clippy::cast_precision_loss)]
pub(super) fn tab_width(width: f32, spacing: f32, count: usize, gaps: f32) -> f32 {
    if count == 0 {
        return 0.0;
    }
    let count = count as f32;
    (spacing.mul_add(-(count - 1.0), width - gaps) / count).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slotmap::SlotMap;

    fn entities(count: usize) -> Vec<Entity> {
        let mut map = SlotMap::<Entity, ()>::with_key();
        (0..count).map(|_| map.insert(())).collect()
    }

    fn settle(state: &mut TabState) {
        let start = Instant::now();
        for frame in 1..=120 {
            if !state.tick(start + Duration::from_millis(frame * 16)) {
                return;
            }
        }
        panic!("the tabs never settled");
    }

    /// Where `entity` is drawn in `placed`, and how wide.
    fn of(placed: &[(Entity, f32, f32)], entity: Entity) -> (f32, f32) {
        placed
            .iter()
            .find(|(placed, ..)| *placed == entity)
            .map(|(_, x, width)| (*x, *width))
            .expect("the tab is in the row")
    }

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01
    }

    /// Three tabs in a row 300 wide, 0 apart: 100 each.
    const WIDTH: f32 = 300.0;

    #[test]
    fn the_slot_is_where_the_pointer_passed_the_middle_of() {
        let state = TabState::default();
        assert_eq!(state.slot_for(3, WIDTH, 0.0, 10.0), 0);
        assert_eq!(state.slot_for(3, WIDTH, 0.0, 60.0), 1);
        assert_eq!(state.slot_for(3, WIDTH, 0.0, 240.0), 2);
        assert_eq!(state.slot_for(3, WIDTH, 0.0, 290.0), 3);
    }

    #[test]
    fn the_gap_stays_put_under_the_pointer() {
        let mut state = TabState::default();
        state.set_open(Some(1), 3, 50.0);
        // Laid out for it: tabs 250 / 3 wide, the gap from 83.3 to 133.3.
        assert_eq!(state.slot_for(3, WIDTH, 0.0, 100.0), 1);
        // The left half of the tab after the gap keeps it there.
        assert_eq!(state.slot_for(3, WIDTH, 0.0, 150.0), 1);
        // Its right half moves it on.
        assert_eq!(state.slot_for(3, WIDTH, 0.0, 200.0), 2);
    }

    #[test]
    fn a_dragged_tab_leaves_the_row_and_the_rest_spread() {
        let tabs = entities(3);
        let mut state = TabState::default();
        state.hide(tabs[0], &tabs, WIDTH, 0.0);
        let placed = state.layout(&tabs, WIDTH, 0.0);
        assert_eq!(placed.len(), 2, "the dragged tab is not drawn");
        // The others start where they were.
        assert!(close(of(&placed, tabs[1]), (100.0, 100.0)));

        settle(&mut state);
        let placed = state.layout(&tabs, WIDTH, 0.0);
        assert!(close(of(&placed, tabs[1]), (0.0, 150.0)));
        assert!(close(of(&placed, tabs[2]), (150.0, 150.0)));
    }

    #[test]
    fn the_gap_opens_and_the_tabs_narrow_to_fit() {
        let tabs = entities(3);
        let mut state = TabState::default();
        assert!(state.set_open(Some(1), 3, 50.0));
        // It opens from nothing: the second tab is right after the first.
        let even = (WIDTH - 8.0) / 3.0;
        assert!((state.layout(&tabs, WIDTH, 4.0)[1].1 - (even + 4.0)).abs() < 0.01);
        settle(&mut state);
        let placed = state.layout(&tabs, WIDTH, 4.0);
        let tab = (WIDTH - 50.0 - 8.0) / 3.0;
        assert!(
            placed
                .iter()
                .all(|(_, _, width)| (width - tab).abs() < 0.01)
        );
        assert!((placed[1].1 - (tab + 4.0 + 50.0)).abs() < 0.01);
        // The row still ends where the bar does.
        let (_, x, width) = placed[2];
        assert!((x + width - WIDTH).abs() < 0.01);

        assert!(state.set_open(None, 3, 50.0));
        settle(&mut state);
        assert!(!state.is_active());
    }

    #[test]
    fn a_dropped_tab_grows_out_of_the_gap_into_its_place() {
        let tabs = entities(3);
        let mut state = TabState::default();
        state.hide(tabs[0], &tabs, WIDTH, 0.0);
        // The gap after the last of the other two.
        state.set_open(Some(2), 2, 75.0);
        settle(&mut state);
        let now = Instant::now();
        state.expect(tabs.clone(), WIDTH, 0.0, now);

        // Not reordered yet: nothing happens.
        state.sync(&tabs, WIDTH, 0.0, now);
        assert_eq!(state.hidden(), Some(tabs[0]));

        // The first tab went last.
        let reordered = [tabs[1], tabs[2], tabs[0]];
        state.sync(&reordered, WIDTH, 0.0, now);
        assert_eq!(state.hidden(), None);
        let placed = state.layout(&reordered, WIDTH, 0.0);
        // It starts as the gap was, 75 wide at the end of the row.
        assert!(close(of(&placed, tabs[0]), (225.0, 75.0)));
        assert!(close(of(&placed, tabs[1]), (0.0, 112.5)));

        settle(&mut state);
        assert!(!state.is_active());
        assert!(close(
            of(&state.layout(&reordered, WIDTH, 0.0), tabs[0]),
            (200.0, 100.0)
        ));
    }

    #[test]
    fn a_tab_put_back_grows_from_nothing_at_its_place() {
        let tabs = entities(3);
        let mut state = TabState::default();
        state.hide(tabs[1], &tabs, WIDTH, 0.0);
        settle(&mut state);
        state.unhide(&tabs, WIDTH, 0.0);
        let placed = state.layout(&tabs, WIDTH, 0.0);
        assert!(close(of(&placed, tabs[1]), (150.0, 0.0)));
        settle(&mut state);
        assert!(!state.is_active());
        assert!(close(
            of(&state.layout(&tabs, WIDTH, 0.0), tabs[1]),
            (100.0, 100.0)
        ));
    }

    #[test]
    fn a_reorder_that_never_came_puts_the_tab_back() {
        let tabs = entities(2);
        let mut state = TabState::default();
        state.hide(tabs[0], &tabs, WIDTH, 0.0);
        let now = Instant::now();
        state.expect(tabs.clone(), WIDTH, 0.0, now);
        state.sync(&tabs, WIDTH, 0.0, now + EXPECT_FOR * 2);
        assert_eq!(state.hidden(), None);
        settle(&mut state);
        assert!(!state.is_active());
    }
}
