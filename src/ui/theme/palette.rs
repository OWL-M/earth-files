// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! The app's colour palette.
//!
//! A [`Palette`] holds about 300 derived colours, produced from an 11-step
//! Oklch neutral ramp by colour stepping, compositing and WCAG contrast picks.
//! Vendored widget styles read these fields directly (`component.hover`,
//! `container.divider`, `control_7()`, …), so the palette must reproduce them
//! exactly to preserve the app's appearance.
//!
//! [`DARK`] and [`LIGHT`] are snapshots of the COSMIC dark and light themes,
//! stored as Rust source in `generated.rs`. They preserve the derived colours
//! without computing them at runtime. Deriving them would require several
//! hundred lines of colour maths and two palette RON files to compute these
//! two constants.
//!
//! `iced::theme::palette::Extended::generate` produces five
//! `weak`/`base`/`strong` pairs through lightening and mixing. COSMIC uses a
//! `Container`/`Component` model and colour stepping, so using iced's generator
//! would change every colour and require rewriting the palette's consumers.
//!
//! Custom COSMIC accent colours are not followed. `AppTheme::theme` requests
//! only the two built-in themes.

use palette::Srgba;

use super::Spacing;

mod generated;

pub use generated::{DARK, LIGHT};

/// The colours of one widget, in each of its states.
#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    /// The base color of the widget
    pub base: Srgba,
    /// The color of the widget when it is hovered
    pub hover: Srgba,
    /// The color of the widget when it is pressed
    pub pressed: Srgba,
    /// The color of the widget when it is selected
    pub selected: Srgba,
    /// The text color of the widget when it is selected
    pub selected_text: Srgba,
    /// The color of the widget when it is focused
    pub focus: Srgba,
    /// The color of dividers for this widget
    pub divider: Srgba,
    /// The color of text for this widget
    pub on: Srgba,
    /// The color of the widget when it is disabled
    pub disabled: Srgba,
    /// The color of text in the widget when it is disabled
    pub on_disabled: Srgba,
    /// The color of the border for the widget
    pub border: Srgba,
    /// The color of the border for the widget when it is disabled
    pub disabled_border: Srgba,
}

/// A fully transparent component, for styles that paint nothing.
pub static TRANSPARENT_COMPONENT: Component = {
    const CLEAR: Srgba = Srgba::new(0.0, 0.0, 0.0, 0.0);
    Component {
        base: CLEAR,
        hover: CLEAR,
        pressed: CLEAR,
        selected: CLEAR,
        selected_text: CLEAR,
        focus: CLEAR,
        divider: CLEAR,
        on: CLEAR,
        disabled: CLEAR,
        on_disabled: CLEAR,
        border: CLEAR,
        disabled_border: CLEAR,
    }
};

impl Component {
    /// `@hover_state_color`
    #[inline]
    pub fn hover_state_color(&self) -> Srgba {
        self.hover
    }

    /// `@pressed_state_color`
    #[inline]
    pub fn pressed_state_color(&self) -> Srgba {
        self.pressed
    }

    /// `@selected_state_color`
    #[inline]
    pub fn selected_state_color(&self) -> Srgba {
        self.selected
    }

    /// `@selected_state_text_color`
    #[inline]
    pub fn selected_state_text_color(&self) -> Srgba {
        self.selected_text
    }

    /// `@focus_color`
    #[inline]
    pub fn focus_color(&self) -> Srgba {
        self.focus
    }
}

/// One surface level: its background, the components on it, its dividers and
/// its text.
#[derive(Clone, Debug, PartialEq)]
pub struct Container {
    /// The color of the container
    pub base: Srgba,
    /// The color of components in the container
    pub component: Component,
    /// The color of dividers in the container
    pub divider: Srgba,
    /// The color of text in the container
    pub on: Srgba,
    /// The color of `@small_widget_container`
    pub small_widget: Srgba,
}

/// Corner radii, in logical pixels, as `[top_left, top_right, bottom_right,
/// bottom_left]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CornerRadii {
    /// Corner radii of 0
    pub radius_0: [f32; 4],
    /// Smallest size of corner radii that can be non-zero
    pub radius_xs: [f32; 4],
    /// Small corner radii
    pub radius_s: [f32; 4],
    /// Medium corner radii
    pub radius_m: [f32; 4],
    /// Large corner radii
    pub radius_l: [f32; 4],
    /// Extra large corner radii
    pub radius_xl: [f32; 4],
}

/// The raw palette the derived colours were generated from.
///
/// Style code reads only the neutral ramp, through [`Palette::control_0`] and
/// related accessors. The remaining fields keep the snapshot complete.
#[derive(Clone, Debug, PartialEq)]
pub struct Raw {
    /// Name of the palette
    pub name: &'static str,
    /// Utility colour
    pub bright_red: Srgba,
    /// Utility colour
    pub bright_green: Srgba,
    /// Utility colour
    pub bright_orange: Srgba,
    /// Surface gray
    pub gray_1: Srgba,
    /// Surface gray
    pub gray_2: Srgba,
    /// System neutral ramp, darkest to lightest in the light theme
    pub neutral_0: Srgba,
    /// System neutral ramp
    pub neutral_1: Srgba,
    /// System neutral ramp
    pub neutral_2: Srgba,
    /// System neutral ramp
    pub neutral_3: Srgba,
    /// System neutral ramp
    pub neutral_4: Srgba,
    /// System neutral ramp
    pub neutral_5: Srgba,
    /// System neutral ramp
    pub neutral_6: Srgba,
    /// System neutral ramp
    pub neutral_7: Srgba,
    /// System neutral ramp
    pub neutral_8: Srgba,
    /// System neutral ramp
    pub neutral_9: Srgba,
    /// System neutral ramp
    pub neutral_10: Srgba,
    /// Potential accent
    pub accent_blue: Srgba,
    /// Potential accent
    pub accent_indigo: Srgba,
    /// Potential accent
    pub accent_purple: Srgba,
    /// Potential accent
    pub accent_pink: Srgba,
    /// Potential accent
    pub accent_red: Srgba,
    /// Potential accent
    pub accent_orange: Srgba,
    /// Potential accent
    pub accent_yellow: Srgba,
    /// Potential accent
    pub accent_green: Srgba,
    /// Potential accent
    pub accent_warm_grey: Srgba,
    /// Extended palette colour
    pub ext_warm_grey: Srgba,
    /// Extended palette colour
    pub ext_orange: Srgba,
    /// Extended palette colour
    pub ext_yellow: Srgba,
    /// Extended palette colour
    pub ext_blue: Srgba,
    /// Extended palette colour
    pub ext_purple: Srgba,
    /// Extended palette colour
    pub ext_pink: Srgba,
    /// Extended palette colour
    pub ext_indigo: Srgba,
}

/// Every colour and metric one theme renders with.
///
/// This app never enables blur, so `transparent` is always false. The
/// `transparent_*` containers are included for completeness.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// Name of the theme
    pub name: &'static str,
    /// Background element colors
    pub background: Container,
    /// Primary element colors
    pub primary: Container,
    /// Secondary element colors
    pub secondary: Container,
    /// Background element colors, blurred
    pub transparent_background: Container,
    /// Primary element colors, blurred
    pub transparent_primary: Container,
    /// Secondary element colors, blurred
    pub transparent_secondary: Container,
    /// Button component styling
    pub button: Component,
    /// Accent element colors
    pub accent: Component,
    /// Suggested element colors
    pub success: Component,
    /// Destructive element colors
    pub destructive: Component,
    /// Warning element colors
    pub warning: Component,
    /// Accent button element colors
    pub accent_button: Component,
    /// Suggested button element colors
    pub success_button: Component,
    /// Destructive button element colors
    pub destructive_button: Component,
    /// Warning button element colors
    pub warning_button: Component,
    /// Icon button element colors
    pub icon_button: Component,
    /// Link button element colors
    pub link_button: Component,
    /// List button element colors
    pub list_button: Component,
    /// Text button element colors
    pub text_button: Component,
    /// The raw palette these were derived from
    pub palette: Raw,
    /// Spacing
    pub spacing: Spacing,
    /// Corner radii
    pub corner_radii: CornerRadii,
    /// Whether this is a dark theme
    pub is_dark: bool,
    /// Whether this is a high-contrast theme
    pub is_high_contrast: bool,
    /// Whether maximised windows are drawn frosted. The one field of the blur
    /// machinery this app reads: the shell picks an opaque window style when
    /// it is false.
    pub frosted_maximized_apps: bool,
    /// Shade color for dialogs
    pub shade: Srgba,
    /// Accent text color. `None` means the accent base colour is used.
    pub accent_text: Option<Srgba>,
}

#[allow(clippy::doc_markdown)]
impl Palette {
    /// The background container.
    ///
    /// `transparent` selects the blurred variant. It is always false in this
    /// app, which never enables blur.
    #[inline]
    pub fn background(&self, transparent: bool) -> &Container {
        if transparent {
            &self.transparent_background
        } else {
            &self.background
        }
    }

    /// The primary container. See [`Self::background`] for `transparent`.
    #[inline]
    pub fn primary(&self, transparent: bool) -> &Container {
        if transparent {
            &self.transparent_primary
        } else {
            &self.primary
        }
    }

    /// The secondary container. See [`Self::background`] for `transparent`.
    #[inline]
    pub fn secondary(&self, transparent: bool) -> &Container {
        if transparent {
            &self.transparent_secondary
        } else {
            &self.secondary
        }
    }

    /// get control_0 color
    #[inline]
    pub fn control_0(&self) -> Srgba {
        self.palette.neutral_0
    }

    /// get control_1 color
    #[inline]
    pub fn control_1(&self) -> Srgba {
        self.palette.neutral_1
    }

    /// get control_2 color
    #[inline]
    pub fn control_2(&self) -> Srgba {
        self.palette.neutral_2
    }

    /// get control_3 color
    #[inline]
    pub fn control_3(&self) -> Srgba {
        self.palette.neutral_3
    }

    /// get control_4 color
    #[inline]
    pub fn control_4(&self) -> Srgba {
        self.palette.neutral_4
    }

    /// get control_5 color
    #[inline]
    pub fn control_5(&self) -> Srgba {
        self.palette.neutral_5
    }

    /// get control_6 color
    #[inline]
    pub fn control_6(&self) -> Srgba {
        self.palette.neutral_6
    }

    /// get control_7 color
    #[inline]
    pub fn control_7(&self) -> Srgba {
        self.palette.neutral_7
    }

    /// get control_8 color
    #[inline]
    pub fn control_8(&self) -> Srgba {
        self.palette.neutral_8
    }

    /// get control_9 color
    #[inline]
    pub fn control_9(&self) -> Srgba {
        self.palette.neutral_9
    }

    /// get control_10 color
    #[inline]
    pub fn control_10(&self) -> Srgba {
        self.palette.neutral_10
    }

    /// get @accent_color
    #[inline]
    pub fn accent_color(&self) -> Srgba {
        self.accent.base
    }

    /// get @success_color
    #[inline]
    pub fn success_color(&self) -> Srgba {
        self.success.base
    }

    /// get @destructive_color
    #[inline]
    pub fn destructive_color(&self) -> Srgba {
        self.destructive.base
    }

    /// get @warning_color
    #[inline]
    pub fn warning_color(&self) -> Srgba {
        self.warning.base
    }

    /// get @small_widget_divider
    #[inline]
    pub fn small_widget_divider(&self) -> Srgba {
        let mut color = self.palette.neutral_9;
        color.alpha = 0.2;
        color
    }

    /// get @bg_color
    #[inline]
    pub fn bg_color(&self) -> Srgba {
        self.background.base
    }

    /// get @bg_component_color
    #[inline]
    pub fn bg_component_color(&self) -> Srgba {
        self.background.component.base
    }

    /// get @primary_container_color
    #[inline]
    pub fn primary_container_color(&self) -> Srgba {
        self.primary.base
    }

    /// get @primary_component_color
    #[inline]
    pub fn primary_component_color(&self) -> Srgba {
        self.primary.component.base
    }

    /// get @secondary_container_color
    #[inline]
    pub fn secondary_container_color(&self) -> Srgba {
        self.secondary.base
    }

    /// get @secondary_component_color
    #[inline]
    pub fn secondary_component_color(&self) -> Srgba {
        self.secondary.component.base
    }

    /// get @button_bg_color
    #[inline]
    pub fn button_bg_color(&self) -> Srgba {
        self.button.base
    }

    /// get @on_bg_color
    #[inline]
    pub fn on_bg_color(&self) -> Srgba {
        self.background.on
    }

    /// get @on_bg_component_color
    #[inline]
    pub fn on_bg_component_color(&self) -> Srgba {
        self.background.component.on
    }

    /// get @on_primary_color
    #[inline]
    pub fn on_primary_container_color(&self) -> Srgba {
        self.primary.on
    }

    /// get @on_primary_component_color
    #[inline]
    pub fn on_primary_component_color(&self) -> Srgba {
        self.primary.component.on
    }

    /// get @on_secondary_color
    #[inline]
    pub fn on_secondary_container_color(&self) -> Srgba {
        self.secondary.on
    }

    /// get @on_secondary_component_color
    #[inline]
    pub fn on_secondary_component_color(&self) -> Srgba {
        self.secondary.component.on
    }

    /// get @accent_text_color
    #[inline]
    pub fn accent_text_color(&self) -> Srgba {
        self.accent_text.unwrap_or(self.accent.base)
    }

    /// get @success_text_color
    #[inline]
    pub fn success_text_color(&self) -> Srgba {
        self.success.base
    }

    /// get @warning_text_color
    #[inline]
    pub fn warning_text_color(&self) -> Srgba {
        self.warning.base
    }

    /// get @destructive_text_color
    #[inline]
    pub fn destructive_text_color(&self) -> Srgba {
        self.destructive.base
    }

    /// get @on_accent_color
    #[inline]
    pub fn on_accent_color(&self) -> Srgba {
        self.accent.on
    }

    /// get @on_success_color
    #[inline]
    pub fn on_success_color(&self) -> Srgba {
        self.success.on
    }

    /// get @on_warning_color
    #[inline]
    pub fn on_warning_color(&self) -> Srgba {
        self.warning.on
    }

    /// get @on_destructive_color
    #[inline]
    pub fn on_destructive_color(&self) -> Srgba {
        self.destructive.on
    }

    /// get @button_color
    #[inline]
    pub fn button_color(&self) -> Srgba {
        self.button.on
    }

    /// get @bg_divider
    #[inline]
    pub fn bg_divider(&self) -> Srgba {
        self.background.divider
    }

    /// get @bg_component_divider
    #[inline]
    pub fn bg_component_divider(&self) -> Srgba {
        self.background.component.divider
    }

    /// get @primary_container_divider
    #[inline]
    pub fn primary_container_divider(&self) -> Srgba {
        self.primary.divider
    }

    /// get @primary_component_divider
    #[inline]
    pub fn primary_component_divider(&self) -> Srgba {
        self.primary.component.divider
    }

    /// get @secondary_container_divider
    #[inline]
    pub fn secondary_container_divider(&self) -> Srgba {
        self.secondary.divider
    }

    /// get @button_divider
    #[inline]
    pub fn button_divider(&self) -> Srgba {
        self.button.divider
    }

    /// get @window_header_bg
    #[inline]
    pub fn window_header_bg(&self) -> Srgba {
        self.background.base
    }

    /// get @space_none
    #[inline]
    pub fn space_none(&self) -> u16 {
        self.spacing.space_none
    }

    /// get @space_xxxs
    #[inline]
    pub fn space_xxxs(&self) -> u16 {
        self.spacing.space_xxxs
    }

    /// get @space_xxs
    #[inline]
    pub fn space_xxs(&self) -> u16 {
        self.spacing.space_xxs
    }

    /// get @space_xs
    #[inline]
    pub fn space_xs(&self) -> u16 {
        self.spacing.space_xs
    }

    /// get @space_s
    #[inline]
    pub fn space_s(&self) -> u16 {
        self.spacing.space_s
    }

    /// get @space_m
    #[inline]
    pub fn space_m(&self) -> u16 {
        self.spacing.space_m
    }

    /// get @space_l
    #[inline]
    pub fn space_l(&self) -> u16 {
        self.spacing.space_l
    }

    /// get @space_xl
    #[inline]
    pub fn space_xl(&self) -> u16 {
        self.spacing.space_xl
    }

    /// get @space_xxl
    #[inline]
    pub fn space_xxl(&self) -> u16 {
        self.spacing.space_xxl
    }

    /// get @space_xxxl
    #[inline]
    pub fn space_xxxl(&self) -> u16 {
        self.spacing.space_xxxl
    }

    /// get @radius_0
    #[inline]
    pub fn radius_0(&self) -> [f32; 4] {
        self.corner_radii.radius_0
    }

    /// get @radius_xs
    #[inline]
    pub fn radius_xs(&self) -> [f32; 4] {
        self.corner_radii.radius_xs
    }

    /// get @radius_s
    #[inline]
    pub fn radius_s(&self) -> [f32; 4] {
        self.corner_radii.radius_s
    }

    /// get @radius_m
    #[inline]
    pub fn radius_m(&self) -> [f32; 4] {
        self.corner_radii.radius_m
    }

    /// get @radius_l
    #[inline]
    pub fn radius_l(&self) -> [f32; 4] {
        self.corner_radii.radius_l
    }

    /// get @radius_xl
    #[inline]
    pub fn radius_xl(&self) -> [f32; 4] {
        self.corner_radii.radius_xl
    }

    /// get @shade_color
    #[inline]
    pub fn shade_color(&self) -> Srgba {
        self.shade
    }
}
