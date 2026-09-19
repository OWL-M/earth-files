// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! An element to distinguish a boundary between two elements.
//!
//! Vendored from pop-os/libcosmic d9431dc, src/widget/mod.rs (divider module)

/// Horizontal variant of a divider.
pub mod horizontal {
    use iced::widget::{Rule, rule};

    /// Horizontal divider with default thickness
    #[must_use]
    pub fn default<'a>() -> Rule<'a, crate::ui::Theme> {
        rule::horizontal(1).class(crate::ui::theme::Rule::Default)
    }

    /// Horizontal divider with light thickness
    #[must_use]
    pub fn light<'a>() -> Rule<'a, crate::ui::Theme> {
        rule::horizontal(1).class(crate::ui::theme::Rule::LightDivider)
    }

    /// Horizontal divider with heavy thickness.
    #[must_use]
    pub fn heavy<'a>() -> Rule<'a, crate::ui::Theme> {
        rule::horizontal(4).class(crate::ui::theme::Rule::HeavyDivider)
    }
}

/// Vertical variant of a divider.
pub mod vertical {
    use iced::widget::{Rule, rule};

    /// Vertical divider with default thickness
    #[must_use]
    pub fn default<'a>() -> Rule<'a, crate::ui::Theme> {
        rule::vertical(1).class(crate::ui::theme::Rule::Default)
    }

    /// Vertical divider with light thickness
    #[must_use]
    pub fn light<'a>() -> Rule<'a, crate::ui::Theme> {
        rule::vertical(4).class(crate::ui::theme::Rule::LightDivider)
    }

    /// Vertical divider with heavy thickness.
    #[must_use]
    pub fn heavy<'a>() -> Rule<'a, crate::ui::Theme> {
        rule::vertical(10).class(crate::ui::theme::Rule::HeavyDivider)
    }
}
