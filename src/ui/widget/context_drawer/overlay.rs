// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

use crate::ui::Element;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{self, Operation};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Event, Point, Size, mouse};
use iced_core::{Renderer, touch};

pub(super) struct Overlay<'a, 'b, Message> {
    pub(crate) position: Point,
    pub(super) content: &'b mut Element<'a, Message>,
    pub(super) tree: &'b mut widget::Tree,
    pub(super) width: f32,
}

impl<Message> overlay::Overlay<Message, crate::ui::Theme, iced::Renderer> for Overlay<'_, '_, Message>
where
    Message: Clone,
{
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let position = self.position;
        let limits = layout::Limits::new(Size::ZERO, bounds)
            .width(self.width)
            .height(bounds.height - 8.0 - position.y);

        let node = self
            .content
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let node_size = node.size();

        node.move_to(Point {
            x: if bounds.width > node_size.width - 8.0 {
                bounds.width - node_size.width - 8.0
            } else {
                0.0
            },
            y: if bounds.height > node_size.height - 8.0 {
                bounds.height - node_size.height - 8.0
            } else {
                0.0
            },
        })
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        self.content.as_widget_mut().update(
            self.tree,
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        );
        match event {
            Event::Mouse(e) if !matches!(e, mouse::Event::CursorLeft) => {
                if cursor.is_over(layout.bounds()) {
                    shell.capture_event();
                }
            }
            Event::Touch(e) if !matches!(e, touch::Event::FingerLost { .. }) => {
                if cursor.is_over(layout.bounds()) {
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        renderer.with_layer(layout.bounds(), |renderer| {
            self.content.as_widget().draw(
                self.tree,
                renderer,
                theme,
                style,
                layout,
                cursor,
                &layout.bounds(),
            );
        });
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation<()>,
    ) {
        self.content
            .as_widget_mut()
            .operate(self.tree, layout, renderer, operation);
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let viewport = &layout.bounds();
        let interaction = self
            .content
            .as_widget()
            .mouse_interaction(self.tree, layout, cursor, viewport, renderer);
        if let mouse::Interaction::None = interaction
            && cursor.is_over(layout.bounds())
        {
            return mouse::Interaction::Idle;
        }
        interaction
    }

    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &iced::Renderer,
    ) -> Option<overlay::Element<'c, Message, crate::ui::Theme, iced::Renderer>> {
        let viewport = &layout.bounds();

        self.content.as_widget_mut().overlay(
            self.tree,
            layout,
            renderer,
            viewport,
            iced::Vector::default(),
        )
    }
}
