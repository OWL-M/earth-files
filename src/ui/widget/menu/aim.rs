// SPDX-License-Identifier: GPL-3.0-only

//! How the pointer and the keys move through a menu: when a row's child
//! opens, whether the pointer is on its way to an open child, and which row
//! the arrow keys go to.

use std::time::Duration;

use iced_core::{Point, Rectangle};

/// How long the pointer may rest over another row, on its way to an open
/// child, before that row takes over.
pub const AIM_TIME: Duration = Duration::from_millis(300);

/// How long the pointer rests on a dimmed row before its reason shows.
pub const REASON_TIME: Duration = Duration::from_millis(500);

/// When a row's child popup opens.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum ChildOpen {
    /// As soon as the pointer enters the row.
    #[default]
    Row,
    /// As soon as the pointer enters the row's last `width` pixels: a split
    /// row's arrow. Its left part never opens the child.
    Arrow(f32),
    /// Once the pointer has rested on the row this long: a dimmed row's
    /// reason.
    Rest(Duration),
}

/// What the pointer at a row asks of the row's child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    /// Open it now.
    Now,
    /// Open it if the pointer is still on the row after this long.
    After(Duration),
    /// Leave it as it is.
    Not,
}

impl ChildOpen {
    /// What the pointer at `cursor`, on `row`, asks of the row's child.
    /// `entered` is whether the pointer has just come onto the row; `open`
    /// whether the child is open already.
    #[must_use]
    pub fn opening(self, row: Rectangle, cursor: Point, entered: bool, open: bool) -> Opening {
        if open {
            return Opening::Not;
        }
        match self {
            Self::Row if entered => Opening::Now,
            Self::Row => Opening::Not,
            Self::Arrow(width) if in_arrow(row, width, cursor) => Opening::Now,
            Self::Arrow(_) => Opening::Not,
            Self::Rest(time) if entered => Opening::After(time),
            Self::Rest(_) => Opening::Not,
        }
    }

    /// Whether a press and release at `cursor` on `row` is meant for the
    /// child rather than for the row: on a split row's arrow.
    #[must_use]
    pub fn takes_click(self, row: Rectangle, cursor: Point) -> bool {
        matches!(self, Self::Arrow(width) if in_arrow(row, width, cursor))
    }
}

/// Whether `cursor` is in the last `width` pixels of `row`.
fn in_arrow(row: Rectangle, width: f32, cursor: Point) -> bool {
    row.contains(cursor) && cursor.x >= row.x + row.width - width
}

/// Whether the pointer, moving from `from` to `to`, is heading for `child`:
/// `to` lies in the triangle between `from` and the near edge of `child`.
#[must_use]
pub fn aims_at(from: Point, to: Point, child: Rectangle) -> bool {
    if from == to {
        return false;
    }
    let edge_x = if child.x >= from.x {
        child.x
    } else {
        child.x + child.width
    };
    let top = Point::new(edge_x, child.y);
    let bottom = Point::new(edge_x, child.y + child.height);
    in_triangle(to, from, top, bottom)
}

/// Whether `p` lies in the triangle `a`, `b`, `c`, edges included.
fn in_triangle(p: Point, a: Point, b: Point, c: Point) -> bool {
    let side = |p: Point, q: Point, r: Point| (p.x - r.x) * (q.y - r.y) - (q.x - r.x) * (p.y - r.y);
    let d1 = side(p, a, b);
    let d2 = side(p, b, c);
    let d3 = side(p, c, a);
    let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(negative && positive)
}

/// The row the arrow keys go to from `from`: the next (`down`) or previous
/// row that keys stop on, wrapping round; the first or last when none is
/// chosen yet. `None` when no row is.
#[must_use]
pub fn next_selectable(selectable: &[bool], from: Option<usize>, down: bool) -> Option<usize> {
    let len = selectable.len();
    if len == 0 {
        return None;
    }
    let start = match (from, down) {
        (Some(i), true) => i + 1,
        (Some(i), false) => i + len - 1,
        (None, true) => 0,
        (None, false) => len - 1,
    };
    (0..len)
        .map(|step| {
            if down {
                (start + step) % len
            } else {
                (start + len - step) % len
            }
        })
        .find(|&i| selectable[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROW: Rectangle = Rectangle {
        x: 0.0,
        y: 100.0,
        width: 290.0,
        height: 36.0,
    };

    #[test]
    fn a_plain_parent_opens_on_entering_the_row() {
        let at = Point::new(20.0, 110.0);
        assert_eq!(ChildOpen::Row.opening(ROW, at, true, false), Opening::Now);
        assert_eq!(ChildOpen::Row.opening(ROW, at, false, false), Opening::Not);
        assert_eq!(ChildOpen::Row.opening(ROW, at, true, true), Opening::Not);
    }

    #[test]
    fn a_split_row_opens_only_from_its_arrow() {
        let arrow = ChildOpen::Arrow(39.0);
        let left = Point::new(100.0, 110.0);
        let on_arrow = Point::new(270.0, 110.0);
        assert_eq!(arrow.opening(ROW, left, true, false), Opening::Not);
        assert_eq!(
            arrow.opening(ROW, on_arrow, false, false),
            Opening::Now,
            "moving onto it"
        );
        assert_eq!(arrow.opening(ROW, on_arrow, true, true), Opening::Not);
        assert!(arrow.takes_click(ROW, on_arrow));
        assert!(!arrow.takes_click(ROW, left));
        assert!(!ChildOpen::Row.takes_click(ROW, on_arrow));
    }

    #[test]
    fn a_reason_waits_for_the_pointer_to_rest() {
        let rest = ChildOpen::Rest(REASON_TIME);
        let at = Point::new(100.0, 110.0);
        assert_eq!(
            rest.opening(ROW, at, true, false),
            Opening::After(REASON_TIME)
        );
        assert_eq!(rest.opening(ROW, at, false, false), Opening::Not);
    }

    #[test]
    fn moving_towards_the_child_is_aiming_and_away_is_not() {
        // A child to the right, beside rows 100..136
        let child = Rectangle::new(Point::new(290.0, 100.0), iced_core::Size::new(250.0, 300.0));
        let from = Point::new(200.0, 118.0);
        assert!(
            aims_at(from, Point::new(230.0, 140.0), child),
            "down and right, inside"
        );
        assert!(
            !aims_at(from, Point::new(200.0, 150.0), child),
            "straight down"
        );
        assert!(!aims_at(from, Point::new(170.0, 140.0), child), "away");
        assert!(!aims_at(from, from, child), "not moving");
        // A child opened to the left
        let left_child = Rectangle::new(
            Point::new(-250.0, 100.0),
            iced_core::Size::new(250.0, 300.0),
        );
        assert!(aims_at(
            Point::new(60.0, 118.0),
            Point::new(30.0, 140.0),
            left_child
        ));
    }

    #[test]
    fn arrow_keys_skip_rows_they_do_not_stop_on_and_wrap() {
        let rows = [true, false, true, true, false];
        assert_eq!(next_selectable(&rows, None, true), Some(0));
        assert_eq!(next_selectable(&rows, None, false), Some(3));
        assert_eq!(next_selectable(&rows, Some(0), true), Some(2));
        assert_eq!(next_selectable(&rows, Some(3), true), Some(0), "wraps");
        assert_eq!(
            next_selectable(&rows, Some(0), false),
            Some(3),
            "wraps back"
        );
        assert_eq!(next_selectable(&[false, false], None, true), None);
        assert_eq!(next_selectable(&[], None, true), None);
    }
}
