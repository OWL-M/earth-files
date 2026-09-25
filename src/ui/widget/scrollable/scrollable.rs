// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/scrollable/scrollable.rs

use super::Smooth;
use crate::ui::Element;
use iced::widget;

pub fn scrollable<'a, Message>(element: impl Into<Element<'a, Message>>) -> Smooth<'a, Message> {
    vertical(element)
}

// `iced_widget 0.14`'s `Scrollbar` has `width`, `margin`, `scroller_width`,
// `alignment` and `spacing`, and nothing that insets the scrollbar *track* at
// its ends (`margin` is a horizontal inset of the bar from the edge). So the
// vertical scrollbar track runs the full height. Insetting it by 8px at each
// end would mean vendoring iced's ~2,600-line `scrollable`; it is not
// approximated here.
pub fn vertical<'a, Message>(element: impl Into<Element<'a, Message>>) -> Smooth<'a, Message> {
    Smooth::new(widget::scrollable(element)).direction(widget::scrollable::Direction::Vertical(
        widget::scrollable::Scrollbar::new()
            .width(8.0)
            .scroller_width(8.0),
    ))
}

pub fn horizontal<'a, Message>(element: impl Into<Element<'a, Message>>) -> Smooth<'a, Message> {
    Smooth::new(widget::scrollable(element)).direction(widget::scrollable::Direction::Horizontal(
        widget::scrollable::Scrollbar::new()
            .width(8.0)
            .scroller_width(8.0),
    ))
}
