// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Create choices using radio buttons.
//!
//! Vendored from pop-os/libcosmic, src/widget/radio.rs
use crate::ui::Theme;
use crate::ui::theme;
use iced::border;
use iced_core::event::Event;
use iced_core::widget::tree::{self, Tree};
use iced_core::{
    Border, Clipboard, Element, Layout, Length, Pixels, Rectangle, Shell, Size, Vector, Widget,
    layout, mouse, overlay, renderer, touch,
};

use iced::widget::radio as iced_radio;
pub use iced::widget::radio::Catalog;

pub fn radio<'a, Message: Clone, V, F>(
    label: impl Into<Element<'a, Message, Theme, crate::ui::Renderer>>,
    value: V,
    selected: Option<V>,
    f: F,
) -> Radio<'a, Message, crate::ui::Renderer>
where
    V: Eq + Copy,
    F: FnOnce(V) -> Message,
{
    Radio::new(label, value, selected, f)
}

/// A circular button representing a choice.
///
/// # Example
///
/// Marked `ignore`: the `text::heading(..) -> Into<Element>` inference in it
/// does not resolve. The widget itself compiles and is exercised by the crate.
/// ```ignore
/// # type Radio<'a, Message> =
/// #     earth_files::ui::widget::Radio<'a, Message>;
/// #
/// # use earth_files::ui::widget::text;
/// # use iced::widget::column;
/// #[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// pub enum Choice {
///     A,
///     B,
///     C,
///     All,
/// }
///
/// #[derive(Debug, Clone, Copy)]
/// pub enum Message {
///     RadioSelected(Choice),
/// }
///
/// let selected_choice = Some(Choice::A);
///
/// let a = Radio::new(
///     text::heading("A"),
///     Choice::A,
///     selected_choice,
///     Message::RadioSelected,
/// );
///
/// let b = Radio::new(
///     text::heading("B"),
///     Choice::B,
///     selected_choice,
///     Message::RadioSelected,
/// );
///
/// let c = Radio::new(
///     text::heading("C"),
///     Choice::C,
///     selected_choice,
///     Message::RadioSelected,
/// );
///
/// let all = Radio::new(
///     column![
///         text::heading("All"),
///         text::body("A, B and C"),
///     ],
///     Choice::All,
///     selected_choice,
///     Message::RadioSelected
/// );
///
/// let content = column![a, b, c, all];
/// ```
#[allow(missing_debug_implementations)]
pub struct Radio<'a, Message, Renderer = crate::ui::Renderer>
where
    Renderer: iced_core::Renderer,
{
    is_selected: bool,
    on_click: Message,
    label: Option<Element<'a, Message, Theme, Renderer>>,
    width: Length,
    size: f32,
    spacing: f32,
}

impl<'a, Message, Renderer> Radio<'a, Message, Renderer>
where
    Message: Clone,
    Renderer: iced_core::Renderer,
{
    /// The default size of a [`Radio`] button.
    pub const DEFAULT_SIZE: f32 = 16.0;

    /// Creates a new [`Radio`] button.
    ///
    /// It expects:
    ///   * the value related to the [`Radio`] button
    ///   * the label of the [`Radio`] button
    ///   * the current selected value
    ///   * a function that will be called when the [`Radio`] is selected. It
    ///     receives the value of the radio and must produce a `Message`.
    pub fn new<T, F, V>(label: T, value: V, selected: Option<V>, f: F) -> Self
    where
        V: Eq + Copy,
        F: FnOnce(V) -> Message,
        T: Into<Element<'a, Message, Theme, Renderer>>,
    {
        Radio {
            is_selected: Some(value) == selected,
            on_click: f(value),
            label: Some(label.into()),
            width: Length::Shrink,
            size: Self::DEFAULT_SIZE,
            spacing: theme::spacing().space_xs as f32,
        }
    }

    /// Creates a new [`Radio`] button without a label.
    ///
    /// This is intended for internal use with the settings item builder,
    /// where the label comes from the settings item title instead.
    pub(crate) fn new_no_label<V, F>(value: V, selected: Option<V>, f: F) -> Self
    where
        V: Eq + Copy,
        F: FnOnce(V) -> Message,
    {
        Radio {
            is_selected: Some(value) == selected,
            on_click: f(value),
            label: None,
            width: Length::Shrink,
            size: Self::DEFAULT_SIZE,
            spacing: theme::spacing().space_xs as f32,
        }
    }

    #[must_use]
    /// Sets the size of the [`Radio`] button.
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = size.into().0;
        self
    }

    #[must_use]
    /// Sets the width of the [`Radio`] button.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    #[must_use]
    /// Sets the spacing between the [`Radio`] button and the text.
    pub fn spacing(mut self, spacing: impl Into<Pixels>) -> Self {
        self.spacing = spacing.into().0;
        self
    }
}

/// Whether a press landed on the radio and has not been released yet.
#[derive(Default)]
struct State {
    is_pressed: bool,
}

impl<Message, Renderer> Widget<Message, Theme, Renderer> for Radio<'_, Message, Renderer>
where
    Message: Clone,
    Renderer: iced_core::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        if let Some(label) = &self.label {
            vec![Tree::new(label)]
        } else {
            vec![]
        }
    }

    fn diff(&self, tree: &mut Tree) {
        if let Some(label) = &self.label {
            tree.diff_children(std::slice::from_ref(label));
        }
    }
    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: Length::Shrink,
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        if let Some(label) = &mut self.label {
            layout::next_to_each_other(
                &limits.width(self.width),
                self.spacing,
                |_| layout::Node::new(Size::new(self.size, self.size)),
                |limits| {
                    label
                        .as_widget_mut()
                        .layout(&mut tree.children[0], renderer, limits)
                },
            )
        } else {
            layout::Node::new(Size::new(self.size, self.size))
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced_core::widget::Operation<()>,
    ) {
        if let Some(label) = &mut self.label {
            label.as_widget_mut().operate(
                &mut tree.children[0],
                layout.children().nth(1).unwrap(),
                renderer,
                operation,
            );
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
        if let Some(label) = &mut self.label {
            label.as_widget_mut().update(
                &mut tree.children[0],
                event,
                layout.children().nth(1).unwrap(),
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }

        if !shell.is_event_captured() {
            let state = tree.state.downcast_mut::<State>();
            match event {
                // Capture the press so an enclosing button (the settings item
                // row) does not arm too: it would never see the release this
                // radio captures, and fire on some later stray one.
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                | Event::Touch(touch::Event::FingerPressed { .. })
                    if cursor.is_over(layout.bounds()) =>
                {
                    state.is_pressed = true;
                    shell.capture_event();
                }
                // Only a release completing our own press selects; one ending
                // a drag begun elsewhere belongs to whoever took that press.
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                | Event::Touch(touch::Event::FingerLifted { .. })
                    if state.is_pressed =>
                {
                    state.is_pressed = false;
                    if cursor.is_over(layout.bounds()) {
                        shell.publish(self.on_click.clone());
                    }
                    shell.capture_event();
                }
                Event::Mouse(mouse::Event::CursorLeft)
                | Event::Touch(touch::Event::FingerLost { .. }) => {
                    state.is_pressed = false;
                }
                _ => {}
            }
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
        let interaction = if let Some(label) = &self.label {
            label.as_widget().mouse_interaction(
                &tree.children[0],
                layout.children().nth(1).unwrap(),
                cursor,
                viewport,
                renderer,
            )
        } else {
            mouse::Interaction::default()
        };

        if interaction == mouse::Interaction::default() {
            if cursor.is_over(layout.bounds()) {
                mouse::Interaction::Pointer
            } else {
                mouse::Interaction::default()
            }
        } else {
            interaction
        }
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
        let is_mouse_over = cursor.is_over(layout.bounds());

        let custom_style = if is_mouse_over {
            theme.style(
                &(),
                iced_radio::Status::Hovered {
                    is_selected: self.is_selected,
                },
            )
        } else {
            theme.style(
                &(),
                iced_radio::Status::Active {
                    is_selected: self.is_selected,
                },
            )
        };

        let (dot_bounds, label_layout) = if self.label.is_some() {
            let mut children = layout.children();
            let dot_bounds = children.next().unwrap().bounds();
            (dot_bounds, children.next())
        } else {
            (layout.bounds(), None)
        };

        {
            let size = dot_bounds.width;
            let dot_size = 6.0;

            renderer.fill_quad(
                renderer::Quad {
                    bounds: dot_bounds,
                    border: Border {
                        radius: (size / 2.0).into(),
                        width: custom_style.border_width,
                        color: custom_style.border_color,
                    },
                    ..renderer::Quad::default()
                },
                custom_style.background,
            );

            if self.is_selected {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            x: dot_bounds.x + (size - dot_size) / 2.0,
                            y: dot_bounds.y + (size - dot_size) / 2.0,
                            width: dot_size,
                            height: dot_size,
                        },
                        border: border::rounded(dot_size / 2.0),
                        ..renderer::Quad::default()
                    },
                    custom_style.dot_color,
                );
            }
        }

        if let (Some(label), Some(label_layout)) = (&self.label, label_layout) {
            label.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                label_layout,
                cursor,
                viewport,
            );
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.label.as_mut()?.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().nth(1).unwrap(),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Renderer> From<Radio<'a, Message, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a + Clone,
    Renderer: 'a + iced_core::Renderer,
{
    fn from(radio: Radio<'a, Message, Renderer>) -> Element<'a, Message, Theme, Renderer> {
        Element::new(radio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::{Point, clipboard};
    use iced_runtime::user_interface::{Cache, UserInterface};

    use crate::ui::{Renderer, widget::space};

    const WINDOW: Size = Size::new(200.0, 100.0);
    const RADIO: u8 = 1;
    const BUTTON: u8 = 2;

    /// A 16 px radio at the origin, followed by 100 px of plain space; with
    /// `in_button`, both sit in an unpadded button — the settings item shape.
    fn view(in_button: bool) -> Element<'static, u8, Theme, Renderer> {
        let row = iced::widget::row![
            Radio::new_no_label(RADIO, None, |v| v),
            space::horizontal().width(Length::Fixed(100.0)),
        ];
        if in_button {
            crate::ui::widget::button::custom(row)
                .padding(0)
                .on_press(BUTTON)
                .into()
        } else {
            row.into()
        }
    }

    struct Harness {
        in_button: bool,
        renderer: Renderer,
        cache: Option<Cache>,
        cursor: mouse::Cursor,
    }

    impl Harness {
        fn new(in_button: bool) -> Self {
            Self {
                in_button,
                renderer: iced_texture_cache::testing::headless_tiny_skia(),
                cache: Some(Cache::default()),
                cursor: mouse::Cursor::Unavailable,
            }
        }

        fn send(&mut self, event: Event) -> Vec<u8> {
            let mut messages = Vec::new();
            let cache = self.cache.take().unwrap();
            let mut ui =
                UserInterface::build(view(self.in_button), WINDOW, cache, &mut self.renderer);
            let _ = ui.update(
                &[event],
                self.cursor,
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            self.cache = Some(ui.into_cache());
            messages
        }

        fn move_to(&mut self, x: f32, y: f32) -> Vec<u8> {
            let position = Point::new(x, y);
            self.cursor = mouse::Cursor::Available(position);
            self.send(Event::Mouse(mouse::Event::CursorMoved { position }))
        }

        fn press(&mut self) -> Vec<u8> {
            self.send(Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            )))
        }

        fn release(&mut self) -> Vec<u8> {
            self.send(Event::Mouse(mouse::Event::ButtonReleased(
                mouse::Button::Left,
            )))
        }
    }

    #[test]
    fn release_without_press_does_not_select() {
        let mut h = Harness::new(false);
        // A drag that started elsewhere, released over the radio.
        h.move_to(8.0, 8.0);
        assert_eq!(h.release(), Vec::<u8>::new());
    }

    #[test]
    fn click_selects() {
        let mut h = Harness::new(false);
        h.move_to(8.0, 8.0);
        assert_eq!(h.press(), Vec::<u8>::new());
        assert_eq!(h.release(), vec![RADIO]);
    }

    #[test]
    fn press_dragged_off_does_not_select() {
        let mut h = Harness::new(false);
        h.move_to(8.0, 8.0);
        h.press();
        h.move_to(60.0, 8.0);
        assert_eq!(h.release(), Vec::<u8>::new());
    }

    #[test]
    fn click_in_button_leaves_button_unarmed() {
        let mut h = Harness::new(true);
        h.move_to(8.0, 8.0);
        h.press();
        assert_eq!(h.release(), vec![RADIO]);
        // Were the button still armed by that press, a later bare release
        // over it would fire it.
        h.move_to(60.0, 8.0);
        assert_eq!(h.release(), Vec::<u8>::new());
    }
}
