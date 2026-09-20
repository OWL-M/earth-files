// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Stylesheet implementations for widgets native to iced.
//!
//! iced has no `icon_color` field on `iced_core::theme::Style`,
//! `iced_core::renderer::Style` or `iced_widget::container::Style`, so the
//! styles use `text_color` for symbolic icon inheritance; see
//! [`crate::ui::theme::icon_color`] and [`header_bar_colors`] for the
//! [`Container::HeaderBar`] override.

use iced::overlay::menu;
use iced::theme::Base;
use iced::widget::scrollable::AutoScroll;
use iced::widget::{
    button as iced_button, checkbox as iced_checkbox, container as iced_container, pick_list,
    progress_bar, radio, rule, scrollable, svg, text_editor, text_input,
};
use iced_core::{Background, Border, Color, Shadow, Vector};
// `toggler`'s `Style`/`Catalog` are this crate's vendored ones, not iced's.
use crate::ui::widget::toggler;
use palette::WithAlpha;
use std::rc::Rc;

use crate::ui::theme::palette::TRANSPARENT_COMPONENT;
use crate::ui::theme::{Component, Layer, Palette, Theme, over};

use crate::ui::convert::{ToBackground, ToColor, ToRadius};

/// A button style from the theme and the button's status
type ButtonStyleFn = Box<dyn Fn(&Theme, iced_button::Status) -> iced_button::Style>;

pub mod application {
    use crate::ui::convert::ToColor;
    use iced::theme::Style as Appearance;

    use crate::ui::theme::Theme;

    #[derive(Default)]
    pub enum Application {
        #[default]
        Default,
        Custom(Box<dyn Fn(&Theme) -> Appearance>),
    }

    impl Application {
        pub fn custom<F: Fn(&Theme) -> Appearance + 'static>(f: F) -> Self {
            Self::Custom(Box::new(f))
        }
    }

    pub fn style(theme: &Theme) -> iced::theme::Style {
        let cosmic = theme.cosmic();

        iced::theme::Style {
            background_color: cosmic.bg_color().to_color(),
            text_color: cosmic.on_bg_color().to_color(),
        }
    }
}

/// Styles for the button widget from iced-rs.
#[derive(Default)]
pub enum Button {
    Deactivated,
    Destructive,
    Positive,
    #[default]
    Primary,
    Secondary,
    Text,
    Link,
    LinkActive,
    Transparent,
    Card,
    Custom(ButtonStyleFn),
}

impl iced_button::Catalog for Theme {
    type Class<'a> = Button;

    fn default<'a>() -> Self::Class<'a> {
        Button::default()
    }

    fn style(&self, class: &Self::Class<'_>, status: iced_button::Status) -> iced_button::Style {
        if let Button::Custom(f) = class {
            return f(self, status);
        }
        let cosmic = self.cosmic();
        let corner_radii = &cosmic.corner_radii;
        let component = class.cosmic(self);

        let mut appearance = iced_button::Style {
            border: Border {
                radius: match class {
                    Button::Link => corner_radii.radius_0.to_radius(),
                    Button::Card => corner_radii.radius_xs.to_radius(),
                    _ => corner_radii.radius_xl.to_radius(),
                },
                ..Default::default()
            },
            background: match class {
                Button::Link | Button::Text => None,
                Button::LinkActive => Some(Background::Color(component.divider.to_color())),
                _ => Some(Background::Color(component.base.to_color())),
            },
            text_color: match class {
                Button::Link | Button::LinkActive => component.base.to_color(),
                _ => component.on.to_color(),
            },
            ..iced_button::Style::default()
        };

        match status {
            iced_button::Status::Active => {}
            iced_button::Status::Hovered => {
                appearance.background = match class {
                    Button::Link => None,
                    Button::LinkActive => Some(Background::Color(component.divider.to_color())),
                    _ => Some(Background::Color(component.hover.to_color())),
                };
            }
            iced_button::Status::Pressed => {
                appearance.background = match class {
                    Button::Link => None,
                    Button::LinkActive => Some(Background::Color(component.divider.to_color())),
                    _ => Some(Background::Color(component.pressed.to_color())),
                };
            }
            iced_button::Status::Disabled => {
                // Card color is not transparent when it isn't clickable
                if matches!(class, Button::Card) {
                    return appearance;
                }
                appearance.background = appearance.background.map(|background| match background {
                    Background::Color(color) => Background::Color(Color {
                        a: color.a * 0.5,
                        ..color
                    }),
                    Background::Gradient(gradient) => {
                        Background::Gradient(gradient.scale_alpha(0.5))
                    }
                });
                appearance.text_color = Color {
                    a: appearance.text_color.a * 0.5,
                    ..appearance.text_color
                };
            }
        };
        appearance
    }
}

impl Button {
    #[allow(clippy::trivially_copy_pass_by_ref)]
    #[allow(clippy::match_same_arms)]
    fn cosmic<'a>(&'a self, theme: &'a Theme) -> &'a Component {
        let cosmic = theme.cosmic();
        match self {
            Self::Primary => &cosmic.accent_button,
            Self::Secondary => &theme.current_container().component,
            Self::Positive => &cosmic.success_button,
            Self::Destructive => &cosmic.destructive_button,
            Self::Text => &cosmic.text_button,
            Self::Link => &cosmic.link_button,
            Self::LinkActive => &cosmic.link_button,
            Self::Transparent => &TRANSPARENT_COMPONENT,
            Self::Deactivated => &theme.current_container().component,
            Self::Card => &theme.current_container().component,
            Self::Custom { .. } => &TRANSPARENT_COMPONENT,
        }
    }
}

/*
 * Checkbox
 */
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Checkbox {
    #[default]
    Primary,
    Secondary,
    Success,
    Danger,
}

impl iced_checkbox::Catalog for Theme {
    type Class<'a> = Checkbox;

    fn default<'a>() -> Self::Class<'a> {
        Checkbox::default()
    }

    #[allow(clippy::too_many_lines)]
    fn style(
        &self,
        class: &Self::Class<'_>,
        status: iced_checkbox::Status,
    ) -> iced_checkbox::Style {
        let cosmic = self.cosmic();

        let corners = &cosmic.corner_radii;

        let disabled = matches!(status, iced_checkbox::Status::Disabled { .. });
        match status {
            iced_checkbox::Status::Active { is_checked }
            | iced_checkbox::Status::Disabled { is_checked } => {
                let mut active = match class {
                    Checkbox::Primary => iced_checkbox::Style {
                        background: Background::Color(if is_checked {
                            cosmic.accent.base.to_color()
                        } else {
                            self.current_container().small_widget.to_color()
                        }),
                        icon_color: cosmic.accent.on.to_color(),
                        border: Border {
                            radius: corners.radius_xs.to_radius(),
                            width: if is_checked { 0.0 } else { 1.0 },
                            color: if is_checked {
                                cosmic.accent.base
                            } else {
                                cosmic.palette.neutral_8
                            }
                            .to_color(),
                        },

                        text_color: None,
                    },
                    Checkbox::Secondary => iced_checkbox::Style {
                        background: Background::Color(if is_checked {
                            cosmic
                                .background(self.transparent)
                                .component
                                .base
                                .to_color()
                        } else {
                            self.current_container().small_widget.to_color()
                        }),
                        icon_color: cosmic.background(self.transparent).on.to_color(),
                        border: Border {
                            radius: corners.radius_xs.to_radius(),
                            width: if is_checked { 0.0 } else { 1.0 },
                            color: cosmic.palette.neutral_8.to_color(),
                        },
                        text_color: None,
                    },
                    Checkbox::Success => iced_checkbox::Style {
                        background: Background::Color(if is_checked {
                            cosmic.success.base.to_color()
                        } else {
                            self.current_container().small_widget.to_color()
                        }),
                        icon_color: cosmic.success.on.to_color(),
                        border: Border {
                            radius: corners.radius_xs.to_radius(),
                            width: if is_checked { 0.0 } else { 1.0 },
                            color: if is_checked {
                                cosmic.success.base
                            } else {
                                cosmic.palette.neutral_8
                            }
                            .to_color(),
                        },
                        text_color: None,
                    },
                    Checkbox::Danger => iced_checkbox::Style {
                        background: Background::Color(if is_checked {
                            cosmic.destructive.base.to_color()
                        } else {
                            self.current_container().small_widget.to_color()
                        }),
                        icon_color: cosmic.destructive.on.to_color(),
                        border: Border {
                            radius: corners.radius_xs.to_radius(),
                            width: if is_checked { 0.0 } else { 1.0 },
                            color: if is_checked {
                                cosmic.destructive.base
                            } else {
                                cosmic.palette.neutral_8
                            }
                            .to_color(),
                        },
                        text_color: None,
                    },
                };
                if disabled {
                    match &mut active.background {
                        Background::Color(color) => {
                            color.a /= 2.;
                        }
                        Background::Gradient(gradient) => {
                            *gradient = gradient.scale_alpha(0.5);
                        }
                    }
                    if let Some(c) = active.text_color.as_mut() {
                        c.a /= 2.
                    };
                    active.border.color.a /= 2.;
                }
                active
            }
            iced_checkbox::Status::Hovered { is_checked } => {
                let cur_container = self.current_container().small_widget;
                let hovered_bg = over(cosmic.palette.neutral_0.with_alpha(0.1), cur_container);
                match class {
                    Checkbox::Primary => iced_checkbox::Style {
                        background: Background::Color(if is_checked {
                            cosmic.accent.hover_state_color().to_color()
                        } else {
                            hovered_bg.to_color()
                        }),
                        icon_color: cosmic.accent.on.to_color(),
                        border: Border {
                            radius: corners.radius_xs.to_radius(),
                            width: if is_checked { 0.0 } else { 1.0 },
                            color: if is_checked {
                                cosmic.accent.base
                            } else {
                                cosmic.palette.neutral_8
                            }
                            .to_color(),
                        },
                        text_color: None,
                    },
                    Checkbox::Secondary => iced_checkbox::Style {
                        background: Background::Color(if is_checked {
                            self.current_container().component.hover.to_color()
                        } else {
                            hovered_bg.to_color()
                        }),
                        icon_color: self.current_container().on.to_color(),
                        border: Border {
                            radius: corners.radius_xs.to_radius(),
                            width: if is_checked { 0.0 } else { 1.0 },
                            color: if is_checked {
                                self.current_container().base
                            } else {
                                cosmic.palette.neutral_8
                            }
                            .to_color(),
                        },
                        text_color: None,
                    },
                    Checkbox::Success => iced_checkbox::Style {
                        background: Background::Color(if is_checked {
                            cosmic.success.hover.to_color()
                        } else {
                            hovered_bg.to_color()
                        }),
                        icon_color: cosmic.success.on.to_color(),
                        border: Border {
                            radius: corners.radius_xs.to_radius(),
                            width: if is_checked { 0.0 } else { 1.0 },
                            color: if is_checked {
                                cosmic.success.base
                            } else {
                                cosmic.palette.neutral_8
                            }
                            .to_color(),
                        },
                        text_color: None,
                    },
                    Checkbox::Danger => iced_checkbox::Style {
                        background: Background::Color(if is_checked {
                            cosmic.destructive.hover.to_color()
                        } else {
                            hovered_bg.to_color()
                        }),
                        icon_color: cosmic.destructive.on.to_color(),
                        border: Border {
                            radius: corners.radius_xs.to_radius(),
                            width: if is_checked { 0.0 } else { 1.0 },
                            color: if is_checked {
                                cosmic.destructive.base
                            } else {
                                cosmic.palette.neutral_8
                            }
                            .to_color(),
                        },
                        text_color: None,
                    },
                }
            }
        }
    }
}

/*
 * Container
 */
#[derive(Default)]
pub enum Container<'a> {
    WindowBackground,
    Background,
    Card,
    ContextDrawer {
        transparent: bool,
    },
    Custom(Box<dyn Fn(&Theme) -> iced_container::Style + 'a>),
    Dialog(bool),
    Dropdown,
    HeaderBar {
        focused: bool,
        sharp_corners: bool,
        transparent: bool,
    },
    List,
    Primary,
    Secondary,
    Tooltip,
    #[default]
    Transparent,
}

impl<'a> Container<'a> {
    pub fn custom<F: Fn(&Theme) -> iced_container::Style + 'a>(f: F) -> Self {
        Self::Custom(Box::new(f))
    }

    #[must_use]
    pub fn background(theme: &Palette, transparent: bool) -> iced_container::Style {
        iced_container::Style {
            text_color: Some(theme.background(transparent).on.to_color()),
            background: Some(iced::Background::Color(
                theme.background(transparent).base.to_color(),
            )),
            border: Border {
                radius: theme.corner_radii.radius_s.to_radius(),
                ..Default::default()
            },
            shadow: Shadow::default(),
            snap: true,
        }
    }

    #[must_use]
    pub fn primary(theme: &Palette, transparent: bool) -> iced_container::Style {
        iced_container::Style {
            text_color: Some(theme.primary(transparent).on.to_color()),
            background: Some(iced::Background::Color(
                theme.primary(transparent).base.to_color(),
            )),
            border: Border {
                radius: theme.corner_radii.radius_s.to_radius(),
                ..Default::default()
            },
            shadow: Shadow::default(),
            snap: true,
        }
    }

    #[must_use]
    pub fn secondary(theme: &Palette, transparent: bool) -> iced_container::Style {
        iced_container::Style {
            text_color: Some(theme.secondary(transparent).on.to_color()),
            background: Some(iced::Background::Color(
                theme.secondary(transparent).base.to_color(),
            )),
            border: Border {
                radius: theme.corner_radii.radius_s.to_radius(),
                ..Default::default()
            },
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

impl<'a> From<iced_container::StyleFn<'a, Theme>> for Container<'a> {
    fn from(value: iced_container::StyleFn<'a, Theme>) -> Self {
        Self::custom(value)
    }
}

/// The header bar's `(icon colour, text colour)`.
///
/// `Container::HeaderBar` is the only container class with different icon and
/// text colours. `crate::ui::widget::header_bar` reads this pair to install an
/// override with `crate::ui::theme::with_icon_color`.
#[must_use]
pub fn header_bar_colors(theme: &Theme, focused: bool) -> (Color, Color) {
    let cosmic = theme.cosmic();

    if focused {
        (
            cosmic.accent_text_color().to_color(),
            cosmic.background(theme.transparent).on.to_color(),
        )
    } else {
        use crate::ui::widget::text_input::input::ColorExt;
        let unfocused_color = cosmic
            .background(theme.transparent)
            .component
            .on
            .to_color()
            .blend_alpha(cosmic.background(theme.transparent).base.to_color(), 0.5);
        (unfocused_color, unfocused_color)
    }
}

impl iced_container::Catalog for Theme {
    type Class<'a> = Container<'a>;

    fn default<'a>() -> Self::Class<'a> {
        Container::default()
    }

    fn style(&self, class: &Self::Class<'_>) -> iced_container::Style {
        let cosmic = self.cosmic();

        // Ensures visually aligned radii for content and window corners
        let window_corner_radius = cosmic.radius_s().map(|x| if x < 4.0 { x } else { x + 4.0 });

        match class {
            Container::Transparent => {
                let component = &self.current_container().component;

                iced_container::Style {
                    text_color: Some(component.on.to_color()),
                    background: None,
                    border: Border {
                        radius: 0.into(),
                        ..Default::default()
                    },
                    shadow: Shadow::default(),
                    snap: true,
                }
            }

            Container::Custom(f) => f(self),

            Container::WindowBackground => iced_container::Style {
                text_color: Some(cosmic.background(self.transparent).on.to_color()),
                background: Some(iced::Background::Color(
                    cosmic.background(self.transparent).base.to_color(),
                )),
                border: Border {
                    radius: [
                        cosmic.corner_radii.radius_0[0],
                        cosmic.corner_radii.radius_0[1],
                        window_corner_radius[2],
                        window_corner_radius[3],
                    ]
                    .to_radius(),
                    ..Default::default()
                },
                shadow: Shadow::default(),
                snap: true,
            },

            Container::List => {
                let component = &self.current_container().component;
                iced_container::Style {
                    text_color: Some(component.on.to_color()),
                    background: Some(Background::Color(component.base.to_color())),
                    border: iced::Border {
                        radius: cosmic.corner_radii.radius_s.to_radius(),
                        ..Default::default()
                    },
                    shadow: Shadow::default(),
                    snap: true,
                }
            }

            Container::HeaderBar {
                focused,
                sharp_corners,
                transparent,
            } => {
                let (_icon_color, text_color) = header_bar_colors(self, *focused);

                iced_container::Style {
                    text_color: Some(text_color),
                    background: if *transparent {
                        None
                    } else {
                        Some(iced::Background::Color(
                            cosmic.background(self.transparent).base.to_color(),
                        ))
                    },
                    border: Border {
                        radius: [
                            if *sharp_corners {
                                cosmic.corner_radii.radius_0[0]
                            } else {
                                window_corner_radius[0]
                            },
                            if *sharp_corners {
                                cosmic.corner_radii.radius_0[1]
                            } else {
                                window_corner_radius[1]
                            },
                            cosmic.corner_radii.radius_0[2],
                            cosmic.corner_radii.radius_0[3],
                        ]
                        .to_radius(),
                        ..Default::default()
                    },
                    snap: true,
                    shadow: Shadow::default(),
                }
            }

            Container::ContextDrawer { transparent } => {
                let mut a = Container::primary(cosmic, self.transparent && *transparent);

                if cosmic.is_high_contrast {
                    a.border.width = 1.;
                    a.border.color = cosmic.primary(self.transparent).divider.to_color();
                }
                a
            }

            Container::Background => Container::background(cosmic, self.transparent),

            Container::Primary => Container::primary(cosmic, self.transparent),

            Container::Secondary => Container::secondary(cosmic, self.transparent),

            Container::Dropdown => iced_container::Style {
                text_color: None,
                background: Some(iced::Background::Color(
                    cosmic.bg_component_color().to_color(),
                )),
                border: Border {
                    color: cosmic.bg_component_divider().to_color(),
                    width: 1.0,
                    radius: cosmic.corner_radii.radius_s.to_radius(),
                },
                shadow: Shadow::default(),
                snap: true,
            },

            Container::Tooltip => iced_container::Style {
                text_color: None,
                background: Some(iced::Background::Color(cosmic.palette.neutral_2.to_color())),
                border: Border {
                    radius: cosmic.corner_radii.radius_l.to_radius(),
                    ..Default::default()
                },
                shadow: Shadow::default(),
                snap: true,
            },

            Container::Card => {
                let cosmic = self.cosmic();

                match self.layer {
                    Layer::Background => iced_container::Style {
                        text_color: Some(
                            cosmic.background(self.transparent).component.on.to_color(),
                        ),
                        background: Some(iced::Background::Color(
                            cosmic
                                .background(self.transparent)
                                .component
                                .base
                                .to_color(),
                        )),
                        border: Border {
                            radius: cosmic.corner_radii.radius_s.to_radius(),
                            ..Default::default()
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    Layer::Primary => iced_container::Style {
                        text_color: Some(cosmic.primary(self.transparent).component.on.to_color()),
                        background: Some(iced::Background::Color(
                            cosmic.primary(self.transparent).component.base.to_color(),
                        )),
                        border: Border {
                            radius: cosmic.corner_radii.radius_s.to_radius(),
                            ..Default::default()
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    Layer::Secondary => iced_container::Style {
                        text_color: Some(
                            cosmic.secondary(self.transparent).component.on.to_color(),
                        ),
                        background: Some(iced::Background::Color(
                            cosmic.secondary(self.transparent).component.base.to_color(),
                        )),
                        border: Border {
                            radius: cosmic.corner_radii.radius_s.to_radius(),
                            ..Default::default()
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                }
            }

            Container::Dialog(is_overlay) => iced_container::Style {
                text_color: Some(cosmic.primary(self.transparent).on.to_color()),
                background: Some(iced::Background::Color(
                    cosmic
                        .primary(self.transparent && !is_overlay)
                        .base
                        .to_color(),
                )),
                border: Border {
                    color: cosmic
                        .primary(self.transparent && !is_overlay)
                        .divider
                        .to_color(),
                    width: 1.0,
                    radius: cosmic.corner_radii.radius_m.to_radius(),
                },
                shadow: Shadow {
                    color: cosmic.shade.to_color(),
                    offset: Vector::new(0.0, 4.0),
                    blur_radius: 16.0,
                },
                snap: true,
            },
        }
    }
}

impl menu::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> <Self as menu::Catalog>::Class<'a> {}

    fn style(&self, class: &<Self as menu::Catalog>::Class<'_>) -> menu::Style {
        let cosmic = self.cosmic();

        menu::Style {
            text_color: cosmic.on_bg_color().to_color(),
            background: Background::Color(cosmic.background(self.transparent).base.to_color()),
            border: Border {
                radius: cosmic.corner_radii.radius_m.to_radius(),
                ..Default::default()
            },
            selected_text_color: cosmic.accent_text_color().to_color(),
            selected_background: Background::Color(
                cosmic
                    .background(self.transparent)
                    .component
                    .hover
                    .to_color(),
            ),
            shadow: Default::default(),
        }
    }
}

impl pick_list::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> <Self as pick_list::Catalog>::Class<'a> {}

    fn style(
        &self,
        class: &<Self as pick_list::Catalog>::Class<'_>,
        status: pick_list::Status,
    ) -> pick_list::Style {
        let cosmic = &self.cosmic();
        let hc = cosmic.is_high_contrast;
        let appearance = pick_list::Style {
            text_color: cosmic.on_bg_color().to_color(),
            background: Color::TRANSPARENT.into(),
            placeholder_color: cosmic.on_bg_color().to_color(),
            border: Border {
                radius: cosmic.corner_radii.radius_m.to_radius(),
                width: if hc { 1. } else { 0. },
                color: if hc {
                    self.current_container().component.border.to_color()
                } else {
                    Color::TRANSPARENT
                },
            },
            handle_color: cosmic.on_bg_color().to_color(),
        };

        match status {
            pick_list::Status::Active => appearance,
            pick_list::Status::Hovered => pick_list::Style {
                background: Background::Color(cosmic.background(self.transparent).base.to_color()),
                ..appearance
            },
            pick_list::Status::Opened { is_hovered: _ } => appearance,
        }
    }
}

/*
 * Radio
 */
impl radio::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> Self::Class<'a> {}

    fn style(&self, class: &Self::Class<'_>, status: radio::Status) -> radio::Style {
        let cur_container = self.current_container();
        let theme = self.cosmic();

        match status {
            radio::Status::Active { is_selected } => radio::Style {
                background: if is_selected {
                    theme.accent.base.to_color().into()
                } else {
                    cur_container.small_widget.to_color().into()
                },
                dot_color: theme.accent.on.to_color(),
                border_width: 1.0,
                border_color: if is_selected {
                    theme.accent.base.to_color()
                } else {
                    theme.palette.neutral_8.to_color()
                },
                text_color: None,
            },
            radio::Status::Hovered { is_selected } => {
                let bg = if is_selected {
                    theme.accent.base
                } else {
                    self.current_container().small_widget
                };
                let hovered_bg = over(theme.palette.neutral_0.with_alpha(0.1), bg).to_color();
                radio::Style {
                    background: hovered_bg.into(),
                    dot_color: theme.accent.on.to_color(),
                    border_width: 1.0,
                    border_color: if is_selected {
                        theme.accent.base.to_color()
                    } else {
                        theme.palette.neutral_8.to_color()
                    },
                    text_color: None,
                }
            }
        }
    }
}

/*
 * Toggler
 */
impl toggler::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> Self::Class<'a> {}

    fn style(&self, class: &Self::Class<'_>, status: toggler::Status) -> toggler::Style {
        let cosmic = self.cosmic();
        const HANDLE_MARGIN: f32 = 2.0;
        let neutral_10 = cosmic.palette.neutral_10.with_alpha(0.1);

        let mut active = toggler::Style {
            background: if matches!(status, toggler::Status::Active { is_toggled: true }) {
                cosmic.accent.base.to_background()
            } else if cosmic.is_dark {
                cosmic.palette.neutral_6.to_background()
            } else {
                cosmic.palette.neutral_5.to_background()
            },
            foreground: cosmic.palette.neutral_2.to_background(),
            border_radius: cosmic.radius_xl().to_radius(),
            handle_radius: cosmic
                .radius_xl()
                .map(|x| (x - HANDLE_MARGIN).max(0.0))
                .to_radius(),
            handle_margin: HANDLE_MARGIN,
            background_border_width: 0.0,
            background_border_color: Color::TRANSPARENT,
            foreground_border_width: 0.0,
            foreground_border_color: Color::TRANSPARENT,
            text_color: None,
            padding_ratio: 0.0,
        };
        match status {
            toggler::Status::Active { is_toggled } => active,
            toggler::Status::Hovered { is_toggled } => {
                let is_active = matches!(status, toggler::Status::Hovered { is_toggled: true });
                toggler::Style {
                    background: if is_active {
                        over(neutral_10, cosmic.accent_color())
                    } else {
                        over(
                            neutral_10,
                            if cosmic.is_dark {
                                cosmic.palette.neutral_6
                            } else {
                                cosmic.palette.neutral_5
                            },
                        )
                    }
                    .to_background(),
                    ..active
                }
            }
            toggler::Status::Disabled { is_toggled } => {
                active.background = active.background.scale_alpha(0.5);
                active.foreground = active.foreground.scale_alpha(0.5);
                active
            }
        }
    }
}

/*
 * Progress Bar
 */
#[derive(Default)]
pub enum ProgressBar {
    #[default]
    Primary,
    Success,
    Danger,
    Custom(Box<dyn Fn(&Theme) -> progress_bar::Style>),
}

impl ProgressBar {
    pub fn custom<F: Fn(&Theme) -> progress_bar::Style + 'static>(f: F) -> Self {
        Self::Custom(Box::new(f))
    }
}

impl progress_bar::Catalog for Theme {
    type Class<'a> = ProgressBar;

    fn default<'a>() -> Self::Class<'a> {
        ProgressBar::default()
    }

    fn style(&self, class: &Self::Class<'_>) -> progress_bar::Style {
        let theme = self.cosmic();

        let (active_track, inactive_track) = if theme.is_high_contrast {
            (
                theme.accent_text_color(),
                if theme.is_dark {
                    theme.palette.neutral_6
                } else {
                    theme.palette.neutral_4
                },
            )
        } else {
            (
                theme.accent.base,
                theme.background(self.transparent).divider,
            )
        };
        let border = Border {
            radius: theme.corner_radii.radius_xl.to_radius(),
            color: if theme.is_high_contrast && !theme.is_dark {
                self.current_container().component.border.to_color()
            } else {
                Color::TRANSPARENT
            },
            width: if theme.is_high_contrast && !theme.is_dark {
                1.
            } else {
                0.
            },
        };
        match class {
            ProgressBar::Primary => progress_bar::Style {
                background: inactive_track.to_color().into(),
                bar: active_track.to_color().into(),
                border,
            },
            ProgressBar::Success => progress_bar::Style {
                background: inactive_track.to_color().into(),
                bar: theme.success.base.to_color().into(),
                border,
            },
            ProgressBar::Danger => progress_bar::Style {
                background: inactive_track.to_color().into(),
                bar: theme.destructive.base.to_color().into(),
                border,
            },
            ProgressBar::Custom(f) => f(self),
        }
    }
}

/*
 * Rule
 */
#[derive(Default)]
pub enum Rule {
    #[default]
    Default,
    LightDivider,
    HeavyDivider,
    Custom(Box<dyn Fn(&Theme) -> rule::Style>),
}

impl Rule {
    pub fn custom<F: Fn(&Theme) -> rule::Style + 'static>(f: F) -> Self {
        Self::Custom(Box::new(f))
    }
}

impl rule::Catalog for Theme {
    type Class<'a> = Rule;

    fn default<'a>() -> Self::Class<'a> {
        Rule::default()
    }

    fn style(&self, class: &Self::Class<'_>) -> rule::Style {
        match class {
            Rule::Default => rule::Style {
                color: self.current_container().divider.to_color(),
                radius: 0.0.into(),
                fill_mode: rule::FillMode::Full,
                snap: true,
            },
            Rule::LightDivider => rule::Style {
                color: self.current_container().divider.to_color(),
                radius: 0.0.into(),
                fill_mode: rule::FillMode::Padded(8),
                snap: true,
            },
            Rule::HeavyDivider => rule::Style {
                color: self.current_container().divider.to_color(),
                radius: 2.0.into(),
                fill_mode: rule::FillMode::Full,
                snap: true,
            },
            Rule::Custom(f) => f(self),
        }
    }
}

#[derive(Default, Clone, Copy)]
pub enum Scrollable {
    #[default]
    Permanent,
    Minimal,
}

/*
 * Scrollable
 */
impl scrollable::Catalog for Theme {
    type Class<'a> = Scrollable;

    fn default<'a>() -> Self::Class<'a> {
        Scrollable::default()
    }

    fn style(&self, class: &Self::Class<'_>, status: scrollable::Status) -> scrollable::Style {
        match status {
            scrollable::Status::Active {
                is_horizontal_scrollbar_disabled,
                is_vertical_scrollbar_disabled,
            } => {
                let cosmic = self.cosmic();
                let neutral_5 = cosmic.palette.neutral_5.with_alpha(0.7);
                let neutral_6 = cosmic.palette.neutral_6.with_alpha(0.7);
                let mut a = scrollable::Style {
                    container: iced_container::transparent(self),
                    vertical_rail: scrollable::Rail {
                        border: Border {
                            radius: cosmic.corner_radii.radius_s.to_radius(),
                            ..Default::default()
                        },
                        background: None,
                        scroller: scrollable::Scroller {
                            background: if cosmic.is_dark {
                                neutral_6.to_background()
                            } else {
                                neutral_5.to_background()
                            },
                            border: Border {
                                radius: cosmic.corner_radii.radius_s.to_radius(),
                                ..Default::default()
                            },
                        },
                    },
                    horizontal_rail: scrollable::Rail {
                        border: Border {
                            radius: cosmic.corner_radii.radius_s.to_radius(),
                            ..Default::default()
                        },
                        background: None,
                        scroller: scrollable::Scroller {
                            background: if cosmic.is_dark {
                                neutral_6.to_background()
                            } else {
                                neutral_5.to_background()
                            },
                            border: Border {
                                radius: cosmic.corner_radii.radius_s.to_radius(),
                                ..Default::default()
                            },
                        },
                    },
                    gap: None,
                    // Middle-click autoscroll overlay; the app has its own edge
                    // autoscroll and never enables this one
                    auto_scroll: AutoScroll {
                        background: Color::TRANSPARENT.into(),
                        border: Border::default(),
                        shadow: Shadow::default(),
                        icon: Color::TRANSPARENT,
                    },
                };
                let small_widget_container = self.current_container().small_widget.with_alpha(0.7);

                if matches!(class, Scrollable::Permanent) {
                    a.horizontal_rail.background =
                        Some(Background::Color(small_widget_container.to_color()));
                    a.vertical_rail.background =
                        Some(Background::Color(small_widget_container.to_color()));
                }

                a
            }
            scrollable::Status::Hovered { .. } | scrollable::Status::Dragged { .. } => {
                let cosmic = self.cosmic();
                // Only the rail under the pointer, or being dragged, brightens
                let (horizontal_active, vertical_active) = match status {
                    scrollable::Status::Hovered {
                        is_horizontal_scrollbar_hovered,
                        is_vertical_scrollbar_hovered,
                        ..
                    } => (
                        is_horizontal_scrollbar_hovered,
                        is_vertical_scrollbar_hovered,
                    ),
                    scrollable::Status::Dragged {
                        is_horizontal_scrollbar_dragged,
                        is_vertical_scrollbar_dragged,
                        ..
                    } => (
                        is_horizontal_scrollbar_dragged,
                        is_vertical_scrollbar_dragged,
                    ),
                    scrollable::Status::Active { .. } => (false, false),
                };
                let base = if cosmic.is_dark {
                    cosmic.palette.neutral_6.with_alpha(0.7)
                } else {
                    cosmic.palette.neutral_5.with_alpha(0.7)
                };
                let active = over(cosmic.palette.neutral_0.with_alpha(0.2), base);
                let scroller = |is_active: bool| scrollable::Scroller {
                    background: if is_active { active } else { base }.to_background(),
                    border: Border {
                        radius: cosmic.corner_radii.radius_s.to_radius(),
                        ..Default::default()
                    },
                };
                let mut a: scrollable::Style = scrollable::Style {
                    container: iced_container::Style::default(),
                    vertical_rail: scrollable::Rail {
                        border: Border {
                            radius: cosmic.corner_radii.radius_s.to_radius(),
                            ..Default::default()
                        },
                        background: None,
                        scroller: scroller(vertical_active),
                    },
                    horizontal_rail: scrollable::Rail {
                        border: Border {
                            radius: cosmic.corner_radii.radius_s.to_radius(),
                            ..Default::default()
                        },
                        background: None,
                        scroller: scroller(horizontal_active),
                    },
                    gap: None,
                    // Middle-click autoscroll overlay; the app has its own edge
                    // autoscroll and never enables this one
                    auto_scroll: AutoScroll {
                        background: Color::TRANSPARENT.into(),
                        border: Border::default(),
                        shadow: Shadow::default(),
                        icon: Color::TRANSPARENT,
                    },
                };

                if matches!(class, Scrollable::Permanent) {
                    let small_widget_container =
                        self.current_container().small_widget.with_alpha(0.7);

                    a.horizontal_rail.background =
                        Some(Background::Color(small_widget_container.to_color()));
                    a.vertical_rail.background =
                        Some(Background::Color(small_widget_container.to_color()));
                }

                a
            }
        }
    }
}

#[derive(Clone, Default)]
pub enum Svg {
    /// Apply a custom appearance filter
    Custom(Rc<dyn Fn(&Theme) -> svg::Style>),
    /// No filtering is applied
    #[default]
    Default,
}

impl Svg {
    pub fn custom<F: Fn(&Theme) -> svg::Style + 'static>(f: F) -> Self {
        Self::Custom(Rc::new(f))
    }
}

impl svg::Catalog for Theme {
    type Class<'a> = Svg;

    fn default<'a>() -> Self::Class<'a> {
        Svg::default()
    }

    fn style(&self, class: &Self::Class<'_>, status: svg::Status) -> svg::Style {
        #[allow(clippy::match_same_arms)]
        match class {
            Svg::Default => svg::Style::default(),
            Svg::Custom(appearance) => appearance(self),
        }
    }
}

/*
 * Text
 */
#[derive(Clone, Copy, Default)]
pub enum Text {
    Accent,
    #[default]
    Default,
    Color(Color),
    // A plain fn pointer rather than `dyn Fn`, since this must be `Copy`
    Custom(fn(&Theme) -> crate::ui::widget::text::Style),
}

impl From<Color> for Text {
    fn from(color: Color) -> Self {
        Self::Color(color)
    }
}

// This crate's `ui::widget::text::Style` adds `selected_fill` and
// `selected_text_color` to iced's `text::Style`, which has only `color`.
// `Theme` implements this crate's catalog for vendored selectable `Text` and
// iced's catalog for the plain `Text` used internally by iced widgets.
// `Text::Custom` returns the style with selection colours, and iced's
// implementation narrows it to `color`.
impl crate::ui::widget::text::Catalog for Theme {
    type Class<'a> = Text;

    fn default<'a>() -> Self::Class<'a> {
        Text::default()
    }

    fn style(&self, class: &Self::Class<'_>) -> crate::ui::widget::text::Style {
        let selected_fill = self.cosmic().accent.base.to_color();
        let selected_text_color = Some(self.cosmic().on_accent_color().to_color());
        match class {
            Text::Accent => crate::ui::widget::text::Style {
                color: Some(self.cosmic().accent_text_color().to_color()),
                selected_fill,
                selected_text_color,
            },
            Text::Default => crate::ui::widget::text::Style {
                color: None,
                selected_fill,
                selected_text_color,
            },
            Text::Color(c) => crate::ui::widget::text::Style {
                color: Some(*c),
                selected_fill,
                selected_text_color,
            },
            Text::Custom(f) => f(self),
        }
    }
}

impl iced::widget::text::Catalog for Theme {
    type Class<'a> = Text;

    fn default<'a>() -> Self::Class<'a> {
        Text::default()
    }

    fn style(&self, class: &Self::Class<'_>) -> iced::widget::text::Style {
        iced::widget::text::Style {
            color: <Self as crate::ui::widget::text::Catalog>::style(self, class).color,
        }
    }
}

#[derive(Copy, Clone, Default)]
pub enum TextInput {
    #[default]
    Default,
    Search,
}

/*
 * Text Input
 */
impl text_input::Catalog for Theme {
    type Class<'a> = TextInput;

    fn default<'a>() -> Self::Class<'a> {
        TextInput::default()
    }

    fn style(&self, class: &Self::Class<'_>, status: text_input::Status) -> text_input::Style {
        let palette = self.cosmic();
        let bg = self.current_container().small_widget.with_alpha(0.25);

        let neutral_9 = palette.palette.neutral_9;
        let value = neutral_9.to_color();
        let placeholder = neutral_9.with_alpha(0.7).to_color();
        let selection = palette.accent.base.to_color();

        let mut appearance = match class {
            TextInput::Default => text_input::Style {
                background: bg.to_color().into(),
                border: Border {
                    radius: palette.corner_radii.radius_s.to_radius(),
                    width: 1.0,
                    color: self.current_container().component.divider.to_color(),
                },
                icon: self.current_container().on.to_color(),
                placeholder,
                value,
                selection,
            },
            TextInput::Search => text_input::Style {
                background: bg.to_color().into(),
                border: Border {
                    radius: palette.corner_radii.radius_m.to_radius(),
                    ..Default::default()
                },
                icon: self.current_container().on.to_color(),
                placeholder,
                value,
                selection,
            },
        };

        match status {
            text_input::Status::Active => appearance,
            text_input::Status::Hovered => {
                let bg = self.current_container().small_widget.with_alpha(0.25);

                match class {
                    TextInput::Default => text_input::Style {
                        background: bg.to_color().into(),
                        border: Border {
                            radius: palette.corner_radii.radius_s.to_radius(),
                            width: 1.0,
                            color: self.current_container().on.to_color(),
                        },
                        icon: self.current_container().on.to_color(),
                        placeholder,
                        value,
                        selection,
                    },
                    TextInput::Search => text_input::Style {
                        background: bg.to_color().into(),
                        border: Border {
                            radius: palette.corner_radii.radius_m.to_radius(),
                            ..Default::default()
                        },
                        icon: self.current_container().on.to_color(),
                        placeholder,
                        value,
                        selection,
                    },
                }
            }
            text_input::Status::Focused { is_hovered } => {
                let bg = self.current_container().small_widget.with_alpha(0.25);

                match class {
                    TextInput::Default => text_input::Style {
                        background: bg.to_color().into(),
                        border: Border {
                            radius: palette.corner_radii.radius_s.to_radius(),
                            width: 1.0,
                            color: palette.accent.base.to_color(),
                        },
                        icon: self.current_container().on.to_color(),
                        placeholder,
                        value,
                        selection,
                    },
                    TextInput::Search => text_input::Style {
                        background: bg.to_color().into(),
                        border: Border {
                            radius: palette.corner_radii.radius_m.to_radius(),
                            ..Default::default()
                        },
                        icon: self.current_container().on.to_color(),
                        placeholder,
                        value,
                        selection,
                    },
                }
            }
            text_input::Status::Disabled => {
                appearance.background = match appearance.background {
                    Background::Color(color) => Background::Color(Color {
                        a: color.a * 0.5,
                        ..color
                    }),
                    Background::Gradient(gradient) => {
                        Background::Gradient(gradient.scale_alpha(0.5))
                    }
                };
                appearance.border.color.a /= 2.;
                appearance.icon.a /= 2.;
                appearance.placeholder.a /= 2.;
                appearance.value.a /= 2.;
                appearance
            }
        }
    }
}

#[derive(Default)]
pub enum TextEditor<'a> {
    #[default]
    Default,
    Custom(text_editor::StyleFn<'a, Theme>),
}

impl<'a> From<text_editor::StyleFn<'a, Theme>> for TextEditor<'a> {
    fn from(style: text_editor::StyleFn<'a, Theme>) -> Self {
        Self::Custom(style)
    }
}

impl iced::widget::text_editor::Catalog for Theme {
    type Class<'a> = TextEditor<'a>;

    fn default<'a>() -> Self::Class<'a> {
        TextEditor::default()
    }

    fn style(
        &self,
        class: &Self::Class<'_>,
        status: iced::widget::text_editor::Status,
    ) -> iced::widget::text_editor::Style {
        if let TextEditor::Custom(style) = class {
            return style(self, status);
        }

        let cosmic = self.cosmic();

        let selection = cosmic.accent.base.to_color();
        let value = cosmic.palette.neutral_9.to_color();
        let placeholder = cosmic.palette.neutral_9.with_alpha(0.7).to_color();
        let icon: Color = cosmic.background(self.transparent).on.to_color();

        match status {
            iced::widget::text_editor::Status::Active
            | iced::widget::text_editor::Status::Hovered
            | iced::widget::text_editor::Status::Disabled => iced::widget::text_editor::Style {
                background: cosmic.bg_color().to_color().into(),
                border: Border {
                    radius: cosmic.corner_radii.radius_0.to_radius(),
                    width: f32::from(cosmic.space_xxxs()),
                    color: cosmic.bg_divider().to_color(),
                },
                placeholder,
                value,
                selection,
            },
            iced::widget::text_editor::Status::Focused { is_hovered } => {
                iced::widget::text_editor::Style {
                    background: cosmic.bg_color().to_color().into(),
                    border: Border {
                        radius: cosmic.corner_radii.radius_0.to_radius(),
                        width: f32::from(cosmic.space_xxxs()),
                        color: cosmic.accent.base.to_color(),
                    },
                    placeholder,
                    value,
                    selection,
                }
            }
        }
    }
}

impl Base for Theme {
    fn default(preference: iced::theme::Mode) -> Self {
        match preference {
            iced::theme::Mode::Light => Theme::light(),
            iced::theme::Mode::Dark | iced::theme::Mode::None => Theme::dark(),
        }
    }

    fn mode(&self) -> iced::theme::Mode {
        if self.theme_type.is_dark() {
            iced::theme::Mode::Dark
        } else {
            iced::theme::Mode::Light
        }
    }

    fn base(&self) -> iced::theme::Style {
        iced::theme::Style {
            background_color: self.cosmic().bg_color().to_color(),
            text_color: self.cosmic().on_bg_color().to_color(),
        }
    }

    fn palette(&self) -> Option<iced::theme::Palette> {
        Some(iced::theme::Palette {
            primary: self.cosmic().accent.base.to_color(),
            success: self.cosmic().success.base.to_color(),
            warning: self.cosmic().warning.base.to_color(),
            danger: self.cosmic().destructive.base.to_color(),
            background: self.cosmic().bg_color().to_color(),
            text: self.cosmic().on_bg_color().to_color(),
        })
    }

    fn name(&self) -> &str {
        self.cosmic().name
    }
}
