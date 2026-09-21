// SPDX-License-Identifier: GPL-3.0-only

//! The boundary between the nav bar and the content, and when it shows
//! itself.
//!
//! The pointer crosses this strip on its way between the sidebar and the file
//! list all the time, so the line stays out of sight until the pointer has
//! stopped on it for [`LINE_DELAY`]. Pressing draws it at once, and holds it
//! for the whole drag however far the pointer wanders. The resize cursor is
//! the handle's own and appears straight away.

use std::time::{Duration, Instant};

use iced_core::event::Event;
use iced_core::widget::{Operation, Tree, tree};
use iced_core::{
    Clipboard, Layout, Length, Rectangle, Renderer as _, Shell, Vector, Widget, layout, mouse,
    overlay, renderer,
};

use crate::ui::Element;
use crate::ui::convert::ToColor;

/// How long before the boundary draws itself, by when the pointer has
/// clearly stopped rather than passed through
const LINE_DELAY: Duration = Duration::from_millis(700);

/// How far the handle reaches back over the panel, so that the boundary
/// itself can be grabbed and not only the gap beside it. The same trick the
/// list-view column dividers use.
pub const GRAB: f32 = 6.0;

/// Thickness of the line, straddling the boundary
const LINE_WIDTH: f32 = 2.0;

/// Wrap the nav bar's drag handle so it reveals itself on a rest or a press.
pub fn nav_bar_divider<'a, Message>(
    content: impl Into<Element<'a, Message>>,
) -> NavBarDivider<'a, Message> {
    NavBarDivider {
        content: content.into(),
    }
}

#[allow(missing_debug_implementations)]
pub struct NavBarDivider<'a, Message> {
    content: Element<'a, Message>,
}

#[derive(Default)]
struct State {
    /// When the pointer arrived, or `None` while it is elsewhere
    hovered_since: Option<Instant>,
    pressed: bool,
}

impl State {
    /// Whether the boundary should be drawn: rested on long enough, or held
    fn revealed(&self) -> bool {
        self.pressed
            || self
                .hovered_since
                .is_some_and(|since| since.elapsed() >= LINE_DELAY)
    }
}

impl<Message> Widget<Message, crate::ui::Theme, crate::ui::Renderer>
    for NavBarDivider<'_, Message>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

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
        renderer: &crate::ui::Renderer,
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
        renderer: &crate::ui::Renderer,
        operation: &mut dyn Operation,
    ) {
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
        renderer: &crate::ui::Renderer,
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

        let over = cursor_position.is_over(layout.bounds());
        let state = tree.state.downcast_mut::<State>();
        let was_revealed = state.revealed();

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if over => {
                state.pressed = true;
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.pressed = false;
            }
            _ => {}
        }

        if over {
            if state.hovered_since.is_none() {
                state.hovered_since = Some(Instant::now());
            }
        } else {
            state.hovered_since = None;
        }

        // A pointer standing still sends nothing, so ask to be drawn again
        // when the delay is up. Held, the line is already shown.
        if let Some(since) = state.hovered_since
            && !state.pressed
            && since.elapsed() < LINE_DELAY
        {
            shell.request_redraw_at(since + LINE_DELAY);
        }

        if was_revealed != state.revealed() {
            shell.request_redraw();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &crate::ui::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().unwrap(),
            cursor_position,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        renderer_style: &renderer::Style,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            renderer_style,
            layout.children().next().unwrap(),
            cursor_position,
            viewport,
        );

        if !tree.state.downcast_ref::<State>().revealed() {
            return;
        }

        // The text colour: the boundary is being pointed at, not called out.
        let bounds = layout.bounds();
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bounds.x + GRAB - LINE_WIDTH / 2.0,
                    y: bounds.y,
                    width: LINE_WIDTH,
                    height: bounds.height,
                },
                snap: true,
                ..renderer::Quad::default()
            },
            theme.cosmic().on_bg_color().to_color(),
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &crate::ui::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, crate::ui::Theme, crate::ui::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<NavBarDivider<'a, Message>> for Element<'a, Message> {
    fn from(divider: NavBarDivider<'a, Message>) -> Element<'a, Message> {
        Element::new(divider)
    }
}
