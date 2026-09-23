// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/autosize.rs
//!
//! Autosize Container, which will resize the window to its contents.

pub use iced::widget::container::{Catalog, Style};
use iced_core::event::Event;
use iced_core::widget::{Id, Operation, Tree};
use iced_core::{
    Clipboard, Element, Layout, Length, Rectangle, Shell, Vector, Widget, layout, mouse, overlay,
    renderer,
};

pub fn autosize<'a, Message: 'static, Theme, E>(
    content: E,
    id: Id,
) -> Autosize<'a, Message, Theme, crate::ui::Renderer>
where
    E: Into<Element<'a, Message, Theme, crate::ui::Renderer>>,
    Theme: iced::widget::container::Catalog,
    <Theme as iced::widget::container::Catalog>::Class<'a>: From<crate::ui::theme::Container<'a>>,
{
    Autosize::new(content, id)
}

/// An element decorating some content.
///
/// It is normally used for alignment purposes.
#[allow(missing_debug_implementations)]
pub struct Autosize<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    content: Element<'a, Message, Theme, Renderer>,
    id: Id,
    limits: layout::Limits,
    auto_width: bool,
    auto_height: bool,
}

impl<'a, Message, Theme, Renderer> Autosize<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    /// Creates an empty [`IdContainer`].
    pub(crate) fn new<T>(content: T, id: Id) -> Self
    where
        T: Into<Element<'a, Message, Theme, Renderer>>,
    {
        Autosize {
            content: content.into(),
            id,
            limits: layout::Limits::NONE,
            auto_width: true,
            auto_height: true,
        }
    }

    #[inline]
    pub fn limits(mut self, limits: layout::Limits) -> Self {
        self.limits = limits;
        self
    }

    #[inline]
    pub fn auto_width(mut self, auto_width: bool) -> Self {
        self.auto_width = auto_width;
        self
    }

    #[inline]
    pub fn auto_height(mut self, auto_height: bool) -> Self {
        self.auto_height = auto_height;
        self
    }

    #[inline]
    pub fn max_width(mut self, v: f32) -> Self {
        self.limits = self.limits.max_width(v);
        self
    }

    #[inline]
    pub fn max_height(mut self, v: f32) -> Self {
        self.limits = self.limits.max_height(v);
        self
    }

    #[inline]
    pub fn min_width(mut self, v: f32) -> Self {
        self.limits = self.limits.min_width(v);
        self
    }

    #[inline]
    pub fn min_height(mut self, v: f32) -> Self {
        self.limits = self.limits.min_height(v);
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Autosize<'_, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> iced_core::Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let mut my_limits = self.limits;
        let min = limits.min();
        let max = limits.max();
        if !self.auto_width {
            my_limits = limits.min_width(min.width).max_width(max.width);
        }
        if !self.auto_height {
            my_limits = limits.min_height(min.height).max_height(max.height);
        }
        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &my_limits);
        let size = node.size();
        layout::Node::with_children(size, vec![node])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(Some(&self.id), layout.bounds());
        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout.children().next().unwrap(),
                renderer,
                operation,
            );
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().unwrap(),
            cursor_position,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let content_layout = layout.children().next().unwrap();
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            content_layout,
            cursor_position,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        renderer_style: &renderer::Style,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let content_layout = layout.children().next().unwrap();
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            renderer_style,
            content_layout,
            cursor_position,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<Autosize<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Renderer: 'a + iced_core::Renderer,
    Theme: 'a,
{
    fn from(c: Autosize<'a, Message, Theme, Renderer>) -> Element<'a, Message, Theme, Renderer> {
        Element::new(c)
    }
}
