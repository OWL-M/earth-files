// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! A button for toggling the navigation side panel.
//!
//! Vendored from pop-os/libcosmic, src/widget/nav_bar_toggle.rs
//!
//! Verbatim apart from the paths: `crate::Element` is this crate's
//! [`crate::ui::Element`] and `crate::theme::Button` is
//! [`crate::ui::theme::Button`], both of which this crate already owns.

use crate::ui::{Element, widget};
use derive_setters::Setters;

#[derive(Setters)]
pub struct NavBarToggle<Message> {
    /// Whether the sidebar is shown, which the icon tells
    active: bool,
    #[setters(strip_option)]
    on_toggle: Option<Message>,
    class: crate::ui::theme::Button,
    selected: bool,
}

#[must_use]
pub const fn nav_bar_toggle<Message>() -> NavBarToggle<Message> {
    NavBarToggle {
        active: false,
        on_toggle: None,
        class: crate::ui::theme::Button::NavToggle,
        selected: false,
    }
}

impl<Message: 'static + Clone> From<NavBarToggle<Message>> for Element<'_, Message> {
    fn from(nav_bar_toggle: NavBarToggle<Message>) -> Self {
        // The header's outlines: the pane, or the pane folded away
        let icon = if nav_bar_toggle.active {
            widget::icon::line::SIDEBAR_SHOWN
        } else {
            widget::icon::line::SIDEBAR_HIDDEN
        };
        widget::button::icon(widget::icon::line::handle(icon))
            .padding([8, 16])
            .on_press_maybe(nav_bar_toggle.on_toggle)
            .selected(nav_bar_toggle.selected)
            .class(nav_bar_toggle.class)
            .into()
    }
}
