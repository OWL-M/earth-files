// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic d9431dc, src/widget/scrollable/scrollable.rs

use iced::Renderer;

use crate::ui::Element;
use iced::widget;

pub fn scrollable<'a, Message>(
    element: impl Into<Element<'a, Message>>,
) -> widget::Scrollable<'a, Message, crate::ui::Theme, Renderer> {
    vertical(element)
}

// `Scrollable::scroller_width`, `::scrollbar_width` and `::scrollbar_padding`
// are all fork-only convenience setters (fork `iced/widget/src/scrollable.rs`
// `:282`, `:261`, `:303`). Each one reached into the `Direction`'s `Scrollbar`
// and wrote one field, so the first two are restated by configuring the
// `Scrollbar` directly, which is where upstream puts `width` and
// `scroller_width`.
//
// # `scrollbar_padding` has no upstream equivalent
//
// The fork added a sixth field, `padding`, to `Scrollbar` (`:445`) on top of
// upstream's five, and used it to inset the scrollbar *track* at both ends:
// `y: bounds.y + padding` and `height: bounds.height - … - 2.0 * padding`
// (fork `:2420`, `:2424`). Upstream `iced_widget 0.14`'s `Scrollbar` has
// `width`, `margin`, `scroller_width`, `alignment` and `spacing` and nothing
// that does this. `margin` is a *horizontal* inset of the bar from the edge,
// which the fork has as well and separately, so it is not the same quantity.
//
// This app's vertical scrollbar track ran from 8px below the top
// to 8px above the bottom and now runs the full height, so the thumb is drawn
// slightly longer and slightly higher at the extremes. Confirmed on screen
// against a pre-flip libcosmic build: at the file list's scrollbar column the
// first 8px below the list's top edge read background there and thumb here.
// Reproducing it means vendoring iced's ~2,600-line `scrollable`; it is not
// approximated here.
pub fn vertical<'a, Message>(
    element: impl Into<Element<'a, Message>>,
) -> widget::Scrollable<'a, Message, crate::ui::Theme, Renderer> {
    widget::scrollable(element).direction(widget::scrollable::Direction::Vertical(
        widget::scrollable::Scrollbar::new()
            .width(8.0)
            .scroller_width(8.0),
    ))
}

pub fn horizontal<'a, Message>(
    element: impl Into<Element<'a, Message>>,
) -> widget::Scrollable<'a, Message, crate::ui::Theme, Renderer> {
    widget::scrollable(element).direction(widget::scrollable::Direction::Horizontal(
        widget::scrollable::Scrollbar::new()
            .width(8.0)
            .scroller_width(8.0),
    ))
}
