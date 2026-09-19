// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic d9431dc, src/widget/tab_bar.rs
//!
//! A collection of tabs for developing a tabbed interface.
//!
//! See the [`segmented_button`] module for more details.

use super::segmented_button::{
    self, HorizontalSegmentedButton, Model, SegmentedButton, Selectable, VerticalSegmentedButton,
};

/// A collection of tabs for developing a tabbed interface.
///
/// The data for the widget comes from a model supplied by the application.
///
/// For details on the model, see the [`segmented_button`] module for more details.
pub fn horizontal<SelectionMode: Default, Message: Clone + 'static>(
    model: &Model<SelectionMode>,
) -> HorizontalSegmentedButton<'_, SelectionMode, Message>
where
    Model<SelectionMode>: Selectable,
{
    // Upstream reads the active COSMIC theme's spacing; this app owns its
    // spacing tables (`crate::ui::theme::spacing()`).
    let spacing = crate::ui::theme::spacing();
    let space_s = spacing.space_s;
    let space_xs = spacing.space_xs;

    segmented_button::horizontal(model)
        .minimum_button_width(76)
        .maximum_button_width(250)
        .button_height(44)
        .button_padding([space_s, space_xs, space_s, space_xs])
        .style(crate::ui::theme::SegmentedButton::TabBar)
}

/// A collection of tabs for developing a tabbed interface.
///
/// The data for the widget comes from a model that is maintained the application.
/// For details on the model, see the [`segmented_button`] module for more details.
pub fn vertical<SelectionMode, Message: Clone + 'static>(
    model: &Model<SelectionMode>,
) -> VerticalSegmentedButton<'_, SelectionMode, Message>
where
    Model<SelectionMode>: Selectable,
    SelectionMode: Default,
{
    // Upstream reads the active COSMIC theme's spacing; this app owns its
    // spacing tables (`crate::ui::theme::spacing()`).
    let spacing = crate::ui::theme::spacing();
    let space_s = spacing.space_s;
    let space_xs = spacing.space_xs;

    SegmentedButton::new(model)
        .minimum_button_width(76)
        .maximum_button_width(250)
        .button_height(44)
        .button_padding([space_s, space_xs, space_s, space_xs])
        .style(crate::ui::theme::SegmentedButton::TabBar)
}
