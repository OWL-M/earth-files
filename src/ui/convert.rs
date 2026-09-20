// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Conversions into `iced_core` types (`Pixels`, `Padding`, `Radius`, `Color`,
//! `Background`, `Length`) and back to `palette` colours, as extension traits.
//!
//! `iced_core 0.14` has no `From` impls for these, and the orphan rule prevents
//! adding them here because both the traits and types belong to other crates.
//! The style layer uses `.into()` with the target type inferred from the
//! assigned field, so replacements must work in the same expressions.
//!
//! Extension methods (`.to_color()`, `.to_radius()`, `.to_padding()` and
//! `.to_pixels()`) preserve that order. Free functions such as `px(16)` or
//! `padding([4; 4])` would reverse the reading order of method chains at about
//! 200 sites in `theme/style/iced.rs` alone.

use iced_core::border::Radius;
use iced_core::{Color, Padding, Pixels};

/// `Pixels` from a `u16`.
pub trait ToPixels {
    fn to_pixels(self) -> Pixels;
}

impl ToPixels for u16 {
    #[inline]
    fn to_pixels(self) -> Pixels {
        Pixels(f32::from(self))
    }
}

/// `Padding` from `[top, right, bottom, left]`.
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

/// `Radius` from `[top_left, top_right, bottom_right, bottom_left]`.
///
/// This is the order the whole `CornerRadii` table in
/// [`crate::ui::theme::palette`] is written in, so getting it wrong would
/// rotate every rounded corner in the app rather than fail to compile.
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

/// `Color` from a `palette` colour.
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
        Color::from_rgba8(
            self.red,
            self.green,
            self.blue,
            f32::from(self.alpha) / 255.0,
        )
    }
}

impl ToColor for palette::Srgb<f32> {
    #[inline]
    fn to_color(self) -> Color {
        Color::from_rgb(self.red, self.green, self.blue)
    }
}

/// `palette::Srgba<f32>` from a `Color`.
///
/// The reverse direction, which the style code uses to lift an iced `Color`
/// back into the palette space [`crate::ui::theme::over`] composites in.
pub trait ToSrgba {
    fn to_srgba(self) -> palette::Srgba<f32>;
}

impl ToSrgba for Color {
    #[inline]
    fn to_srgba(self) -> palette::Srgba<f32> {
        palette::Srgba::new(self.r, self.g, self.b, self.a)
    }
}

/// `Background` from anything that converts to a `Color`.
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

/// `Length` from a `u16`.
///
/// `Length` has `From<f32>`, `From<u32>` and `From<Pixels>`, but a `u16`
/// literal does not coerce.
pub trait ToLength {
    fn to_length(self) -> iced_core::Length;
}

impl ToLength for u16 {
    #[inline]
    fn to_length(self) -> iced_core::Length {
        iced_core::Length::Fixed(f32::from(self))
    }
}

/// `Column::push_maybe` / `Row::push_maybe`: push a child only when it is
/// `Some`.
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
