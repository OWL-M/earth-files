// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/nav_bar.rs
//!
//! Navigation side panel for switching between views.
//!
//! For details on the model, see the [`segmented_button`] module.

use apply::Apply;
use iced::advanced::graphics::text::Paragraph as GraphicsParagraph;
use iced::advanced::text::{Alignment, LineHeight, Paragraph as _, Shaping, Text, Wrapping};
use iced::{Background, Length, Padding, Pixels, Size, window};
use iced_core::{Border, Color, Shadow, alignment};

use crate::ui::Theme;
use crate::ui::convert::{ToColor, ToRadius};
use crate::ui::theme;
use crate::ui::widget::Icon;
use crate::ui::widget::{Container, container};
use crate::ui::widget::{menu, scrollable, segmented_button};

/// How far the scrollbar sits in from every edge of the panel
const SCROLLBAR_MARGIN: f32 = 5.0;

/// Size of the entry labels. Set on the widget below rather than left to its
/// default, so that [`min_width`] measures what is drawn.
const FONT_SIZE: f32 = 14.0;

/// Size of the eject icon on a closable entry, as both callers set it
const CLOSE_ICON_SIZE: f32 = 16.0;

/// How far each level of nesting indents an entry. Set on the widget below
/// rather than left to its default, for the same reason as [`FONT_SIZE`].
const INDENT_SPACING: u16 = 16;

/// Room the minimum width keeps beyond what the entries strictly need, in
/// steps of `space_xxs`. At the minimum a label would otherwise sit right on
/// the edge of being clipped, with the scrollbar over it: the bar overlays
/// the entries rather than taking room of its own.
const MIN_WIDTH_SLACK_STEPS: f32 = 8.0;

/// Widest the panel is ever drawn, and so where a drag of it stops. Past
/// this the panel would stay put while the slot it sits in kept growing,
/// pushing the file view along with nothing to show for it.
pub const MAX_WIDTH: f32 = 280.0;

pub type Id = segmented_button::Entity;
pub type Model = segmented_button::SingleSelectModel;

/// Navigation side panel for switching between views.
///
/// For details on the model, see the [`segmented_button`] module.
pub fn nav_bar<Message: Clone + 'static>(
    model: &segmented_button::SingleSelectModel,
    on_activate: fn(segmented_button::Entity) -> Message,
) -> NavBar<'_, Message> {
    NavBar {
        segmented_button: segmented_button::vertical(model).on_activate(on_activate),
    }
}

#[must_use]
pub struct NavBar<'a, Message: Clone + 'static> {
    segmented_button:
        segmented_button::VerticalSegmentedButton<'a, segmented_button::SingleSelect, Message>,
}

impl<'a, Message: Clone + 'static> NavBar<'a, Message> {
    #[inline]
    pub fn close_icon(mut self, close_icon: Icon) -> Self {
        self.segmented_button = self.segmented_button.close_icon(close_icon);
        self
    }

    #[inline]
    pub fn context_menu(mut self, context_menu: Option<Vec<menu::Tree<Message>>>) -> Self {
        self.segmented_button = self.segmented_button.context_menu(context_menu);
        self
    }

    /// Pre-convert this widget into the [`Container`] widget that it becomes.
    #[must_use]
    #[inline]
    pub fn into_container(self) -> Container<'a, Message, crate::ui::Theme, iced::Renderer> {
        Container::from(self)
    }

    /// Emitted when a tab close button is pressed.
    pub fn on_close<T>(mut self, on_close: T) -> Self
    where
        T: Fn(Id) -> Message + 'static,
    {
        self.segmented_button = self.segmented_button.on_close(on_close);
        self
    }

    /// Emitted when a button is right-clicked.
    pub fn on_context<T>(mut self, on_context: T) -> Self
    where
        T: Fn(Id) -> Message + 'static,
    {
        self.segmented_button = self.segmented_button.on_context(on_context);
        self
    }

    /// Emitted when the middle mouse button is pressed on a button.
    pub fn on_middle_press<T>(mut self, on_middle_press: T) -> Self
    where
        T: Fn(Id) -> Message + 'static,
    {
        self.segmented_button = self.segmented_button.on_middle_press(on_middle_press);
        self
    }

    /// Make the bookmarks destinations for a file drag.
    ///
    /// The segmented button tracks the hovered destination itself and draws it
    /// with its existing hover style, without sending `enter`/`leave`
    /// notifications to the application. `None` marks a bookmark that is not
    /// a destination.
    pub fn on_file_drop<T>(mut self, on_file_drop: T) -> Self
    where
        T: Fn(Id) -> Option<Message> + 'static,
    {
        self.segmented_button = self.segmented_button.on_file_drop(on_file_drop);
        self
    }

    pub fn with_positioner(mut self, positioner: crate::ui::surface::Positioner) -> Self {
        self.segmented_button = self.segmented_button.with_positioner(positioner);
        self
    }

    pub fn window_id(mut self, id: window::Id) -> Self {
        self.segmented_button = self.segmented_button.window_id(id);
        self
    }

    pub fn window_id_maybe(mut self, id: Option<window::Id>) -> Self {
        self.segmented_button = self.segmented_button.window_id_maybe(id);

        self
    }

    pub fn on_surface_action(
        mut self,
        handler: impl Fn(crate::ui::surface::Action<Message>) -> Message + Send + Sync + 'static,
    ) -> Self {
        self.segmented_button = self.segmented_button.on_surface_action(handler);
        self
    }
}

impl<'a, Message: Clone + 'static> From<NavBar<'a, Message>>
    for Container<'a, Message, crate::ui::Theme, iced::Renderer>
{
    fn from(this: NavBar<'a, Message>) -> Self {
        let spacing = crate::ui::theme::spacing();
        let space_s = spacing.space_s;
        let space_xxs = spacing.space_xxs;

        this.segmented_button
            .button_height(32)
            .button_padding([space_s, space_xxs, space_s, space_xxs])
            .button_spacing(space_xxs)
            .spacing(space_xxs)
            .style(crate::ui::theme::SegmentedButton::NavBar)
            .font_size(FONT_SIZE)
            .indent_spacing(INDENT_SPACING)
            .apply(container)
            .padding(
                Padding::ZERO
                    .left(f32::from(space_xxs))
                    .right(f32::from(space_xxs)),
            )
            .apply(scrollable)
            // Held clear of the panel's trailing edge, so the bar and the
            // divider drawn on that edge are not on top of each other
            .direction(iced::widget::scrollable::Direction::Vertical(
                iced::widget::scrollable::Scrollbar::new()
                    .width(8.0)
                    .scroller_width(8.0)
                    .margin(SCROLLBAR_MARGIN),
            ))
            .class(crate::ui::theme::style::iced::Scrollable::Minimal)
            .height(Length::Fill)
            .apply(container)
            // iced insets a scrollbar from the sides but not from the ends,
            // so the scrollable is held off the top and bottom instead. The
            // panel's background still reaches them: it is painted here.
            .padding(Padding::ZERO.top(SCROLLBAR_MARGIN).bottom(SCROLLBAR_MARGIN))
            .height(Length::Fill)
            .class(theme::Container::custom(nav_bar_style))
    }
}

impl<'a, Message: Clone + 'static> From<NavBar<'a, Message>> for crate::ui::Element<'a, Message> {
    fn from(this: NavBar<'a, Message>) -> Self {
        Container::from(this).into()
    }
}

/// The narrowest the panel can be drawn without clipping an entry.
///
/// Measured from the model rather than from a layout pass, so it is current
/// the moment an entry is added, renamed or removed. It must agree with
/// `segmented_button::widget::button_dimensions`, which lays the entries out;
/// everything it accounts for is accounted for here.
#[must_use]
pub fn min_width(model: &segmented_button::SingleSelectModel) -> f32 {
    let spacing = crate::ui::theme::spacing();
    // The panel's own padding, and what `From<NavBar>` gives the buttons
    let around = f32::from(spacing.space_xxs) * 2.0;
    let button_padding = f32::from(spacing.space_s) * 2.0;
    let button_spacing = f32::from(spacing.space_xxs);
    let font = crate::ui::font::default();

    let widest = model
        .iter()
        .map(|entity| {
            let mut width = 0.0_f32;
            let mut icon_spacing = 0.0;

            if let Some(text) = model.text(entity).filter(|text| !text.is_empty()) {
                icon_spacing = button_spacing;
                width += label_width(text, font);
            }

            if let Some(indent) = model.indent(entity) {
                width += f32::from(indent) * f32::from(INDENT_SPACING);
            }

            if let Some(icon) = model.icon(entity) {
                width += f32::from(icon.size) + icon_spacing;
            }

            if model.is_closable(entity) {
                width += CLOSE_ICON_SIZE + button_spacing;
            }

            width + button_padding
        })
        .fold(0.0_f32, f32::max);

    widest + around + f32::from(spacing.space_xxs) * MIN_WIDTH_SLACK_STEPS
}

/// The width one entry's label takes, shaped the way the panel draws it
fn label_width(label: &str, font: crate::ui::font::Font) -> f32 {
    GraphicsParagraph::with_text(Text {
        content: label,
        bounds: Size::INFINITE,
        size: Pixels(FONT_SIZE),
        line_height: LineHeight::default(),
        font,
        align_x: Alignment::Left,
        align_y: alignment::Vertical::Center,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::default(),
    })
    .min_bounds()
    .width
}

#[must_use]
pub fn nav_bar_style(theme: &Theme) -> iced::widget::container::Style {
    let cosmic = &theme.cosmic();
    iced::widget::container::Style {
        text_color: Some(cosmic.on_bg_color().to_color()),
        background: Some(Background::Color(
            cosmic.primary(theme.transparent).base.to_color(),
        )),
        border: Border {
            width: 0.0,
            color: Color::TRANSPARENT,
            radius: cosmic.corner_radii.radius_s.to_radius(),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}
