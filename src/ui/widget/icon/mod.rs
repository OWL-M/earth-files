// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Lazily-generated SVG icon widget for Iced.
//!
//! Vendored from pop-os/libcosmic, src/widget/icon/mod.rs

mod bundle;
mod named;
use std::sync::Arc;

pub use named::{IconFallback, Named};

mod handle;
pub use handle::{Data, Handle, from_path, from_raster_bytes, from_raster_pixels, from_svg_bytes};

use crate::ui::Element;
use crate::ui::convert::ToRadius;
use crate::ui::widget::svg::Svg;
use derive_setters::Setters;
use iced::widget::Image;
use iced::{ContentFit, Length, Radians, Rectangle};
use iced_core::Rotation;

/// Create an [`Icon`] from a pre-existing [`Handle`]
pub fn icon(handle: Handle) -> Icon {
    Icon {
        content_fit: ContentFit::Fill,
        handle,
        height: None,
        size: 16,
        opacity: 1.0,
        class: crate::ui::theme::Svg::default(),
        rotation: None,
        width: None,
    }
}

/// Create an icon handle from its XDG icon name.
pub fn from_name(name: impl Into<Arc<str>>) -> Named {
    Named::new(name)
}

/// An image which may be an SVG or PNG.
#[must_use]
#[derive(Clone, Setters)]
pub struct Icon {
    #[setters(skip)]
    handle: Handle,
    class: crate::ui::theme::Svg,
    #[setters(skip)]
    pub(super) size: u16,
    opacity: f32,
    content_fit: ContentFit,
    #[setters(strip_option)]
    width: Option<Length>,
    #[setters(strip_option)]
    height: Option<Length>,
    #[setters(strip_option)]
    rotation: Option<Rotation>,
}

impl Icon {
    #[must_use]
    pub fn into_svg_handle(self) -> Option<iced::widget::svg::Handle> {
        match self.handle.data {
            Data::Image(_) => (),
            Data::Svg(handle) => return Some(handle),
        }

        None
    }

    pub fn size(mut self, size: u16) -> Self {
        self.size = size;
        self
    }

    #[must_use]
    fn view<'a, Message: 'a>(self) -> Element<'a, Message> {
        let from_image = |handle| {
            Image::new(handle)
                .width(
                    self.width
                        .unwrap_or_else(|| Length::Fixed(f32::from(self.size))),
                )
                .height(
                    self.height
                        .unwrap_or_else(|| Length::Fixed(f32::from(self.size))),
                )
                .opacity(self.opacity)
                .rotation(self.rotation.unwrap_or_default())
                .content_fit(self.content_fit)
                .into()
        };

        let from_svg = |handle| {
            // `iced::widget::Svg` has no `symbolic` flag and `renderer::Style`
            // no `icon_color`; both are provided by `crate::ui::widget::svg`,
            // whose `draw` resolves the inherited colour through
            // `crate::ui::theme::icon_color`.
            let class = self.class.clone();

            Svg::<crate::ui::Theme>::new(handle)
                .class(class)
                .symbolic(self.handle.symbolic)
                .width(
                    self.width
                        .unwrap_or_else(|| Length::Fixed(f32::from(self.size))),
                )
                .height(
                    self.height
                        .unwrap_or_else(|| Length::Fixed(f32::from(self.size))),
                )
                .opacity(self.opacity)
                .rotation(self.rotation.unwrap_or_default())
                .content_fit(self.content_fit)
                .into()
        };

        match self.handle.data {
            Data::Image(handle) => from_image(handle),
            Data::Svg(handle) => from_svg(handle),
        }
    }
}

impl<'a, Message: 'a> From<Icon> for Element<'a, Message> {
    fn from(icon: Icon) -> Self {
        icon.view::<Message>()
    }
}

/// Draw an icon in the given bounds via the runtime's renderer.
pub fn draw(renderer: &mut iced::Renderer, handle: &Handle, icon_bounds: Rectangle) {
    match handle.clone().data {
        Data::Svg(handle) => iced_core::svg::Renderer::draw_svg(
            renderer,
            iced_core::svg::Svg::new(handle),
            icon_bounds,
            icon_bounds,
        ),

        Data::Image(handle) => {
            iced_core::image::Renderer::draw_image(
                renderer,
                iced_core::Image {
                    handle,
                    filter_method: iced_core::image::FilterMethod::Linear,
                    rotation: Radians(0.),
                    border_radius: [0.0; 4].to_radius(),
                    opacity: 1.0,
                    snap: true,
                },
                icon_bounds,
                icon_bounds,
            );
        }
    }
}
