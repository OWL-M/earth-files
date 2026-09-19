// Copyright 2019 Héctor Ramón, Iced contributors
// SPDX-License-Identifier: MIT
//
// Vendored from `iced_widget 0.14.2`, `src/svg.rs`, with one addition: the
// `symbolic` flag and the four lines of `draw` that fill in a symbolic icon's
// colour from the inherited icon colour when the class leaves it unset.
//
// `renderer::Style` has no `icon_color`, so the value comes from
// [`crate::ui::theme::icon_color`], which resolves the same channel out of
// `renderer::Style::text_color` plus the explicit scope that
// [`crate::ui::theme::with_icon_color`] installs where the two diverge.
//
//! A vector graphics widget that honours the inherited symbolic icon colour.

use iced::advanced::widget::Tree;
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer, svg};
use iced::window;
use iced::{ContentFit, Event, Length, Point, Rectangle, Rotation, Size, Vector};

use std::path::PathBuf;

pub use iced::widget::svg::{Catalog, Handle, Status, Style};

use crate::ui::Element;

/// A vector graphics image.
///
/// An [`Svg`] image resizes smoothly without losing any quality.
pub struct Svg<'a, Theme = crate::ui::Theme>
where
    Theme: Catalog,
{
    handle: Handle,
    width: Length,
    height: Length,
    content_fit: ContentFit,
    class: Theme::Class<'a>,
    rotation: Rotation,
    opacity: f32,
    status: Option<Status>,
    symbolic: bool,
}

impl<'a, Theme> Svg<'a, Theme>
where
    Theme: Catalog,
{
    /// Creates a new [`Svg`] from the given [`Handle`].
    pub fn new(handle: impl Into<Handle>) -> Self {
        Svg {
            handle: handle.into(),
            width: Length::Fill,
            height: Length::Shrink,
            content_fit: ContentFit::Contain,
            class: <Theme as Catalog>::default(),
            rotation: Rotation::default(),
            opacity: 1.0,
            status: None,
            symbolic: false,
        }
    }

    /// Creates a new [`Svg`] that will display the contents of the file at the
    /// provided path.
    #[must_use]
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self::new(Handle::from_path(path))
    }

    /// Sets the width of the [`Svg`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height of the [`Svg`].
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the [`ContentFit`] of the [`Svg`].
    ///
    /// Defaults to [`ContentFit::Contain`]
    #[must_use]
    pub fn content_fit(self, content_fit: ContentFit) -> Self {
        Self {
            content_fit,
            ..self
        }
    }

    /// Symbolic icons inherit their colour when the class does not set one.
    ///
    /// Vendored from the fork's `Svg::symbolic`.
    #[must_use]
    pub fn symbolic(mut self, symbolic: bool) -> Self {
        self.symbolic = symbolic;
        self
    }

    /// Sets the style class of the [`Svg`].
    #[must_use]
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// Applies the given [`Rotation`] to the [`Svg`].
    #[must_use]
    pub fn rotation(mut self, rotation: impl Into<Rotation>) -> Self {
        self.rotation = rotation.into();
        self
    }

    /// Sets the opacity of the [`Svg`].
    ///
    /// It should be in the [0.0, 1.0] range: `0.0` meaning completely
    /// transparent, and `1.0` meaning completely opaque.
    #[must_use]
    pub fn opacity(mut self, opacity: impl Into<f32>) -> Self {
        self.opacity = opacity.into();
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Svg<'_, Theme>
where
    Renderer: svg::Renderer,
    Theme: Catalog,
{
    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // The raw w/h of the underlying image
        let Size { width, height } = renderer.measure_svg(&self.handle);
        let image_size = Size::new(width as f32, height as f32);

        // The rotated size of the svg
        let rotated_size = self.rotation.apply(image_size);

        // The size to be available to the widget prior to `Shrink`ing
        let raw_size = limits.resolve(self.width, self.height, rotated_size);

        // The uncropped size of the image when fit to the bounds above
        let full_size = self.content_fit.fit(rotated_size, raw_size);

        // Shrink the widget to fit the resized image, if requested
        let final_size = Size {
            width: match self.width {
                Length::Shrink => f32::min(raw_size.width, full_size.width),
                _ => raw_size.width,
            },
            height: match self.height {
                Length::Shrink => f32::min(raw_size.height, full_size.height),
                _ => raw_size.height,
            },
        };

        layout::Node::new(final_size)
    }

    fn update(
        &mut self,
        _state: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let current_status = if cursor.is_over(layout.bounds()) {
            Status::Hovered
        } else {
            Status::Idle
        };

        if let Event::Window(window::Event::RedrawRequested(_now)) = event {
            self.status = Some(current_status);
        } else if self.status.is_some_and(|status| status != current_status) {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        _state: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        renderer_style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let Size { width, height } = renderer.measure_svg(&self.handle);
        let image_size = Size::new(width as f32, height as f32);
        let rotated_size = self.rotation.apply(image_size);

        let bounds = layout.bounds();
        let adjusted_fit = self.content_fit.fit(rotated_size, bounds.size());
        let scale = Vector::new(
            adjusted_fit.width / rotated_size.width,
            adjusted_fit.height / rotated_size.height,
        );

        let final_size = image_size * scale;

        let position = match self.content_fit {
            ContentFit::None => Point::new(
                bounds.x + (rotated_size.width - adjusted_fit.width) / 2.0,
                bounds.y + (rotated_size.height - adjusted_fit.height) / 2.0,
            ),
            _ => Point::new(
                bounds.center_x() - final_size.width / 2.0,
                bounds.center_y() - final_size.height / 2.0,
            ),
        };

        let drawing_bounds = Rectangle::new(position, final_size);

        let mut style = theme.style(&self.class, self.status.unwrap_or(Status::Idle));

        // Fill in a symbolic icon's colour from the inherited icon colour when
        // the class leaves it unset.
        if self.symbolic && style.color.is_none() {
            style.color = Some(crate::ui::theme::icon_color(renderer_style));
        }

        renderer.draw_svg(
            svg::Svg {
                handle: self.handle.clone(),
                color: style.color,
                rotation: self.rotation.radians(),
                opacity: self.opacity,
            },
            drawing_bounds,
            bounds,
        );
    }
}

impl<'a, Message> From<Svg<'a, crate::ui::Theme>> for Element<'a, Message>
where
    Message: 'a,
{
    fn from(icon: Svg<'a, crate::ui::Theme>) -> Element<'a, Message> {
        Element::new(icon)
    }
}
