// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Menu bar styling, ported from libcosmic `d9431dc`, `src/theme/style/menu_bar.rs`
//! (itself from `iced_aw`, MIT).
//!
//! [`Appearance`] and [`StyleSheet`] are vendored here for
//! [`crate::ui::theme::Theme`], and [`crate::ui::widget::menu`] uses these types.
//! Re-exporting `cosmic::style::menu_bar` was necessary while libcosmic owned
//! the implementation for `cosmic::Theme`: a vendored trait would have been a
//! distinct type. Phase 3 moves both the theme and implementation into this
//! crate.

use iced_core::Color;

use crate::ui::convert::ToColor;
use crate::ui::theme::Theme;

/// The appearance of a menu bar and its menus.
#[derive(Debug, Clone, Copy)]
pub struct Appearance {
    /// The background color of the menu bar and its menus.
    pub background: Color,
    /// The border width of the menu bar and its menus.
    pub border_width: f32,
    /// The border radius of the menu bar.
    pub bar_border_radius: [f32; 4],
    /// The border radius of the menus.
    pub menu_border_radius: [f32; 4],
    /// The border [`Color`] of the menu bar and its menus.
    pub border_color: Color,
    /// The expand value of the menus' background
    pub background_expand: [u16; 4],
    /// The highlighted path [`Color`] of the the menu bar and its menus.
    pub path: Color,
}

/// The style sheet of a menu bar and its menus.
pub trait StyleSheet {
    /// The supported style of the [`StyleSheet`].
    type Style: Default;

    /// Produces the [`Appearance`] of a menu bar and its menus.
    fn appearance(&self, style: &Self::Style, is_overlay: bool) -> Appearance;
}

/// The style of a menu bar and its menus.
///
/// Every call site uses `Default`, so this omits libcosmic's
/// `Custom(Arc<dyn StyleSheet<Style = Theme>>)` variant and its
/// `impl From<fn(&Theme) -> Appearance>` and
/// `impl StyleSheet for fn(&Theme) -> Appearance`. Those implementations could
/// be added with the local trait; they were not permitted while the trait
/// belonged to libcosmic.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MenuBarStyle {
    /// The default style.
    #[default]
    Default,
}

impl StyleSheet for Theme {
    type Style = MenuBarStyle;

    fn appearance(&self, style: &Self::Style, is_overlay: bool) -> Appearance {
        let cosmic = self.cosmic();
        let component = &cosmic.background(!is_overlay && self.transparent).component;
        let bg = component.base;

        match style {
            MenuBarStyle::Default => Appearance {
                background: bg.to_color(),
                border_width: 1.0,
                bar_border_radius: cosmic.corner_radii.radius_xl,
                menu_border_radius: cosmic.corner_radii.radius_s,
                border_color: component.divider.to_color(),
                background_expand: [1; 4],
                path: component.hover.to_color(),
            },
        }
    }
}
