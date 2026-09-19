// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/id_container.rs

use iced_core::event::Event;
use iced_core::widget::{Id, Operation, Tree};
use iced_core::{
    Clipboard, Element, Layout, Length, Rectangle, Shell, Vector, Widget, layout, mouse, overlay,
    renderer,
};
pub use iced::widget::container::{Catalog, Style};

pub fn id_container<'a, Message: 'static, Theme, E>(
    content: E,
    id: Id,
) -> IdContainer<'a, Message, Theme, iced::Renderer>
where
    E: Into<Element<'a, Message, Theme, iced::Renderer>>,
    Theme: iced::widget::container::Catalog,
    <Theme as iced::widget::container::Catalog>::Class<'a>: From<crate::ui::theme::Container<'a>>,
{
    IdContainer::new(content, id)
}

/// An element decorating some content.
///
/// It is normally used for alignment purposes.
#[allow(missing_debug_implementations)]
pub struct IdContainer<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    content: Element<'a, Message, Theme, Renderer>,
    id: Id,
}

impl<'a, Message, Theme, Renderer> IdContainer<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    /// Creates an empty [`IdContainer`].
    pub(crate) fn new<T>(content: T, id: Id) -> Self
    where
        T: Into<Element<'a, Message, Theme, Renderer>>,
    {
        IdContainer {
            content: content.into(),
            id,
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for IdContainer<'_, Message, Theme, Renderer>
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
        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
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
                layout
                    .children()
                    .next()
                    .unwrap(),
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
            layout
                .children()
                .next()
                .unwrap(),
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
            layout
                .children()
                .next()
                .unwrap(),
            renderer,
            viewport,
            translation,
        )
    }

    // TODO(dnd): tab drag-to-reorder must be rebuilt on smithay-clipboard. iced
    // 0.14's `Widget` has no `drag_destinations` pass-through, so the tab bar
    // leaf is unreachable from the root walk.

}

impl<'a, Message, Theme, Renderer> From<IdContainer<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Renderer: 'a + iced_core::Renderer,
    Theme: 'a,
{
    fn from(c: IdContainer<'a, Message, Theme, Renderer>) -> Element<'a, Message, Theme, Renderer> {
        Element::new(c)
    }
}
