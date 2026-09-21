// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/mod.rs (tooltip module)

use crate::ui::Element;

use crate::ui::convert::ToPixels;
pub use iced::widget::tooltip::Position;

pub type Tooltip<'a, Message> =
    iced::widget::Tooltip<'a, Message, crate::ui::Theme, crate::ui::Renderer>;

pub fn tooltip<'a, Message>(
    content: impl Into<Element<'a, Message>>,
    tooltip: impl Into<Element<'a, Message>>,
    position: Position,
) -> Tooltip<'a, Message> {
    let xxs = crate::ui::theme::spacing().space_xxs;

    Tooltip::new(content, tooltip, position)
        .class(crate::ui::theme::Container::Tooltip)
        .padding(xxs.to_pixels())
        .gap(1)
}
