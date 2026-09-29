// SPDX-License-Identifier: GPL-3.0-only

//! A box whose width springs open to the left.
//!
//! [`SpringWidth`] lays its content out at its natural width, but reports a
//! width of its own that follows that one on a spring: from a starting width
//! when it first appears (e.g. that of the button it replaces), so the
//! content slides out to the left of where that stood. The box keeps its
//! right edge; the content rides its left edge and is clipped to it, so its
//! right part is what comes into view last. A press on a part not yet
//! revealed does not reach it. On an overshoot the content stays against the
//! right edge, the extra room left empty beside it.
//!
//! Collapsed, it springs back to the starting width, the content sliding
//! back out to the right, and publishes a message once it is at rest, so
//! the caller can then put back what it replaced.
//!
//! The spring lives in the widget's tree state and advances on
//! `RedrawRequested`, as in [`SpringHeight`](super::SpringHeight), whose
//! spring it shares.

use std::time::Instant;

use iced_core::event::Event;
use iced_core::widget::{Operation, Tree, tree};
use iced_core::{
    Clipboard, Element, Layout, Length, Point, Rectangle, Shell, Size, Vector, Widget, layout,
    mouse, overlay, renderer, window,
};
use iced_texture_cache::iced_animate::Spring;

use super::spring_height::{MAX_FRAME, SETTLED, SPRING, revealed};
use crate::ui::{Renderer, Theme};

/// Room left around the box for what the content draws outside its own
/// bounds: e.g. a focused search field's ring, 2 px out.
const OUTSET: f32 = 4.0;

/// Where the content may draw: the box, grown by [`OUTSET`] on every side
/// but the right one while the content is still cut off there.
fn clip_bounds(bounds: Rectangle, content: Rectangle) -> Rectangle {
    let cut = content.x + content.width > bounds.x + bounds.width + 0.5;
    Rectangle {
        x: bounds.x - OUTSET,
        y: bounds.y - OUTSET,
        width: bounds.width + if cut { OUTSET } else { 2.0 * OUTSET },
        height: bounds.height + 2.0 * OUTSET,
    }
}

/// Wraps `content` so its width springs open, from `from` to the content's
/// natural width. See the [module documentation](self).
pub fn spring_width<'a, Message>(
    from: f32,
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> SpringWidth<'a, Message> {
    SpringWidth {
        content: content.into(),
        from,
        collapsed: false,
        on_collapsed: None,
    }
}

#[allow(missing_debug_implementations)]
pub struct SpringWidth<'a, Message> {
    content: Element<'a, Message, Theme, Renderer>,
    /// The width it opens from and collapses back to.
    from: f32,
    collapsed: bool,
    on_collapsed: Option<Message>,
}

impl<Message> SpringWidth<'_, Message> {
    /// Springs the box back to its starting width while `collapsed` holds.
    /// The pointer stops reaching the content as it closes; keyboard events
    /// and operations (focus) still do.
    #[must_use]
    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    /// Publishes `message` once the box has sprung back to its starting
    /// width and come to rest, so the caller knows it can remove it.
    #[must_use]
    pub fn on_collapsed(mut self, message: Message) -> Self {
        self.on_collapsed = Some(message);
        self
    }
}

#[derive(Default)]
struct State {
    /// The width, once the content has been measured.
    spring: Option<Spring>,
    /// When the last frame advanced the spring; `None` while it is at rest.
    last_frame: Option<Instant>,
    /// Whether `on_collapsed` was published for the current collapse.
    collapse_published: bool,
}

impl State {
    /// The width to show now.
    fn width(&self) -> f32 {
        self.spring.map_or(0.0, |spring| spring.position().max(0.0))
    }

    fn is_moving(&self) -> bool {
        self.spring
            .is_some_and(|spring| !spring.is_settled_within(SETTLED))
    }
}

impl<Message> Widget<Message, Theme, Renderer> for SpringWidth<'_, Message> {
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
        tree.diff_children(&[&self.content]);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, self.content.as_widget().size().height)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let content =
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, &limits.loose());
        let natural = content.size().width;
        let target = if self.collapsed { self.from } else { natural };

        let state = tree.state.downcast_mut::<State>();
        match &mut state.spring {
            Some(spring) => spring.set_target(target),
            // Opens from the starting width the first time it is laid out.
            None => {
                let mut spring = Spring::new(SPRING, self.from);
                spring.set_target(target);
                state.spring = Some(spring);
            }
        }
        let width = state.width().min(limits.max().width);

        // Short of the natural width the content rides the left edge, its
        // right part cut off; past it, on an overshoot, it stays against the
        // right edge.
        let x = (width - natural).max(0.0);
        let height = content.size().height;
        layout::Node::with_children(
            Size::new(width, height),
            vec![content.move_to(Point::new(x, 0.0))],
        )
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
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let state = tree.state.downcast_mut::<State>();
            if state.is_moving() {
                let dt = state.last_frame.map_or(0.0, |last_frame| {
                    now.saturating_duration_since(last_frame)
                        .as_secs_f32()
                        .min(MAX_FRAME)
                });
                let before = state.width();
                if let Some(spring) = &mut state.spring {
                    spring.tick(dt);
                    if spring.is_settled_within(SETTLED) {
                        spring.snap();
                    }
                }
                state.last_frame = state.is_moving().then_some(*now);
                if state.width() != before {
                    shell.invalidate_layout();
                }
                if state.is_moving() {
                    shell.request_redraw();
                }
            }
            if !self.collapsed {
                state.collapse_published = false;
            } else if !state.is_moving() && !state.collapse_published {
                state.collapse_published = true;
                if let Some(message) = self.on_collapsed.take() {
                    shell.publish(message);
                }
            }
        }

        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().unwrap(),
            revealed(cursor, layout.bounds()),
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
            revealed(cursor, layout.bounds()),
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
        let bounds = layout.bounds();
        let content = layout.children().next().unwrap().bounds();
        let Some(clip) = clip_bounds(bounds, content).intersection(viewport) else {
            return;
        };
        <Renderer as iced_core::Renderer>::with_layer(renderer, clip, |renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout.children().next().unwrap(),
                revealed(cursor, bounds),
                &clip,
            );
        });
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

impl<'a, Message: 'a> From<SpringWidth<'a, Message>> for Element<'a, Message, Theme, Renderer> {
    fn from(spring_width: SpringWidth<'a, Message>) -> Self {
        Element::new(spring_width)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use iced_core::widget::Id;
    use iced_core::{Alignment, clipboard};
    use iced_runtime::user_interface::{Cache, UserInterface};

    use crate::ui::widget::{id_container, mouse_area, space};

    const WINDOW: Size = Size::new(400.0, 100.0);

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Msg {
        Pressed,
        Collapsed,
    }

    struct Harness {
        collapsed: bool,
        published: Vec<Msg>,
        redraw: bool,
        renderer: Renderer,
        cache: Option<Cache>,
        cursor: mouse::Cursor,
        now: Instant,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                collapsed: false,
                published: Vec::new(),
                redraw: false,
                renderer: iced_texture_cache::testing::headless_tiny_skia(),
                cache: Some(Cache::default()),
                cursor: mouse::Cursor::Unavailable,
                now: Instant::now(),
            }
        }

        /// A 200 px wide field that publishes on a press, opening from 32 px,
        /// at the right end of a row as wide as the window.
        fn view(&self) -> Element<'static, Msg, Theme, Renderer> {
            let field = id_container(
                mouse_area(
                    space::horizontal()
                        .width(Length::Fixed(200.0))
                        .height(Length::Fixed(32.0)),
                )
                .on_press(Msg::Pressed),
                Id::new("content"),
            );
            let spring = spring_width(32.0, field)
                .collapsed(self.collapsed)
                .on_collapsed(Msg::Collapsed);
            iced::widget::row![
                space::horizontal().width(Length::Fill),
                id_container(spring, Id::new("box"))
            ]
            .align_y(Alignment::Center)
            .into()
        }

        fn with_ui<R>(
            &mut self,
            f: impl FnOnce(&mut UserInterface<'_, Msg, Theme, Renderer>, &mut Renderer) -> R,
        ) -> R {
            let cache = self.cache.take().unwrap();
            let view = self.view();
            let mut ui = UserInterface::build(view, WINDOW, cache, &mut self.renderer);
            let result = f(&mut ui, &mut self.renderer);
            self.cache = Some(ui.into_cache());
            result
        }

        fn send(&mut self, event: Event) {
            let cursor = self.cursor;
            let mut messages = Vec::new();
            let state = self.with_ui(|ui, renderer| {
                ui.update(
                    &[event],
                    cursor,
                    renderer,
                    &mut clipboard::Null,
                    &mut messages,
                )
                .0
            });
            self.redraw = matches!(
                state,
                iced_runtime::user_interface::State::Updated {
                    redraw_request: window::RedrawRequest::NextFrame,
                    ..
                }
            );
            self.published.extend(messages);
        }

        fn frame(&mut self) {
            self.now += Duration::from_millis(16);
            self.send(Event::Window(window::Event::RedrawRequested(self.now)));
        }

        fn bounds(&mut self, id: &'static str) -> Rectangle {
            struct Bounds(Id, Option<Rectangle>);
            impl Operation for Bounds {
                fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                    operate(self);
                }
                fn container(&mut self, found: Option<&Id>, bounds: Rectangle) {
                    if found == Some(&self.0) {
                        self.1 = Some(bounds);
                    }
                }
            }
            self.with_ui(|ui, renderer| {
                let mut bounds = Bounds(Id::new(id), None);
                ui.operate(renderer, &mut bounds);
                bounds.1.expect("laid out")
            })
        }

        /// Runs frames until the box stops asking for them, returning its
        /// bounds after each.
        fn settle(&mut self) -> Vec<Rectangle> {
            let mut seen = Vec::new();
            for _ in 0..200 {
                self.frame();
                seen.push(self.bounds("box"));
                if !self.redraw {
                    return seen;
                }
            }
            panic!("never settled: {seen:?}");
        }
    }

    #[test]
    fn the_clip_leaves_room_for_a_ring_but_not_past_a_cut() {
        let content = Rectangle::new(Point::new(100.0, 10.0), Size::new(200.0, 32.0));

        let open = Rectangle::new(Point::new(100.0, 10.0), Size::new(200.0, 32.0));
        assert_eq!(
            clip_bounds(open, content),
            Rectangle::new(Point::new(96.0, 6.0), Size::new(208.0, 40.0))
        );

        let opening = Rectangle::new(Point::new(100.0, 10.0), Size::new(80.0, 32.0));
        assert_eq!(
            clip_bounds(opening, content),
            Rectangle::new(Point::new(96.0, 6.0), Size::new(84.0, 40.0))
        );
    }

    #[test]
    fn it_opens_from_the_starting_width_to_the_left() {
        let mut harness = Harness::new();
        let first = harness.bounds("box");
        assert_eq!(first.width, 32.0);
        assert_eq!(first.x + first.width, WINDOW.width);

        let seen = harness.settle();
        assert!(
            seen.iter().all(|b| b.x + b.width == WINDOW.width),
            "kept its right edge: {seen:?}"
        );
        assert!(
            seen.iter().any(|b| b.width > 32.0 && b.width < 200.0),
            "animated: {seen:?}"
        );
        assert!(seen.iter().any(|b| b.width > 200.5), "overshot: {seen:?}");
        assert_eq!(seen.last().unwrap().width, 200.0);
        assert_eq!(harness.bounds("content").x, WINDOW.width - 200.0);
    }

    #[test]
    fn the_content_rides_the_left_edge_until_it_is_open() {
        let mut harness = Harness::new();
        for _ in 0..3 {
            harness.frame();
        }
        let shown = harness.bounds("box");
        assert!(shown.width < 200.0, "{shown:?}");
        assert_eq!(harness.bounds("content").x, shown.x);
    }

    #[test]
    fn on_an_overshoot_the_content_keeps_to_the_right_edge() {
        let mut harness = Harness::new();
        for _ in 0..200 {
            harness.frame();
            let shown = harness.bounds("box");
            if shown.width > 200.0 {
                assert_eq!(harness.bounds("content").x, WINDOW.width - 200.0);
                return;
            }
        }
        panic!("never overshot");
    }

    #[test]
    fn a_press_on_a_part_not_yet_shown_does_not_reach_the_content() {
        let mut harness = Harness::new();
        for _ in 0..3 {
            harness.frame();
        }
        let shown = harness.bounds("box");
        assert!(shown.width < 190.0, "{shown:?}");

        harness.cursor = mouse::Cursor::Available(Point::new(shown.x - 5.0, 16.0));
        harness.send(Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        )));
        assert!(harness.published.is_empty(), "{:?}", harness.published);

        harness.cursor = mouse::Cursor::Available(Point::new(shown.x + 5.0, 16.0));
        harness.send(Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        )));
        assert_eq!(harness.published, [Msg::Pressed]);
    }

    #[test]
    fn collapsed_it_springs_back_and_then_says_so_once() {
        let mut harness = Harness::new();
        let _ = harness.settle();
        assert!(harness.published.is_empty(), "{:?}", harness.published);

        harness.collapsed = true;
        let seen = harness.settle();
        assert!(
            seen.iter().any(|b| b.width > 32.0 && b.width < 200.0),
            "animated: {seen:?}"
        );
        assert_eq!(seen.last().unwrap().width, 32.0);
        assert_eq!(harness.published, [Msg::Collapsed]);

        harness.frame();
        harness.frame();
        assert_eq!(harness.published, [Msg::Collapsed], "published once");
    }

    #[test]
    fn reopened_while_collapsing_it_turns_back() {
        let mut harness = Harness::new();
        let _ = harness.settle();

        harness.collapsed = true;
        for _ in 0..4 {
            harness.frame();
        }
        let midway = harness.bounds("box").width;
        assert!(midway > 32.0 && midway < 200.0, "{midway}");

        harness.collapsed = false;
        let seen = harness.settle();
        assert_eq!(seen.last().unwrap().width, 200.0);
        assert!(harness.published.is_empty(), "{:?}", harness.published);
    }
}
