// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Contains stylesheet implementation for [`crate::ui::widget::button`].

use iced_core::{Background, Color};

use crate::ui::theme::palette::TRANSPARENT_COMPONENT;
use crate::ui::theme::{Component, Theme};
use crate::ui::widget::button::{Catalog, Style};
use crate::ui::convert::{ToColor, ToRadius};

#[derive(Default)]
pub enum Button {
    AppletIcon,
    AppletMenu,
    Custom {
        active: Box<dyn Fn(bool, &Theme) -> Style>,
        disabled: Box<dyn Fn(&Theme) -> Style>,
        hovered: Box<dyn Fn(bool, &Theme) -> Style>,
        pressed: Box<dyn Fn(bool, &Theme) -> Style>,
    },
    Destructive,
    HeaderBar,
    Icon,
    IconVertical,
    Image,
    Link,
    LinkActive,
    ListItem([f32; 4]),
    MenuFolder,
    MenuItem,
    MenuRoot,
    NavToggle,
    #[default]
    Standard,
    Suggested,
    Text,
    Transparent,
}

pub fn appearance(
    theme: &Theme,
    focused: bool,
    selected: bool,
    disabled: bool,
    style: &Button,
    color: impl Fn(&Component) -> (Color, Option<Color>, Option<Color>),
) -> Style {
    let cosmic = theme.cosmic();
    let mut corner_radii = &cosmic.corner_radii.radius_xl;
    let mut appearance = Style::new();
    let hc = cosmic.is_high_contrast;
    match style {
        Button::Standard
        | Button::Text
        | Button::Suggested
        | Button::Destructive
        | Button::Transparent => {
            let style_component = match style {
                Button::Standard => &cosmic.button,
                Button::Text => &cosmic.text_button,
                Button::Suggested => &cosmic.accent_button,
                Button::Destructive => &cosmic.destructive_button,
                Button::Transparent => &TRANSPARENT_COMPONENT,
                _ => return appearance,
            };

            let (background, text, icon) = color(style_component);
            appearance.background = Some(Background::Color(background));
            if !matches!(style, Button::Standard) {
                appearance.text_color = text;
                appearance.icon_color = icon;
            } else if hc {
                appearance.border_color = style_component.border.to_color();
                appearance.border_width = 1.;
            }
        }

        Button::Icon | Button::IconVertical | Button::HeaderBar | Button::NavToggle => {
            if matches!(style, Button::IconVertical) {
                corner_radii = &cosmic.corner_radii.radius_m;
                if selected {
                    appearance.overlay = Some(Background::Color(cosmic.icon_button.selected_state_color().to_color()));
                }
            }
            if matches!(style, Button::NavToggle) {
                corner_radii = &cosmic.corner_radii.radius_s;
            }

            let (background, text, icon) = color(&cosmic.icon_button);
            appearance.background = Some(Background::Color(background));
            // Only override icon button colors when it is disabled
            appearance.icon_color = if disabled { icon } else { None };
            appearance.text_color = if disabled { text } else { None };
        }

        Button::Image => {
            appearance.background = None;
            appearance.text_color = Some(cosmic.accent_text_color().to_color());
            appearance.icon_color = Some(cosmic.accent.base.to_color());

            corner_radii = &cosmic.corner_radii.radius_s;
            appearance.border_radius = (*corner_radii).to_radius();

            if focused || selected {
                appearance.border_width = 2.0;
                appearance.border_color = cosmic.accent.base.to_color();
            } else if hc {
                appearance.border_color = theme.current_container().component.divider.to_color();
                appearance.border_width = 1.;
            }

            return appearance;
        }

        Button::Link => {
            appearance.background = None;
            appearance.icon_color = Some(cosmic.accent_text_color().to_color());
            appearance.text_color = Some(cosmic.accent_text_color().to_color());
            corner_radii = &cosmic.corner_radii.radius_0;
        }

        Button::LinkActive => {
            appearance.background = Some(Background::Color(cosmic.text_button.hover.to_color()));
            appearance.icon_color = Some(cosmic.accent_text_color().to_color());
            appearance.text_color = Some(cosmic.accent_text_color().to_color());
            corner_radii = &cosmic.corner_radii.radius_xs;
        }

        Button::Custom { .. } => (),
        Button::AppletMenu => {
            let (background, _, _) = color(&cosmic.text_button);
            appearance.background = Some(Background::Color(background));

            appearance.icon_color = Some(cosmic.background(theme.transparent).on.to_color());
            appearance.text_color = Some(cosmic.background(theme.transparent).on.to_color());
            corner_radii = &cosmic.corner_radii.radius_0;
        }
        Button::AppletIcon => {
            let (background, _, _) = color(&cosmic.text_button);
            appearance.background = Some(Background::Color(background));

            appearance.icon_color = Some(cosmic.background(theme.transparent).on.to_color());
            appearance.text_color = Some(cosmic.background(theme.transparent).on.to_color());
        }
        Button::MenuFolder => {
            // Menu folders cannot be disabled, ignore customized icon and text color
            let component = &cosmic.background(theme.transparent).component;
            let (background, _, _) = color(component);
            appearance.background = Some(Background::Color(background));
            appearance.icon_color = Some(component.on.to_color());
            appearance.text_color = Some(component.on.to_color());
            corner_radii = &cosmic.corner_radii.radius_s;
        }
        Button::ListItem(radii) => {
            corner_radii = radii;
            let (background, text, icon) = color(&cosmic.list_button);

            if selected {
                appearance.background = Some(Background::Color(
                    cosmic.primary(theme.transparent).component.hover.to_color(),
                ));
                appearance.icon_color = Some(cosmic.accent.base.to_color());
                appearance.text_color = Some(cosmic.accent_text_color().to_color());
            } else {
                appearance.background = Some(Background::Color(background));
                appearance.icon_color = icon;
                appearance.text_color = text;
            }
        }
        Button::MenuItem => {
            let (background, text, icon) = color(&cosmic.background(theme.transparent).component);
            appearance.background = Some(Background::Color(background));
            appearance.icon_color = icon;
            appearance.text_color = text;
            corner_radii = &cosmic.corner_radii.radius_s;
        }
        Button::MenuRoot => {
            appearance.background = None;
            appearance.icon_color = None;
            appearance.text_color = None;
        }
    }

    appearance.border_radius = (*corner_radii).to_radius();

    if focused {
        appearance.outline_width = 1.0;
        appearance.outline_color = cosmic.accent.base.to_color();
        appearance.border_width = 2.0;
        appearance.border_color = Color::TRANSPARENT;
    }

    appearance
}

impl Catalog for Theme {
    type Class = Button;

    fn active(&self, focused: bool, selected: bool, style: &Self::Class) -> Style {
        if let Button::Custom { active, .. } = style {
            return active(focused, self);
        }

        appearance(self, focused, selected, false, style, move |component| {
            let text_color = if matches!(
                style,
                Button::Icon | Button::IconVertical | Button::HeaderBar
            ) && selected
            {
                Some(self.cosmic().accent_text_color().to_color())
            } else {
                Some(component.on.to_color())
            };

            (component.base.to_color(), text_color, text_color)
        })
    }

    fn disabled(&self, style: &Self::Class) -> Style {
        if let Button::Custom { disabled, .. } = style {
            return disabled(self);
        }

        appearance(self, false, false, true, style, |component| {
            let mut background = component.base.to_color();
            if !matches!(
                style,
                Button::MenuFolder | Button::MenuItem | Button::MenuRoot
            ) {
                background.a *= 0.5;
            }
            (
                background,
                Some(component.on_disabled.to_color()),
                Some(component.on_disabled.to_color()),
            )
        })
    }

    fn drop_target(&self, style: &Self::Class) -> Style {
        self.active(false, false, style)
    }

    fn hovered(&self, focused: bool, selected: bool, style: &Self::Class) -> Style {
        if let Button::Custom { hovered, .. } = style {
            return hovered(focused, self);
        }

        appearance(
            self,
            focused || matches!(style, Button::Image),
            selected,
            false,
            style,
            |component| {
                let text_color = if matches!(
                    style,
                    Button::Icon | Button::IconVertical | Button::HeaderBar
                ) && selected
                {
                    Some(self.cosmic().accent_text_color().to_color())
                } else {
                    Some(component.on.to_color())
                };

                (component.hover.to_color(), text_color, text_color)
            },
        )
    }

    fn pressed(&self, focused: bool, selected: bool, style: &Self::Class) -> Style {
        if let Button::Custom { pressed, .. } = style {
            return pressed(focused, self);
        }

        appearance(self, focused, selected, false, style, |component| {
            let text_color = if matches!(
                style,
                Button::Icon | Button::IconVertical | Button::HeaderBar
            ) && selected
            {
                Some(self.cosmic().accent_text_color().to_color())
            } else {
                Some(component.on.to_color())
            };

            (component.pressed.to_color(), text_color, text_color)
        })
    }

    fn selection_background(&self) -> Background {
        Background::Color(self.cosmic().primary(self.transparent).base.to_color())
    }
}
