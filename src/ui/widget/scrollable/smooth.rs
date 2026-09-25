// SPDX-License-Identifier: GPL-3.0-only

//! Smooth scrolling around iced's own [`Scrollable`].
//!
//! iced 0.14 moves its scrollable in steps: 60 px per wheel notch, and a click
//! on the empty scrollbar track jumps. [`Smooth`] does not reimplement any of
//! that. It hands every event to the inner scrollable and watches what the
//! event did to the offset. When a wheel notch or a track click moved it,
//! [`Smooth`] puts the offset back and glides there on a spring instead. So
//! everything iced decides stays iced's: whether content captures the wheel
//! first, how far a notch goes, where the edges are.
//!
//! A touchpad follows the fingers 1:1, as iced already does. When the fingers
//! lift, the compositor says so (`iced_exwlshell::scroll::Stop`), and the list
//! coasts on with the speed it had, slowing under friction.
//!
//! Programmatic scrolling is instant with [`super::scroll_to`] and glides with
//! [`super::glide_to`].

use std::time::{Duration, Instant};

use iced::widget::scrollable::{AbsoluteOffset, Direction, RelativeOffset, Scrollable, Viewport};
use iced_core::event::Event;
use iced_core::widget::operation::scrollable::Scrollable as ScrollableState;
use iced_core::widget::{Id, Operation, Tree, tree};
use iced_core::{
    Clipboard, Element, Layout, Length, Rectangle, Shell, Size, Vector, Widget, keyboard, layout,
    mouse, overlay, renderer, window,
};
use iced_exwlshell::scroll::{self as frames, Source, StopReceiver};
use iced_texture_cache::iced_animate::{Decay, Spring, SpringParams};

use crate::ui::{Renderer, Theme};

/// How a glide moves: `QUICK`'s pace, brisk enough to follow a wheel spun
/// notch after notch.
const GLIDE: SpringParams = SpringParams::new(0.0, Duration::from_millis(220));

/// Friction on a coast, `UIScrollView`'s normal deceleration.
const FRICTION: f32 = Decay::NORMAL_RATE;

/// How close to its end a glide or coast has to be to stop, in pixels.
const SETTLED: f32 = 0.5;

/// The stretch of finger movement a coast takes its speed from.
const VELOCITY_WINDOW_MS: f32 = 100.0;

/// A pause this long between the last movement and the lift means the
/// fingers came to rest first: no coast.
const RESTED_MS: f32 = 50.0;

/// Slower than this, a lift does not coast, in pixels per second.
const MIN_COAST_SPEED: f32 = 60.0;

/// A frame longer than this is advanced as this, so a stall resumes the
/// motion where it stopped instead of landing it in one step.
const MAX_FRAME: f32 = 1.0 / 15.0;

/// iced's [`Scrollable`] with smooth wheel, track, touchpad and programmatic
/// scrolling. See the [module documentation](self).
#[allow(missing_debug_implementations)]
pub struct Smooth<'a, Message> {
    inner: Scrollable<'a, Message, Theme, Renderer>,
    id: Option<Id>,
}

impl<'a, Message> Smooth<'a, Message> {
    pub(super) fn new(inner: Scrollable<'a, Message, Theme, Renderer>) -> Self {
        Self { inner, id: None }
    }

    /// Sets the [`Id`], which [`super::scroll_to`] and [`super::glide_to`] target.
    #[must_use]
    pub fn id(mut self, id: impl Into<Id>) -> Self {
        let id = id.into();
        self.inner = self.inner.id(id.clone());
        self.id = Some(id);
        self
    }

    /// Sets the message published whenever the viewport moves, glides included.
    #[must_use]
    pub fn on_scroll(mut self, f: impl Fn(Viewport) -> Message + 'a) -> Self {
        self.inner = self.inner.on_scroll(f);
        self
    }

    /// Sets the width.
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.inner = self.inner.width(width);
        self
    }

    /// Sets the height.
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.inner = self.inner.height(height);
        self
    }

    /// Sets the [`Direction`] and its scrollbars.
    #[must_use]
    pub fn direction(mut self, direction: impl Into<Direction>) -> Self {
        self.inner = self.inner.direction(direction);
        self
    }

    /// Sets the style class.
    #[must_use]
    pub fn class(
        mut self,
        class: impl Into<<Theme as iced::widget::scrollable::Catalog>::Class<'a>>,
    ) -> Self {
        self.inner = self.inner.class(class);
        self
    }

    fn inner(&self) -> &dyn Widget<Message, Theme, Renderer> {
        &self.inner
    }
}

/// A request left by [`super::glide_to`] for the next event to start.
#[derive(Debug, Default)]
pub(super) struct GlideRequest(pub(super) Option<AbsoluteOffset<Option<f32>>>);

/// What the offset is doing on its own.
#[derive(Debug, Clone, Copy)]
enum Motion {
    Idle,
    Glide { x: Spring, y: Spring },
    Coast { x: Decay, y: Decay },
}

impl Motion {
    fn position(&self) -> Option<Vector> {
        match self {
            Self::Idle => None,
            Self::Glide { x, y } => Some(Vector::new(x.position(), y.position())),
            Self::Coast { x, y } => Some(Vector::new(x.position(), y.position())),
        }
    }

    fn velocity(&self) -> Vector {
        match self {
            Self::Idle => Vector::ZERO,
            Self::Glide { x, y } => Vector::new(x.velocity(), y.velocity()),
            Self::Coast { x, y } => Vector::new(x.velocity(), y.velocity()),
        }
    }

    /// Where a glide is headed. A coast has no target.
    fn target(&self) -> Option<Vector> {
        match self {
            Self::Glide { x, y } => Some(Vector::new(x.target(), y.target())),
            _ => None,
        }
    }

    /// A glide from `from` to `to` that keeps whatever velocity `self` had.
    fn glide(&self, from: Vector, to: Vector) -> Self {
        let velocity = self.velocity();
        let spring = |from: f32, to: f32, velocity: f32| {
            let mut spring = Spring::new(GLIDE, from).with_velocity(velocity);
            spring.set_target(to);
            spring
        };
        Self::Glide {
            x: spring(from.x, to.x, velocity.x),
            y: spring(from.y, to.y, velocity.y),
        }
    }

    /// Advances by `dt` seconds within `0..=max`. Returns `false` once there
    /// is nothing left to do.
    fn tick(&mut self, dt: f32, max: Vector) -> bool {
        match self {
            Self::Idle => false,
            Self::Glide { x, y } => {
                x.tick(dt);
                y.tick(dt);
                if x.is_settled_within(SETTLED) && y.is_settled_within(SETTLED) {
                    x.snap();
                    y.snap();
                    return false;
                }
                true
            }
            Self::Coast { x, y } => {
                x.tick(dt);
                y.tick(dt);
                // The edge stops a coast dead.
                for (axis, max) in [(&mut *x, max.x), (&mut *y, max.y)] {
                    if !(0.0..=max).contains(&axis.position()) {
                        *axis = Decay::new(axis.position().clamp(0.0, max), 0.0, FRICTION);
                    }
                }
                if x.is_settled_within(SETTLED) && y.is_settled_within(SETTLED) {
                    x.snap();
                    y.snap();
                    return false;
                }
                true
            }
        }
    }
}

/// One touchpad movement: when, and how far the offset went.
#[derive(Debug, Clone, Copy)]
struct Sample {
    /// Compositor timestamp in milliseconds, when it sent one.
    time: Option<u32>,
    /// When the event reached us, for compositors that send no timestamp.
    received: Instant,
    moved: Vector,
}

#[derive(Debug)]
struct State {
    motion: Motion,
    /// When the motion last advanced.
    last_frame: Option<Instant>,
    /// An instant move (`scroll_to`, `scroll_by`, `snap_to`) arrived since the
    /// last event, and takes over from the motion.
    moved_instantly: bool,
    /// Shift turns a vertical notch horizontal, as iced does.
    modifiers: keyboard::Modifiers,
    /// Touchpad movement since the fingers last came down, newest last.
    samples: Vec<Sample>,
    stop: StopReceiver,
    request: GlideRequest,
}

impl Default for State {
    fn default() -> Self {
        Self {
            motion: Motion::Idle,
            last_frame: None,
            moved_instantly: false,
            modifiers: keyboard::Modifiers::default(),
            samples: Vec::new(),
            stop: StopReceiver::new(),
            request: GlideRequest::default(),
        }
    }
}

impl State {
    fn start(&mut self, motion: Motion) {
        if matches!(self.motion, Motion::Idle) {
            self.last_frame = Some(Instant::now());
        }
        self.motion = motion;
    }

    fn cancel(&mut self) {
        self.motion = Motion::Idle;
        self.last_frame = None;
    }
}

/// The inner scrollable's offset and extent.
#[derive(Debug, Clone, Copy)]
struct Extent {
    offset: Vector,
    max: Vector,
}

impl Extent {
    fn clamp(&self, offset: Vector) -> Vector {
        Vector::new(
            offset.x.clamp(0.0, self.max.x),
            offset.y.clamp(0.0, self.max.y),
        )
    }
}

/// Reads the offset of the first scrollable it meets, and does not descend.
struct Probe(Option<Extent>);

impl Operation for Probe {
    fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn Operation)) {}

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn ScrollableState,
    ) {
        self.0.get_or_insert(Extent {
            offset: translation,
            max: Vector::new(
                (content_bounds.width - bounds.width).max(0.0),
                (content_bounds.height - bounds.height).max(0.0),
            ),
        });
    }
}

/// Moves the first scrollable it meets to an offset, and does not descend.
struct Place(Option<Vector>);

impl Operation for Place {
    fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn Operation)) {}

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
        _translation: Vector,
        state: &mut dyn ScrollableState,
    ) {
        if let Some(offset) = self.0.take() {
            state.scroll_to(AbsoluteOffset {
                x: Some(offset.x),
                y: Some(offset.y),
            });
        }
    }
}

/// Passes an operation on to the inner scrollable, noting whether it moved it.
struct Watch<'a> {
    operation: &'a mut dyn Operation,
    moved: bool,
}

impl Operation for Watch<'_> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        self.operation.traverse(operate);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        state: &mut dyn ScrollableState,
    ) {
        let mut watched = Watched {
            state,
            moved: false,
        };
        self.operation
            .scrollable(id, bounds, content_bounds, translation, &mut watched);
        self.moved |= watched.moved;
    }
}

struct Watched<'a> {
    state: &'a mut dyn ScrollableState,
    moved: bool,
}

impl ScrollableState for Watched<'_> {
    fn snap_to(&mut self, offset: RelativeOffset<Option<f32>>) {
        self.moved = true;
        self.state.snap_to(offset);
    }

    fn scroll_to(&mut self, offset: AbsoluteOffset<Option<f32>>) {
        self.moved = true;
        self.state.scroll_to(offset);
    }

    fn scroll_by(&mut self, offset: AbsoluteOffset, bounds: Rectangle, content_bounds: Rectangle) {
        self.moved = true;
        self.state.scroll_by(offset, bounds, content_bounds);
    }
}

/// How far iced moves the offset for a notch of `lines`: 60 px a line, and
/// Shift turns vertical into horizontal. Every scrollable here is anchored at
/// its start, so the sign needs no flip.
fn notch(lines: Vector, modifiers: keyboard::Modifiers) -> Vector {
    let lines = if modifiers.shift() {
        Vector::new(lines.y, lines.x)
    } else {
        lines
    };
    lines * -60.0
}

impl<Message> Smooth<'_, Message> {
    fn probe(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
    ) -> Option<Extent> {
        let mut probe = Probe(None);
        self.inner.operate(tree, layout, renderer, &mut probe);
        probe.0
    }

    fn place(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, offset: Vector) {
        self.inner
            .operate(tree, layout, renderer, &mut Place(Some(offset)));
    }
}

/// The speed the fingers had when they lifted at `lifted` (compositor
/// milliseconds), in pixels per second, or `None` if they had come to rest.
fn lift_velocity(samples: &[Sample], lifted: Option<u32>, now: Instant) -> Option<Vector> {
    let last = samples.last()?;
    let ms = |duration: Duration| duration.as_secs_f32() * 1e3;

    // How long before the last sample each one came, in milliseconds: by the
    // compositor's clock where every sample has it, by arrival otherwise.
    let ages: Vec<f32> = match last.time {
        Some(newest) if samples.iter().all(|sample| sample.time.is_some()) => samples
            .iter()
            .map(|sample| newest.wrapping_sub(sample.time.unwrap_or(newest)) as f32)
            .collect(),
        _ => samples
            .iter()
            .map(|sample| ms(last.received.saturating_duration_since(sample.received)))
            .collect(),
    };
    let rested = match (lifted, last.time) {
        (Some(lifted), Some(newest)) => lifted.wrapping_sub(newest) as f32,
        _ => ms(now.saturating_duration_since(last.received)),
    };
    if rested > RESTED_MS {
        return None;
    }

    // The first sample in the window only marks where it starts: its own
    // movement happened before it.
    let first = ages.iter().position(|&age| age <= VELOCITY_WINDOW_MS)?;
    let span = ages[first] / 1e3;
    if span <= 0.0 {
        return None;
    }
    let moved = samples[first + 1..]
        .iter()
        .fold(Vector::ZERO, |sum, sample| sum + sample.moved);
    let velocity = moved * (1.0 / span);

    (velocity.x.hypot(velocity.y) >= MIN_COAST_SPEED).then_some(velocity)
}

impl<Message> Widget<Message, Theme, Renderer> for Smooth<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.inner())]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[self.inner()]);
    }

    fn size(&self) -> Size<Length> {
        self.inner.size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = self.inner.layout(&mut tree.children[0], renderer, limits);
        layout::Node::with_children(node.size(), vec![node])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let state = tree.state.downcast_mut::<State>();
        // Where `iced_exwlshell` delivers the end of a touchpad gesture.
        operation.custom(self.id.as_ref(), layout.bounds(), &mut state.stop);
        operation.custom(self.id.as_ref(), layout.bounds(), &mut state.request);
        let mut watch = Watch {
            operation,
            moved: false,
        };
        self.inner.operate(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            &mut watch,
        );
        // An instant move overrides a glide asked for before it; one asked for
        // after it arrives later and still stands.
        if watch.moved {
            state.request.0 = None;
            state.moved_instantly = true;
        }
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
        let (state, children) = (tree.state.downcast_mut::<State>(), &mut tree.children);
        let inner_tree = &mut children[0];
        let inner_layout = layout.children().next().unwrap();

        let Some(extent) = self.probe(inner_tree, inner_layout, renderer) else {
            self.inner.update(
                inner_tree,
                event,
                inner_layout,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
            return;
        };

        if std::mem::take(&mut state.moved_instantly) {
            state.cancel();
        }
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) = event {
            state.modifiers = *modifiers;
        }

        if let Some(request) = state.request.0.take() {
            let from = state.motion.position().unwrap_or(extent.offset);
            let to = state.motion.target().unwrap_or(from);
            let to = extent.clamp(Vector::new(
                request.x.unwrap_or(to.x),
                request.y.unwrap_or(to.y),
            ));
            state.start(state.motion.glide(from, to));
            shell.request_redraw();
        }

        if let Some(stop) = state.stop.take_stop() {
            let samples = std::mem::take(&mut state.samples);
            if let Some(velocity) = lift_velocity(&samples, stop.time, Instant::now()) {
                let velocity = Vector::new(
                    if stop.axes.x { velocity.x } else { 0.0 },
                    if stop.axes.y { velocity.y } else { 0.0 },
                );
                state.start(Motion::Coast {
                    x: Decay::new(extent.offset.x, velocity.x, FRICTION),
                    y: Decay::new(extent.offset.y, velocity.y, FRICTION),
                });
                shell.request_redraw();
            }
        }

        if let Event::Window(window::Event::RedrawRequested(now)) = event
            && let Some(last_frame) = state.last_frame
        {
            let dt = now
                .saturating_duration_since(last_frame)
                .as_secs_f32()
                .min(MAX_FRAME);
            let moving = state.motion.tick(dt, extent.max);
            if let Some(position) = state.motion.position() {
                self.place(inner_tree, inner_layout, renderer, extent.clamp(position));
            }
            if moving {
                state.last_frame = Some(*now);
                shell.request_redraw();
            } else {
                state.cancel();
            }
        }

        let Some(displayed) = self.probe(inner_tree, inner_layout, renderer) else {
            return;
        };
        // A notch moves on from where the glide in progress is going, not from
        // where it has got to. But where the list shows an edge the notch
        // points past, iced cannot move, and not moving would not tell whether
        // the content took the event: step 1 px back from that edge so it can.
        // iced hands its content the cursor plus the offset, so the cursor
        // steps the other way and the content sees exactly what is on screen.
        // A cursor that would leave the list by that step — within 1 px of
        // its edge — skips it, and the notch is not reversed there.
        let notched = match event {
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x, y },
            }) => Some(notch(Vector::new(*x, *y), state.modifiers)),
            _ => None,
        };
        let mut inner_cursor = cursor;
        if let Some(notch) = notched
            && state.motion.target().is_some()
        {
            let inward = |offset: f32, notch: f32, max: f32| {
                if notch < 0.0 && offset <= 0.0 && max >= 1.0 {
                    1.0
                } else if notch > 0.0 && offset >= max && max >= 1.0 {
                    max - 1.0
                } else {
                    offset
                }
            };
            let stepped = Vector::new(
                inward(displayed.offset.x, notch.x, extent.max.x),
                inward(displayed.offset.y, notch.y, extent.max.y),
            );
            let step = stepped - displayed.offset;
            if step != Vector::ZERO
                && let Some(position) = cursor.position_over(inner_layout.bounds())
                && inner_layout.bounds().contains(position - step)
            {
                self.place(inner_tree, inner_layout, renderer, stepped);
                inner_cursor = mouse::Cursor::Available(position - step);
            }
        }
        let Some(before) = self.probe(inner_tree, inner_layout, renderer) else {
            return;
        };
        self.inner.update(
            inner_tree,
            event,
            inner_layout,
            inner_cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        let Some(after) = self.probe(inner_tree, inner_layout, renderer) else {
            return;
        };
        let moved = after.offset - before.offset;
        let has_moved = moved.x.abs() > f32::EPSILON || moved.y.abs() > f32::EPSILON;

        match event {
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { .. },
            }) => {
                if before.offset != displayed.offset || has_moved {
                    self.place(inner_tree, inner_layout, renderer, displayed.offset);
                }
                if has_moved && let Some(notch) = notched {
                    let from = state.motion.target().unwrap_or(displayed.offset);
                    let to = after.clamp(from + notch);
                    state.start(state.motion.glide(displayed.offset, to));
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { .. },
            }) => {
                let frame = frames::current();
                if has_moved {
                    state.cancel();
                }
                // Only the list the fingers moved owns the gesture: every list
                // sees the event, and the last to claim it gets the stop.
                if has_moved
                    && frame.and_then(|frame| frame.source) == Some(Source::Finger)
                    && state.stop.claim()
                {
                    let received = Instant::now();
                    // A new gesture starts a new record.
                    if state
                        .samples
                        .last()
                        .is_some_and(|last| received - last.received > Duration::from_millis(500))
                    {
                        state.samples.clear();
                    }
                    state.samples.push(Sample {
                        time: frame.and_then(|frame| frame.time),
                        received,
                        moved,
                    });
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if moved.x.abs() > SETTLED || moved.y.abs() > SETTLED =>
            {
                // A click on the empty track, which iced answers by jumping
                // and grabbing the scroller. Glide there instead, and let go of
                // the scroller: the click was not the start of a drag.
                self.place(inner_tree, inner_layout, renderer, displayed.offset);
                self.inner.update(
                    inner_tree,
                    &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                    inner_layout,
                    mouse::Cursor::Unavailable,
                    renderer,
                    clipboard,
                    shell,
                    viewport,
                );
                state.start(state.motion.glide(displayed.offset, after.offset));
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonPressed(_)) if cursor.is_over(layout.bounds()) => {
                state.cancel();
            }
            Event::Window(window::Event::RedrawRequested(_)) => {}
            // Anything else that moved the offset — the scroller dragged, the
            // content resized — takes over from the motion.
            _ if has_moved => state.cancel(),
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.inner.mouse_interaction(
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
        self.inner.draw(
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
        self.inner.overlay(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Smooth<'a, Message>> for Element<'a, Message, Theme, Renderer> {
    fn from(smooth: Smooth<'a, Message>) -> Self {
        Element::new(smooth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::{Point, clipboard};
    use iced_runtime::user_interface::{Cache, UserInterface};

    const SIZE: Size = Size::new(100.0, 100.0);

    /// A 100 × 100 list over 1000 px of content, whose most is 900.
    fn list() -> Element<'static, (), Theme, Renderer> {
        use crate::ui::widget::space;

        super::super::vertical(space::vertical().height(Length::Fixed(1000.0)))
            .id(Id::new("list"))
            .width(Length::Fixed(SIZE.width))
            .height(Length::Fixed(SIZE.height))
            .into()
    }

    struct Harness {
        view: fn() -> Element<'static, (), Theme, Renderer>,
        /// Messages published so far.
        published: usize,
        renderer: Renderer,
        cache: Option<Cache>,
        cursor: mouse::Cursor,
        now: Instant,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                view: list,
                published: 0,
                renderer: iced_texture_cache::testing::headless_tiny_skia(),
                cache: Some(Cache::default()),
                cursor: mouse::Cursor::Available(Point::new(50.0, 50.0)),
                now: Instant::now(),
            }
        }

        fn with_ui<R>(
            &mut self,
            f: impl FnOnce(&mut UserInterface<'_, (), Theme, Renderer>, &mut Renderer) -> R,
        ) -> R {
            let cache = self.cache.take().unwrap();
            let mut ui = UserInterface::build((self.view)(), SIZE, cache, &mut self.renderer);
            let result = f(&mut ui, &mut self.renderer);
            self.cache = Some(ui.into_cache());
            result
        }

        fn send(&mut self, event: Event) {
            let cursor = self.cursor;
            let mut messages = Vec::new();
            self.with_ui(|ui, renderer| {
                let _ = ui.update(
                    &[event],
                    cursor,
                    renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
            });
            self.published += messages.len();
        }

        fn offset(&mut self) -> f32 {
            self.with_ui(|ui, renderer| {
                let mut probe = Probe(None);
                ui.operate(renderer, &mut probe);
                probe.0.unwrap().offset.y
            })
        }

        fn operate(&mut self, operation: &mut dyn Operation) {
            self.with_ui(|ui, renderer| ui.operate(renderer, operation));
        }

        fn frame(&mut self) {
            self.now = self.now.max(Instant::now()) + Duration::from_millis(16);
            self.send(Event::Window(window::Event::RedrawRequested(self.now)));
        }

        /// Runs frames until the offset has held for half a second, returning
        /// every one seen. iced rounds the offset to whole pixels, so the last
        /// frames of a glide can read the same twice without having stopped.
        fn settle(&mut self) -> Vec<f32> {
            let mut seen = vec![self.offset()];
            let mut held = 0;
            for _ in 0..300 {
                self.frame();
                let offset = self.offset();
                held = if offset == *seen.last().unwrap() {
                    held + 1
                } else {
                    0
                };
                seen.push(offset);
                if held == 30 {
                    return seen;
                }
            }
            panic!("never settled: {seen:?}");
        }

        fn wheel(&mut self, lines: f32) {
            self.send(Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: lines },
            }));
        }
    }

    #[test]
    fn a_wheel_notch_glides_instead_of_jumping() {
        let mut h = Harness::new();
        h.wheel(-1.0);
        assert_eq!(h.offset(), 0.0, "no jump");

        let seen = h.settle();
        assert_eq!(*seen.last().unwrap(), 60.0);
        assert!(
            seen.iter().any(|&offset| offset > 0.0 && offset < 60.0),
            "passed through in between: {seen:?}"
        );
        assert!(
            seen.windows(2).all(|w| w[1] >= w[0]),
            "never backs up: {seen:?}"
        );
    }

    #[test]
    fn notches_in_quick_succession_add_up() {
        let mut h = Harness::new();
        h.wheel(-1.0);
        h.frame();
        h.wheel(-1.0);
        h.frame();
        h.wheel(-1.0);
        assert_eq!(*h.settle().last().unwrap(), 180.0);
    }

    #[test]
    fn a_glide_stops_at_the_edge() {
        let mut h = Harness::new();
        for _ in 0..20 {
            h.wheel(-1.0);
        }
        assert_eq!(*h.settle().last().unwrap(), 900.0);
    }

    fn glide_to(id: &'static str, y: f32) -> super::super::GlideTo<()> {
        super::super::GlideTo {
            id: Id::new(id),
            offset: AbsoluteOffset {
                x: None,
                y: Some(y),
            },
            output: std::marker::PhantomData,
        }
    }

    #[test]
    fn glide_to_glides_the_list_it_names() {
        let mut h = Harness::new();
        h.operate(&mut glide_to("another", 500.0));
        h.frame();
        assert_eq!(h.offset(), 0.0, "another id is not this list");

        h.operate(&mut glide_to("list", 500.0));
        h.frame();
        let seen = h.settle();
        assert_eq!(*seen.last().unwrap(), 500.0);
        assert!(
            seen.iter()
                .filter(|&&offset| offset > 0.0 && offset < 500.0)
                .count()
                > 3,
            "moved over several frames: {seen:?}"
        );
    }

    #[test]
    fn an_instant_move_stays_instant() {
        let mut h = Harness::new();
        h.operate(&mut Place(Some(Vector::new(0.0, 300.0))));
        assert_eq!(h.offset(), 300.0);
    }

    #[test]
    fn a_click_on_the_track_glides_and_does_not_grab_the_scroller() {
        let mut h = Harness::new();
        // The bar is the 8 px on the right; the scroller is its top 10 px.
        h.cursor = mouse::Cursor::Available(Point::new(96.0, 95.0));
        h.send(Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        )));
        assert_eq!(h.offset(), 0.0, "no jump");

        h.cursor = mouse::Cursor::Available(Point::new(96.0, 20.0));
        h.send(Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(96.0, 20.0),
        }));
        assert_eq!(h.offset(), 0.0, "the scroller is not being dragged");

        assert_eq!(*h.settle().last().unwrap(), 900.0);
    }

    #[test]
    fn an_instant_move_during_a_glide_wins() {
        let mut h = Harness::new();
        h.wheel(-1.0);
        h.frame();
        h.frame();
        // What `scroll_to` does to the inner scrollable.
        h.operate(&mut Place(Some(Vector::new(0.0, 600.0))));
        h.frame();
        assert_eq!(h.offset(), 600.0, "the glide let go");
        assert_eq!(*h.settle().last().unwrap(), 600.0);
    }

    #[test]
    fn a_notch_back_at_the_edge_is_not_lost() {
        let mut h = Harness::new();
        h.wheel(-1.0);
        h.wheel(1.0);
        assert_eq!(*h.settle().last().unwrap(), 0.0);

        // And at the far end.
        for _ in 0..20 {
            h.wheel(-1.0);
        }
        let _ = h.settle();
        h.wheel(1.0);
        h.wheel(-1.0);
        assert_eq!(*h.settle().last().unwrap(), 900.0);
    }

    fn scroll_to(y: f32) -> impl Operation {
        iced_core::widget::operation::scrollable::scroll_to(
            Id::new("list"),
            AbsoluteOffset {
                x: None,
                y: Some(y),
            },
        )
    }

    #[test]
    fn scroll_to_the_same_offset_still_stops_a_glide() {
        let mut h = Harness::new();
        h.wheel(-3.0);
        h.operate(&mut scroll_to(0.0));
        assert_eq!(*h.settle().last().unwrap(), 0.0);
    }

    #[test]
    fn the_later_of_glide_to_and_scroll_to_wins() {
        let mut h = Harness::new();
        h.operate(&mut glide_to("list", 500.0));
        h.operate(&mut scroll_to(600.0));
        assert_eq!(*h.settle().last().unwrap(), 600.0);

        h.operate(&mut scroll_to(100.0));
        h.operate(&mut glide_to("list", 300.0));
        assert_eq!(*h.settle().last().unwrap(), 300.0);
    }

    #[test]
    fn during_a_glide_the_wheel_reaches_what_is_on_screen() {
        // The wheel area is the only thing on screen before the list scrolls.
        let mut h = Harness::new();
        h.view = list_with_a_wheel_area;
        h.operate(&mut glide_to("list", 500.0));
        h.frame();
        let shown = h.offset();
        assert!(
            shown < 50.0,
            "the area is still under the cursor at {shown}"
        );
        h.wheel(-1.0);
        assert_eq!(h.published, 1, "the area got the wheel");
    }

    /// A 100 × 100 list whose first 100 px handle the wheel themselves.
    fn list_with_a_wheel_area() -> Element<'static, (), Theme, Renderer> {
        use crate::ui::widget::space;
        use iced::widget::{column, mouse_area};

        super::super::vertical(column![
            mouse_area(
                space::vertical()
                    .width(Length::Fill)
                    .height(Length::Fixed(100.0))
            )
            .on_scroll(|_| ()),
            space::vertical().height(Length::Fixed(900.0)),
        ])
        .id(Id::new("list"))
        .width(Length::Fixed(SIZE.width))
        .height(Length::Fixed(SIZE.height))
        .into()
    }

    #[test]
    fn at_the_edge_the_wheel_still_reaches_what_is_on_screen() {
        let mut h = Harness::new();
        h.view = list_with_a_wheel_area;
        h.operate(&mut glide_to("list", 500.0));
        assert_eq!(h.offset(), 0.0, "the list shows its top edge");

        // Up, past that edge, at the last half pixel of the area.
        h.cursor = mouse::Cursor::Available(Point::new(50.0, 99.5));
        h.wheel(1.0);
        assert_eq!(h.published, 1, "the area got the wheel");
    }

    #[test]
    fn the_edge_step_does_not_pull_in_a_cursor_from_outside() {
        let mut h = Harness::new();
        h.wheel(-1.0);
        assert_eq!(h.offset(), 0.0, "a glide to 60 is queued at the top edge");

        // Just below the list, over whatever is there.
        h.cursor = mouse::Cursor::Available(Point::new(50.0, 100.5));
        h.wheel(1.0);
        assert_eq!(*h.settle().last().unwrap(), 60.0);
    }

    #[test]
    fn a_click_in_the_list_stops_a_glide() {
        let mut h = Harness::new();
        h.wheel(-3.0);
        h.frame();
        h.frame();
        let stopped = h.offset();
        assert!(stopped > 0.0 && stopped < 180.0);
        h.send(Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        )));
        h.frame();
        h.frame();
        assert_eq!(h.offset(), stopped);
    }

    fn samples(every_ms: u32, moved: f32, count: u32) -> Vec<Sample> {
        let received = Instant::now();
        (0..count)
            .map(|i| Sample {
                time: Some(1000 + i * every_ms),
                received,
                moved: Vector::new(0.0, moved),
            })
            .collect()
    }

    #[test]
    fn a_lift_takes_the_speed_of_the_last_movement() {
        let samples = samples(10, 10.0, 20);
        let velocity = lift_velocity(&samples, Some(1000 + 19 * 10 + 5), Instant::now()).unwrap();
        assert!((velocity.y - 1000.0).abs() < 1.0, "{velocity:?}");
    }

    #[test]
    fn fingers_that_came_to_rest_do_not_coast() {
        let samples = samples(10, 10.0, 20);
        assert!(lift_velocity(&samples, Some(1000 + 19 * 10 + 200), Instant::now()).is_none());
    }

    #[test]
    fn one_movement_or_a_slow_one_does_not_coast() {
        assert!(lift_velocity(&samples(10, 10.0, 1), Some(1000), Instant::now()).is_none());
        assert!(lift_velocity(&samples(10, 0.2, 20), Some(1190), Instant::now()).is_none());
    }

    #[test]
    fn a_coast_stops_dead_at_the_edge() {
        let mut motion = Motion::Coast {
            x: Decay::new(0.0, 0.0, FRICTION),
            y: Decay::new(800.0, 5000.0, FRICTION),
        };
        let max = Vector::new(0.0, 900.0);
        let mut frames = 0;
        while motion.tick(1.0 / 60.0, max) {
            frames += 1;
            assert!(motion.position().unwrap().y <= 900.0);
        }
        assert!(
            frames < 10,
            "stopped when it hit the edge, after {frames} frames"
        );
        assert_eq!(motion.position().unwrap().y, 900.0);
    }
}
