// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/settings/mod.rs

pub mod item;
pub mod section;

pub use self::item::{item, item_row};
pub use self::section::{Section, section};

use crate::ui::convert::ToPixels;
use crate::ui::theme;
use crate::ui::{Element, Renderer, Theme};
use iced::widget::Column;

/// A column with a predefined style for creating a settings panel
#[must_use]
pub fn view_column<Message: 'static>(
    children: Vec<Element<Message>>,
) -> Column<Message, Theme, Renderer> {
    crate::ui::widget::Column::with_children(children).spacing(theme::spacing().space_m.to_pixels())
}
