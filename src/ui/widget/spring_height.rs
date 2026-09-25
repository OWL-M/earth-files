// SPDX-License-Identifier: GPL-3.0-only

//! A box whose height springs to fit its content.
//!
//! [`SpringHeight`] lays its content out at its natural height, but reports a
//! height of its own that follows that one on a spring: from 0 when it first
//! appears, so the content unrolls downward, and on to each new natural height
//! when the content grows or shrinks, keeping its velocity through the change.
//! The content is anchored at the top and clipped to the box; a press on a
//! part not yet revealed does not reach it. On an overshoot the content is
//! laid out that much taller, so a background it draws fills the box.
//!
//! The spring lives in the widget's tree state, so nothing outside has to
//! drive it. It advances on `RedrawRequested` and asks for the next frame
//! until it settles. That works inside an overlay too: iced sends overlays
//! every event, and lays one out again in the same update when it
//! invalidates its layout.

use std::time::{Duration, Instant};

use iced_core::event::Event;
use iced_core::widget::{Operation, Tree, tree};
use iced_core::{
    Clipboard, Element, Layout, Length, Rectangle, Shell, Size, Vector, Widget, layout, mouse,
    overlay, renderer, window,
};
use iced_texture_cache::iced_animate::{Spring, SpringParams};

use crate::ui::{Renderer, Theme};

/// How the height moves: a small overshoot, then it settles.
const SPRING: SpringParams = SpringParams::new(0.25, Duration::from_millis(300));

/// How close to its target the height has to be to stop, in pixels.
const SETTLED: f32 = 0.5;

/// A frame longer than this is advanced as this, so a stall resumes the
/// motion where it stopped instead of landing it in one step.
const MAX_FRAME: f32 = 1.0 / 15.0;

/// Wraps `content` so its height springs to fit it. See the
/// [module documentation](self).
pub fn spring_height<'a, Message>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> SpringHeight<'a, Message> {
    SpringHeight {
        content: content.into(),
    }
}

#[allow(missing_debug_implementations)]
pub struct SpringHeight<'a, Message> {
    content: Element<'a, Message, Theme, Renderer>,
}

#[derive(Default)]
struct State {
    /// The height, once the content has been measured.
    spring: Option<Spring>,
    /// When the last frame advanced the spring; `None` while it is at rest.
    last_frame: Option<Instant>,
}

impl State {
    /// The height to show now.
    fn height(&self) -> f32 {
        self.spring.map_or(0.0, |spring| spring.position().max(0.0))
    }

    fn is_moving(&self) -> bool {
        self.spring
            .is_some_and(|spring| !spring.is_settled_within(SETTLED))
    }
}

impl<Message> Widget<Message, Theme, Renderer> for SpringHeight<'_, Message> {
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
        Size::new(self.content.as_widget().size().width, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let mut content =
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, limits);
        let natural = content.size().height;

        let state = tree.state.downcast_mut::<State>();
        match &mut state.spring {
            Some(spring) => spring.set_target(natural),
            // Unrolls from nothing the first time it is laid out.
            None => {
                let mut spring = Spring::new(SPRING, 0.0);
                spring.set_target(natural);
                state.spring = Some(spring);
            }
        }
        // Deliberately ignores `limits.min().height`: the box unrolls from
        // 0, which would otherwise be clamped up to a popover's 1 px
        // minimum.
        let height = state.height().min(limits.max().height);

        // On an overshoot the content is stretched to the box, so its
        // background does not stop short of the bottom edge.
        if height > natural {
            let stretched =
                layout::Limits::new(Size::new(limits.min().width, height), limits.max());
            content =
                self.content
                    .as_widget_mut()
                    .layout(&mut tree.children[0], renderer, &stretched);
        }

        layout::Node::with_children(Size::new(content.size().width, height), vec![content])
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
                let before = state.height();
                if let Some(spring) = &mut state.spring {
                    spring.tick(dt);
                    if spring.is_settled_within(SETTLED) {
                        spring.snap();
                    }
                }
                state.last_frame = state.is_moving().then_some(*now);
                if state.height() != before {
                    shell.invalidate_layout();
                }
                if state.is_moving() {
                    shell.request_redraw();
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
        let Some(clip) = bounds.intersection(viewport) else {
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

/// The cursor as the content may see it: only over the part revealed. A
/// `Levitating` cursor (e.g. over another layer) is landed first, so it is
/// still treated as over the box when it is.
fn revealed(cursor: mouse::Cursor, bounds: Rectangle) -> mouse::Cursor {
    if cursor.land().is_over(bounds) {
        cursor
    } else {
        mouse::Cursor::Unavailable
    }
}

impl<'a, Message: 'a> From<SpringHeight<'a, Message>> for Element<'a, Message, Theme, Renderer> {
    fn from(spring_height: SpringHeight<'a, Message>) -> Self {
        Element::new(spring_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::widget::Id;
    use iced_core::{Point, clipboard};
    use iced_runtime::user_interface::{Cache, UserInterface};

    use crate::ui::widget::{id_container, mouse_area, space};

    const WINDOW: Size = Size::new(200.0, 400.0);

    /// A 100 px wide column of `rows` rows, 20 px each, that publishes on a
    /// press, inside a box whose height springs.
    fn rows(rows: usize) -> Element<'static, (), Theme, Renderer> {
        let rows = space::vertical()
            .width(Length::Fixed(100.0))
            .height(Length::Fixed(20.0 * rows as f32));
        let content = id_container(mouse_area(rows).on_press(()), Id::new("content"));
        id_container(spring_height(content), Id::new("box")).into()
    }

    struct Harness {
        rows: usize,
        /// Whether the box is shown as a popover's popup instead of inline.
        popup: bool,
        published: usize,
        redraw: bool,
        layout_changed: bool,
        renderer: Renderer,
        cache: Option<Cache>,
        cursor: mouse::Cursor,
        now: Instant,
    }

    impl Harness {
        fn new(rows: usize) -> Self {
            Self {
                rows,
                popup: false,
                published: 0,
                redraw: false,
                layout_changed: false,
                renderer: iced_texture_cache::testing::headless_tiny_skia(),
                cache: Some(Cache::default()),
                cursor: mouse::Cursor::Unavailable,
                now: Instant::now(),
            }
        }

        fn view(&self) -> Element<'static, (), Theme, Renderer> {
            if self.popup {
                crate::ui::widget::popover(space::horizontal().width(Length::Fixed(100.0)))
                    .position(crate::ui::widget::popover::Position::Bottom)
                    .popup(rows(self.rows))
                    .into()
            } else {
                rows(self.rows)
            }
        }

        fn with_ui<R>(
            &mut self,
            f: impl FnOnce(&mut UserInterface<'_, (), Theme, Renderer>, &mut Renderer) -> R,
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
            self.layout_changed = matches!(
                state,
                iced_runtime::user_interface::State::Updated {
                    has_layout_changed: true,
                    ..
                }
            );
            self.published += messages.len();
        }

        fn frame(&mut self) {
            self.now += Duration::from_millis(16);
            self.send(Event::Window(window::Event::RedrawRequested(self.now)));
        }

        /// Re-sends `RedrawRequested` at the same instant, without advancing
        /// time — what iced_winit does when a frame invalidates layout.
        fn repeat_frame(&mut self) {
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

        fn height(&mut self) -> f32 {
            self.bounds("box").height
        }

        /// Runs frames until the box stops asking for them, returning the
        /// height after each.
        fn settle(&mut self) -> Vec<f32> {
            let mut seen = Vec::new();
            for _ in 0..200 {
                self.frame();
                seen.push(self.height());
                if !self.redraw {
                    return seen;
                }
            }
            panic!("never settled: {seen:?}");
        }
    }

    #[test]
    fn it_unrolls_from_nothing_to_the_content() {
        let mut harness = Harness::new(5);
        assert_eq!(harness.height(), 0.0);
        let seen = harness.settle();
        assert_eq!(*seen.last().unwrap(), 100.0, "{seen:?}");
        assert!(
            seen.iter().any(|h| *h > 0.0 && *h < 100.0),
            "animated: {seen:?}"
        );
    }

    #[test]
    fn a_repeated_frame_does_not_lay_out_again() {
        let mut harness = Harness::new(5);
        for _ in 0..3 {
            harness.frame();
        }
        assert!(
            harness.layout_changed,
            "a frame that moves it lays it out again"
        );
        harness.repeat_frame();
        assert!(!harness.layout_changed, "a repeat of that frame does not");
    }

    #[test]
    fn it_overshoots_before_it_settles() {
        let mut harness = Harness::new(5);
        let seen = harness.settle();
        assert!(seen.iter().any(|height| *height > 100.5), "{seen:?}");
    }

    /// Coverage test: the overshoot-stretch behavior already exists in
    /// `layout` (content laid out taller than natural when the box
    /// overshoots); this only asserts it.
    #[test]
    fn an_overshoot_stretches_the_content_to_the_box() {
        let mut harness = Harness::new(5);
        let mut overshot = false;
        for _ in 0..200 {
            harness.frame();
            let box_bounds = harness.bounds("box");
            if box_bounds.height > 100.0 {
                overshot = true;
                let content_bounds = harness.bounds("content");
                assert!(
                    (content_bounds.height - box_bounds.height).abs() < 0.01,
                    "box={box_bounds:?} content={content_bounds:?}"
                );
            }
            if !harness.redraw {
                break;
            }
        }
        assert!(overshot, "never overshot");
    }

    #[test]
    fn content_growing_mid_flight_carries_on_from_where_it_was() {
        let mut harness = Harness::new(5);

        // The same spring, fed the same frames: the first frame advances by
        // 0, every later one by 16 ms, and the retarget keeps its velocity.
        let mut model = Spring::new(SPRING, 0.0);
        model.set_target(100.0);
        model.tick(0.0);
        for _ in 1..5 {
            model.tick(Duration::from_millis(16).as_secs_f32());
        }

        for _ in 0..5 {
            harness.frame();
        }
        let before = harness.height();
        assert!(before > 0.0 && before < 100.0, "{before}");
        assert!(
            (before - model.position()).abs() < 0.01,
            "widget vs model before retarget: before={before} model={}",
            model.position()
        );

        harness.rows = 10;
        harness.frame();
        let after = harness.height();
        assert!(after > before, "kept going: {before} -> {after}");

        model.set_target(200.0);
        model.tick(Duration::from_millis(16).as_secs_f32());
        assert!(
            (after - model.position()).abs() < 0.01,
            "widget vs model after retarget: after={after} model={}",
            model.position()
        );

        let seen = harness.settle();
        assert_eq!(*seen.last().unwrap(), 200.0, "{seen:?}");
    }

    #[test]
    fn at_rest_it_asks_for_no_more_frames() {
        let mut harness = Harness::new(5);
        let _ = harness.settle();
        harness.frame();
        assert!(!harness.redraw);
        assert_eq!(harness.height(), 100.0);
    }

    #[test]
    fn a_press_below_what_is_shown_does_not_reach_the_content() {
        let mut harness = Harness::new(5);
        for _ in 0..3 {
            harness.frame();
        }
        let shown = harness.bounds("box");
        assert!(shown.height < 90.0, "{shown:?}");

        harness.cursor = mouse::Cursor::Available(Point::new(50.0, 95.0));
        harness.send(Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        )));
        assert_eq!(harness.published, 0, "hidden row pressed");

        let _ = harness.settle();
        harness.send(Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        )));
        assert_eq!(harness.published, 1, "shown row pressed");
    }

    #[test]
    fn it_unrolls_as_a_popover_popup() {
        let mut harness = Harness::new(5);
        harness.popup = true;
        assert_eq!(harness.height(), 0.0);
        let seen = harness.settle();
        assert_eq!(*seen.last().unwrap(), 100.0, "{seen:?}");
        assert!(
            seen.iter().any(|h| *h > 0.0 && *h < 100.0),
            "animated: {seen:?}"
        );
    }
}
