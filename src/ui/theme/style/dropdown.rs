// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

use iced::{Background, Color};

use crate::ui::convert::{ToColor, ToRadius};
use crate::ui::theme::Theme;
use crate::ui::widget::dropdown;

impl dropdown::menu::StyleSheet for Theme {
    type Style = ();

    fn appearance(&self, _style: &Self::Style) -> dropdown::menu::Appearance {
        let cosmic = self.cosmic();

        dropdown::menu::Appearance {
            text_color: cosmic.on_bg_color().to_color(),
            background: Background::Color(
                cosmic
                    .background(self.transparent)
                    .component
                    .base
                    .to_color(),
            ),
            border_width: 0.0,
            border_radius: cosmic.corner_radii.radius_m.to_radius(),
            border_color: Color::TRANSPARENT,

            hovered_text_color: cosmic.on_bg_color().to_color(),
            hovered_background: Background::Color(
                cosmic.primary(self.transparent).component.hover.to_color(),
            ),

            selected_text_color: cosmic.accent_text_color().to_color(),
            selected_background: Background::Color(
                cosmic.primary(self.transparent).component.hover.to_color(),
            ),

            description_color: cosmic
                .primary(self.transparent)
                .component
                .on_disabled
                .to_color(),
        }
    }
}
