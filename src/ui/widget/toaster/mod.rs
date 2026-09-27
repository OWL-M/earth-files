// Copyright 2024 wiiznokes
// SPDX-License-Identifier: MPL-2.0

//! A widget that displays toasts.
//!
//! Vendored from pop-os/libcosmic, src/widget/toaster/
//!
//! Load-bearing for the Undo affordance after a move to trash
//! (`app.rs` `Message::UndoTrash`), so it is vendored rather than dropped.

use std::collections::VecDeque;
use std::rc::Rc;

use iced::Task;
use iced_core::Element;
use slotmap::{SlotMap, new_key_type};
use widget::Toaster;

use crate::ui::convert::{PushMaybe, ToPadding, ToPixels};
use crate::ui::theme::{Container as ContainerClass, Spacing, spacing};
use crate::ui::widget::{Column, Row, button, container, icon, text};

mod widget;

/// Create a new Toaster widget.
///
/// `pinned`, when given, is drawn in the corner below the toasts, which
/// stack above it and move with its height: e.g. a progress card.
pub fn toaster<'a, Message: Clone + 'static>(
    toasts: &'a Toasts<Message>,
    pinned: Option<Element<'a, Message, crate::ui::Theme, crate::ui::Renderer>>,
    content: impl Into<Element<'a, Message, crate::ui::Theme, crate::ui::Renderer>>,
) -> Element<'a, Message, crate::ui::Theme, crate::ui::Renderer> {
    let Spacing {
        space_xxxs,
        space_xxs,
        space_s,
        space_m,
        ..
    } = spacing();

    let make_toast = move |(id, toast): (ToastId, &'a Toast<Message>)| {
        let row = Row::with_capacity(2)
            .push(text(&toast.message))
            .push(
                Row::with_capacity(2)
                    .push_maybe(toast.action.as_ref().map(|action| {
                        button::text(&action.description).on_press((action.message)(id))
                    }))
                    .push(
                        button::icon(icon::from_name("window-close-symbolic"))
                            .on_press((toasts.on_close)(id)),
                    )
                    .align_y(iced::Alignment::Center)
                    .spacing(space_xxs.to_pixels()),
            )
            .align_y(iced::Alignment::Center)
            .spacing(space_s.to_pixels());

        container(row)
            .padding(([space_xxs, space_s, space_xxs, space_m]).to_padding())
            .class(ContainerClass::Tooltip)
    };

    let has_toasts = !toasts.toasts.is_empty();
    let has_pinned = pinned.is_some();
    let is_empty = !has_toasts && !has_pinned;

    // The toasts and the pinned card are kept in separate columns, nested in
    // an outer one, rather than one flat column with the card pushed last:
    // iced diffs a column's children by position, so if the card shared the
    // toasts' column its index would shift by one every time a toast opened
    // or closed, and its widget state (its spring, its button state) would
    // be rebuilt from scratch instead of carried over.
    let toasts_col = toasts
        .queue
        .iter()
        .filter_map(|id| Some((*id, toasts.toasts.get(*id)?)))
        .rev()
        .map(make_toast)
        .fold(Column::with_capacity(toasts.toasts.len()), Column::push)
        .spacing(space_xxxs.to_pixels())
        // Flush with the corner they are shown in, however wide each is.
        .align_x(iced::Alignment::End);
    // The gap to the card below, only when there is a toast to leave it under.
    let toasts_col = if has_toasts && has_pinned {
        toasts_col.padding([0, 0, space_xxxs, 0].to_padding())
    } else {
        toasts_col
    };

    let col = Column::with_capacity(2)
        .push(toasts_col)
        .push_maybe(pinned)
        .align_x(iced::Alignment::End);

    Toaster::new(col.into(), content.into(), is_empty).into()
}

/// Duration for the [`Toast`]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Duration {
    #[default]
    Short,
    Long,
    Custom(std::time::Duration),
}

impl Duration {
    fn duration(&self) -> std::time::Duration {
        match self {
            Duration::Short => std::time::Duration::from_millis(5000),
            Duration::Long => std::time::Duration::from_millis(15000),
            Duration::Custom(duration) => *duration,
        }
    }
}

impl From<std::time::Duration> for Duration {
    fn from(value: std::time::Duration) -> Self {
        Self::Custom(value)
    }
}

/// Action that can be triggered by the user.
///
/// Example: `undo`
#[derive(Clone)]
pub struct Action<Message> {
    pub description: String,
    pub message: Rc<dyn Fn(ToastId) -> Message>,
}

impl<Message> std::fmt::Debug for Action<Message> {
    #[cold]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Action")
            .field("description", &self.description)
            .finish()
    }
}

/// Represent the data used to display a [`Toast`]
#[derive(Debug, Clone)]
pub struct Toast<Message> {
    message: String,
    action: Option<Action<Message>>,
    duration: Duration,
}

impl<Message> Toast<Message> {
    /// Construct a new [`Toast`] with the provided message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            action: None,
            duration: Duration::default(),
        }
    }

    /// Set the [`Action`] of this [`Toast`]
    #[must_use]
    pub fn action(
        mut self,
        description: String,
        message: impl Fn(ToastId) -> Message + 'static,
    ) -> Self {
        self.action.replace(Action {
            description,
            message: Rc::new(message),
        });
        self
    }

    /// Set the [`Duration`] of this [`Toast`]
    #[must_use]
    pub fn duration(mut self, duration: impl Into<Duration>) -> Self {
        self.duration = duration.into();
        self
    }
}

new_key_type! { pub struct ToastId; }

#[derive(Debug, Clone)]
pub struct Toasts<Message> {
    toasts: SlotMap<ToastId, Toast<Message>>,
    queue: VecDeque<ToastId>,
    on_close: fn(ToastId) -> Message,
    limit: usize,
}

impl<Message: Clone + Send + 'static> Toasts<Message> {
    pub fn new(on_close: fn(ToastId) -> Message) -> Self {
        let limit = 5;
        Self {
            toasts: SlotMap::with_capacity_and_key(limit),
            queue: VecDeque::new(),
            on_close,
            limit,
        }
    }

    /// Add a new [`Toast`]
    pub fn push(&mut self, toast: Toast<Message>) -> Task<Message> {
        self.push_with_id(toast).1
    }

    /// Add a new [`Toast`] and say which one it is, for a caller that means
    /// to remove it before its time is up.
    pub fn push_with_id(&mut self, toast: Toast<Message>) -> (ToastId, Task<Message>) {
        // Making room evicts the oldest. An evicted toast closes like any
        // other, and its owner is told the same way, so a toast that was
        // something's only control does not vanish without that something
        // hearing of it.
        let on_close = self.on_close;
        let mut closed = Vec::new();
        while self.toasts.len() >= self.limit {
            let evicted = self
                .queue
                .pop_front()
                .expect("Queue must contain all toast ids");
            self.toasts.remove(evicted);
            closed.push(Task::done(on_close(evicted)));
        }

        let duration = toast.duration.duration();

        let id = self.toasts.insert(toast);
        self.queue.push_back(id);

        closed.push(Task::future(async move {
            tokio::time::sleep(duration).await;
            on_close(id)
        }));
        (id, Task::batch(closed))
    }

    /// Remove a [`Toast`]
    pub fn remove(&mut self, id: ToastId) {
        self.toasts.remove(id);
        if let Some(pos) = self.queue.iter().position(|key| *key == id) {
            self.queue.remove(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use iced_core::widget::tree::{self, Tree};
    use iced_core::widget::{Id, Operation};
    use iced_core::{
        Clipboard, Element, Event, Layout, Length, Point, Rectangle, Shell, Size, Widget,
        clipboard, layout, mouse, renderer, window,
    };
    use iced_runtime::user_interface::{Cache, UserInterface};

    use super::*;
    use crate::ui::widget::{id_container, mouse_area, space, spring_height};

    const WINDOW: Size = Size::new(400.0, 300.0);

    /// A minimal widget for testing whether the toaster's overlay captures
    /// an event before it reaches the content, without the confounder a
    /// `mouse_area` would introduce: `mouse_area` gates every event,
    /// release and scroll included, on the cursor still being over its
    /// bounds, so it cannot tell "the overlay captured the event" apart
    /// from "the base cursor was hidden while it was over the card" (the
    /// latter happens for *any* event whenever `mouse_interaction` reports
    /// non-`None`, e.g. because the cursor is over the card, regardless of
    /// which event is being processed). `Sink` instead publishes on a
    /// scroll unconditionally, and tracks its own drag state for a release
    /// (like a real drag surface, e.g. `slider`, would) rather than
    /// checking where the cursor ended up.
    struct Sink<Message>(Message);

    impl<Message: Clone> Widget<Message, crate::ui::Theme, crate::ui::Renderer> for Sink<Message> {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Fill, Length::Fill)
        }

        fn layout(
            &mut self,
            _tree: &mut Tree,
            _renderer: &crate::ui::Renderer,
            limits: &layout::Limits,
        ) -> layout::Node {
            layout::Node::new(limits.max())
        }

        fn draw(
            &self,
            _tree: &Tree,
            _renderer: &mut crate::ui::Renderer,
            _theme: &crate::ui::Theme,
            _style: &renderer::Style,
            _layout: Layout<'_>,
            _cursor: mouse::Cursor,
            _viewport: &Rectangle,
        ) {
        }

        fn tag(&self) -> tree::Tag {
            tree::Tag::of::<bool>()
        }

        fn state(&self) -> tree::State {
            tree::State::new(false)
        }

        fn update(
            &mut self,
            tree: &mut Tree,
            event: &Event,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            _renderer: &crate::ui::Renderer,
            _clipboard: &mut dyn Clipboard,
            shell: &mut Shell<'_, Message>,
            _viewport: &Rectangle,
        ) {
            let dragging = tree.state.downcast_mut::<bool>();
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                    if cursor.is_over(layout.bounds()) =>
                {
                    *dragging = true;
                    shell.capture_event();
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    if *dragging {
                        *dragging = false;
                        shell.publish(self.0.clone());
                    }
                }
                Event::Mouse(mouse::Event::WheelScrolled { .. }) => {
                    shell.publish(self.0.clone());
                }
                _ => {}
            }
        }
    }

    /// Looks up a single container's bounds by `id`, via an [`Operation`].
    fn bounds_of(
        ui: &mut UserInterface<'_, (), crate::ui::Theme, crate::ui::Renderer>,
        renderer: &crate::ui::Renderer,
        id: &Id,
    ) -> Option<Rectangle> {
        struct Bounds<'a>(&'a Id, Option<Rectangle>);
        impl Operation for Bounds<'_> {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
                if id == Some(self.0) {
                    self.1 = Some(bounds);
                }
            }
        }
        let mut bounds = Bounds(id, None);
        ui.operate(renderer, &mut bounds);
        bounds.1
    }

    /// Lays out a toaster with a 100×40 pinned card: the card's bounds and
    /// every other container's.
    fn containers(toasts: &Toasts<()>) -> (Rectangle, Vec<Rectangle>) {
        struct Bounds(Option<Rectangle>, Vec<Rectangle>);
        impl Operation for Bounds {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
                if id == Some(&Id::new("card")) {
                    self.0 = Some(bounds);
                } else {
                    self.1.push(bounds);
                }
            }
        }

        let card = id_container(
            space::vertical()
                .width(Length::Fixed(100.0))
                .height(Length::Fixed(40.0)),
            Id::new("card"),
        );
        let view = toaster(toasts, Some(card.into()), space::horizontal());
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(view, WINDOW, Cache::default(), &mut renderer);
        let mut bounds = Bounds(None, Vec::new());
        ui.operate(&renderer, &mut bounds);
        (bounds.0.expect("the card is laid out"), bounds.1)
    }

    #[test]
    fn the_pinned_card_sits_in_the_corner_without_toasts() {
        let (card, _) = containers(&Toasts::new(|_| ()));
        assert_eq!(card.x + card.width, WINDOW.width - widget::OFFSET);
        assert_eq!(card.y + card.height, WINDOW.height - widget::OFFSET);
    }

    #[test]
    fn toasts_stack_above_the_pinned_card() {
        let mut toasts = Toasts::new(|_| ());
        let _ = toasts.push(Toast::new("hello"));
        let (card, others) = containers(&toasts);
        assert_eq!(card.y + card.height, WINDOW.height - widget::OFFSET);

        // `Operation::container` fires for every `Column`/`Row`/`Container`
        // on the way down, not just the toast, and each one on the toasts'
        // side is right-aligned like the toast itself (the wrapping columns'
        // bounds enclose it). The right-edge condition here is load-bearing,
        // not just for the clearer message in the explicit assert below: it
        // narrows the candidates to the ones sharing the toast's exact
        // width (the column wrappers and the toast itself), before the leaf
        // filter picks the innermost. Dropping it also lets in narrower
        // sibling elements (a button, the icon inside it) that happen to
        // share the exact same bounds as each other without being nested,
        // which the leaf filter (bounds containment) cannot tell apart from
        // real nesting, and empties the result.
        let aligned: Vec<Rectangle> = others
            .iter()
            .copied()
            .filter(|toast| {
                toast.y + toast.height <= card.y && toast.x + toast.width == card.x + card.width
            })
            .collect();
        let leaves: Vec<_> = aligned
            .iter()
            .copied()
            .filter(|&candidate| {
                !aligned.iter().any(|&other| {
                    other != candidate
                        && other.y >= candidate.y
                        && other.y + other.height <= candidate.y + candidate.height
                })
            })
            .collect();
        assert_eq!(
            leaves.len(),
            1,
            "exactly one toast above {card:?}: {others:?}"
        );
        assert_eq!(
            leaves[0].x + leaves[0].width,
            card.x + card.width,
            "the toast and the card share their right edge (align_x End)"
        );
    }

    #[test]
    fn the_pinned_card_keeps_its_state_when_a_toast_arrives() {
        // Stateful: springs its height open from 0. `id_container` wraps
        // `spring_height` (rather than the other way around) so the bounds
        // this test reads back are the animated height, not the fixed size
        // of the content underneath.
        let card = || {
            id_container(
                spring_height(
                    space::vertical()
                        .width(Length::Fixed(100.0))
                        .height(Length::Fixed(40.0)),
                ),
                Id::new("card"),
            )
        };

        let toasts = Toasts::new(|_| ());
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut cache = Cache::default();
        let mut now = Instant::now();

        // Settle the card open, with no toasts: run frames until the spring
        // stops asking for another one (it may overshoot before settling,
        // so a bare `height >= 40.0` would stop mid-bounce).
        let mut height = 0.0;
        let mut settled = false;
        for _ in 0..200 {
            let view = toaster(&toasts, Some(card().into()), space::horizontal());
            let mut ui = UserInterface::build(view, WINDOW, cache, &mut renderer);
            now += Duration::from_millis(16);
            let (state, _) = ui.update(
                &[Event::Window(window::Event::RedrawRequested(now))],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut clipboard::Null,
                &mut Vec::new(),
            );
            height = bounds_of(&mut ui, &renderer, &Id::new("card"))
                .expect("the card is laid out")
                .height;
            cache = ui.into_cache();
            let redraw = matches!(
                state,
                iced_runtime::user_interface::State::Updated {
                    redraw_request: window::RedrawRequest::NextFrame,
                    ..
                }
            );
            if !redraw {
                settled = true;
                break;
            }
        }
        assert!(settled, "never settled: last height {height}");
        assert_eq!(height, 40.0, "the card settled open before a toast arrived");

        // A toast arrives; rebuild the UI from the same cache, with no
        // further frames.
        let mut toasts = toasts;
        let _ = toasts.push(Toast::new("hello"));
        let view = toaster(&toasts, Some(card().into()), space::horizontal());
        let mut ui = UserInterface::build(view, WINDOW, cache, &mut renderer);
        let height = bounds_of(&mut ui, &renderer, &Id::new("card"))
            .expect("the card is laid out")
            .height;

        assert_eq!(
            height, 40.0,
            "the card's spring state was rebuilt from scratch when a toast arrived"
        );
    }

    #[test]
    fn a_press_on_the_card_does_not_reach_the_content() {
        let toasts = Toasts::new(|_| ());
        let card = id_container(
            text("x")
                .width(Length::Fixed(100.0))
                .height(Length::Fixed(40.0)),
            Id::new("card"),
        );
        let content =
            mouse_area(space::horizontal().width(Length::Fill).height(Length::Fill)).on_press(());
        let view = toaster(&toasts, Some(card.into()), content);
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(view, WINDOW, Cache::default(), &mut renderer);

        let card = bounds_of(&mut ui, &renderer, &Id::new("card")).expect("the card is laid out");
        let center = Point::new(card.x + card.width / 2.0, card.y + card.height / 2.0);

        let mut messages = Vec::new();
        let _ = ui.update(
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(center),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(
            messages.is_empty(),
            "a press on the card reached the content: {messages:?}"
        );

        // Control: a press away from the card does reach the content.
        let mut messages = Vec::new();
        let _ = ui.update(
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(Point::new(10.0, 10.0)),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert_eq!(
            messages,
            vec![()],
            "a press far from the card did not reach the content"
        );
    }

    #[test]
    fn a_scroll_over_the_card_does_not_reach_the_content() {
        let toasts = Toasts::new(|_| ());
        let card = id_container(
            text("x")
                .width(Length::Fixed(100.0))
                .height(Length::Fixed(40.0)),
            Id::new("card"),
        );
        let content: Element<'_, (), crate::ui::Theme, crate::ui::Renderer> =
            Element::new(Sink(()));
        let view = toaster(&toasts, Some(card.into()), content);
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(view, WINDOW, Cache::default(), &mut renderer);

        let card = bounds_of(&mut ui, &renderer, &Id::new("card")).expect("the card is laid out");
        let center = Point::new(card.x + card.width / 2.0, card.y + card.height / 2.0);
        let scroll = || {
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
            })
        };

        let mut messages = Vec::new();
        let _ = ui.update(
            &[scroll()],
            mouse::Cursor::Available(center),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(
            messages.is_empty(),
            "a scroll over the card reached the content: {messages:?}"
        );

        // Control: a scroll away from the card does reach the content.
        let mut messages = Vec::new();
        let _ = ui.update(
            &[scroll()],
            mouse::Cursor::Available(Point::new(10.0, 10.0)),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert_eq!(
            messages,
            vec![()],
            "a scroll far from the card did not reach the content"
        );
    }

    #[test]
    fn a_release_over_the_card_still_reaches_the_content() {
        // A `mouse_area`'s own `on_release` requires the cursor to still be
        // over its bounds at release — a property of that widget, not of
        // whether the event reached it — which would confound this test:
        // the base cursor is hidden (`Cursor::Unavailable`) for the whole
        // batch whenever `mouse_interaction` reports non-`None`, including
        // here, since the release lands over the card. `Sink` tracks
        // its own drag state instead and reacts to `ButtonReleased`
        // unconditionally, so it exercises what this test is actually
        // about: whether the overlay captures the release event itself (it
        // must not).
        let toasts = Toasts::new(|_| ());
        let card = id_container(
            text("x")
                .width(Length::Fixed(100.0))
                .height(Length::Fixed(40.0)),
            Id::new("card"),
        );
        let content: Element<'_, (), crate::ui::Theme, crate::ui::Renderer> =
            Element::new(Sink(()));
        let view = toaster(&toasts, Some(card.into()), content);
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(view, WINDOW, Cache::default(), &mut renderer);

        let card = bounds_of(&mut ui, &renderer, &Id::new("card")).expect("the card is laid out");
        let center = Point::new(card.x + card.width / 2.0, card.y + card.height / 2.0);

        // A drag begun away from the card...
        let _ = ui.update(
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(Point::new(10.0, 8.0)),
            &mut renderer,
            &mut clipboard::Null,
            &mut Vec::new(),
        );

        // ...and released over the card must still end in the content: the
        // release is not one of the events the overlay captures.
        let mut messages = Vec::new();
        let _ = ui.update(
            &[Event::Mouse(mouse::Event::ButtonReleased(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(center),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert_eq!(
            messages,
            vec![()],
            "a release over the card did not reach the content"
        );
    }
}
