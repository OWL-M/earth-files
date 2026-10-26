// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/scrollable/scrollable.rs

use super::Smooth;
use super::smooth::Axis;
use crate::ui::Element;
use iced::widget;

pub fn scrollable<'a, Message>(element: impl Into<Element<'a, Message>>) -> Smooth<'a, Message> {
    vertical(element)
}

pub fn vertical<'a, Message>(element: impl Into<Element<'a, Message>>) -> Smooth<'a, Message> {
    Smooth::new(widget::scrollable(element), Axis::Vertical)
}

pub fn horizontal<'a, Message>(element: impl Into<Element<'a, Message>>) -> Smooth<'a, Message> {
    Smooth::new(widget::scrollable(element), Axis::Horizontal)
}
