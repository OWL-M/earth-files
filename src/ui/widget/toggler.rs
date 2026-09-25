//! Show toggle controls using togglers.

// `Instant` is unused under this crate's feature set; kept so the vendored
// file stays comparable with its source.
#[allow(unused_imports)]
use std::time::{Duration, Instant};

use crate::ui::anim;

use crate::ui::Element;
use iced::widget::Id;
pub use iced::widget::toggler::Status;
use iced_core::renderer::{self, Renderer};
use iced_core::widget::{self, Tree, tree};
#[allow(unused_imports)]
use iced_core::{
    Border, Clipboard, Event, Layout, Length, Pixels, Rectangle, Shell, Size, Widget, alignment,
    event, layout, mouse, text, touch, window,
};

// `Style` and `Catalog` are defined here rather than re-exported from iced,
// because `iced_widget 0.14`'s `toggler::Style` is a different shape:
//
//   * it has no `handle_radius: Radius` or `handle_margin: f32`, which this
//     widget's `draw` reads (the handle's corner radius, and the inset of the
//     handle from the track); iced's handle is always inset by a ratio of the
//     height and always drawn perfectly round.
//   * its `border_radius` is `Option<Radius>`, where `None` means "perfectly
//     round"; here it is a plain `Radius`.
//
// This shape is what `theme::style::iced`'s `Catalog` impl fills in.

/// The appearance of a toggler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// The background [`Background`] of the toggler.
    pub background: iced_core::Background,
    /// The width of the background border of the toggler.
    pub background_border_width: f32,
    /// The [`iced_core::Color`] of the background border of the toggler.
    pub background_border_color: iced_core::Color,
    /// The foreground [`Background`] of the toggler.
    pub foreground: iced_core::Background,
    /// The width of the foreground border of the toggler.
    pub foreground_border_width: f32,
    /// The [`iced_core::Color`] of the foreground border of the toggler.
    pub foreground_border_color: iced_core::Color,
    /// The border radius of the toggler.
    pub border_radius: iced_core::border::Radius,
    /// the radius of the handle of the toggler
    pub handle_radius: iced_core::border::Radius,
    /// the space between the handle and the border of the toggler
    pub handle_margin: f32,
    /// The ratio of separation between the background and the toggle in relative height.
    pub padding_ratio: f32,
    /// The text [`iced_core::Color`] of the toggler.
    pub text_color: Option<iced_core::Color>,
}

/// The theme catalog of a [`Toggler`].
pub trait Catalog: Sized {
    /// The item class of the [`Catalog`].
    type Class<'a>;

    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;

    /// The [`Style`] of a class with the given status.
    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style;
}

pub fn toggler<'a, Message>(is_checked: bool) -> Toggler<'a, Message> {
    Toggler::new(is_checked)
}
/// A toggler widget.
#[allow(missing_debug_implementations)]
pub struct Toggler<'a, Message> {
    id: Id,
    is_toggled: bool,
    on_toggle: Option<Box<dyn Fn(bool) -> Message + 'a>>,
    label: Option<String>,
    width: Length,
    size: f32,
    text_size: Option<f32>,
    text_line_height: text::LineHeight,
    text_alignment: text::Alignment,
    text_shaping: text::Shaping,
    spacing: f32,
    font: Option<crate::ui::font::Font>,
    duration: Duration,
}

impl<'a, Message> Toggler<'a, Message> {
    /// The default size of a [`Toggler`].
    pub const DEFAULT_SIZE: f32 = 24.0;

    /// Creates a new [`Toggler`].
    ///
    /// It expects:
    ///   * a boolean describing whether the [`Toggler`] is checked or not
    ///   * An optional label for the [`Toggler`]
    ///   * a function that will be called when the [`Toggler`] is toggled. It
    ///     will receive the new state of the [`Toggler`] and must produce a
    ///     `Message`.
    pub fn new(is_toggled: bool) -> Self {
        Toggler {
            id: Id::unique(),
            is_toggled,
            on_toggle: None,
            label: None,
            width: Length::Shrink,
            size: Self::DEFAULT_SIZE,
            text_size: None,
            text_line_height: text::LineHeight::default(),
            text_alignment: text::Alignment::Left,
            text_shaping: text::Shaping::Advanced,
            spacing: 0.0,
            font: None,
            duration: Duration::from_millis(200),
        }
    }

    /// Sets the size of the [`Toggler`].
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = size.into().0;
        self
    }

    /// Sets the width of the [`Toggler`].
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the text size o the [`Toggler`].
    pub fn text_size(mut self, text_size: impl Into<Pixels>) -> Self {
        self.text_size = Some(text_size.into().0);
        self
    }

    /// Sets the text [`LineHeight`] of the [`Toggler`].
    pub fn text_line_height(mut self, line_height: impl Into<text::LineHeight>) -> Self {
        self.text_line_height = line_height.into();
        self
    }

    /// Sets the horizontal alignment of the text of the [`Toggler`]
    pub fn text_alignment(mut self, alignment: text::Alignment) -> Self {
        self.text_alignment = alignment;
        self
    }

    /// Sets the [`text::Shaping`] strategy of the [`Toggler`].
    pub fn text_shaping(mut self, shaping: text::Shaping) -> Self {
        self.text_shaping = shaping;
        self
    }

    /// Sets the spacing between the [`Toggler`] and the text.
    pub fn spacing(mut self, spacing: impl Into<Pixels>) -> Self {
        self.spacing = spacing.into().0;
        self
    }

    /// Sets the [`Font`] of the text of the [`Toggler`]
    ///
    /// [`Font`]: iced::text::Renderer::Font
    pub fn font(mut self, font: impl Into<crate::ui::font::Font>) -> Self {
        self.font = Some(font.into());
        self
    }

    pub fn id(mut self, id: Id) -> Self {
        self.id = id;
        self
    }

    pub fn duration(mut self, dur: Duration) -> Self {
        self.duration = dur;
        self
    }

    pub fn on_toggle(mut self, on_toggle: impl Fn(bool) -> Message + 'a) -> Self {
        self.on_toggle = Some(Box::new(on_toggle));
        self
    }

    pub fn on_toggle_maybe(mut self, on_toggle: Option<impl Fn(bool) -> Message + 'a>) -> Self {
        self.on_toggle = on_toggle.map(|t| Box::new(t) as _);
        self
    }

    /// Sets the label of the [`Button`].
    pub fn label(mut self, label: impl Into<Option<String>>) -> Self {
        self.label = label.into();
        self
    }
}

impl<'a, Message> Widget<Message, crate::ui::Theme, crate::ui::Renderer> for Toggler<'a, Message> {
    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Shrink)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            prev_toggled: self.is_toggled,
            ..State::default()
        })
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &crate::ui::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(self.width);

        next_to_each_other(
            &limits,
            self.spacing,
            |limits| {
                if let Some(label) = self.label.as_deref() {
                    let state = tree.state.downcast_mut::<State>();
                    let node = iced_core::widget::text::layout(
                        &mut state.text,
                        renderer,
                        limits,
                        label,
                        widget::text::Format {
                            width: self.width,
                            height: Length::Shrink,
                            line_height: self.text_line_height,
                            size: self.text_size.map(iced::Pixels),
                            font: self.font,
                            align_x: self.text_alignment,
                            align_y: alignment::Vertical::Top,
                            shaping: self.text_shaping,
                            wrapping: iced_core::text::Wrapping::default(),
                        },
                    );
                    match self.width {
                        Length::Fill => {
                            let size = node.size();
                            layout::Node::with_children(
                                Size::new(limits.width(Length::Fill).max().width, size.height),
                                vec![node],
                            )
                        }
                        _ => node,
                    }
                } else {
                    layout::Node::new(iced_core::Size::ZERO)
                }
            },
            |_| layout::Node::new(Size::new(48., 24.)),
        )
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        _renderer: &crate::ui::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let Some(on_toggle) = self.on_toggle.as_ref() else {
            return;
        };
        let state = tree.state.downcast_mut::<State>();

        // animate external changes
        if state.prev_toggled != self.is_toggled {
            state.anim.changed(self.duration);
            shell.request_redraw();
            state.prev_toggled = self.is_toggled;
        }

        match event {
            // Capturing the press keeps an enclosing button (a settings row)
            // from arming on it; the toggler's release would otherwise be
            // taken from that button, leaving it armed for a later release.
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                if cursor_position.is_over(layout.bounds()) {
                    state.is_pressed = true;
                    shell.capture_event();
                }
            }
            // Only a release that completes this toggler's own press toggles,
            // and only that release is captured; any other belongs to the
            // widget that got its press.
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. }) => {
                if std::mem::take(&mut state.is_pressed) {
                    if cursor_position.is_over(layout.bounds()) {
                        shell.publish((on_toggle)(!self.is_toggled));
                        state.anim.changed(self.duration);
                        state.prev_toggled = !self.is_toggled;
                    }
                    shell.capture_event();
                }
            }
            Event::Touch(touch::Event::FingerLost { .. }) => {
                state.is_pressed = false;
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                state.anim.anim_done(self.duration);
                if state.anim.last_change.is_some() {
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let toggler_bounds = layout
                    .children()
                    .nth(1)
                    .map_or_else(|| layout.bounds(), |l| l.bounds());
                let hovered = cursor_position.is_over(toggler_bounds);
                if state.hovered != hovered {
                    state.hovered = hovered;
                    shell.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _state: &Tree,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &crate::ui::Renderer,
    ) -> mouse::Interaction {
        if cursor_position.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();

        let mut children = layout.children();
        let label_layout = children.next().unwrap();

        if let Some(_label) = &self.label {
            let state: &State = tree.state.downcast_ref();
            iced::widget::text::draw(
                renderer,
                style,
                label_layout.bounds(),
                state.text.raw(),
                iced::widget::text::Style::default(),
                viewport,
            );
        }

        let toggler_layout = children.next().unwrap();
        let bounds = toggler_layout.bounds();

        let is_mouse_over = cursor_position.is_over(bounds);

        // let style = blend_appearances(
        //     theme.style(
        //         &(),
        //         if is_mouse_over {
        //             Status::Hovered { is_toggled: false }
        //         } else {
        //             Status::Active { is_toggled: false }
        //         },
        //     ),
        //     theme.style(
        //         &(),
        //         if is_mouse_over {
        //             Status::Hovered { is_toggled: true }
        //         } else {
        //             Status::Active { is_toggled: true }
        //         },
        //     ),
        //     percent,
        // );

        let style = theme.style(
            &(),
            if is_mouse_over {
                Status::Hovered {
                    is_toggled: self.is_toggled,
                }
            } else {
                Status::Active {
                    is_toggled: self.is_toggled,
                }
            },
        );

        let space = style.handle_margin;

        let toggler_background_bounds = Rectangle {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
        };

        renderer.fill_quad(
            renderer::Quad {
                bounds: toggler_background_bounds,
                border: Border {
                    radius: style.border_radius,
                    ..Default::default()
                },
                ..renderer::Quad::default()
            },
            style.background,
        );
        let mut t = state.anim.t(self.duration, self.is_toggled);

        let toggler_foreground_bounds = Rectangle {
            x: bounds.x
                + anim::slerp(
                    space,
                    bounds.width - space - (bounds.height - (2.0 * space)),
                    t,
                ),

            y: bounds.y + space,
            width: bounds.height - (2.0 * space),
            height: bounds.height - (2.0 * space),
        };

        renderer.fill_quad(
            renderer::Quad {
                bounds: toggler_foreground_bounds,
                border: Border {
                    radius: style.handle_radius,
                    ..Default::default()
                },
                ..renderer::Quad::default()
            },
            style.foreground,
        );
    }
}

impl<'a, Message: 'static> From<Toggler<'a, Message>> for Element<'a, Message> {
    fn from(toggler: Toggler<'a, Message>) -> Element<'a, Message> {
        Element::new(toggler)
    }
}

/// Produces a [`Node`] with two children nodes one right next to each other.
pub fn next_to_each_other(
    limits: &iced_core::layout::Limits,
    spacing: f32,
    left: impl FnOnce(&iced_core::layout::Limits) -> iced_core::layout::Node,
    right: impl FnOnce(&iced_core::layout::Limits) -> iced_core::layout::Node,
) -> iced_core::layout::Node {
    let mut right_node = right(limits);
    let right_size = right_node.size();

    let left_limits = limits.shrink(Size::new(right_size.width + spacing, 0.0));
    let mut left_node = left(&left_limits);
    let left_size = left_node.size();

    let (left_y, right_y) = if left_size.height > right_size.height {
        (0.0, (left_size.height - right_size.height) / 2.0)
    } else {
        ((right_size.height - left_size.height) / 2.0, 0.0)
    };

    left_node = left_node.move_to(iced::Point::new(0.0, left_y));
    right_node = right_node.move_to(iced::Point::new(left_size.width + spacing, right_y));

    iced_core::layout::Node::with_children(
        Size::new(
            left_size.width + spacing + right_size.width,
            left_size.height.max(right_size.height),
        ),
        vec![left_node, right_node],
    )
}

#[derive(Debug, Default)]
pub struct State {
    text: widget::text::State<<crate::ui::Renderer as iced_core::text::Renderer>::Paragraph>,
    anim: anim::State,
    prev_toggled: bool,
    hovered: bool,
    /// Whether the current press started over the toggler.
    is_pressed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Point;
    use iced_core::clipboard;
    use iced_runtime::user_interface::{Cache, UserInterface};

    const WINDOW: Size = Size::new(200.0, 200.0);
    /// Over the toggler's 48×24 track.
    const TOGGLER: Point = Point::new(10.0, 10.0);
    /// Over the enclosing button, beside the toggler.
    const BUTTON: Point = Point::new(100.0, 10.0);
    /// Over nothing.
    const ELSEWHERE: Point = Point::new(150.0, 150.0);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Msg {
        Toggled(bool),
        Button,
    }

    /// A toggler inside a button that also toggles, as a settings row is.
    fn view() -> crate::ui::Element<'static, Msg> {
        let content = iced::widget::row![
            toggler(false).on_toggle(Msg::Toggled),
            crate::ui::widget::space::horizontal()
                .width(Length::Fixed(100.0))
                .height(Length::Fixed(24.0)),
        ];
        crate::ui::widget::button::custom(content)
            .padding(0)
            .on_press(Msg::Button)
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

        fn send(&mut self, event: Event, at: Point) -> Vec<Msg> {
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

        fn press(&mut self, at: Point) -> Vec<Msg> {
            self.send(
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                at,
            )
        }

        fn release(&mut self, at: Point) -> Vec<Msg> {
            self.send(
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                at,
            )
        }
    }

    #[test]
    fn a_click_toggles() {
        let mut ui = Harness::new();

        assert_eq!(ui.press(TOGGLER), []);
        assert_eq!(ui.release(TOGGLER), [Msg::Toggled(true)]);
    }

    #[test]
    fn a_release_without_a_press_does_not_toggle() {
        let mut ui = Harness::new();

        assert_eq!(ui.press(ELSEWHERE), []);
        assert_eq!(ui.release(TOGGLER), []);
    }

    /// A press that leaves the toggler cancels its click, as a button's does.
    #[test]
    fn a_press_released_elsewhere_does_not_toggle() {
        let mut ui = Harness::new();

        assert_eq!(ui.press(TOGGLER), []);
        assert_eq!(ui.release(ELSEWHERE), []);
        assert_eq!(ui.press(ELSEWHERE), []);
        assert_eq!(ui.release(TOGGLER), []);
    }

    /// Clicking the toggler must not leave the enclosing button armed, or a
    /// later release over the row fires the button without its own press.
    #[test]
    fn a_click_on_the_toggler_leaves_the_enclosing_button_unarmed() {
        let mut ui = Harness::new();
        ui.press(TOGGLER);
        ui.release(TOGGLER);

        assert_eq!(ui.press(ELSEWHERE), []);
        assert_eq!(ui.release(BUTTON), []);
    }

    /// A press on the row beside the toggler is the button's click, wherever
    /// over the row it is released.
    #[test]
    fn a_press_beside_the_toggler_released_over_it_is_the_buttons_click() {
        let mut ui = Harness::new();

        assert_eq!(ui.press(BUTTON), []);
        assert_eq!(ui.release(TOGGLER), [Msg::Button]);
    }
}
