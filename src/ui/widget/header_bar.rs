// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! The window header bar.
//!
//! Vendored from pop-os/libcosmic, src/widget/header_bar.rs
//!
//! The title is ellipsized with the standalone
//! [`crate::ui::widget::ellipsize::Ellipsize`] widget, given the same size,
//! line height and font as `text::heading`.

use crate::ui::Apply;
use crate::ui::convert::{PushMaybe, ToPadding, ToPixels};
use crate::ui::theme::{Density, Spacing};
use crate::ui::widget::Row;
use crate::ui::widget::ellipsize::{Ellipsize, Mode as EllipsizeMode};
use crate::ui::{Element, theme, widget};
use derive_setters::Setters;
use iced_core::widget::tree;
use iced_core::{Length, Size, Vector, Widget, layout, text};
use std::borrow::Cow;

#[must_use]
pub fn header_bar<'a, Message>() -> HeaderBar<'a, Message> {
    HeaderBar {
        title: Cow::Borrowed(""),
        on_close: None,
        on_drag: None,
        on_maximize: None,
        on_minimize: None,
        on_right_click: None,
        start: Vec::new(),
        center: Vec::new(),
        end: Vec::new(),
        density: None,
        focused: false,
        maximized: false,
        sharp_corners: false,
        is_ssd: false,
        on_double_click: None,
    }
}

#[derive(Setters)]
pub struct HeaderBar<'a, Message> {
    /// Defines the title of the window
    #[setters(skip)]
    title: Cow<'a, str>,

    /// A message emitted when the close button is pressed.
    #[setters(strip_option)]
    on_close: Option<Message>,

    /// A message emitted when dragged.
    #[setters(strip_option)]
    on_drag: Option<Message>,

    /// A message emitted when the maximize button is pressed.
    #[setters(strip_option)]
    on_maximize: Option<Message>,

    /// A message emitted when the minimize button is pressed.
    #[setters(strip_option)]
    on_minimize: Option<Message>,

    /// A message emitted when the header is double clicked,
    /// usually used to maximize the window.
    #[setters(strip_option)]
    on_double_click: Option<Message>,

    /// A message emitted when the header is right clicked.
    #[setters(strip_option)]
    on_right_click: Option<Message>,

    /// Elements packed at the start of the headerbar.
    #[setters(skip)]
    start: Vec<Element<'a, Message>>,

    /// Elements packed in the center of the headerbar.
    #[setters(skip)]
    center: Vec<Element<'a, Message>>,

    /// Elements packed at the end of the headerbar.
    #[setters(skip)]
    end: Vec<Element<'a, Message>>,

    /// Controls the density of the headerbar.
    #[setters(strip_option)]
    density: Option<Density>,

    /// Focused state of the window
    focused: bool,

    /// Maximized state of the window
    maximized: bool,

    /// Whether the corners of the window should be sharp
    sharp_corners: bool,

    /// HeaderBar used for server-side decorations
    is_ssd: bool,
}

impl<'a, Message: Clone + 'static> HeaderBar<'a, Message> {
    /// Defines the title of the window
    #[must_use]
    pub fn title(mut self, title: impl Into<Cow<'a, str>> + 'a) -> Self {
        self.title = title.into();
        self
    }

    /// Pushes an element to the start region.
    #[must_use]
    pub fn start(mut self, widget: impl Into<Element<'a, Message>> + 'a) -> Self {
        self.start.push(widget.into());
        self
    }

    /// Pushes an element to the center region.
    #[must_use]
    pub fn center(mut self, widget: impl Into<Element<'a, Message>> + 'a) -> Self {
        self.center.push(widget.into());
        self
    }

    /// Pushes an element to the end region.
    #[must_use]
    pub fn end(mut self, widget: impl Into<Element<'a, Message>> + 'a) -> Self {
        self.end.push(widget.into());
        self
    }
}

pub struct HeaderBarWidget<'a, Message> {
    start: Element<'a, Message>,
    /// The title after the start, when the centre holds something else.
    title: Option<Element<'a, Message>>,
    center: Option<Element<'a, Message>>,
    end: Element<'a, Message>,
}

impl<'a, Message> HeaderBarWidget<'a, Message> {
    pub fn new(
        start: Element<'a, Message>,
        title: Option<Element<'a, Message>>,
        center: Option<Element<'a, Message>>,
        end: Element<'a, Message>,
    ) -> Self {
        Self {
            start,
            title,
            center,
            end,
        }
    }

    /// Start, end, then the centre and the title if there are any: the
    /// order of the tree's children and the layout's nodes.
    fn elems(&self) -> impl Iterator<Item = &Element<'a, Message>> {
        [
            Some(&self.start),
            Some(&self.end),
            self.center.as_ref(),
            self.title.as_ref(),
        ]
        .into_iter()
        .flatten()
    }

    fn elems_mut(&mut self) -> impl Iterator<Item = &mut Element<'a, Message>> {
        [
            Some(&mut self.start),
            Some(&mut self.end),
            self.center.as_mut(),
            self.title.as_mut(),
        ]
        .into_iter()
        .flatten()
    }
}

impl<'a, Message: Clone + 'static> Widget<Message, crate::ui::Theme, crate::ui::Renderer>
    for HeaderBarWidget<'a, Message>
{
    fn diff(&self, tree: &mut tree::Tree) {
        tree.diff_children(&self.elems().collect::<Vec<_>>());
    }

    fn children(&self) -> Vec<tree::Tree> {
        self.elems().map(tree::Tree::new).collect()
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: Length::Fill,
            height: Length::Shrink,
        }
    }

    fn layout(
        &mut self,
        tree: &mut tree::Tree,
        renderer: &crate::ui::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let width = limits.max().width;
        let height = limits.max().height;
        let gap = 8.0;

        let end_node =
            self.end
                .as_widget_mut()
                .layout(&mut tree.children[1], renderer, &limits.loose());
        let end_width = end_node.size().width;

        // The centre's natural width is kept for it: the menu bar in the
        // start folds once the whole path would not fit beside it (see
        // `responsive_container::with_reserve`), and a title after the start
        // shortens first, before the crumbs do. Measured on its own, this
        // also keeps the centre from jittering as it shortens.
        let natural_width = self.center.as_mut().map_or(0.0, |center| {
            center
                .as_widget_mut()
                .layout(&mut tree.children[2], renderer, &limits.loose())
                .size()
                .width
        });
        let reserve = if self.center.is_some() {
            natural_width + gap
        } else {
            0.0
        };

        // The start is laid out in all the room the end leaves, so what it
        // needs is never squeezed
        let start_available = (width - end_width - gap).max(0.0);
        let start_node = crate::ui::widget::responsive_container::with_reserve(reserve, || {
            self.start.as_widget_mut().layout(
                &mut tree.children[0],
                renderer,
                &layout::Limits::new(Size::ZERO, Size::new(start_available, height)),
            )
        });
        let start_width = start_node.size().width;

        // The title takes what is left once the centre's width is kept
        let title_index = 2 + usize::from(self.center.is_some());
        let title_node = self.title.as_mut().map(|title| {
            let room = (width - end_width - gap - start_width - gap - reserve).max(0.0);
            title.as_widget_mut().layout(
                &mut tree.children[title_index],
                renderer,
                &layout::Limits::new(Size::ZERO, Size::new(room, height)),
            )
        });
        let title_end = title_node
            .as_ref()
            .filter(|node| node.size().width > 0.0)
            .map_or(start_width, |node| start_width + gap + node.size().width);

        let vcenter = |node: layout::Node, x: f32| -> layout::Node {
            let dy = ((height - node.size().height) / 2.0).max(0.0);
            node.translate(Vector::new(x, dy))
        };

        let mut child_nodes = Vec::with_capacity(4);
        child_nodes.push(vcenter(start_node, 0.0));
        child_nodes.push(vcenter(end_node, width - end_width));

        if let Some(center) = &mut self.center {
            let slot_start = title_end + gap;
            let slot_end = (width - end_width - gap).max(slot_start);
            let slot_width = slot_end - slot_start;

            let node = center.as_widget_mut().layout(
                &mut tree.children[2],
                renderer,
                &layout::Limits::new(Size::ZERO, Size::new(slot_width, height)),
            );

            let ideal_x = (width - natural_width) / 2.0;
            let max_x = (width - end_width - gap - natural_width).max(slot_start);
            let center_x = ideal_x.clamp(slot_start, max_x);

            child_nodes.push(vcenter(node, center_x));
        }
        if let Some(node) = title_node {
            child_nodes.push(vcenter(node, start_width + gap));
        }

        layout::Node::with_children(Size::new(width, height), child_nodes)
    }

    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &iced_core::renderer::Style,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        viewport: &iced_core::Rectangle,
    ) {
        self.elems()
            .zip(&tree.children)
            .zip(layout.children())
            .for_each(|((e, s), l)| {
                e.as_widget()
                    .draw(s, renderer, theme, style, l, cursor, viewport);
            });
    }

    fn update(
        &mut self,
        state: &mut tree::Tree,
        event: &iced_core::Event,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        renderer: &crate::ui::Renderer,
        clipboard: &mut dyn iced_core::Clipboard,
        shell: &mut iced_core::Shell<'_, Message>,
        viewport: &iced_core::Rectangle,
    ) {
        self.elems_mut()
            .zip(&mut state.children)
            .zip(layout.children())
            .for_each(|((e, s), l)| {
                e.as_widget_mut()
                    .update(s, event, l, cursor, renderer, clipboard, shell, viewport);
            });
    }

    fn mouse_interaction(
        &self,
        state: &tree::Tree,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        viewport: &iced_core::Rectangle,
        renderer: &crate::ui::Renderer,
    ) -> iced_core::mouse::Interaction {
        self.elems()
            .zip(&state.children)
            .zip(layout.children())
            .map(|((e, s), l)| {
                e.as_widget()
                    .mouse_interaction(s, l, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or(iced_core::mouse::Interaction::None)
    }

    fn operate(
        &mut self,
        state: &mut tree::Tree,
        layout: iced_core::Layout<'_>,
        renderer: &crate::ui::Renderer,
        operation: &mut dyn iced_core::widget::Operation<()>,
    ) {
        self.elems_mut()
            .zip(&mut state.children)
            .zip(layout.children())
            .for_each(|((e, s), l)| {
                e.as_widget_mut().operate(s, l, renderer, operation);
            });
    }

    fn overlay<'b>(
        &'b mut self,
        state: &'b mut tree::Tree,
        layout: iced_core::Layout<'b>,
        renderer: &crate::ui::Renderer,
        viewport: &iced_core::Rectangle,
        translation: Vector,
    ) -> Option<iced_core::overlay::Element<'b, Message, crate::ui::Theme, crate::ui::Renderer>>
    {
        self.elems_mut()
            .zip(&mut state.children)
            .zip(layout.children())
            .find_map(|((e, s), l)| {
                e.as_widget_mut()
                    .overlay(s, l, renderer, viewport, translation)
            })
    }
}

impl<'a, Message: Clone + 'static> From<HeaderBarWidget<'a, Message>> for Element<'a, Message> {
    fn from(w: HeaderBarWidget<'a, Message>) -> Self {
        Element::new(w)
    }
}

impl<'a, Message: Clone + 'static> HeaderBar<'a, Message> {
    /// Converts the headerbar builder into an Iced element.
    pub fn view(mut self) -> Element<'a, Message> {
        let Spacing {
            space_xxxs,
            space_xxs,
            ..
        } = theme::spacing();
        let is_ssd = self.is_ssd;

        // Take ownership of the regions to be packed.
        let start = std::mem::take(&mut self.start);
        let center = std::mem::take(&mut self.center);
        let mut end = std::mem::take(&mut self.end);

        // Also packs the window controls at the very end.
        end.push(self.window_controls(space_xxs));

        let padding = if is_ssd {
            [2, 8, 2, 8]
        } else {
            match (
                self.density.unwrap_or_else(theme::header_size),
                self.maximized, // window border handling
            ) {
                (Density::Compact, true) => [4, 8, 4, 8],
                (Density::Compact, false) => [3, 7, 4, 7],
                (_, true) => [8, 8, 8, 8],
                (_, false) => [7, 7, 8, 7],
            }
        };

        let start = Row::with_children(start)
            .spacing(space_xxxs.to_pixels())
            .align_y(iced::Alignment::Center)
            .into();
        let title = (!self.title.is_empty()).then(|| {
            Ellipsize::new(self.title, EllipsizeMode::End(1))
                .size(14.0)
                .line_height(text::LineHeight::Absolute(iced::Pixels(21.0)))
                .font(crate::ui::font::bold())
                .into()
        });
        // The title is centred unless the centre holds something else; then
        // it follows the start
        let (title, center) = if center.is_empty() {
            (None, title)
        } else {
            (
                title,
                Some(
                    Row::with_children(center)
                        .spacing(space_xxxs.to_pixels())
                        .align_y(iced::Alignment::Center)
                        .into(),
                ),
            )
        };
        let end = Row::with_children(end)
            .spacing(space_xxs.to_pixels())
            .align_y(iced::Alignment::Center)
            .into();

        let mut widget = HeaderBarWidget::new(start, title, center, end)
            .apply(widget::container)
            .class(theme::Container::HeaderBar {
                focused: self.focused,
                sharp_corners: self.sharp_corners,
                transparent: !is_ssd,
            })
            .height(Length::Fixed(32.0 + padding[0] as f32 + padding[2] as f32))
            .padding(padding.to_padding())
            // `iced`'s `MouseArea` has no `on_drag`; this crate's vendored
            // `mouse_area` does.
            .apply(crate::mouse_area::MouseArea::new);

        // `mouse_area`'s setters take a closure of the event's geometry
        // (`on_drag` takes `Fn(Option<Rectangle>) -> Message`, `on_press` takes
        // Fn(Option<Point>) -> Message`). The header bar does not use the
        // position, so the argument is discarded.
        if let Some(message) = self.on_drag {
            widget = widget.on_drag(move |_| message.clone());
        }
        if let Some(message) = self.on_maximize {
            widget = widget.on_release(move |_| message.clone());
        }
        if let Some(message) = self.on_double_click {
            widget = widget.on_double_click(move |_| message.clone());
        }
        if let Some(message) = self.on_right_click {
            widget = widget.on_right_press(move |_| message.clone());
        }

        widget.into()
    }

    /// Creates the widget for window controls.
    fn window_controls(&mut self, spacing: u16) -> Element<'a, Message> {
        macro_rules! icon {
            ($name:expr, $size:expr, $on_press:expr) => {{
                widget::icon::from_name($name)
                    .apply(widget::button::icon)
                    .padding(8)
                    .class(theme::Button::HeaderBar)
                    .selected(self.focused)
                    .icon_size($size)
                    .on_press($on_press)
            }};
        }

        Row::with_capacity(3)
            .push_maybe(
                self.on_minimize
                    .take()
                    .map(|m| icon!("window-minimize-symbolic", 16, m)),
            )
            .push_maybe(self.on_maximize.take().map(|m| {
                if self.maximized {
                    icon!("window-restore-symbolic", 16, m)
                } else {
                    icon!("window-maximize-symbolic", 16, m)
                }
            }))
            .push_maybe(
                self.on_close
                    .take()
                    .map(|m| icon!("window-close-symbolic", 16, m)),
            )
            .spacing(spacing.to_pixels())
            .align_y(iced::Alignment::Center)
            .into()
    }
}

impl<'a, Message: Clone + 'static> From<HeaderBar<'a, Message>> for Element<'a, Message> {
    fn from(headerbar: HeaderBar<'a, Message>) -> Self {
        headerbar.view()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::iced_core::widget::{Id, Operation};
    use crate::ui::iced_core::{Rectangle, clipboard, mouse, window};
    use crate::ui::iced_runtime::user_interface::{Cache, UserInterface};
    use crate::ui::widget::{id_container, responsive_container, space};

    const WIDTH: f32 = 800.0;

    /// The widest limit a responsive container announced, if any.
    #[derive(Clone, Debug)]
    struct Announced(Option<f32>);

    /// A part `width` wide (`None`: as wide as it is offered), reporting its
    /// bounds as `id`.
    fn part(id: &'static str, width: Option<f32>) -> Element<'static, Announced> {
        id_container(
            space::horizontal()
                .width(width.map_or(Length::Fill, Length::Fixed))
                .height(Length::Fixed(32.0)),
            Id::new(id),
        )
        .into()
    }

    /// Lays out a header 800 wide, returning each part's bounds by id.
    fn layout(header: HeaderBarWidget<'static, Announced>) -> Vec<(Id, Rectangle)> {
        struct Bounds(Vec<(Id, Rectangle)>);
        impl Operation for Bounds {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
                if let Some(id) = id {
                    self.0.push((id.clone(), bounds));
                }
            }
        }

        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(
            Element::from(header),
            Size::new(WIDTH, 32.0),
            Cache::default(),
            &mut renderer,
        );
        let mut bounds = Bounds(Vec::new());
        ui.operate(&renderer, &mut bounds);
        bounds.0
    }

    fn bounds(parts: &[(Id, Rectangle)], id: &'static str) -> Rectangle {
        parts
            .iter()
            .find(|(found, _)| *found == Id::new(id))
            .expect("the part is laid out")
            .1
    }

    #[test]
    fn the_start_gets_all_the_room_the_end_leaves() {
        // 800 - 100 (end) - 8
        let parts = layout(HeaderBarWidget::new(
            part("start", None),
            None,
            Some(part("center", Some(200.0))),
            part("end", Some(100.0)),
        ));
        assert_eq!(bounds(&parts, "start").width, 692.0);
    }

    #[test]
    fn the_centre_is_centred_when_it_fits() {
        let parts = layout(HeaderBarWidget::new(
            part("start", Some(100.0)),
            None,
            Some(part("center", Some(200.0))),
            part("end", Some(100.0)),
        ));
        assert_eq!(bounds(&parts, "center").x, (WIDTH - 200.0) / 2.0);
    }

    #[test]
    fn the_title_takes_what_the_centre_leaves() {
        let parts = layout(HeaderBarWidget::new(
            part("start", Some(100.0)),
            Some(part("title", None)),
            Some(part("center", Some(200.0))),
            part("end", Some(100.0)),
        ));
        // 800 - 100 (end) - 8 - 100 (start) - 8 - (200 (centre) + 8)
        let title = bounds(&parts, "title");
        assert_eq!((title.x, title.width), (108.0, 376.0));
        // Right after it, as it leaves no room to centre in
        assert_eq!(bounds(&parts, "center").x, 108.0 + 376.0 + 8.0);
    }

    #[test]
    fn the_menu_bar_decides_whether_it_fits_less_the_centre() {
        let start = || {
            responsive_container(
                id_container(
                    space::horizontal()
                        .width(Length::Fixed(600.0))
                        .height(Length::Fixed(32.0)),
                    Id::new("content"),
                ),
                Id::new("menu"),
                |action| match action {
                    crate::ui::surface::Action::ResponsiveMenuBar { limits, .. } => {
                        Announced(Some(limits.max().width))
                    }
                    _ => Announced(None),
                },
            )
        };
        let header = || {
            HeaderBarWidget::new(
                start().into(),
                None,
                Some(part("center", Some(200.0))),
                part("end", Some(100.0)),
            )
        };
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(
            Element::from(header()),
            Size::new(WIDTH, 32.0),
            Cache::default(),
            &mut renderer,
        );
        let mut messages = Vec::new();
        let _ = ui.update(
            &[iced_core::Event::Window(window::Event::RedrawRequested(
                std::time::Instant::now(),
            ))],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        // Laid out in 692 (800 - 100 - 8), it judges by 692 - (200 + 8)
        let announced: Vec<_> = messages.iter().filter_map(|m| m.0).collect();
        assert_eq!(announced, [484.0]);

        // ...while it is still laid out whole, not squeezed to 484
        assert_eq!(bounds(&layout(header()), "content").width, 600.0);
    }
}
