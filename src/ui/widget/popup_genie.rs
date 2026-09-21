// SPDX-License-Identifier: GPL-3.0-only

//! Reports when a popup has finished collapsing.
//!
//! The shell withholds a popup's teardown so the menu can play its exit, and
//! then needs to know when to let go. `Motion::presence` answers that —
//! `Exiting`, then `Gone` — but only a widget is in a position to ask, once
//! per frame, from inside the popup's own tree.
//!
//! Tying the teardown to the animation rather than to a timer matters:
//! `iced_animate` caps a frame at 1/15 s and resumes where it stopped, so a
//! stalled frame stretches the collapse instead of skipping it. A clock
//! would tear the surface down mid-collapse.

use iced_core::event::Event;
use iced_core::widget::{Operation, Tree, tree};
use iced_core::{
    Clipboard, Layout, Length, Rectangle, Shell, Vector, Widget, layout, mouse, overlay, renderer,
    window,
};
use iced_texture_cache::iced_animate::{Motion, MotionKey, Presence};

use crate::ui::Element;

/// Wrap a popup's content so it reports when its exit animation is over.
pub fn popup_genie<'a, Message>(
    motion: Motion,
    key: MotionKey,
    on_gone: Message,
    content: impl Into<Element<'a, Message>>,
) -> PopupGenie<'a, Message> {
    PopupGenie {
        content: content.into(),
        motion,
        key,
        on_gone,
    }
}

#[allow(missing_debug_implementations)]
pub struct PopupGenie<'a, Message> {
    content: Element<'a, Message>,
    motion: Motion,
    key: MotionKey,
    on_gone: Message,
}

/// Whether this tree has already reported.
#[derive(Default)]
struct State {
    reported: bool,
}

impl<Message: Clone> Widget<Message, crate::ui::Theme, crate::ui::Renderer>
    for PopupGenie<'_, Message>
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
        cursor: mouse::Cursor,
        renderer: &crate::ui::Renderer,
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

        // Asked once a frame, because `presence` only moves when the engine
        // ticks and the engine ticks on the redraw the `Host` above requests.
        if !matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
            return;
        }

        let state = tree.state.downcast_mut::<State>();
        if state.reported || self.motion.presence(self.key) != Presence::Gone {
            return;
        }

        // Once only: the shell answers this by destroying the surface, and a
        // second message would arrive after the id is already gone.
        state.reported = true;
        shell.publish(self.on_gone.clone());
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &crate::ui::Renderer,
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
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
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

impl<'a, Message: Clone + 'a> From<PopupGenie<'a, Message>> for Element<'a, Message> {
    fn from(genie: PopupGenie<'a, Message>) -> Element<'a, Message> {
        Element::new(genie)
    }
}
