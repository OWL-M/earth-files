// SPDX-License-Identifier: GPL-3.0-only

//! A wrapper that records where its content is on screen.
//!
//! [`MeasureBounds`] lays its content out untouched and, each time it is
//! drawn, writes the content's bounds in window coordinates into a [`Cell`]
//! the caller owns, so something elsewhere in the window can line up with
//! it: e.g. a dialog centred over the file view rather than the whole
//! window. Layout alone does not know where a widget ends up, so the value
//! is the one from the last draw; a reader sees the previous frame's, which
//! differs only while the window is resized or something slides.

use std::cell::Cell;

use iced_core::event::Event;
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Clipboard, Element, Layout, Length, Rectangle, Shell, Size, Vector, Widget, layout, mouse,
    overlay, renderer,
};

use crate::ui::{Renderer, Theme};

/// Wraps `content`, recording its bounds into `bounds` each time it is drawn.
/// See the [module documentation](self).
pub fn measure_bounds<'a, Message>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    bounds: &'a Cell<Rectangle>,
) -> MeasureBounds<'a, Message> {
    MeasureBounds {
        content: content.into(),
        bounds,
    }
}

#[allow(missing_debug_implementations)]
pub struct MeasureBounds<'a, Message> {
    content: Element<'a, Message, Theme, Renderer>,
    bounds: &'a Cell<Rectangle>,
}

impl<Message> Widget<Message, Theme, Renderer> for MeasureBounds<'_, Message> {
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
        self.bounds.set(layout.bounds());
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

impl<'a, Message: 'a> From<MeasureBounds<'a, Message>> for Element<'a, Message, Theme, Renderer> {
    fn from(measure_bounds: MeasureBounds<'a, Message>) -> Self {
        Element::new(measure_bounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::Point;
    use iced_runtime::user_interface::{Cache, UserInterface};

    use crate::ui::widget::{Row, space};

    #[test]
    fn the_bounds_on_screen_are_recorded_when_drawn() {
        let bounds = Cell::new(Rectangle::default());
        let view: Element<'_, (), Theme, Renderer> = Row::new()
            .push(space::horizontal().width(Length::Fixed(30.0)))
            .push(measure_bounds(
                space::vertical()
                    .width(Length::Fixed(100.0))
                    .height(Length::Fixed(40.0)),
                &bounds,
            ))
            .into();
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(
            view,
            Size::new(200.0, 200.0),
            Cache::default(),
            &mut renderer,
        );
        assert_eq!(bounds.get(), Rectangle::default(), "not drawn yet");
        ui.draw(
            &mut renderer,
            &Theme::default(),
            &renderer::Style::default(),
            mouse::Cursor::Unavailable,
        );
        assert_eq!(
            bounds.get(),
            Rectangle::new(Point::new(30.0, 0.0), Size::new(100.0, 40.0))
        );
    }
}
