// SPDX-License-Identifier: GPL-3.0-only

//! Rows that unfold in place: a row with other ways to do its job ends in a
//! count, and its other ways sit folded right under it, in the same menu.
//! Here: which rows belong to which group, which rows show, where the count
//! is, and what the keys do to a group.

use std::time::Instant;

use iced_core::{Point, Rectangle};
use iced_texture_cache::iced_animate::Spring;

use crate::ui::widget::spring_height::{MAX_FRAME, SETTLED, SPRING};

/// A row's part in a group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Group {
    /// A row on its own.
    #[default]
    None,
    /// A row with other ways: the next `rows` rows are its group, `count`
    /// of them actions (the rest dividers).
    Head { rows: usize, count: usize },
    /// One of the rows of the group of the head above it.
    Member,
}

/// Room the count takes at a group row's end, before [`CHIP_RIGHT`].
pub const CHIP_SLOT: f32 = 36.0;
/// Gap between a group row's right edge and its count.
pub const CHIP_RIGHT: f32 = 12.0;
/// The count's height.
pub const CHIP_HEIGHT: f32 = 20.0;
/// The count's least width.
pub const CHIP_MIN: f32 = 28.0;
/// The count's text size.
pub const CHIP_TEXT: f32 = 12.0;
/// Where the guide line of an unfolded group runs, from the menu's left.
pub const GUIDE_X: f32 = 23.0;

/// For each row, the head of the group it belongs to.
#[must_use]
pub fn heads(groups: &[Group]) -> Vec<Option<usize>> {
    let mut out = vec![None; groups.len()];
    for (i, group) in groups.iter().enumerate() {
        if let Group::Head { rows, .. } = group {
            for member in out.iter_mut().skip(i + 1).take(*rows) {
                *member = Some(i);
            }
        }
    }
    out
}

/// Which rows show while the group of `unfolded` is unfolded: every row but
/// the members of other groups.
#[must_use]
pub fn shown(groups: &[Group], unfolded: Option<usize>) -> Vec<bool> {
    heads(groups)
        .into_iter()
        .map(|head| head.is_none() || head == unfolded)
        .collect()
}

/// For each row, whether row `index` is part of the group headed by
/// `head`: the row itself or one of its rows.
#[must_use]
pub fn in_group(groups: &[Group], head: usize, index: usize) -> bool {
    match groups.get(head) {
        Some(Group::Head { rows, .. }) => (head..=head + rows).contains(&index),
        _ => false,
    }
}

/// The count drawn `width` wide on the group row at `row`.
#[must_use]
pub fn chip_rect(row: Rectangle, width: f32) -> Rectangle {
    Rectangle {
        x: row.x + row.width - CHIP_RIGHT - width,
        y: row.center_y() - CHIP_HEIGHT / 2.0,
        width,
        height: CHIP_HEIGHT,
    }
}

/// How wide the count is with `label` in it.
#[must_use]
pub fn chip_width(label: &str) -> f32 {
    let text = crate::ui::font::text_width(label, CHIP_TEXT, crate::ui::font::default());
    (text + 14.0).clamp(CHIP_MIN, CHIP_SLOT)
}

/// The count of the group row at `row`, which has `count` other ways,
/// unfolded or not: where it is drawn, and where the pointer is on it.
#[must_use]
pub fn chip(row: Rectangle, count: usize, unfolded: bool) -> Rectangle {
    chip_rect(row, chip_width(&chip_label(count, unfolded)))
}

/// Whether `cursor` is on that count.
#[must_use]
pub fn on_chip(row: Rectangle, count: usize, unfolded: bool, cursor: Point) -> bool {
    chip(row, count, unfolded).contains(cursor)
}

/// What the count reads: how many other ways, or that they show.
#[must_use]
pub fn chip_label(count: usize, unfolded: bool) -> String {
    if unfolded {
        String::from("−")
    } else {
        format!("+{count}")
    }
}

/// A key that folds and unfolds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldKey {
    /// → unfolds the chosen row's group and goes into it.
    Right,
    /// ← folds the group the chosen row is in or heads.
    Left,
    /// Space unfolds a folded group's row, and folds an unfolded group.
    Space,
}

/// What `key` does with row `index` chosen while `unfolded` is unfolded:
/// the group unfolded after, and the row chosen after. `None` when it does
/// nothing here.
#[must_use]
pub fn on_key(
    groups: &[Group],
    selectable: &[bool],
    index: usize,
    unfolded: Option<usize>,
    key: FoldKey,
) -> Option<(Option<usize>, usize)> {
    let head = match groups.get(index)? {
        Group::Head { .. } => index,
        Group::Member => heads(groups)[index]?,
        Group::None => return None,
    };
    let is_open = unfolded == Some(head);
    let unfold = || {
        let Group::Head { rows, .. } = groups[head] else {
            return (Some(head), head);
        };
        let first = (head + 1..=head + rows).find(|i| selectable.get(*i).copied().unwrap_or(false));
        (Some(head), first.unwrap_or(head))
    };
    match key {
        FoldKey::Right if index == head => Some(unfold()),
        FoldKey::Right => None,
        FoldKey::Left | FoldKey::Space if is_open => Some((None, head)),
        FoldKey::Left => None,
        FoldKey::Space => Some(unfold()),
    }
}

/// A group opening or closing: how much of its rows shows, on the spring
/// the app's other unrolling boxes use.
#[derive(Clone, Debug)]
pub struct Reveal {
    /// The group's row.
    pub head: usize,
    /// How many rows follow it in its group.
    pub rows: usize,
    /// The height of those rows, all showing.
    pub full: f32,
    /// Whether it is closing.
    pub folding: bool,
    spring: Spring,
    last: Option<Instant>,
}

impl Reveal {
    /// The group of `head`, its `rows` `full` tall, opening from `from`
    /// showing.
    #[must_use]
    pub fn unfold(head: usize, rows: usize, full: f32, from: f32) -> Self {
        Self::moving(head, rows, full, from, full)
    }

    /// The group of `head`, its `rows` `full` tall, closing from `from`
    /// showing.
    #[must_use]
    pub fn fold(head: usize, rows: usize, full: f32, from: f32) -> Self {
        let mut reveal = Self::moving(head, rows, full, from, 0.0);
        reveal.folding = true;
        reveal
    }

    fn moving(head: usize, rows: usize, full: f32, from: f32, to: f32) -> Self {
        let mut spring = Spring::new(SPRING, from);
        spring.set_target(to);
        Self {
            head,
            rows,
            full,
            folding: false,
            spring,
            last: None,
        }
    }

    /// Whether row `index` is one of the group's rows (not its row).
    #[must_use]
    pub fn contains(&self, index: usize) -> bool {
        index > self.head && index <= self.head + self.rows
    }

    /// How much of the rows shows, never more than all of them.
    #[must_use]
    pub fn shown(&self) -> f32 {
        self.spring.position().clamp(0.0, self.full)
    }

    /// How much of the rows is hidden.
    #[must_use]
    pub fn hidden(&self) -> f32 {
        self.full - self.shown()
    }

    /// Whether it is still on its way.
    #[must_use]
    pub fn is_moving(&self) -> bool {
        !self.spring.is_settled_within(SETTLED)
    }

    /// Moves it on to `now`; the first frame only starts the clock, and
    /// returns whether the visible height changed.
    pub fn advance(&mut self, now: Instant) -> bool {
        let dt = self.last.map_or(0.0, |last| {
            now.saturating_duration_since(last)
                .as_secs_f32()
                .min(MAX_FRAME)
        });
        let before = self.shown();
        self.spring.tick(dt);
        if self.spring.is_settled_within(SETTLED) {
            self.spring.snap();
        }
        self.last = Some(now);
        (self.shown() - before).abs() > f32::EPSILON
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::Size;

    /// Open, Cut ▸ (Move to), Copy ▸ (path, divider, to), Paste
    fn menu() -> Vec<Group> {
        vec![
            Group::None,
            Group::Head { rows: 1, count: 1 },
            Group::Member,
            Group::Head { rows: 3, count: 2 },
            Group::Member,
            Group::Member,
            Group::Member,
            Group::None,
        ]
    }

    #[test]
    fn rows_belong_to_the_group_above_them() {
        assert_eq!(
            heads(&menu()),
            [None, None, Some(1), None, Some(3), Some(3), Some(3), None]
        );
    }

    #[test]
    fn only_the_unfolded_group_shows_its_rows() {
        assert_eq!(
            shown(&menu(), None),
            [true, true, false, true, false, false, false, true]
        );
        assert_eq!(
            shown(&menu(), Some(3)),
            [true, true, false, true, true, true, true, true]
        );
    }

    #[test]
    fn the_count_sits_at_the_row_end() {
        let row = Rectangle::new(Point::new(0.0, 100.0), Size::new(360.0, 36.0));
        let chip = chip(row, 2, false);
        assert_eq!(chip.x + chip.width, 348.0, "12 from the row's end");
        assert_eq!((chip.y, chip.height), (108.0, 20.0), "centred, 20 tall");
        assert!(chip.width >= CHIP_MIN && chip.width <= CHIP_SLOT);
        assert_eq!(chip_label(2, false), "+2");
        assert_eq!(chip_label(2, true), "−");
    }

    /// Only the count itself is on the count: not the rest of the row's end.
    #[test]
    fn the_pointer_is_on_the_count_only_over_it() {
        let row = Rectangle::new(Point::new(0.0, 100.0), Size::new(360.0, 36.0));
        let chip = chip(row, 2, false);
        assert!(on_chip(row, 2, false, chip.center()));
        assert!(
            !on_chip(row, 2, false, Point::new(chip.center().x, 102.0)),
            "above it"
        );
        assert!(
            !on_chip(row, 2, false, Point::new(354.0, 118.0)),
            "after it"
        );
        assert!(
            !on_chip(row, 2, false, Point::new(chip.x - 3.0, 118.0)),
            "before it"
        );
    }

    #[test]
    fn a_group_is_its_row_and_its_rows() {
        let groups = menu();
        assert!(in_group(&groups, 3, 3));
        assert!(in_group(&groups, 3, 6));
        assert!(!in_group(&groups, 3, 7));
        assert!(!in_group(&groups, 3, 2));
        assert!(!in_group(&groups, 0, 0), "not a group row");
    }

    #[test]
    fn right_unfolds_and_goes_in_and_left_folds_back() {
        let groups = menu();
        // Copy path, the divider, Copy to
        let selectable = [true, true, true, true, true, false, true, true];
        assert_eq!(
            on_key(&groups, &selectable, 3, None, FoldKey::Right),
            Some((Some(3), 4))
        );
        // From another group's row it folds that one
        assert_eq!(
            on_key(&groups, &selectable, 1, Some(3), FoldKey::Right),
            Some((Some(1), 2))
        );
        assert_eq!(
            on_key(&groups, &selectable, 6, Some(3), FoldKey::Left),
            Some((None, 3))
        );
        assert_eq!(
            on_key(&groups, &selectable, 3, Some(3), FoldKey::Left),
            Some((None, 3))
        );
        assert_eq!(on_key(&groups, &selectable, 3, None, FoldKey::Left), None);
        assert_eq!(on_key(&groups, &selectable, 0, None, FoldKey::Right), None);
        assert_eq!(
            on_key(&groups, &selectable, 7, Some(3), FoldKey::Left),
            None
        );
    }

    #[test]
    fn right_on_an_unfolded_row_goes_into_it() {
        let selectable = [true; 8];
        assert_eq!(
            on_key(&menu(), &selectable, 3, Some(3), FoldKey::Right),
            Some((Some(3), 4))
        );
    }

    #[test]
    fn space_folds_and_unfolds() {
        let groups = menu();
        let selectable = [true; 8];
        assert_eq!(
            on_key(&groups, &selectable, 3, None, FoldKey::Space),
            Some((Some(3), 4))
        );
        assert_eq!(
            on_key(&groups, &selectable, 3, Some(3), FoldKey::Space),
            Some((None, 3))
        );
        assert_eq!(
            on_key(&groups, &selectable, 5, Some(3), FoldKey::Space),
            Some((None, 3))
        );
        assert_eq!(on_key(&groups, &selectable, 0, None, FoldKey::Space), None);
    }

    /// Runs `reveal` at 60 frames a second until it rests; the heights it
    /// showed on the way.
    fn run(reveal: &mut Reveal) -> Vec<f32> {
        let start = Instant::now();
        let mut shown = Vec::new();
        for frame in 0..200 {
            reveal.advance(start + std::time::Duration::from_millis(frame * 16));
            shown.push(reveal.shown());
            if !reveal.is_moving() {
                break;
            }
        }
        shown
    }

    /// A group opens from nothing to all of its rows over a few frames, and
    /// closes back the same way.
    #[test]
    fn a_group_opens_and_closes_over_time() {
        let mut open = Reveal::unfold(3, 2, 64.0, 0.0);
        assert_eq!(open.shown(), 0.0);
        let shown = run(&mut open);
        assert!(shown.len() > 5, "more than a jump: {shown:?}");
        assert!(
            shown.iter().all(|h| (0.0..=64.0).contains(h)),
            "never past all"
        );
        assert_eq!(open.shown(), 64.0);
        assert!(!open.is_moving());

        let mut close = Reveal::fold(3, 2, 64.0, 64.0);
        assert!(close.folding);
        assert!(close.contains(4) && close.contains(5));
        assert!(!close.contains(3) && !close.contains(6));
        let shown = run(&mut close);
        assert!(shown.len() > 5);
        assert_eq!(close.shown(), 0.0);
        assert_eq!(close.hidden(), 64.0);
    }

    /// Closing a group that was still opening carries on from where it got
    /// to, rather than jumping.
    #[test]
    fn closing_mid_way_starts_where_it_is() {
        let close = Reveal::fold(3, 2, 64.0, 20.0);
        assert_eq!(close.shown(), 20.0);
        assert!(close.is_moving());
    }
}
