// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/button/image.rs

use super::Builder;
use crate::ui::Element;
use iced::widget::image::Handle;
use iced::widget;
use iced_core::font::Weight;
use iced_core::widget::Id;
use iced_core::{Length, Padding};
use std::borrow::Cow;
use crate::ui::convert::ToRadius;

pub type Button<'a, Message> = Builder<'a, Message, Image<Handle, Message>>;

/// A button constructed from an image handle, using image button styling.
pub fn image<'a, Message>(handle: impl Into<Handle> + 'a) -> Button<'a, Message> {
    Button::new(Image {
        image: widget::image(handle).border_radius(([9.0; 4]).to_radius()),
        selected: false,
        on_remove: None,
    })
}

/// The image variant of a button.
pub struct Image<Handle, Message> {
    image: widget::Image<Handle>,
    selected: bool,
    on_remove: Option<Message>,
}

impl<'a, Message> Button<'a, Message> {
    #[inline]
    pub fn new(variant: Image<Handle, Message>) -> Self {
        Self {
            id: Id::unique(),
            label: Cow::Borrowed(""),
            #[cfg(feature = "a11y")]
            name: Cow::Borrowed(""),
            #[cfg(feature = "a11y")]
            description: Cow::Borrowed(""),
            tooltip: Cow::Borrowed(""),
            on_press: None,
            width: Length::Shrink,
            height: Length::Shrink,
            padding: Padding::from(0),
            spacing: 0,
            icon_size: 16,
            line_height: 20,
            font_size: 14,
            font_weight: Weight::Normal,
            class: crate::ui::theme::style::Button::Image,
            variant,
        }
    }

    #[inline]
    pub fn on_remove(mut self, message: Message) -> Self {
        self.variant.on_remove = Some(message);
        self
    }

    #[inline]
    pub fn on_remove_maybe(mut self, message: Option<Message>) -> Self {
        self.variant.on_remove = message;
        self
    }

    #[inline]
    pub fn selected(mut self, selected: bool) -> Self {
        self.variant.selected = selected;
        self
    }
}

impl<'a, Message> From<Button<'a, Message>> for Element<'a, Message>
where
    Handle: Clone,
    Message: Clone + 'static,
{
    fn from(builder: Button<'a, Message>) -> Element<'a, Message> {
        let content = builder
            .variant
            .image
            .width(builder.width)
            .height(builder.height);

        let mut button = super::custom_image_button(content, builder.variant.on_remove)
            .padding(0)
            .selected(builder.variant.selected)
            .id(builder.id)
            .on_press_maybe(builder.on_press)
            .class(builder.class);

        #[cfg(feature = "a11y")]
        {
            button = button.name(builder.name).description(builder.description);
        }

        button.into()
    }
}
