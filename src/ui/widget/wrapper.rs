// Copyright 2024 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/wrapper.rs
//!
//! Reference-counted wrappers used to share widget state and elements.

use std::borrow::Borrow;
use std::cell::RefCell;
use std::rc::Rc;

use crate::ui::Element;
use iced::{Length, Rectangle, Size};
use iced_core::widget::tree;
use iced_core::{Widget, widget};

/// Shared, mutable state of a widget, kept on the UI thread: it is neither
/// `Send` nor `Sync`, so it cannot leave it. A popup's view that shares it
/// stays there too: see [`crate::ui::surface::View`].
#[derive(Debug)]
pub struct RcWrapper<T> {
    pub(crate) data: Rc<RefCell<T>>,
}

impl<T: Default> Default for RcWrapper<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Clone for RcWrapper<T> {
    fn clone(&self) -> Self {
        Self {
            data: Rc::clone(&self.data),
        }
    }
}

impl<T> RcWrapper<T> {
    pub fn new(element: T) -> Self {
        Self {
            data: Rc::new(RefCell::new(element)),
        }
    }

    pub fn with_data<O>(&self, f: impl FnOnce(&T) -> O) -> O {
        f(&RefCell::borrow(&self.data))
    }

    pub fn with_data_mut<O>(&self, f: impl FnOnce(&mut T) -> O) -> O {
        f(&mut RefCell::borrow_mut(&self.data))
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
    fn clones_share_the_data() {
        let w = RcWrapper::new(1u8);
        let c = w.clone();
        c.with_data_mut(|v| *v = 2);
        drop(c);
        assert_eq!(w.with_data(|v| *v), 2);
    }
}
