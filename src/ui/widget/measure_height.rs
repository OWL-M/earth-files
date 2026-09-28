// SPDX-License-Identifier: GPL-3.0-only

//! A wrapper that records its content's laid-out height.
//!
//! [`MeasureHeight`] lays its content out untouched and writes the height it
//! came to, plus a fixed extra, into a [`Cell`] the caller owns, so something
//! laid out elsewhere can make room for it: e.g. a file list keeping space
//! under its last row for a card floating over the corner. The value is the
//! one from the last layout, so a reader laid out before this widget sees
//! the previous frame's; it catches up on the next layout.

use std::cell::Cell;

use iced_core::event::Event;
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Clipboard, Element, Layout, Length, Rectangle, Shell, Size, Vector, Widget, layout, mouse,
    overlay, renderer,
};

use crate::ui::{Renderer, Theme};

/// Wraps `content`, recording its height into `height` on each layout. See
/// the [module documentation](self).
pub fn measure_height<'a, Message>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    height: &'a Cell<f32>,
) -> MeasureHeight<'a, Message> {
    MeasureHeight {
        content: content.into(),
        height,
        extra: 0.0,
    }
}

#[allow(missing_debug_implementations)]
pub struct MeasureHeight<'a, Message> {
    content: Element<'a, Message, Theme, Renderer>,
    height: &'a Cell<f32>,
    extra: f32,
}

impl<Message> MeasureHeight<'_, Message> {
    /// Adds `extra` to the height recorded: e.g. a margin around the content
    /// that is not part of its layout.
    #[must_use]
    pub const fn plus(mut self, extra: f32) -> Self {
        self.extra = extra;
        self
    }
}

impl<Message> Widget<Message, Theme, Renderer> for MeasureHeight<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content]);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let content = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        self.height.set(content.size().height + self.extra);
        layout::Node::with_children(content.size(), vec![content])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            operation,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().unwrap(),
            cursor,
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
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().unwrap(),
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().unwrap(),
            cursor,
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

impl<'a, Message: 'a> From<MeasureHeight<'a, Message>> for Element<'a, Message, Theme, Renderer> {
    fn from(measure_height: MeasureHeight<'a, Message>) -> Self {
        Element::new(measure_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_runtime::user_interface::{Cache, UserInterface};

    use crate::ui::widget::space;

    #[test]
    fn the_content_height_and_the_extra_are_recorded() {
        let height = Cell::new(0.0);
        let content = space::vertical()
            .width(Length::Fixed(100.0))
            .height(Length::Fixed(40.0));
        let view: Element<'_, (), Theme, Renderer> =
            measure_height(content, &height).plus(15.0).into();
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let _ui = UserInterface::build(
            view,
            Size::new(200.0, 200.0),
            Cache::default(),
            &mut renderer,
        );
        assert_eq!(height.get(), 55.0);
    }
}
