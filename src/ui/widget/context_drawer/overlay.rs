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

impl<Message> overlay::Overlay<Message, crate::ui::Theme, crate::ui::Renderer>
    for Overlay<'_, '_, Message>
where
    Message: Clone,
{
    fn layout(&mut self, renderer: &crate::ui::Renderer, bounds: Size) -> layout::Node {
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
        renderer: &crate::ui::Renderer,
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
        // Releases and lifts are left uncaptured: a captured event never
        // reaches the base tree, so a press made there (a rubber-band
        // selection, a file drag) and released over the drawer would leave
        // that gesture armed. While the pointer is over the drawer,
        // `mouse_interaction` is never `None`, so the runtime hands the base
        // tree an unavailable cursor; a widget beneath must then only end its
        // gesture, not treat the release as a click (see `MouseArea`, whose
        // `on_release` needs the cursor over it).
        match event {
            Event::Mouse(e)
                if !matches!(
                    e,
                    mouse::Event::CursorLeft | mouse::Event::ButtonReleased(_)
                ) =>
            {
                if cursor.is_over(layout.bounds()) {
                    shell.capture_event();
                }
            }
            Event::Touch(e)
                if !matches!(
                    e,
                    touch::Event::FingerLost { .. } | touch::Event::FingerLifted { .. }
                ) && cursor.is_over(layout.bounds()) =>
            {
                shell.capture_event();
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        renderer: &mut crate::ui::Renderer,
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
        renderer: &crate::ui::Renderer,
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
        renderer: &crate::ui::Renderer,
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
        renderer: &crate::ui::Renderer,
    ) -> Option<overlay::Element<'c, Message, crate::ui::Theme, crate::ui::Renderer>> {
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

#[cfg(test)]
mod tests {
    use crate::ui::widget::context_drawer::context_drawer;
    use iced::{Event, Length, Point, Size, mouse};
    use iced_core::clipboard;
    use iced_runtime::user_interface::{Cache, UserInterface};

    const WINDOW: Size = Size::new(800.0, 400.0);

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Pressed,
        Released,
        Drag,
        DragEnd,
        Close,
    }

    fn view() -> crate::ui::Element<'static, Msg> {
        let list = crate::mouse_area::MouseArea::new(
            crate::ui::widget::space::horizontal()
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(|_| Msg::Pressed)
        .on_release(|_| Msg::Released)
        .on_drag(|_| Msg::Drag)
        .on_drag_end(|_| Msg::DragEnd);
        context_drawer(
            None,
            None,
            None,
            None,
            Msg::Close,
            list,
            crate::ui::widget::text("details"),
            200.0,
        )
        .into()
    }

    struct Harness {
        renderer: crate::ui::Renderer,
        cache: Option<Cache>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                renderer: iced_texture_cache::testing::headless_tiny_skia(),
                cache: Some(Cache::default()),
            }
        }

        fn send(&mut self, at: Point, event: Event) -> Vec<Msg> {
            let mut messages = Vec::new();
            let mut ui = UserInterface::build(
                view(),
                WINDOW,
                self.cache.take().unwrap(),
                &mut self.renderer,
            );
            let _ = ui.update(
                &[event],
                mouse::Cursor::Available(at),
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            self.cache = Some(ui.into_cache());
            messages
        }
    }

    fn moved(h: &mut Harness, at: Point) -> Vec<Msg> {
        h.send(at, Event::Mouse(mouse::Event::CursorMoved { position: at }))
    }

    fn pressed(h: &mut Harness, at: Point) -> Vec<Msg> {
        h.send(
            at,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        )
    }

    fn released(h: &mut Harness, at: Point) -> Vec<Msg> {
        h.send(
            at,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        )
    }

    /// The overlay spans the content but for an 8px margin at the window's
    /// right and bottom edges, the only place the list below can be pressed.
    const LIST: Point = Point::new(796.0, 200.0);
    const DRAWER: Point = Point::new(700.0, 250.0);

    #[test]
    fn a_list_drag_released_over_the_drawer_ends() {
        let mut h = Harness::new();
        moved(&mut h, LIST);
        assert_eq!(pressed(&mut h, LIST), [Msg::Pressed]);
        assert_eq!(moved(&mut h, Point::new(796.0, 230.0)), [Msg::Drag]);
        moved(&mut h, DRAWER);

        assert_eq!(released(&mut h, DRAWER), [Msg::DragEnd]);
        // Back over the list with no button held: no rubber band follows.
        assert_eq!(moved(&mut h, Point::new(796.0, 395.0)), []);
    }

    /// The drawer takes the pointer's movement over it, so the list never
    /// sees the press travel away: to the list the release looks like one on
    /// the spot. It must end the press without counting as a click on it.
    #[test]
    fn a_list_press_released_over_the_drawer_is_not_a_click() {
        let mut h = Harness::new();
        moved(&mut h, LIST);
        assert_eq!(pressed(&mut h, LIST), [Msg::Pressed]);
        assert_eq!(moved(&mut h, DRAWER), []);

        assert_eq!(released(&mut h, DRAWER), []);
        // Back over the list with no button held: nothing is left armed.
        assert_eq!(moved(&mut h, Point::new(796.0, 395.0)), []);
    }

    #[test]
    fn a_click_on_the_drawer_does_not_reach_the_list() {
        let mut h = Harness::new();
        moved(&mut h, DRAWER);
        assert_eq!(pressed(&mut h, DRAWER), []);
        assert_eq!(released(&mut h, DRAWER), []);
        assert_eq!(moved(&mut h, LIST), []);
    }
}
