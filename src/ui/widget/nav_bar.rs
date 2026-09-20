// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/nav_bar.rs
//!
//! Navigation side panel for switching between views.
//!
//! For details on the model, see the [`segmented_button`] module.

use apply::Apply;
use iced::{Background, Length, window};
use iced_core::{Border, Color, Shadow};

use crate::ui::Theme;
use crate::ui::convert::{ToColor, ToRadius};
use crate::ui::theme;
use crate::ui::widget::Icon;
use crate::ui::widget::{Container, container};
use crate::ui::widget::{menu, scrollable, segmented_button};

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
            .apply(container)
            .padding(space_xxs)
            .apply(scrollable)
            .class(crate::ui::theme::style::iced::Scrollable::Minimal)
            .height(Length::Fill)
            .apply(container)
            .height(Length::Fill)
            .class(theme::Container::custom(nav_bar_style))
    }
}

impl<'a, Message: Clone + 'static> From<NavBar<'a, Message>> for crate::ui::Element<'a, Message> {
    fn from(this: NavBar<'a, Message>) -> Self {
        Container::from(this).into()
    }
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
