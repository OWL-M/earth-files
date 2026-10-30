// Copyright 2023 System76 <info@system76.com>
// Copyright 2019 Héctor Ramón, Iced contributors
// SPDX-License-Identifier: MPL-2.0 AND MIT

//! Vendored from pop-os/libcosmic, src/widget/dropdown/menu/mod.rs

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

mod appearance;
pub use appearance::{Appearance, StyleSheet};

use crate::ui::convert::ToRadius;
use crate::ui::widget::icon;
use crate::ui::widget::wrapper::RcWrapper;
use iced::widget::Container;
use iced_core::event::Event;
use iced_core::layout::{self, Layout};
use iced_core::text::{self, Text};
use iced_core::widget::Tree;
use iced_core::{
    Border, Clipboard, Element, Length, Padding, Pixels, Point, Rectangle, Renderer, Shadow, Shell,
    Size, Vector, Widget, alignment, mouse, overlay, renderer, svg, touch,
};

/// A list of selectable options.
#[must_use]
pub struct Menu<'a, S, Message>
where
    S: AsRef<str>,
    [S]: std::borrow::ToOwned,
{
    state: State,
    options: Cow<'a, [S]>,
    icons: Cow<'a, [icon::Handle]>,
    hovered_option: Arc<Mutex<Option<usize>>>,
    selected_option: Option<usize>,
    on_selected: Box<dyn FnMut(usize) -> Message + 'a>,
    close_on_selected: Option<Message>,
    on_option_hovered: Option<&'a dyn Fn(usize) -> Message>,
    width: f32,
    padding: Padding,
    menu_padding: Option<Padding>,
    option_spacing: f32,
    text_size: Option<f32>,
    text_line_height: text::LineHeight,
    style: (),
    centered_animated: bool,
}

impl<'a, S: AsRef<str>, Message: 'a + std::clone::Clone> Menu<'a, S, Message>
where
    [S]: std::borrow::ToOwned,
{
    /// Creates a new [`Menu`] with the given [`State`], a list of options, and
    /// the message to produced when an option is selected.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: State,
        options: Cow<'a, [S]>,
        icons: Cow<'a, [icon::Handle]>,
        hovered_option: Arc<Mutex<Option<usize>>>,
        selected_option: Option<usize>,
        on_selected: impl FnMut(usize) -> Message + 'a,
        on_option_hovered: Option<&'a dyn Fn(usize) -> Message>,
        close_on_selected: Option<Message>,
    ) -> Self {
        Menu {
            state,
            options,
            icons,
            hovered_option,
            selected_option,
            on_selected: Box::new(on_selected),
            on_option_hovered,
            width: 0.0,
            padding: Padding::ZERO,
            menu_padding: None,
            option_spacing: 0.0,
            text_size: None,
            text_line_height: text::LineHeight::default(),
            style: Default::default(),
            close_on_selected,
            centered_animated: false,
        }
    }

    /// Center the popup horizontally below the picker and reuse the completion-list spring.
    pub fn centered_animated(mut self, enabled: bool) -> Self {
        self.centered_animated = enabled;
        self
    }

    /// Sets the width of the [`Menu`].
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Sets the [`Padding`] of the [`Menu`].
    pub fn padding<P: Into<Padding>>(mut self, padding: P) -> Self {
        self.padding = padding.into();
        self
    }

    /// Sets the inset around the list, independently of each option's padding.
    pub fn menu_padding(mut self, padding: impl Into<Padding>) -> Self {
        self.menu_padding = Some(padding.into());
        self
    }

    /// Sets the gap between options.
    pub fn option_spacing(mut self, spacing: f32) -> Self {
        self.option_spacing = spacing.max(0.0);
        self
    }

    /// Sets the text size of the [`Menu`].
    pub fn text_size(mut self, text_size: impl Into<Pixels>) -> Self {
        self.text_size = Some(text_size.into().0);
        self
    }

    /// Sets the text [`LineHeight`] of the [`Menu`].
    pub fn text_line_height(mut self, line_height: impl Into<text::LineHeight>) -> Self {
        self.text_line_height = line_height.into();
        self
    }

    /// Turns the [`Menu`] into an overlay [`Element`] at the given target
    /// position.
    ///
    /// The `target_height` will be used to display the menu either on top
    /// of the target or under it, depending on the screen position and the
    /// dimensions of the [`Menu`].
    #[must_use]
    pub fn overlay(
        self,
        position: Point,
        target_height: f32,
    ) -> overlay::Element<'a, Message, crate::ui::Theme, crate::ui::Renderer> {
        overlay::Element::new(Box::new(Overlay::new(self, target_height, position)))
    }

    /// Turns the [`Menu`] into a popup [`Element`] at the given target
    /// position.
    ///
    /// The `target_height` will be used to display the menu either on top
    /// of the target or under it, depending on the screen position and the
    /// dimensions of the [`Menu`].
    #[must_use]
    pub fn popup(self, position: Point, target_height: f32) -> crate::ui::Element<'a, Message> {
        Overlay::new(self, target_height, position).into()
    }
}

/// The local state of a [`Menu`].
#[must_use]
#[derive(Debug, Clone)]
pub struct State {
    pub(crate) tree: RcWrapper<Tree>,
    /// Which side a centred menu opens on, and the window height and
    /// picker place it was decided for: decided once as it opens, so it
    /// holds while the spring overshoots.
    side: RcWrapper<Option<Side>>,
}

/// Where a centred menu opens, for what.
#[derive(Clone, Copy, Debug)]
struct Side {
    above: bool,
    window_height: f32,
    picker_y: f32,
}

impl State {
    /// Creates a new [`State`] for a [`Menu`].
    pub fn new() -> Self {
        Self {
            tree: RcWrapper::new(Tree::empty()),
            side: RcWrapper::new(None),
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

struct Overlay<'a, Message> {
    state: RcWrapper<Tree>,
    side: RcWrapper<Option<Side>>,
    container: crate::ui::Element<'a, Message>,
    centered_animated: bool,
    width: f32,
    target_height: f32,
    style: (),
    position: Point,
}

impl<'a, Message: Clone + 'a> Overlay<'a, Message> {
    pub fn new<S: AsRef<str>>(
        menu: Menu<'a, S, Message>,
        target_height: f32,
        position: Point,
    ) -> Self
    where
        [S]: ToOwned,
    {
        let Menu {
            state,
            options,
            icons,
            hovered_option,
            selected_option,
            on_selected,
            on_option_hovered,
            width,
            padding,
            menu_padding,
            option_spacing,
            text_size,
            text_line_height,
            style,
            close_on_selected,
            centered_animated,
        } = menu;

        let outer_padding = menu_padding.unwrap_or(padding);
        let inner_radius_inset = centered_animated.then_some(
            outer_padding
                .top
                .max(outer_padding.right)
                .max(outer_padding.bottom)
                .max(outer_padding.left),
        );
        let container = Container::new(crate::ui::widget::scrollable::vertical(
            Container::new(List {
                options,
                icons,
                hovered_option,
                selected_option,
                on_selected,
                close_on_selected,
                on_option_hovered,
                text_size,
                text_line_height,
                padding,
                spacing: option_spacing,
                inner_radius_inset,
            })
            .padding(outer_padding),
        ))
        .class(crate::ui::theme::style::Container::Dropdown);

        let mut container: crate::ui::Element<'a, Message> = if centered_animated {
            crate::ui::widget::spring_height(container).into()
        } else {
            container.into()
        };
        state
            .tree
            .with_data_mut(|tree| tree.diff(container.as_widget_mut()));

        Self {
            state: state.tree,
            side: state.side,
            container,
            centered_animated,
            width,
            target_height,
            style,
            position,
        }
    }

    fn _layout(&mut self, renderer: &crate::ui::Renderer, bounds: Size) -> layout::Node {
        if self.centered_animated {
            // This mode receives the picker's horizontal center, not its left edge.
            let below = (self.position.y + self.target_height).clamp(0.0, bounds.height);
            let space_below = bounds.height - below;
            let space_above = self.position.y.clamp(0.0, bounds.height);
            let width = self.width.min(bounds.width);
            let mut lay_out = |height: f32| {
                let limits =
                    layout::Limits::new(Size::ZERO, Size::new(bounds.width, height)).width(width);
                self.state.with_data_mut(|tree| {
                    self.container
                        .as_widget_mut()
                        .layout(tree, renderer, &limits)
                })
            };
            // Below, as it normally opens, where it fits or there is no more
            // room above; else above, its bottom on the picker's top. Decided
            // on the first frame, before the spring stretches the list past
            // its own height, and again only if the window or picker moves
            let decided = self.side.with_data(|side| {
                side.filter(|decided| {
                    decided.window_height == bounds.height && decided.picker_y == self.position.y
                })
                .map(|decided| decided.above)
            });
            let above = decided.unwrap_or_else(|| {
                // Measured where it has the most room
                let natural = lay_out(space_below.max(space_above))
                    .children()
                    .first()
                    .map_or(0.0, |list| list.size().height);
                let above = natural > space_below && space_above > space_below;
                self.side.with_data_mut(|side| {
                    *side = Some(Side {
                        above,
                        window_height: bounds.height,
                        picker_y: self.position.y,
                    });
                });
                above
            });
            // Held to the room on its side, overshoot and all
            let node = lay_out(if above { space_above } else { space_below });
            let y = if above {
                (self.position.y - node.size().height).max(0.0)
            } else {
                below
            };
            let x = (self.position.x - node.size().width / 2.0)
                .clamp(0.0, (bounds.width - node.size().width).max(0.0));
            return node.move_to(Point::new(x, y));
        }

        let space_below = bounds.height - (self.position.y + self.target_height);
        let space_above = self.position.y;

        let limits = layout::Limits::new(
            Size::ZERO,
            Size::new(
                bounds.width - self.position.x,
                if space_below > space_above {
                    space_below
                } else {
                    space_above
                },
            ),
        )
        .width(self.width);

        let node = self.state.with_data_mut(|tree| {
            self.container
                .as_widget_mut()
                .layout(tree, renderer, &limits)
        });

        let node_size = node.size();
        node.move_to(if space_below > space_above {
            self.position + Vector::new(0.0, self.target_height)
        } else {
            self.position - Vector::new(0.0, node_size.height)
        })
    }

    fn _update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &crate::ui::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let bounds = layout.bounds();

        self.state.with_data_mut(|tree| {
            self.container.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, &bounds,
            )
        })
    }

    fn _mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &crate::ui::Renderer,
    ) -> mouse::Interaction {
        self.state.with_data(|tree| {
            self.container
                .as_widget()
                .mouse_interaction(tree, layout, cursor, viewport, renderer)
        })
    }

    fn _draw(
        &self,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let appearance = theme.appearance(&self.style);
        let bounds = layout.bounds();

        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: Border {
                    width: appearance.border_width,
                    color: appearance.border_color,
                    radius: if self.centered_animated {
                        theme.cosmic().corner_radii.radius_s.to_radius()
                    } else {
                        appearance.border_radius
                    },
                },
                shadow: Shadow::default(),
                snap: true,
            },
            appearance.background,
        );

        self.state.with_data(|tree| {
            self.container
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, &bounds)
        })
    }
}

impl<'a, Message: Clone + 'a> iced_core::Overlay<Message, crate::ui::Theme, crate::ui::Renderer>
    for Overlay<'a, Message>
{
    fn layout(&mut self, renderer: &crate::ui::Renderer, bounds: Size) -> layout::Node {
        self._layout(renderer, bounds)
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &crate::ui::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        self._update(event, layout, cursor, renderer, clipboard, shell)
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &crate::ui::Renderer,
    ) -> mouse::Interaction {
        self._mouse_interaction(layout, cursor, &layout.bounds(), renderer)
    }

    fn draw(
        &self,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self._draw(renderer, theme, style, layout, cursor);
    }
}

impl<'a, Message: Clone + 'a> iced_core::Widget<Message, crate::ui::Theme, crate::ui::Renderer>
    for Overlay<'a, Message>
{
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.width), Length::Shrink)
    }

    fn layout(
        &mut self,
        _tree: &mut iced_core::widget::Tree,
        renderer: &crate::ui::Renderer,
        limits: &iced_core::layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(self.width);

        self.state.with_data_mut(|tree| {
            self.container
                .as_widget_mut()
                .layout(tree, renderer, &limits)
        })
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &crate::ui::Renderer,
    ) -> mouse::Interaction {
        self._mouse_interaction(layout, cursor, viewport, renderer)
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &crate::ui::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        self._update(event, layout, cursor, renderer, clipboard, shell)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        self._draw(renderer, theme, style, layout, cursor);
    }
}

impl<'a, Message: Clone + 'a> From<Overlay<'a, Message>> for crate::ui::Element<'a, Message> {
    fn from(widget: Overlay<'a, Message>) -> Self {
        Element::new(widget)
    }
}

struct List<'a, S: AsRef<str>, Message>
where
    [S]: std::borrow::ToOwned,
{
    options: Cow<'a, [S]>,
    icons: Cow<'a, [icon::Handle]>,
    hovered_option: Arc<Mutex<Option<usize>>>,
    selected_option: Option<usize>,
    on_selected: Box<dyn FnMut(usize) -> Message + 'a>,
    close_on_selected: Option<Message>,
    on_option_hovered: Option<&'a dyn Fn(usize) -> Message>,
    padding: Padding,
    spacing: f32,
    inner_radius_inset: Option<f32>,
    text_size: Option<f32>,
    text_line_height: text::LineHeight,
}

impl<S: AsRef<str>, Message> List<'_, S, Message>
where
    [S]: std::borrow::ToOwned,
{
    /// The option under `cursor`, if it is over one.
    fn option_at(
        &self,
        cursor: mouse::Cursor,
        layout: Layout<'_>,
        renderer: &crate::ui::Renderer,
    ) -> Option<usize> {
        let position = cursor.position_in(layout.bounds())?;
        let text_size = self
            .text_size
            .unwrap_or_else(|| text::Renderer::default_size(renderer).0);
        let option_height =
            f32::from(self.text_line_height.to_absolute(Pixels(text_size))) + self.padding.y();
        let pitch = option_height + self.spacing;
        let index = (position.y / pitch) as usize;
        (index < self.options.len() && position.y - index as f32 * pitch < option_height)
            .then_some(index)
    }
}

impl<S: AsRef<str>, Message> Widget<Message, crate::ui::Theme, crate::ui::Renderer>
    for List<'_, S, Message>
where
    [S]: std::borrow::ToOwned,
    Message: Clone,
{
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        renderer: &crate::ui::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        use std::f32;

        let limits = limits.width(Length::Fill).height(Length::Shrink);
        let text_size = self
            .text_size
            .unwrap_or_else(|| text::Renderer::default_size(renderer).0);

        let text_line_height = self.text_line_height.to_absolute(Pixels(text_size));

        let size = {
            let intrinsic = Size::new(
                0.0,
                (f32::from(text_line_height) + self.padding.y()) * self.options.len() as f32
                    + self.spacing * self.options.len().saturating_sub(1) as f32,
            );

            limits.resolve(Length::Fill, Length::Shrink, intrinsic)
        };

        layout::Node::new(size)
    }

    fn update(
        &mut self,
        _state: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &crate::ui::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // Where the pointer is now, not where it last moved to: the
                // list scrolls under a pointer that stays put, and the row
                // hovered before the scroll is no longer under it
                if let Some(index) = self.option_at(cursor, layout, renderer) {
                    *self.hovered_option.lock().unwrap() = Some(index);
                    shell.publish((self.on_selected)(index));
                    if let Some(close_on_selected) = self.close_on_selected.as_ref() {
                        shell.publish(close_on_selected.clone());
                    }
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let hovered = self.option_at(cursor, layout, renderer);
                let mut hovered_guard = self.hovered_option.lock().unwrap();
                if *hovered_guard != hovered {
                    *hovered_guard = hovered;
                    shell.request_redraw();
                    if let (Some(index), Some(on_option_hovered)) =
                        (hovered, self.on_option_hovered)
                    {
                        shell.publish(on_option_hovered(index));
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                let mut hovered_guard = self.hovered_option.lock().unwrap();

                if hovered_guard.is_some() {
                    *hovered_guard = None;
                    shell.request_redraw();
                }
            }
            Event::Touch(touch::Event::FingerPressed { .. }) => {
                if let Some(index) = self.option_at(cursor, layout, renderer) {
                    *self.hovered_option.lock().unwrap() = Some(index);
                    shell.publish((self.on_selected)(index));
                    if let Some(close_on_selected) = self.close_on_selected.as_ref() {
                        shell.publish(close_on_selected.clone());
                    }
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _state: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        renderer: &crate::ui::Renderer,
    ) -> mouse::Interaction {
        let is_mouse_over = self.option_at(cursor, layout, renderer).is_some();

        if is_mouse_over {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        state: &Tree,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let appearance = theme.appearance(&());
        // Use the popup's outer corners and actual inset, as in the sidebar.
        let option_radius = self
            .inner_radius_inset
            .map_or(appearance.border_radius, |inset| {
                theme
                    .cosmic()
                    .corner_radii
                    .radius_s
                    .map(|radius| {
                        let radius = radius.max(0.0);
                        (radius - inset).max(inset).min(radius)
                    })
                    .to_radius()
            });
        let bounds = layout.bounds();

        let text_size = self
            .text_size
            .unwrap_or_else(|| text::Renderer::default_size(renderer).0);
        let option_height =
            f32::from(self.text_line_height.to_absolute(Pixels(text_size))) + self.padding.y();

        let offset = viewport.y - bounds.y;
        let pitch = option_height + self.spacing;
        let start = ((offset / pitch) as usize).min(self.options.len());
        let end = ((offset + viewport.height) / pitch).ceil() as usize;

        let visible_options = &self.options[start..end.min(self.options.len())];

        for (i, option) in visible_options.iter().enumerate() {
            let i = start + i;

            let bounds = Rectangle {
                x: bounds.x,
                y: pitch.mul_add(i as f32, bounds.y),
                width: bounds.width,
                height: option_height,
            };

            let hovered_guard = self.hovered_option.lock().unwrap();

            let (color, font) = if self.selected_option == Some(i) {
                let item_x = bounds.x + appearance.border_width;
                let item_width = appearance.border_width.mul_add(-2.0, bounds.width);

                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            x: item_x,
                            width: item_width,
                            ..bounds
                        },
                        border: Border {
                            radius: option_radius,
                            ..Default::default()
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    appearance.selected_background,
                );

                let svg_handle =
                    iced_core::Svg::new(crate::ui::widget::common::object_select().clone())
                        .color(appearance.selected_text_color);

                let bounds = Rectangle {
                    x: item_x + item_width - 16.0 - 8.0,
                    y: bounds.y + (bounds.height / 2.0 - 8.0),
                    width: 16.0,
                    height: 16.0,
                };
                svg::Renderer::draw_svg(renderer, svg_handle, bounds, bounds);

                (appearance.selected_text_color, crate::ui::font::semibold())
            } else if *hovered_guard == Some(i) {
                let item_x = bounds.x + appearance.border_width;
                let item_width = appearance.border_width.mul_add(-2.0, bounds.width);

                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            x: item_x,
                            width: item_width,
                            ..bounds
                        },
                        border: Border {
                            radius: option_radius,
                            ..Default::default()
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    appearance.hovered_background,
                );

                (appearance.hovered_text_color, crate::ui::font::default())
            } else {
                (appearance.text_color, crate::ui::font::default())
            };

            let mut bounds = Rectangle {
                x: bounds.x + self.padding.left,
                y: bounds.center_y(),
                width: f32::INFINITY,
                ..bounds
            };

            if let Some(handle) = self.icons.get(i) {
                let icon_bounds = Rectangle {
                    x: bounds.x,
                    y: bounds.y + 8.0 - (bounds.height / 2.0),
                    width: 20.0,
                    height: 20.0,
                };

                bounds.x += 24.0;
                icon::draw(renderer, handle, icon_bounds);
            }

            text::Renderer::fill_text(
                renderer,
                Text {
                    content: option.as_ref().to_string(),
                    bounds: bounds.size(),
                    size: Pixels(text_size),
                    line_height: self.text_line_height,
                    font,
                    align_x: text::Alignment::Left,
                    align_y: alignment::Vertical::Center,
                    shaping: text::Shaping::Advanced,
                    wrapping: text::Wrapping::default(),
                },
                bounds.position(),
                color,
                *viewport,
            );
        }
    }
}

impl<'a, S: AsRef<str>, Message: 'a> From<List<'a, S, Message>>
    for Element<'a, Message, crate::ui::Theme, crate::ui::Renderer>
where
    [S]: std::borrow::ToOwned,
    Message: Clone,
{
    fn from(list: List<'a, S, Message>) -> Self {
        Element::new(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::{clipboard, window};
    use std::time::{Duration, Instant};

    fn theme_menu(selected: usize, position: Point) -> Overlay<'static, usize> {
        let menu = Menu::new(
            State::new(),
            Cow::Owned(vec!["System", "Dark", "Light"]),
            Cow::Borrowed(&[]),
            Arc::new(Mutex::new(Some(selected))),
            Some(selected),
            |index| index,
            None,
            None,
        )
        .width(120.0)
        .padding([6, 8])
        .menu_padding(6)
        .option_spacing(3.0)
        .text_size(14)
        .centered_animated(true);
        Overlay::new(menu, 36.0, position)
    }

    #[test]
    fn theme_picker_unrolls_below_and_horizontally_centered() {
        let renderer = iced_texture_cache::testing::headless_tiny_skia();
        let viewport = Size::new(400.0, 400.0);
        for selected in 0..3 {
            let mut menu = theme_menu(selected, Point::new(100.0, 150.0));
            let first = menu._layout(&renderer, viewport);
            assert_eq!(first.size().height, 0.0, "opens collapsed");
            let mut messages = Vec::new();
            // An unrevealed row must not accept a press during opening.
            menu._update(
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Layout::new(&first),
                mouse::Cursor::Available(Point::new(100.0, 210.0)),
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
            );
            assert!(messages.is_empty());
            let now = Instant::now();
            let mut saw_partial = false;
            for frame in 0..120 {
                let node = menu._layout(&renderer, viewport);
                let natural = node.children()[0].size().height;
                saw_partial |= node.size().height > 0.0 && node.size().height < natural;
                menu._update(
                    &Event::Window(window::Event::RedrawRequested(
                        now + Duration::from_millis(frame * 16),
                    )),
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                );
            }
            let node = menu._layout(&renderer, viewport);
            assert!(saw_partial, "must animate through intermediate heights");
            let row_height = 14.0 * 1.3 + 12.0;
            let center =
                node.bounds().y + 6.0 + selected as f32 * (row_height + 3.0) + row_height / 2.0;
            assert!((node.size().height - (12.0 + 3.0 * row_height + 6.0)).abs() < 0.01);
            // Gaps are empty space, not part of either adjacent option.
            for gap in 0..2 {
                let y = node.bounds().y + 6.0 + gap as f32 * (row_height + 3.0) + row_height + 1.5;
                menu._update(
                    &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Layout::new(&node),
                    mouse::Cursor::Available(Point::new(100.0, y)),
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                );
                assert!(messages.is_empty());
            }
            assert!((node.bounds().center_x() - 100.0).abs() < 0.01);
            assert!((node.bounds().y - 186.0).abs() < 0.01);
            assert!((node.size().height - node.children()[0].size().height).abs() < 0.01);
            menu._update(
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Layout::new(&node),
                mouse::Cursor::Available(Point::new(100.0, center)),
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
            );
            assert_eq!(messages, vec![selected]);
        }
    }

    /// Lays the menu out frame by frame until its spring has settled.
    fn settle(
        menu: &mut Overlay<'static, usize>,
        renderer: &crate::ui::Renderer,
        viewport: Size,
    ) -> layout::Node {
        let now = Instant::now();
        let mut messages = Vec::new();
        for frame in 0..120 {
            let node = menu._layout(renderer, viewport);
            menu._update(
                &Event::Window(window::Event::RedrawRequested(
                    now + Duration::from_millis(frame * 16),
                )),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
            );
        }
        menu._layout(renderer, viewport)
    }

    /// The side it opens on holds through the whole opening: an overshoot
    /// stretches the list for a few frames, which must not send it above
    /// and back, nor past the window's edge
    #[test]
    fn centered_theme_picker_keeps_its_side_while_it_springs_open() {
        let renderer = iced_texture_cache::testing::headless_tiny_skia();
        let viewport = Size::new(400.0, 400.0);
        let mut menu = theme_menu(1, Point::new(200.0, 254.0));
        let now = Instant::now();
        let mut messages = Vec::new();
        for frame in 0..120 {
            let node = menu._layout(&renderer, viewport);
            let bounds = node.bounds();
            assert!((bounds.y - 290.0).abs() < 0.01, "frame {frame}: {bounds:?}");
            // Its overshoot too stays in the window
            assert!(
                bounds.y + bounds.height <= viewport.height + 0.01,
                "frame {frame}: {bounds:?}"
            );
            menu._update(
                &Event::Window(window::Event::RedrawRequested(
                    now + Duration::from_millis(frame * 16),
                )),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
            );
        }
    }

    /// With no room below the picker, the menu opens above it instead,
    /// whole, inside the window, and its options take presses; with room
    /// below it still opens below
    #[test]
    fn centered_theme_picker_opens_above_when_there_is_no_room_below() {
        let renderer = iced_texture_cache::testing::headless_tiny_skia();
        let viewport = Size::new(400.0, 400.0);
        let row_height = 14.0 * 1.3 + 12.0;
        let natural = 12.0 + 3.0 * row_height + 6.0;

        for picker in [Point::new(200.0, 350.0), Point::new(390.0, 364.0)] {
            let mut menu = theme_menu(1, picker);
            let node = settle(&mut menu, &renderer, viewport);
            let bounds = node.bounds();
            assert!((bounds.height - natural).abs() < 0.01, "whole: {bounds:?}");
            assert!(
                bounds.y + bounds.height <= picker.y + 0.01,
                "above the picker"
            );
            assert!(bounds.x >= 0.0 && bounds.x + bounds.width <= viewport.width);
            assert!(bounds.y >= 0.0);

            let mut messages = Vec::new();
            let option = bounds.y + 6.0 + 2.0 * (row_height + 3.0) + row_height / 2.0;
            menu._update(
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Layout::new(&node),
                mouse::Cursor::Available(Point::new(bounds.center_x(), option)),
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
            );
            assert_eq!(messages, vec![2]);
        }

        let mut menu = theme_menu(1, Point::new(200.0, 150.0));
        let bounds = settle(&mut menu, &renderer, viewport).bounds();
        assert!((bounds.y - 186.0).abs() < 0.01, "below: {bounds:?}");
    }
}
