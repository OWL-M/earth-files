// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/scrollable/mod.rs

#[allow(clippy::module_inception)]
mod scrollable;
mod smooth;

pub use scrollable::{horizontal, scrollable, vertical};
pub use smooth::Smooth;

pub use iced::widget::scrollable::{AbsoluteOffset, RelativeOffset, Viewport};

// `scroll_to`/`scroll_by`/`snap_to` are bare `Operation`s in
// `iced_core::widget::operation::scrollable`; these wrap them into `Task`s.
use iced_core::widget::Id;
use iced_core::widget::operation::scrollable as op;
use iced_runtime::Task;

/// Produces a [`Task`] that scrolls the scrollable with the given [`Id`] to the
/// provided [`AbsoluteOffset`].
pub fn scroll_to<T: Send + 'static>(id: Id, offset: AbsoluteOffset<Option<f32>>) -> Task<T> {
    iced_runtime::task::widget(op::scroll_to(id, offset))
}

/// Produces a [`Task`] that glides the [`Smooth`] scrollable with the given
/// [`Id`] to the provided [`AbsoluteOffset`], where [`scroll_to`] jumps. An
/// axis left `None` keeps where it is going.
pub fn glide_to<T: Send + 'static>(id: Id, offset: AbsoluteOffset<Option<f32>>) -> Task<T> {
    iced_runtime::task::widget(GlideTo {
        id,
        offset,
        output: std::marker::PhantomData,
    })
}

struct GlideTo<T> {
    id: Id,
    offset: AbsoluteOffset<Option<f32>>,
    output: std::marker::PhantomData<T>,
}

impl<T: Send> iced_core::widget::Operation<T> for GlideTo<T> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced_core::widget::Operation<T>)) {
        operate(self);
    }

    fn custom(
        &mut self,
        id: Option<&Id>,
        _bounds: iced_core::Rectangle,
        state: &mut dyn std::any::Any,
    ) {
        if id == Some(&self.id)
            && let Some(request) = state.downcast_mut::<smooth::GlideRequest>()
        {
            request.0 = Some(self.offset);
        }
    }
}

/// Produces a [`Task`] that scrolls the scrollable with the given [`Id`] by the
/// provided [`AbsoluteOffset`].
pub fn scroll_by<T: Send + 'static>(id: Id, offset: AbsoluteOffset) -> Task<T> {
    iced_runtime::task::widget(op::scroll_by(id, offset))
}

/// Produces a [`Task`] that snaps the scrollable with the given [`Id`] to the
/// provided [`RelativeOffset`].
pub fn snap_to<T: Send + 'static>(id: Id, offset: RelativeOffset<Option<f32>>) -> Task<T> {
    iced_runtime::task::widget(op::snap_to(id, offset))
}
