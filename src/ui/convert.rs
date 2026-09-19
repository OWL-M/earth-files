// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! The conversions libcosmic's `iced` fork added as blanket `From` impls,
//! restated as extension traits because we cannot restate them as `From`.
//!
//! The migration brief listed four conversions; compilation found six.
//! The additional fork-only conversions are `Background: From<Srgba>`
//! (13 sites) and `Length: From<u16>` (5 sites).
//!
//! The fork added these to `iced_core` itself:
//!
//! | fork impl | fork source |
//! | --- | --- |
//! | `Pixels: From<u16>` | `iced/core/src/pixels.rs:29` |
//! | `Padding: From<[u16; 4]>` | `iced/core/src/padding.rs:216` |
//! | `Radius: From<[f32; 4]>` | `iced/core/src/border.rs:280` |
//! | `Color: From<palette::Srgba<f32>>` (and `Srgba<u8>`, `Srgb<f32>`) | `iced/core/src/color.rs:278-299` |
//! | `palette::Srgba<f32>: From<Color>` | `iced/core/src/color.rs:307` |
//! | `Background: From<palette::Srgba<f32>>` (and `Srgba<u8>`) | `iced/core/src/background.rs:27,33` |
//! | `Length: From<u16>` | `iced/core/src/length.rs:87` |
//!
//! Upstream `iced_core 0.14` has none of these impls. The orphan rule prevents
//! adding them here because both the traits and types belong to other crates.
//! The ported style layer uses `.into()` with the target type inferred from
//! the assigned field, so replacements must work in the same expressions.
//!
//! Extension methods (`.to_color()`, `.to_radius()`, `.to_padding()` and
//! `.to_pixels()`) preserve that order. Free functions such as `px(16)` or
//! `padding([4; 4])` would reverse the reading order of method chains at about
//! 200 sites in `theme/style/iced.rs` alone.
//!
//! The implementations are copied from the fork, preserving its arithmetic
//! and the array element order required by `Radius` and `Padding`.

use iced_core::border::Radius;
use iced_core::{Color, Padding, Pixels};

/// `Pixels: From<u16>`, fork `iced/core/src/pixels.rs:29`.
pub trait ToPixels {
    fn to_pixels(self) -> Pixels;
}

impl ToPixels for u16 {
    #[inline]
    fn to_pixels(self) -> Pixels {
        Pixels(f32::from(self))
    }
}

/// `Padding: From<[u16; 4]>`, fork `iced/core/src/padding.rs:216`.
///
/// Element order is `[top, right, bottom, left]`, as the fork has it.
pub trait ToPadding {
    fn to_padding(self) -> Padding;
}

impl ToPadding for [u16; 4] {
    #[inline]
    fn to_padding(self) -> Padding {
        Padding {
            top: f32::from(self[0]),
            right: f32::from(self[1]),
            bottom: f32::from(self[2]),
            left: f32::from(self[3]),
        }
    }
}

/// `Radius: From<[f32; 4]>`, fork `iced/core/src/border.rs:280`.
///
/// Element order is `[top_left, top_right, bottom_right, bottom_left]`, as the
/// fork's doc comment spells out. This is the order the whole `CornerRadii`
/// table in [`crate::ui::theme::palette`] is written in, so getting it wrong
/// would rotate every rounded corner in the app rather than fail to compile.
pub trait ToRadius {
    fn to_radius(self) -> Radius;
}

impl ToRadius for [f32; 4] {
    #[inline]
    fn to_radius(self) -> Radius {
        Radius {
            top_left: self[0],
            top_right: self[1],
            bottom_right: self[2],
            bottom_left: self[3],
        }
    }
}

/// `Color: From<palette::Srgba<f32>>` and friends, fork
/// `iced/core/src/color.rs:278-299`.
pub trait ToColor {
    fn to_color(self) -> Color;
}

impl ToColor for palette::Srgba<f32> {
    #[inline]
    fn to_color(self) -> Color {
        Color::from_rgba(self.red, self.green, self.blue, self.alpha)
    }
}

impl ToColor for palette::Srgba<u8> {
    #[inline]
    fn to_color(self) -> Color {
        Color::from_rgba8(self.red, self.green, self.blue, f32::from(self.alpha) / 255.0)
    }
}

impl ToColor for palette::Srgb<f32> {
    #[inline]
    fn to_color(self) -> Color {
        Color::from_rgb(self.red, self.green, self.blue)
    }
}

/// `palette::Srgba<f32>: From<Color>`, fork `iced/core/src/color.rs:307`.
///
/// The reverse direction, which the ported style code uses to lift an iced
/// `Color` back into the palette space [`crate::ui::theme::over`] composites in.
pub trait ToSrgba {
    fn to_srgba(self) -> palette::Srgba<f32>;
}

impl ToSrgba for Color {
    #[inline]
    fn to_srgba(self) -> palette::Srgba<f32> {
        palette::Srgba::new(self.r, self.g, self.b, self.a)
    }
}

/// `Background: From<palette::Srgba<f32>>` / `From<palette::Srgba<u8>>`, fork
/// `iced/core/src/background.rs:27,33`.
///
/// Used at 13 call sites; found during compilation, beyond the four
/// conversions listed in the migration brief.
pub trait ToBackground {
    fn to_background(self) -> iced_core::Background;
}

impl<T> ToBackground for T
where
    T: ToColor,
{
    #[inline]
    fn to_background(self) -> iced_core::Background {
        iced_core::Background::Color(self.to_color())
    }
}

/// `Length: From<u16>`, fork `iced/core/src/length.rs:87`.
///
/// Used at 5 call sites, beyond the four conversions in the migration brief.
/// Upstream has `From<f32>`, `From<u32>` and `From<Pixels>`, but a `u16`
/// literal no longer coerces.
pub trait ToLength {
    fn to_length(self) -> iced_core::Length;
}

impl ToLength for u16 {
    #[inline]
    fn to_length(self) -> iced_core::Length {
        iced_core::Length::Fixed(f32::from(self))
    }
}

/// `Column::push_maybe` / `Row::push_maybe`, fork `iced/widget/src/column.rs:157`
/// and `row.rs`.
///
/// Convenience helper used at 12 call sites, grouped with the other fork
/// compatibility helpers. The body is copied from the fork.
pub trait PushMaybe<'a, Message, Theme, Renderer> {
    #[must_use]
    fn push_maybe(
        self,
        child: Option<impl Into<iced_core::Element<'a, Message, Theme, Renderer>>>,
    ) -> Self;
}

impl<'a, Message, Theme, Renderer> PushMaybe<'a, Message, Theme, Renderer>
    for iced::widget::Column<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    fn push_maybe(
        self,
        child: Option<impl Into<iced_core::Element<'a, Message, Theme, Renderer>>>,
    ) -> Self {
        if let Some(child) = child {
            self.push(child)
        } else {
            self
        }
    }
}

impl<'a, Message, Theme, Renderer> PushMaybe<'a, Message, Theme, Renderer>
    for iced::widget::Row<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    fn push_maybe(
        self,
        child: Option<impl Into<iced_core::Element<'a, Message, Theme, Renderer>>>,
    ) -> Self {
        if let Some(child) = child {
            self.push(child)
        } else {
            self
        }
    }
}
