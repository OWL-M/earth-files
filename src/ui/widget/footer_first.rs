// SPDX-License-Identifier: GPL-3.0-only

//! A body above a footer, where the footer is sized first.
//!
//! A column hands out height in order, so when a dialog is given less than
//! it asks for, its body takes it all and the buttons under it are squeezed
//! flat. Here the footer is laid out first with its full height, and the body
//! gets what is left; a body that can scroll then scrolls instead.

use iced_core::event::Event;
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Clipboard, Layout, Length, Point, Rectangle, Shell, Size, Vector, Widget, layout, mouse,
    overlay, renderer,
};

use crate::ui::Element;

/// `body` above `footer`, `spacing` apart; see the module.
pub fn footer_first<'a, Message>(
    body: impl Into<Element<'a, Message>>,
    footer: impl Into<Element<'a, Message>>,
    spacing: f32,
) -> FooterFirst<'a, Message> {
    FooterFirst {
        children: [body.into(), footer.into()],
        spacing,
    }
}

#[allow(missing_debug_implementations)]
pub struct FooterFirst<'a, Message> {
    /// The body, then the footer
    children: [Element<'a, Message>; 2],
    spacing: f32,
}

impl<Message> Widget<Message, crate::ui::Theme, crate::ui::Renderer> for FooterFirst<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &crate::ui::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let [body, footer] = &mut self.children;
        let [body_tree, footer_tree] = &mut tree.children[..] else {
            unreachable!("two children");
        };
        let max = limits.max();
        let footer_node = footer.as_widget_mut().layout(
            footer_tree,
            renderer,
            &layout::Limits::new(Size::ZERO, max),
        );
        let footer_height = footer_node.size().height;
        let room = (max.height - footer_height - self.spacing).max(0.0);
        let body_node = body.as_widget_mut().layout(
            body_tree,
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(max.width, room)),
        );
        let body_size = body_node.size();
        let width = body_size.width.max(footer_node.size().width);
        let footer_y = body_size.height + self.spacing;
        layout::Node::with_children(
            limits.resolve(
                Length::Shrink,
                Length::Shrink,
                Size::new(width, footer_y + footer_height),
            ),
            vec![body_node, footer_node.move_to(Point::new(0.0, footer_y))],
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &crate::ui::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.traverse(&mut |operation| {
            for ((child, tree), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(tree, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &crate::ui::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((child, tree), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &crate::ui::Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, tree), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        for ((child, tree), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &crate::ui::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, crate::ui::Theme, crate::ui::Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<FooterFirst<'a, Message>> for Element<'a, Message> {
    fn from(widget: FooterFirst<'a, Message>) -> Element<'a, Message> {
        Element::new(widget)
    }
}

#[cfg(test)]
mod tests {
    use super::footer_first;
    use crate::ui::widget::{measure_bounds, space};
    use crate::ui::{Element, Theme};
    use iced_core::{Length, Point, Rectangle, Size, mouse, renderer};
    use iced_runtime::user_interface::{Cache, UserInterface};
    use std::cell::Cell;

    /// Given less height than the body asks for, the footer keeps all of its
    /// own at the bottom, and the body gets the rest
    #[test]
    fn a_short_room_squeezes_the_body_not_the_footer() {
        let (body, footer) = (
            Cell::new(Rectangle::default()),
            Cell::new(Rectangle::default()),
        );
        let view: Element<'_, ()> = footer_first(
            measure_bounds(
                crate::ui::widget::scrollable(
                    space::vertical()
                        .width(Length::Fixed(100.0))
                        .height(Length::Fixed(500.0)),
                ),
                &body,
            ),
            measure_bounds(
                space::vertical()
                    .width(Length::Fixed(100.0))
                    .height(Length::Fixed(40.0)),
                &footer,
            ),
            10.0,
        )
        .into();
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let size = Size::new(100.0, 200.0);
        let mut ui = UserInterface::build(view, size, Cache::default(), &mut renderer);
        ui.draw(
            &mut renderer,
            &Theme::default(),
            &renderer::Style::default(),
            mouse::Cursor::Unavailable,
        );
        assert_eq!(
            footer.get(),
            Rectangle::new(Point::new(0.0, 160.0), Size::new(100.0, 40.0))
        );
        assert_eq!(body.get().height, 150.0);
    }
}
