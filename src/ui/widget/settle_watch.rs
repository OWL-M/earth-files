// SPDX-License-Identifier: GPL-3.0-only

//! Reports once when an animated value comes to rest.
//!
//! The engine ticks inside the widget tree and never rebuilds the view on
//! its own, so a layout that must change when an animation ends needs a
//! message to ask for the rebuild. This is it. It fires once per track
//! (keyed by the track's `MotionKey`), on the first frame that finds the
//! value at rest.

use iced_core::event::Event;
use iced_core::widget::{Operation, Tree, tree};
use iced_core::{
    Clipboard, Layout, Length, Rectangle, Shell, Vector, Widget, layout, mouse, overlay, renderer,
    window,
};
use iced_texture_cache::iced_animate::{Anim, MotionKey};

use crate::ui::Element;

/// Wrap `content` so `on_settled` is published once `value` stops moving.
pub fn settle_watch<'a, Message>(
    value: Anim<Vector>,
    key: MotionKey,
    on_settled: Message,
    content: impl Into<Element<'a, Message>>,
) -> SettleWatch<'a, Message> {
    SettleWatch {
        content: content.into(),
        value,
        key,
        on_settled,
    }
}

#[allow(missing_debug_implementations)]
pub struct SettleWatch<'a, Message> {
    content: Element<'a, Message>,
    value: Anim<Vector>,
    /// The track `value` reads; a new key is a new animation to report.
    key: MotionKey,
    on_settled: Message,
}

#[derive(Default)]
struct State {
    /// The track this tree has already reported for. A tree that survives
    /// into the next animation must report that one too.
    reported: Option<MotionKey>,
}

impl<Message: Clone> Widget<Message, crate::ui::Theme, crate::ui::Renderer>
    for SettleWatch<'_, Message>
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

        // Asked once a frame: the `Host` ticks the engine on the redraw,
        // before the event reaches this widget.
        if !matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
            return;
        }

        let state = tree.state.downcast_mut::<State>();
        if state.reported == Some(self.key) || self.value.is_animating() {
            return;
        }

        state.reported = Some(self.key);
        shell.publish(self.on_settled.clone());
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

impl<'a, Message: Clone + 'a> From<SettleWatch<'a, Message>> for Element<'a, Message> {
    fn from(watch: SettleWatch<'a, Message>) -> Element<'a, Message> {
        Element::new(watch)
    }
}
