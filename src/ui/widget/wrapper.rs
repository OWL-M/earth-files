// Copyright 2024 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/wrapper.rs
//!
//! Reference-counted wrappers used to share widget state and elements.

use std::borrow::Borrow;
use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::rc::Rc;
use std::thread::{self, ThreadId};

use crate::ui::Element;
use iced::{Length, Rectangle, Size};
use iced_core::widget::tree;
use iced_core::{Widget, widget};

#[derive(Debug)]
pub struct RcWrapper<T> {
    /// Dropped by hand, and only on the creating thread; see [`Drop`].
    pub(crate) data: ManuallyDrop<Rc<RefCell<T>>>,
    pub(crate) thread_id: ThreadId,
}

impl<T: Default> Default for RcWrapper<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Clone for RcWrapper<T> {
    /// # Panics
    ///
    /// Will panic if used outside of original thread.
    fn clone(&self) -> Self {
        assert_eq!(self.thread_id, thread::current().id());
        Self {
            data: self.data.clone(),
            thread_id: self.thread_id,
        }
    }
}

impl<T> Drop for RcWrapper<T> {
    /// # Panics
    ///
    /// Will panic if dropped outside of original thread. The `Rc` is then
    /// leaked rather than released: an assertion alone would not stop the
    /// field's own drop from decrementing the non-atomic count on the wrong
    /// thread during unwinding.
    fn drop(&mut self) {
        if self.thread_id == thread::current().id() {
            // SAFETY: dropped exactly once, here, and never touched again.
            unsafe { ManuallyDrop::drop(&mut self.data) };
        } else if !thread::panicking() {
            panic!("RcWrapper dropped off the thread that created it; leaked");
        }
    }
}

// SAFETY: `Rc<RefCell<T>>` is neither `Send` nor `Sync`. These impls exist
// only so a wrapper can ride inside a `surface::View` closure (`Arc<dyn Fn()
// + Send + Sync>`) that a `Task` carries through the executor. The invariant
// is that the wrapper is only ever *moved* between threads, never used there:
// every operation that touches the `Rc` count or the `RefCell` (`clone`,
// `drop`, `with_data`, `with_data_mut`, `overlay`) asserts it runs on the
// thread that created the wrapper, so a violation is a panic, not a data
// race; a drop on the wrong thread leaks the `Rc` rather than touching its
// count.
unsafe impl<M: 'static> Send for RcWrapper<M> {}
unsafe impl<M: 'static> Sync for RcWrapper<M> {}

impl<T> RcWrapper<T> {
    pub fn new(element: T) -> Self {
        Self {
            data: ManuallyDrop::new(Rc::new(RefCell::new(element))),
            thread_id: thread::current().id(),
        }
    }

    /// # Panics
    ///
    /// Will panic if used outside of original thread.
    pub fn with_data<O>(&self, f: impl FnOnce(&T) -> O) -> O {
        assert_eq!(self.thread_id, thread::current().id());
        let my_ref: &T = &RefCell::borrow(self.data.as_ref());
        f(my_ref)
    }

    /// # Panics
    ///
    /// Will panic if used outside of original thread.
    pub fn with_data_mut<O>(&self, f: impl FnOnce(&mut T) -> O) -> O {
        assert_eq!(self.thread_id, thread::current().id());
        let my_refmut: &mut T = &mut RefCell::borrow_mut(self.data.as_ref());
        f(my_refmut)
    }
}

#[derive(Clone)]
pub struct RcElementWrapper<M> {
    pub(crate) element: RcWrapper<Element<'static, M>>,
}

impl<M> RcElementWrapper<M> {
    #[must_use]
    pub fn new(element: Element<'static, M>) -> Self {
        RcElementWrapper {
            element: RcWrapper::new(element),
        }
    }
}

impl<M: 'static> Borrow<dyn Widget<M, crate::ui::Theme, crate::ui::Renderer>>
    for RcElementWrapper<M>
{
    fn borrow(&self) -> &(dyn Widget<M, crate::ui::Theme, crate::ui::Renderer> + 'static) {
        self
    }
}

impl<M> Widget<M, crate::ui::Theme, crate::ui::Renderer> for RcElementWrapper<M> {
    fn size(&self) -> Size<Length> {
        self.element.with_data(|e| e.as_widget().size())
    }

    fn size_hint(&self) -> Size<Length> {
        self.element.with_data(move |e| e.as_widget().size_hint())
    }

    fn layout(
        &mut self,
        tree: &mut tree::Tree,
        renderer: &crate::ui::Renderer,
        limits: &iced_core::layout::Limits,
    ) -> iced_core::layout::Node {
        self.element
            .with_data_mut(|e| e.as_widget_mut().layout(tree, renderer, limits))
    }

    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &iced_core::renderer::Style,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.element.with_data(move |e| {
            e.as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        });
    }

    fn tag(&self) -> tree::Tag {
        self.element.with_data(|e| e.as_widget().tag())
    }

    fn state(&self) -> tree::State {
        self.element.with_data(|e| e.as_widget().state())
    }

    fn children(&self) -> Vec<tree::Tree> {
        self.element.with_data(|e| e.as_widget().children())
    }

    fn diff(&self, tree: &mut tree::Tree) {
        self.element.with_data(|e| e.as_widget().diff(tree));
    }

    fn operate(
        &mut self,
        state: &mut tree::Tree,
        layout: iced_core::Layout<'_>,
        renderer: &crate::ui::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.element.with_data_mut(|e| {
            e.as_widget_mut()
                .operate(state, layout, renderer, operation);
        });
    }

    fn update(
        &mut self,
        state: &mut tree::Tree,
        event: &iced::Event,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        renderer: &crate::ui::Renderer,
        clipboard: &mut dyn iced_core::Clipboard,
        shell: &mut iced_core::Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        self.element.with_data_mut(|e| {
            e.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            )
        })
    }

    fn mouse_interaction(
        &self,
        state: &tree::Tree,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        viewport: &Rectangle,
        renderer: &crate::ui::Renderer,
    ) -> iced_core::mouse::Interaction {
        self.element.with_data(|e| {
            e.as_widget()
                .mouse_interaction(state, layout, cursor, viewport, renderer)
        })
    }

    fn overlay<'a>(
        &'a mut self,
        state: &'a mut tree::Tree,
        layout: iced_core::Layout<'a>,
        renderer: &crate::ui::Renderer,
        viewport: &Rectangle,
        translation: iced_core::Vector,
    ) -> Option<iced_core::overlay::Element<'a, M, crate::ui::Theme, crate::ui::Renderer>> {
        assert_eq!(self.element.thread_id, thread::current().id());
        Rc::get_mut(&mut self.element.data).and_then(|e| {
            e.get_mut()
                .as_widget_mut()
                .overlay(state, layout, renderer, viewport, translation)
        })
    }
}

impl<Message: 'static> From<RcElementWrapper<Message>> for Element<'static, Message> {
    fn from(wrapper: RcElementWrapper<Message>) -> Self {
        Element::new(wrapper)
    }
}

impl<Message: 'static> From<Element<'static, Message>> for RcElementWrapper<Message> {
    fn from(e: Element<'static, Message>) -> Self {
        RcElementWrapper::new(e)
    }
}

#[cfg(test)]
mod tests {
    use super::RcWrapper;

    #[test]
    fn clone_and_drop_on_the_creating_thread_work() {
        let w = RcWrapper::new(1u8);
        let c = w.clone();
        assert_eq!(c.with_data(|v| *v), 1);
        drop(c);
        assert_eq!(w.with_data(|v| *v), 1);
    }

    #[test]
    fn drop_on_another_thread_panics_and_leaks() {
        let w = RcWrapper::new(1u8);
        let c = w.clone();
        assert_eq!(std::rc::Rc::strong_count(&w.data), 2);
        let joined = std::thread::spawn(move || drop(c)).join();
        assert!(joined.is_err());
        // The count was not touched from the other thread: the clone leaked
        assert_eq!(std::rc::Rc::strong_count(&w.data), 2);
        assert_eq!(w.with_data(|v| *v), 1);
    }
}
