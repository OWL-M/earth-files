// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic d9431dc, src/widget/scrollable/mod.rs

mod scrollable;

pub use scrollable::{horizontal, scrollable, vertical};

pub use iced::widget::scrollable::{AbsoluteOffset, RelativeOffset, Viewport};

// `scroll_to`/`scroll_by`/`snap_to` were `Task`-returning helpers on the fork's
// `iced::widget::scrollable`. Upstream ships them as bare `Operation`s in
// `iced_core::widget::operation::scrollable`; these wrap them back into tasks.
use iced_core::widget::Id;
use iced_core::widget::operation::scrollable as op;
use iced_runtime::Task;

/// Produces a [`Task`] that scrolls the scrollable with the given [`Id`] to the
/// provided [`AbsoluteOffset`].
pub fn scroll_to<T: Send + 'static>(
    id: Id,
    offset: AbsoluteOffset<Option<f32>>,
) -> Task<T> {
    iced_runtime::task::widget(op::scroll_to(id, offset))
}

/// Produces a [`Task`] that scrolls the scrollable with the given [`Id`] by the
/// provided [`AbsoluteOffset`].
pub fn scroll_by<T: Send + 'static>(id: Id, offset: AbsoluteOffset) -> Task<T> {
    iced_runtime::task::widget(op::scroll_by(id, offset))
}

/// Produces a [`Task`] that snaps the scrollable with the given [`Id`] to the
/// provided [`RelativeOffset`].
pub fn snap_to<T: Send + 'static>(
    id: Id,
    offset: RelativeOffset<Option<f32>>,
) -> Task<T> {
    iced_runtime::task::widget(op::snap_to(id, offset))
}
