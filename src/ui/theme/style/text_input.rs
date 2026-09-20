// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Contains stylesheet implementation for [`crate::ui::widget::text_input`].

use iced_core::Color;
use palette::WithAlpha;

use crate::ui::convert::{ToColor, ToRadius};
use crate::ui::theme::Theme;
use crate::ui::widget::text_input::input::ColorExt;
use crate::ui::widget::text_input::{Appearance, StyleSheet};

#[derive(Default)]
pub enum TextInput {
    #[default]
    Default,
    EditableText,
    ExpandableSearch,
    Search,
    Inline,
    Custom {
        active: Box<dyn Fn(&Theme) -> Appearance>,
        error: Box<dyn Fn(&Theme) -> Appearance>,
        hovered: Box<dyn Fn(&Theme) -> Appearance>,
        focused: Box<dyn Fn(&Theme) -> Appearance>,
        disabled: Box<dyn Fn(&Theme) -> Appearance>,
    },
}

impl StyleSheet for Theme {
    type Style = TextInput;

    fn active(&self, style: &Self::Style) -> Appearance {
        let palette = self.cosmic();
        let container = self.current_container();

        let background: Color = container.small_widget.with_alpha(0.25).to_color();

        let corner = palette.corner_radii;
        let label_color = palette.palette.neutral_9;
        match style {
            TextInput::Default => Appearance {
                background: background.into(),
                border_radius: corner.radius_s.to_radius(),
                border_width: 2.0,
                border_offset: None,
                border_color: container.component.divider.to_color(),
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::EditableText => Appearance {
                background: Color::TRANSPARENT.into(),
                border_radius: corner.radius_0.to_radius(),
                border_width: 0.0,
                border_offset: None,
                border_color: Color::TRANSPARENT,
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::ExpandableSearch => Appearance {
                background: Color::TRANSPARENT.into(),
                border_radius: corner.radius_xl.to_radius(),
                border_width: 0.0,
                border_offset: None,
                border_color: Color::TRANSPARENT,
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::Search => Appearance {
                background: background.into(),
                border_radius: corner.radius_xl.to_radius(),
                border_width: 2.0,
                border_offset: None,
                border_color: container.component.divider.to_color(),
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::Inline => Appearance {
                background: Color::TRANSPARENT.into(),
                border_radius: corner.radius_0.to_radius(),
                border_width: 0.0,
                border_offset: None,
                border_color: Color::TRANSPARENT,
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::Custom { active, .. } => active(self),
        }
    }

    fn error(&self, style: &Self::Style) -> Appearance {
        let palette = self.cosmic();
        let container = self.current_container();

        let mut background: Color = container.small_widget.to_color();
        background.a = 0.25;

        let corner = palette.corner_radii;
        let label_color = palette.palette.neutral_9;

        match style {
            TextInput::Default => Appearance {
                background: background.into(),
                border_radius: corner.radius_s.to_radius(),
                border_width: 2.0,
                border_offset: Some(2.0),
                border_color: palette.destructive_color().to_color(),
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::Search | TextInput::ExpandableSearch => Appearance {
                background: background.into(),
                border_radius: corner.radius_xl.to_radius(),
                border_width: 0.0,
                border_offset: None,
                border_color: Color::TRANSPARENT,
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::EditableText | TextInput::Inline => Appearance {
                background: Color::TRANSPARENT.into(),
                border_radius: corner.radius_0.to_radius(),
                border_width: 0.0,
                border_offset: None,
                border_color: Color::TRANSPARENT,
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::Custom { error, .. } => error(self),
        }
    }

    fn hovered(&self, style: &Self::Style) -> Appearance {
        let palette = self.cosmic();
        let container = self.current_container();

        let mut background: Color = container.small_widget.to_color();
        background.a = 0.25;

        let corner = palette.corner_radii;
        let label_color = palette.palette.neutral_9;

        match style {
            TextInput::Default => Appearance {
                background: background.into(),
                border_radius: corner.radius_s.to_radius(),
                border_width: 2.0,
                border_offset: None,
                border_color: palette.accent.base.to_color(),
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::Search => Appearance {
                background: background.into(),
                border_radius: corner.radius_xl.to_radius(),
                border_offset: None,
                border_width: 2.0,
                border_color: palette.accent.base.to_color(),
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::ExpandableSearch => Appearance {
                background: background.into(),
                border_radius: corner.radius_xl.to_radius(),
                border_offset: None,
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::EditableText => Appearance {
                background: Color::TRANSPARENT.into(),
                border_radius: corner.radius_0.to_radius(),
                border_width: 0.0,
                border_offset: None,
                border_color: Color::TRANSPARENT,
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::Inline => Appearance {
                background: container.component.hover.to_color().into(),
                border_radius: corner.radius_0.to_radius(),
                border_width: 0.0,
                border_offset: None,
                border_color: Color::TRANSPARENT,
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::Custom { hovered, .. } => hovered(self),
        }
    }

    fn focused(&self, style: &Self::Style) -> Appearance {
        let palette = self.cosmic();
        let container = self.current_container();

        let mut background: Color = container.small_widget.to_color();
        background.a = 0.25;

        let corner = palette.corner_radii;
        let label_color = palette.palette.neutral_9;

        match style {
            TextInput::Default => Appearance {
                background: background.into(),
                border_radius: corner.radius_s.to_radius(),
                border_width: 2.0,
                border_offset: Some(2.0),
                border_color: palette.accent.base.to_color(),
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::Search | TextInput::ExpandableSearch => Appearance {
                background: background.into(),
                border_radius: corner.radius_xl.to_radius(),
                border_width: 2.0,
                border_offset: Some(2.0),
                border_color: palette.accent.base.to_color(),
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::EditableText => Appearance {
                background: Color::TRANSPARENT.into(),
                border_radius: corner.radius_0.to_radius(),
                border_width: 0.0,
                border_offset: None,
                border_color: Color::TRANSPARENT,
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::Inline => Appearance {
                background: Color::TRANSPARENT.into(),
                border_radius: corner.radius_0.to_radius(),
                border_width: 0.0,
                border_offset: None,
                border_color: Color::TRANSPARENT,
                icon_color: None,
                text_color: None,
                placeholder_color: {
                    let color: Color = container.on.to_color();
                    color.blend_alpha(background, 0.7)
                },
                selected_text_color: palette.on_accent_color().to_color(),
                selected_fill: palette.accent_color().to_color(),
                label_color: label_color.to_color(),
            },
            TextInput::Custom { focused, .. } => focused(self),
        }
    }

    fn disabled(&self, style: &Self::Style) -> Appearance {
        if let TextInput::Custom { disabled, .. } = style {
            return disabled(self);
        }

        self.active(style)
    }
}
