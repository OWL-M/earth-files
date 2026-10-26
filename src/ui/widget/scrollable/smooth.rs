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
//! lift, the compositor says so (`iced_exwlshell::gesture::Stop`), and the list
//! coasts on with the speed it had, slowing under friction.
//!
//! Programmatic scrolling is instant with [`super::scroll_to`] and glides with
//! [`super::glide_to`].
//!
//! The scrollbar is [`Smooth`]'s own; iced's is hidden. iced keeps its handle
//! only 2 px long, which over a folder like `/nix/store` all but disappears,
//! and fixes its thickness when the view is built. Here the handle is never
//! shorter than [`MIN_HANDLE`], and the bar rests thin and widens on a spring
//! while the pointer is over it or drags it.

use std::time::{Duration, Instant};

use iced::widget::scrollable::{
    AbsoluteOffset, Catalog, Direction, RelativeOffset, Scrollable, Scrollbar, Status, Viewport,
};
use iced_core::event::Event;
use iced_core::widget::operation::scrollable::Scrollable as ScrollableState;
use iced_core::widget::{Id, Operation, Tree, tree};
use iced_core::{
    Clipboard, Element, Layout, Length, Point, Rectangle, Renderer as _, Shell, Size, Vector,
    Widget, keyboard, layout, mouse, overlay, renderer, touch, window,
};
use iced_exwlshell::gesture::{self as frames, Gesture, GestureReceiver, Source, Stop};
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

/// How thick a bar is at rest.
const BAR_THIN: f32 = 4.0;

/// How thick a bar is under the pointer or while dragged. The pointer finds it
/// across this whole width even while it is thin.
const BAR_WIDE: f32 = 8.0;

/// How a bar widens and narrows.
const BAR_SPRING: SpringParams = SpringParams::new(0.0, Duration::from_millis(150));

/// The shortest a handle gets, so it stays visible and easy to grab over
/// very long content.
const MIN_HANDLE: f32 = 32.0;

/// Which way a [`Smooth`] scrolls, and so which edge its bar is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// The bar is on the right.
    Vertical,
    /// The bar is along the bottom.
    Horizontal,
}

/// iced's [`Scrollable`] with smooth wheel, track, touchpad and programmatic
/// scrolling. See the [module documentation](self).
#[allow(missing_debug_implementations)]
pub struct Smooth<'a, Message> {
    inner: Scrollable<'a, Message, Theme, Renderer>,
    id: Option<Id>,
    axis: Axis,
    /// How far the bar sits in from the edge it is on.
    margin: f32,
    class: <Theme as Catalog>::Class<'a>,
}

impl<'a, Message> Smooth<'a, Message> {
    pub(super) fn new(inner: Scrollable<'a, Message, Theme, Renderer>, axis: Axis) -> Self {
        let direction = match axis {
            Axis::Vertical => Direction::Vertical(Scrollbar::hidden()),
            Axis::Horizontal => Direction::Horizontal(Scrollbar::hidden()),
        };
        Self {
            inner: inner.direction(direction),
            id: None,
            axis,
            margin: 0.0,
            class: <Theme as Catalog>::default(),
        }
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

    /// Sets how far the bar sits in from the edge it is on.
    #[must_use]
    pub fn bar_margin(mut self, margin: f32) -> Self {
        self.margin = margin;
        self
    }

    /// Sets the style class.
    #[must_use]
    pub fn class(mut self, class: impl Into<<Theme as Catalog>::Class<'a>>) -> Self {
        self.class = class.into();
        self.inner = self.inner.class(self.class);
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
    gestures: GestureReceiver,
    request: GlideRequest,
    /// How thick the bar is drawn, between [`BAR_THIN`] and [`BAR_WIDE`].
    thickness: Spring,
    /// When the thickness last advanced, while it is changing.
    thickness_frame: Option<Instant>,
    /// Where along the handle the pointer holds it, `0.0..=1.0`, while the
    /// handle is dragged.
    grabbed: Option<f32>,
    /// The offset and extent the last event left, which the bar is drawn by.
    shown: Option<Extent>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            motion: Motion::Idle,
            last_frame: None,
            moved_instantly: false,
            modifiers: keyboard::Modifiers::default(),
            samples: Vec::new(),
            gestures: GestureReceiver::new(),
            request: GlideRequest::default(),
            thickness: Spring::new(BAR_SPRING, BAR_THIN),
            thickness_frame: None,
            grabbed: None,
            shown: None,
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

    /// Notes a touchpad movement that moved the offset by `moved`, so the
    /// list can coast on when the fingers lift. Only the list the fingers
    /// moved owns the gesture: every list sees the event, and the last to
    /// claim it gets the stop.
    fn record_finger_move(&mut self, moved: Vector) {
        let frame = frames::current();
        if frame.and_then(|frame| frame.source) != Some(Source::Finger) || !self.gestures.claim() {
            return;
        }
        let received = Instant::now();
        // A new gesture starts a new record.
        if self
            .samples
            .last()
            .is_some_and(|last| received - last.received > Duration::from_millis(500))
        {
            self.samples.clear();
        }
        self.samples.push(Sample {
            time: frame.and_then(|frame| frame.time),
            received,
            moved,
        });
    }

    /// Widens the bar while the pointer is over it or drags its handle, and
    /// narrows it otherwise. Returns whether the thickness has to move.
    fn hover(&mut self, over_bar: bool) -> bool {
        let target = if over_bar || self.grabbed.is_some() {
            BAR_WIDE
        } else {
            BAR_THIN
        };
        if self.thickness.target() == target {
            return false;
        }
        self.thickness.set_target(target);
        self.thickness_frame.get_or_insert_with(Instant::now);
        true
    }

    /// Advances the thickness to `now`. Returns whether it is still moving.
    fn tick_thickness(&mut self, now: Instant) -> bool {
        let Some(last) = self.thickness_frame else {
            return false;
        };
        let dt = now
            .saturating_duration_since(last)
            .as_secs_f32()
            .min(MAX_FRAME);
        self.thickness.tick(dt);
        if self.thickness.is_settled_within(0.05) {
            self.thickness.snap();
            self.thickness_frame = None;
            return false;
        }
        self.thickness_frame = Some(now);
        true
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

/// Where a scrollbar and its handle are, for one layout and offset.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bar {
    axis: Axis,
    /// The strip along the edge where the pointer finds the bar.
    lane: Rectangle,
    /// The edge, across the axis, the bar is drawn in from.
    edge: f32,
    /// Where the track starts along the axis, and how long it is.
    start: f32,
    length: f32,
    /// Where the handle starts along the axis, and how long it is.
    handle_start: f32,
    handle_length: f32,
    /// The most the scrollable's offset can be along the axis.
    max: f32,
}

impl Bar {
    /// The bar of a scrollable at `bounds`, or `None` when its content fits
    /// and there is nothing to scroll.
    fn new(axis: Axis, margin: f32, bounds: Rectangle, extent: Extent) -> Option<Self> {
        let lane_width = BAR_WIDE + 2.0 * margin;
        let (lane, edge, start, length, offset, max) = match axis {
            Axis::Vertical => (
                Rectangle {
                    x: bounds.x + bounds.width - lane_width,
                    width: lane_width,
                    ..bounds
                },
                bounds.x + bounds.width - margin,
                bounds.y,
                bounds.height,
                extent.offset.y,
                extent.max.y,
            ),
            Axis::Horizontal => (
                Rectangle {
                    y: bounds.y + bounds.height - lane_width,
                    height: lane_width,
                    ..bounds
                },
                bounds.y + bounds.height - margin,
                bounds.x,
                bounds.width,
                extent.offset.x,
                extent.max.x,
            ),
        };
        if max <= 0.0 || length <= 0.0 {
            return None;
        }
        let handle_length = (length * length / (length + max))
            .max(MIN_HANDLE)
            .min(length);
        let handle_start = start + (offset / max).clamp(0.0, 1.0) * (length - handle_length);
        Some(Self {
            axis,
            lane,
            edge,
            start,
            length,
            handle_start,
            handle_length,
            max,
        })
    }

    /// How far along the axis `point` is.
    fn along(&self, point: Point) -> f32 {
        match self.axis {
            Axis::Vertical => point.y,
            Axis::Horizontal => point.x,
        }
    }

    /// A rectangle `thickness` thick, drawn in from the edge, over `start` and
    /// `length` along the axis.
    fn rectangle(&self, thickness: f32, start: f32, length: f32) -> Rectangle {
        match self.axis {
            Axis::Vertical => Rectangle {
                x: self.edge - thickness,
                y: start,
                width: thickness,
                height: length,
            },
            Axis::Horizontal => Rectangle {
                x: start,
                y: self.edge - thickness,
                width: length,
                height: thickness,
            },
        }
    }

    fn track(&self, thickness: f32) -> Rectangle {
        self.rectangle(thickness, self.start, self.length)
    }

    fn handle(&self, thickness: f32) -> Rectangle {
        self.rectangle(thickness, self.handle_start, self.handle_length)
    }

    /// Whether `point`, inside the lane, is on the handle rather than the
    /// empty track.
    fn is_on_handle(&self, point: Point) -> bool {
        let along = self.along(point);
        (self.handle_start..=self.handle_start + self.handle_length).contains(&along)
    }

    /// Where along the handle `point` is, `0.0..=1.0`.
    fn grab_at(&self, point: Point) -> f32 {
        ((self.along(point) - self.handle_start) / self.handle_length).clamp(0.0, 1.0)
    }

    /// The offset that puts the handle's start at `handle_start`.
    fn offset_for(&self, handle_start: f32) -> f32 {
        let room = self.length - self.handle_length;
        if room <= 0.0 {
            return 0.0;
        }
        ((handle_start - self.start) / room).clamp(0.0, 1.0) * self.max
    }

    /// `offset` along the axis, with the other axis left at `from`.
    fn vector(&self, from: Vector, offset: f32) -> Vector {
        match self.axis {
            Axis::Vertical => Vector::new(from.x, offset),
            Axis::Horizontal => Vector::new(offset, from.y),
        }
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

    /// The bar as the last event left it.
    fn bar(&self, state: &State, layout: Layout<'_>) -> Option<Bar> {
        Bar::new(self.axis, self.margin, layout.bounds(), state.shown?)
    }

    fn is_over_bar(&self, state: &State, layout: Layout<'_>, cursor: mouse::Cursor) -> bool {
        self.bar(state, layout)
            .and_then(|bar| cursor.position_over(bar.lane))
            .is_some()
    }
}

/// Takes every gesture delivered since the last call and returns their stops
/// as one: the latest time, and each axis any of them stopped. Swipes, pinches
/// and holds are dropped.
fn take_stop(gestures: &mut GestureReceiver) -> Option<Stop> {
    let mut merged: Option<Stop> = None;
    while let Some(gesture) = gestures.take() {
        let Gesture::Stop(stop) = gesture else {
            continue;
        };
        match &mut merged {
            None => merged = Some(stop),
            Some(earlier) => {
                earlier.time = stop.time.or(earlier.time);
                earlier.axes.x |= stop.axes.x;
                earlier.axes.y |= stop.axes.y;
            }
        }
    }
    merged
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
        operation.custom(self.id.as_ref(), layout.bounds(), &mut state.gestures);
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

        if let Event::Window(window::Event::RedrawRequested(now)) = event
            && state.tick_thickness(*now)
        {
            shell.request_redraw();
        }
        let bar = Bar::new(self.axis, self.margin, layout.bounds(), extent);
        if bar.is_none() {
            state.grabbed = None;
        }
        if let Some(bar) = bar {
            let over = cursor.position_over(bar.lane);
            // A finger works the bar as the left button does.
            let pressed = match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => over,
                Event::Touch(touch::Event::FingerPressed { position, .. }) => {
                    bar.lane.contains(*position).then_some(*position)
                }
                _ => None,
            };
            let moved_to = match event {
                Event::Mouse(mouse::Event::CursorMoved { position })
                | Event::Touch(touch::Event::FingerMoved { position, .. }) => Some(*position),
                _ => None,
            };
            let released = matches!(
                event,
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                    | Event::Touch(
                        touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. }
                    )
            );
            if let Some(point) = pressed {
                if bar.is_on_handle(point) {
                    state.cancel();
                    state.grabbed = Some(bar.grab_at(point));
                } else {
                    // A click on the empty track glides the handle's middle
                    // to it, as iced jumps it there.
                    let to = bar.offset_for(bar.along(point) - bar.handle_length / 2.0);
                    state.start(
                        state
                            .motion
                            .glide(extent.offset, bar.vector(extent.offset, to)),
                    );
                }
                state.hover(true);
                state.shown = Some(extent);
                shell.capture_event();
                shell.request_redraw();
                return;
            }
            if let (Some(position), Some(grabbed)) = (moved_to, state.grabbed) {
                let to = bar.offset_for(bar.along(position) - grabbed * bar.handle_length);
                let offset = bar.vector(extent.offset, to);
                self.place(inner_tree, inner_layout, renderer, offset);
                state.shown = Some(Extent { offset, ..extent });
                shell.capture_event();
                shell.request_redraw();
                return;
            }
            if released && state.grabbed.is_some() {
                state.grabbed = None;
                state.hover(over.is_some());
                shell.capture_event();
                shell.request_redraw();
                return;
            }
            // The wheel over the bar scrolls this list, and nothing under
            // the bar sees it, as iced's own bar did. A list that cannot
            // move that way leaves the event to the one around it.
            if let (Event::Mouse(mouse::Event::WheelScrolled { delta }), Some(_)) = (event, over) {
                let moved = match *delta {
                    mouse::ScrollDelta::Lines { x, y } => {
                        let from = state.motion.target().unwrap_or(extent.offset);
                        let to = extent.clamp(from + notch(Vector::new(x, y), state.modifiers));
                        if to != from {
                            state.start(state.motion.glide(extent.offset, to));
                            state.shown = Some(extent);
                        }
                        to != from
                    }
                    mouse::ScrollDelta::Pixels { x, y } => {
                        let offset = extent.clamp(extent.offset - Vector::new(x, y));
                        if offset != extent.offset {
                            state.cancel();
                            state.record_finger_move(offset - extent.offset);
                            self.place(inner_tree, inner_layout, renderer, offset);
                            state.shown = Some(Extent { offset, ..extent });
                        }
                        offset != extent.offset
                    }
                };
                if moved {
                    shell.capture_event();
                    shell.request_redraw();
                }
                return;
            }
        }
        let over_bar = bar.and_then(|bar| cursor.position_over(bar.lane)).is_some();
        if state.hover(over_bar) {
            shell.request_redraw();
        }
        // What is under the bar does not see the pointer.
        let cursor = if over_bar {
            mouse::Cursor::Unavailable
        } else {
            cursor
        };

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

        if let Some(stop) = take_stop(&mut state.gestures) {
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
                if has_moved {
                    state.cancel();
                    state.record_finger_move(moved);
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(_)) if cursor.is_over(layout.bounds()) => {
                state.cancel();
            }
            Event::Window(window::Event::RedrawRequested(_)) => {}
            // Anything else that moved the offset — the content resized, say —
            // takes over from the motion.
            _ if has_moved => state.cancel(),
            _ => {}
        }
        state.shown = self.probe(inner_tree, inner_layout, renderer);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        if state.grabbed.is_some() || self.is_over_bar(state, layout, cursor) {
            return mouse::Interaction::None;
        }
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
        let state = tree.state.downcast_ref::<State>();
        let bar = self.bar(state, layout);
        let over_bar = self.is_over_bar(state, layout, cursor);
        self.inner.draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().unwrap(),
            if over_bar || state.grabbed.is_some() {
                mouse::Cursor::Unavailable
            } else {
                cursor
            },
            viewport,
        );

        let (Some(bar), Some(visible)) = (bar, layout.bounds().intersection(viewport)) else {
            return;
        };
        let vertical = self.axis == Axis::Vertical;
        let status = if state.grabbed.is_some() {
            Status::Dragged {
                is_horizontal_scrollbar_dragged: !vertical,
                is_vertical_scrollbar_dragged: vertical,
                is_horizontal_scrollbar_disabled: false,
                is_vertical_scrollbar_disabled: false,
            }
        } else if over_bar {
            Status::Hovered {
                is_horizontal_scrollbar_hovered: !vertical,
                is_vertical_scrollbar_hovered: vertical,
                is_horizontal_scrollbar_disabled: false,
                is_vertical_scrollbar_disabled: false,
            }
        } else {
            Status::Active {
                is_horizontal_scrollbar_disabled: false,
                is_vertical_scrollbar_disabled: false,
            }
        };
        let style = theme.style(&self.class, status);
        let rail = if vertical {
            style.vertical_rail
        } else {
            style.horizontal_rail
        };
        let thickness = state.thickness.position();
        renderer.with_layer(visible, |renderer| {
            if let Some(background) = rail.background {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: bar.track(thickness),
                        border: rail.border,
                        ..renderer::Quad::default()
                    },
                    background,
                );
            }
            renderer.fill_quad(
                renderer::Quad {
                    bounds: bar.handle(thickness),
                    border: rail.scroller.border,
                    ..renderer::Quad::default()
                },
                rail.scroller.background,
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

    fn press_at(h: &mut Harness, x: f32, y: f32) {
        h.cursor = mouse::Cursor::Available(Point::new(x, y));
        h.send(Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        )));
    }

    fn move_to(h: &mut Harness, x: f32, y: f32) {
        h.cursor = mouse::Cursor::Available(Point::new(x, y));
        h.send(Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(x, y),
        }));
    }

    #[test]
    fn a_click_on_the_track_glides_and_does_not_grab_the_scroller() {
        let mut h = Harness::new();
        // The bar is the 8 px on the right; the handle is its top 32 px. A
        // click at 50 centres the handle there: its start at 34 of the 68 it
        // can travel, half of 900.
        h.frame();
        press_at(&mut h, 96.0, 50.0);
        assert_eq!(h.offset(), 0.0, "no jump");

        move_to(&mut h, 96.0, 20.0);
        assert_eq!(h.offset(), 0.0, "the scroller is not being dragged");

        assert_eq!(*h.settle().last().unwrap(), 450.0);
    }

    #[test]
    fn dragging_the_handle_keeps_it_under_the_pointer() {
        let mut h = Harness::new();
        h.frame();
        press_at(&mut h, 96.0, 10.0);
        // 34 px down is half the 68 the handle can travel.
        move_to(&mut h, 96.0, 44.0);
        assert_eq!(h.offset(), 450.0, "instant, no glide");
        // Past the end, the handle stops at it.
        move_to(&mut h, 30.0, 400.0);
        assert_eq!(h.offset(), 900.0);
        h.send(Event::Mouse(mouse::Event::ButtonReleased(
            mouse::Button::Left,
        )));
        move_to(&mut h, 96.0, 10.0);
        assert_eq!(h.offset(), 900.0, "let go");
    }

    #[test]
    fn a_finger_drags_the_handle() {
        let mut h = Harness::new();
        h.frame();
        let finger = touch::Finger(0);
        h.send(Event::Touch(touch::Event::FingerPressed {
            id: finger,
            position: Point::new(96.0, 10.0),
        }));
        h.send(Event::Touch(touch::Event::FingerMoved {
            id: finger,
            position: Point::new(96.0, 44.0),
        }));
        assert_eq!(h.offset(), 450.0);
        h.send(Event::Touch(touch::Event::FingerLost {
            id: finger,
            position: Point::new(96.0, 44.0),
        }));
        h.send(Event::Touch(touch::Event::FingerMoved {
            id: finger,
            position: Point::new(96.0, 10.0),
        }));
        assert_eq!(h.offset(), 450.0, "a lost finger lets go");
    }

    /// The list over a wheel area, under the bar too, that takes every notch.
    fn list_over_a_wheel_area() -> Element<'static, (), Theme, Renderer> {
        use crate::ui::widget::space;
        use iced::widget::mouse_area;

        super::super::vertical(
            mouse_area(
                space::vertical()
                    .width(Length::Fill)
                    .height(Length::Fixed(1000.0)),
            )
            .on_scroll(|_| ()),
        )
        .id(Id::new("list"))
        .width(Length::Fixed(SIZE.width))
        .height(Length::Fixed(SIZE.height))
        .into()
    }

    #[test]
    fn the_wheel_over_the_bar_scrolls_and_does_not_reach_the_content() {
        let mut h = Harness::new();
        h.view = list_over_a_wheel_area;
        h.frame();
        h.cursor = mouse::Cursor::Available(Point::new(96.0, 50.0));
        h.wheel(-1.0);
        assert_eq!(h.published, 0, "the content under the bar saw nothing");
        assert_eq!(*h.settle().last().unwrap(), 60.0);
    }

    #[test]
    fn the_wheel_over_the_bar_at_the_end_is_left_to_the_list_around() {
        let mut h = Harness::new();
        h.view = list_over_a_wheel_area;
        h.operate(&mut Place(Some(Vector::new(0.0, 900.0))));
        h.frame();
        h.cursor = mouse::Cursor::Available(Point::new(96.0, 50.0));
        let cursor = h.cursor;
        let mut messages = Vec::new();
        let status = h.with_ui(|ui, renderer| {
            ui.update(
                &[Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
                })],
                cursor,
                renderer,
                &mut clipboard::Null,
                &mut messages,
            )
            .1
        });
        assert_eq!(status, vec![iced_core::event::Status::Ignored]);
        assert!(messages.is_empty(), "the content under the bar saw nothing");
        assert_eq!(h.offset(), 900.0);
    }

    fn extent(offset: f32, max: f32) -> Extent {
        Extent {
            offset: Vector::new(0.0, offset),
            max: Vector::new(0.0, max),
        }
    }

    const BOUNDS: Rectangle = Rectangle {
        x: 0.0,
        y: 0.0,
        width: 100.0,
        height: 100.0,
    };

    #[test]
    fn the_handle_never_gets_shorter_than_its_minimum() {
        let bar = Bar::new(Axis::Vertical, 0.0, BOUNDS, extent(0.0, 1_000_000.0)).unwrap();
        assert_eq!(bar.handle_length, MIN_HANDLE);
        // Where the content is only a little longer, it is in proportion.
        let bar = Bar::new(Axis::Vertical, 0.0, BOUNDS, extent(0.0, 25.0)).unwrap();
        assert_eq!(bar.handle_length, 80.0);
        // Content that fits has no bar.
        assert!(Bar::new(Axis::Vertical, 0.0, BOUNDS, extent(0.0, 0.0)).is_none());
    }

    #[test]
    fn at_the_end_of_the_content_the_handle_is_at_the_end_of_the_track() {
        let bar = Bar::new(
            Axis::Vertical,
            0.0,
            BOUNDS,
            extent(1_000_000.0, 1_000_000.0),
        )
        .unwrap();
        assert_eq!(bar.handle_start + bar.handle_length, 100.0);
        assert_eq!(bar.offset_for(bar.handle_start), 1_000_000.0);
    }

    #[test]
    fn a_bar_grows_in_from_its_edge() {
        let bar = Bar::new(Axis::Vertical, 5.0, BOUNDS, extent(0.0, 900.0)).unwrap();
        assert_eq!(
            bar.lane.x, 82.0,
            "the pointer finds the wide bar and its margins"
        );
        assert_eq!(bar.handle(BAR_THIN).x, 91.0);
        assert_eq!(bar.handle(BAR_WIDE).x, 87.0);
        assert_eq!(
            bar.handle(BAR_WIDE).x + BAR_WIDE,
            95.0,
            "the outer edge stays put"
        );

        let bar = Bar::new(
            Axis::Horizontal,
            0.0,
            BOUNDS,
            Extent {
                offset: Vector::ZERO,
                max: Vector::new(900.0, 0.0),
            },
        )
        .unwrap();
        assert_eq!(
            bar.handle(BAR_THIN),
            Rectangle::new(Point::new(0.0, 96.0), Size::new(32.0, 4.0))
        );
    }

    /// Ticks the thickness until it settles, returning every value seen.
    fn settle_thickness(state: &mut State) -> Vec<f32> {
        let mut now = Instant::now();
        let mut seen = vec![state.thickness.position()];
        for _ in 0..100 {
            now += Duration::from_millis(16);
            let moving = state.tick_thickness(now);
            seen.push(state.thickness.position());
            if !moving {
                return seen;
            }
        }
        panic!("never settled: {seen:?}");
    }

    #[test]
    fn the_bar_widens_under_the_pointer_and_narrows_after() {
        let mut state = State::default();
        assert_eq!(state.thickness.position(), BAR_THIN);
        assert!(state.hover(true));
        assert!(!state.hover(true), "already widening");
        let seen = settle_thickness(&mut state);
        assert_eq!(*seen.last().unwrap(), BAR_WIDE);
        assert!(
            seen.iter().any(|&t| t > BAR_THIN && t < BAR_WIDE),
            "animates: {seen:?}"
        );

        assert!(state.hover(false));
        assert_eq!(*settle_thickness(&mut state).last().unwrap(), BAR_THIN);
    }

    #[test]
    fn the_bar_stays_wide_while_dragged() {
        let mut state = State::default();
        state.hover(true);
        state.grabbed = Some(0.5);
        assert!(!state.hover(false), "the pointer left the bar mid-drag");
        assert_eq!(state.thickness.target(), BAR_WIDE);
        state.grabbed = None;
        assert!(state.hover(false));
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
