// SPDX-License-Identifier: GPL-3.0-only

//! User theme files.
//!
//! A theme file overrides parts of a built-in palette. `themes/dark.ron` is
//! applied to the built-in dark palette and `themes/light.ron` to the light
//! one, so the file's name decides what it starts from and every field in it
//! is optional. A missing file means the built-in palette is used unchanged,
//! which is not an error.
//!
//! Both files are read once, during startup. Nothing reloads them, so the
//! palette can be leaked and handed out as `&'static` exactly as the built-in
//! tables are.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Once, OnceLock};

use palette::Srgba;
use serde::Deserialize;

use super::Spacing;
use super::palette::{Component, Container, CornerRadii, Palette, Raw};

/// How far a state colour moves away from its base, as a fraction of the
/// distance to the text colour. These decide how a custom theme feels under
/// the pointer, and they are the numbers to reach for when it feels wrong.
const HOVER_MIX: f32 = 0.10;
const PRESSED_MIX: f32 = 0.20;
const FOCUS_MIX: f32 = 0.20;
const SELECTED_MIX: f32 = 0.10;
const BORDER_MIX: f32 = 0.15;
/// Alpha applied to a colour to show it is unavailable
const DISABLED_ALPHA: f32 = 0.5;
/// Alpha of a divider drawn in the text colour
const DIVIDER_ALPHA: f32 = 0.2;
/// Alpha of the subtle fill behind a small widget
const SMALL_WIDGET_ALPHA: f32 = 0.1;
/// How strongly `neutral_tint` colours the otherwise grey ramp
const NEUTRAL_TINT_MIX: f32 = 0.25;

/// A colour written as `"#rgb"`, `"#rrggbb"` or `"#rrggbbaa"`
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub Srgba);

impl TryFrom<String> for Color {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let digits = text.strip_prefix('#').unwrap_or(&text);
        let channel = |at: usize, width: usize| -> Option<f32> {
            let part = digits.get(at..at + width)?;
            let value = u8::from_str_radix(part, 16).ok()?;
            // A single digit means both nibbles, so "f" is "ff"
            let value = if width == 1 { value * 17 } else { value };
            Some(f32::from(value) / 255.0)
        };
        let (r, g, b, a) = match digits.len() {
            3 => (channel(0, 1), channel(1, 1), channel(2, 1), Some(1.0)),
            4 => (channel(0, 1), channel(1, 1), channel(2, 1), channel(3, 1)),
            6 => (channel(0, 2), channel(2, 2), channel(4, 2), Some(1.0)),
            8 => (channel(0, 2), channel(2, 2), channel(4, 2), channel(6, 2)),
            _ => (None, None, None, None),
        };
        match (r, g, b, a) {
            (Some(r), Some(g), Some(b), Some(a)) => Ok(Self(Srgba::new(r, g, b, a))),
            _ => Err(format!(
                "{text:?} is not a colour; write one as \"#rrggbb\" or \"#rrggbbaa\""
            )),
        }
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::try_from(text).map_err(serde::de::Error::custom)
    }
}

fn mix(from: Srgba, to: Srgba, amount: f32) -> Srgba {
    let lerp = |a: f32, b: f32| a + (b - a) * amount;
    Srgba::new(
        lerp(from.red, to.red),
        lerp(from.green, to.green),
        lerp(from.blue, to.blue),
        from.alpha,
    )
}

fn with_alpha(color: Srgba, alpha: f32) -> Srgba {
    Srgba::new(color.red, color.green, color.blue, alpha)
}

/// One channel of an sRGB colour, undone back to light intensity.
///
/// sRGB values are stored with a transfer curve applied, so they cannot be
/// weighed against each other directly: a mid grey is stored near 0.5 but
/// carries about a fifth of the light. Undoing the curve first is what makes
/// the comparison below match what an eye sees.
fn linearize(channel: f32) -> f32 {
    if channel <= 0.040_45 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

/// Relative luminance, as the contrast formula defines it
fn relative_luminance(color: Srgba) -> f32 {
    0.2126f32.mul_add(
        linearize(color.red),
        0.7152f32.mul_add(linearize(color.green), 0.0722 * linearize(color.blue)),
    )
}

/// The contrast ratio between two colours, from 1 to 21
fn contrast(a: Srgba, b: Srgba) -> f32 {
    let (a, b) = (relative_luminance(a), relative_luminance(b));
    let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
    (lighter + 0.05) / (darker + 0.05)
}

const BLACK: Srgba = Srgba::new(0.0, 0.0, 0.0, 1.0);
const WHITE: Srgba = Srgba::new(1.0, 1.0, 1.0, 1.0);

/// A colour with any transparency removed, for use as the bottom of a stack
fn opaque(color: Srgba) -> Srgba {
    Srgba::new(color.red, color.green, color.blue, 1.0)
}

/// `color` painted over `surface`, which is what the eye actually sees.
///
/// A colour may be partly or wholly transparent, and colours carried by the
/// theme file can be. Its own channels then say nothing about what is on
/// screen: a fully transparent fill shows the surface under it, whatever its
/// stored red, green and blue happen to be.
fn over(color: Srgba, surface: Srgba) -> Srgba {
    let alpha = color.alpha.clamp(0.0, 1.0);
    let blend = |fore: f32, back: f32| fore.mul_add(alpha, back * (1.0 - alpha));
    Srgba::new(
        blend(color.red, surface.red),
        blend(color.green, surface.green),
        blend(color.blue, surface.blue),
        1.0,
    )
}

/// Whichever of black and white stays legible on `base` drawn over `surface`.
///
/// Judged against the pressed fill as well as the base, because the states
/// derived below move the fill towards this very colour and so close some of
/// the gap. Picking for the base alone can leave the worst state unreadable.
fn text_on(base: Srgba, surface: Srgba) -> Srgba {
    let visible = over(base, surface);
    let worst_case = |foreground: Srgba| {
        let pressed = over(mix(base, foreground, PRESSED_MIX), surface);
        contrast(visible, foreground).min(contrast(pressed, foreground))
    };
    if worst_case(BLACK) >= worst_case(WHITE) {
        BLACK
    } else {
        WHITE
    }
}

/// Every state of a widget, worked out from the one colour the user gave.
///
/// States move away from the base towards the text colour, so the same rule
/// reads correctly on a light and a dark theme.
fn component_from(base: Srgba, surface: Srgba) -> Component {
    let on = text_on(base, surface);
    Component {
        base,
        hover: mix(base, on, HOVER_MIX),
        pressed: mix(base, on, PRESSED_MIX),
        selected: mix(base, on, SELECTED_MIX),
        selected_text: on,
        focus: mix(base, on, FOCUS_MIX),
        divider: with_alpha(on, DIVIDER_ALPHA),
        on,
        disabled: with_alpha(base, DISABLED_ALPHA),
        on_disabled: with_alpha(on, DISABLED_ALPHA),
        border: mix(base, on, BORDER_MIX),
        disabled_border: with_alpha(mix(base, on, BORDER_MIX), DISABLED_ALPHA),
    }
}

/// A surface and the widgets drawn on it, from the surface colour alone
fn container_from(base: Srgba, component_base: Srgba, surface: Srgba) -> Container {
    let on = text_on(base, surface);
    Container {
        base,
        // Widgets on this surface are seen against the surface itself
        component: component_from(component_base, over(base, surface)),
        divider: with_alpha(on, DIVIDER_ALPHA),
        on,
        small_widget: with_alpha(on, SMALL_WIDGET_ALPHA),
    }
}

/// Put `tint` on every piece of text this surface draws.
///
/// Not just the surface's own text: an inactive tab, a list row and a card
/// all take their colour from the surface's component, so tinting only the
/// container leaves them at their built-in grey.
fn tint_text(container: &mut Container, tint: Srgba) {
    container.on = tint;
    container.divider = with_alpha(tint, DIVIDER_ALPHA);
    container.small_widget = with_alpha(tint, SMALL_WIDGET_ALPHA);
    container.component.on = tint;
    container.component.selected_text = tint;
    container.component.on_disabled = with_alpha(tint, DISABLED_ALPHA);
    container.component.divider = with_alpha(tint, DIVIDER_ALPHA);
}

/// A widget's colours: either one colour with the rest worked out, or the
/// states spelled out individually.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum ComponentOverride {
    /// `accent: "#d65d0e"`
    Base(Color),
    /// `accent: (base: "#d65d0e", hover: "#e07b30")`
    States(Box<ComponentStates>),
}

/// The states of a widget, each optional
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ComponentStates {
    pub base: Option<Color>,
    pub hover: Option<Color>,
    pub pressed: Option<Color>,
    pub selected: Option<Color>,
    pub selected_text: Option<Color>,
    pub focus: Option<Color>,
    pub divider: Option<Color>,
    pub on: Option<Color>,
    pub disabled: Option<Color>,
    pub on_disabled: Option<Color>,
    pub border: Option<Color>,
    pub disabled_border: Option<Color>,
}

impl ComponentOverride {
    /// Apply to `current`.
    ///
    /// A new base means every state that was not named is worked out from it,
    /// because the built-in states belong to the colour being replaced.
    fn apply(&self, current: &Component, surface: Srgba) -> Component {
        let (states, derived) = match self {
            Self::Base(color) => (
                ComponentStates::default(),
                Some(component_from(color.0, surface)),
            ),
            Self::States(states) => (
                (**states).clone(),
                states.base.map(|base| component_from(base.0, surface)),
            ),
        };
        let from = derived.as_ref().unwrap_or(current);
        Component {
            base: states.base.map_or(from.base, |c| c.0),
            hover: states.hover.map_or(from.hover, |c| c.0),
            pressed: states.pressed.map_or(from.pressed, |c| c.0),
            selected: states.selected.map_or(from.selected, |c| c.0),
            selected_text: states.selected_text.map_or(from.selected_text, |c| c.0),
            focus: states.focus.map_or(from.focus, |c| c.0),
            divider: states.divider.map_or(from.divider, |c| c.0),
            on: states.on.map_or(from.on, |c| c.0),
            disabled: states.disabled.map_or(from.disabled, |c| c.0),
            on_disabled: states.on_disabled.map_or(from.on_disabled, |c| c.0),
            border: states.border.map_or(from.border, |c| c.0),
            disabled_border: states.disabled_border.map_or(from.disabled_border, |c| c.0),
        }
    }
}

/// Corner radii: one value for all of them, or each size named
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum RadiiOverride {
    All {
        all: f32,
    },
    Each {
        radius_xs: Option<f32>,
        radius_s: Option<f32>,
        radius_m: Option<f32>,
        radius_l: Option<f32>,
        radius_xl: Option<f32>,
    },
}

impl RadiiOverride {
    fn apply(&self, current: CornerRadii) -> CornerRadii {
        let corners = |value: f32| [value; 4];
        match self {
            // `radius_0` stays zero: it is what styles ask for when they want
            // a square corner
            Self::All { all } => CornerRadii {
                radius_0: current.radius_0,
                radius_xs: corners(*all),
                radius_s: corners(*all),
                radius_m: corners(*all),
                radius_l: corners(*all),
                radius_xl: corners(*all),
            },
            Self::Each {
                radius_xs,
                radius_s,
                radius_m,
                radius_l,
                radius_xl,
            } => CornerRadii {
                radius_0: current.radius_0,
                radius_xs: radius_xs.map_or(current.radius_xs, corners),
                radius_s: radius_s.map_or(current.radius_s, corners),
                radius_m: radius_m.map_or(current.radius_m, corners),
                radius_l: radius_l.map_or(current.radius_l, corners),
                radius_xl: radius_xl.map_or(current.radius_xl, corners),
            },
        }
    }
}

/// Spacing: a named preset, or each step given in logical pixels
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum SpacingOverride {
    Preset(SpacingPreset),
    Each(Box<SpacingSteps>),
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub enum SpacingPreset {
    Compact,
    Standard,
    Spacious,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SpacingSteps {
    pub space_none: Option<u16>,
    pub space_xxxs: Option<u16>,
    pub space_xxs: Option<u16>,
    pub space_xs: Option<u16>,
    pub space_s: Option<u16>,
    pub space_m: Option<u16>,
    pub space_l: Option<u16>,
    pub space_xl: Option<u16>,
    pub space_xxl: Option<u16>,
    pub space_xxxl: Option<u16>,
}

impl SpacingOverride {
    fn apply(&self, current: Spacing) -> Spacing {
        match self {
            Self::Preset(preset) => {
                let scale = match preset {
                    SpacingPreset::Compact => 0.75,
                    SpacingPreset::Standard => 1.0,
                    SpacingPreset::Spacious => 1.5,
                };
                let step = |value: u16| {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let scaled = (f32::from(value) * scale).round() as u16;
                    scaled
                };
                Spacing {
                    space_none: current.space_none,
                    space_xxxs: step(current.space_xxxs),
                    space_xxs: step(current.space_xxs),
                    space_xs: step(current.space_xs),
                    space_s: step(current.space_s),
                    space_m: step(current.space_m),
                    space_l: step(current.space_l),
                    space_xl: step(current.space_xl),
                    space_xxl: step(current.space_xxl),
                    space_xxxl: step(current.space_xxxl),
                }
            }
            Self::Each(steps) => Spacing {
                space_none: steps.space_none.unwrap_or(current.space_none),
                space_xxxs: steps.space_xxxs.unwrap_or(current.space_xxxs),
                space_xxs: steps.space_xxs.unwrap_or(current.space_xxs),
                space_xs: steps.space_xs.unwrap_or(current.space_xs),
                space_s: steps.space_s.unwrap_or(current.space_s),
                space_m: steps.space_m.unwrap_or(current.space_m),
                space_l: steps.space_l.unwrap_or(current.space_l),
                space_xl: steps.space_xl.unwrap_or(current.space_xl),
                space_xxl: steps.space_xxl.unwrap_or(current.space_xxl),
                space_xxxl: steps.space_xxxl.unwrap_or(current.space_xxxl),
            },
        }
    }
}

/// The grey ramp the interface is built from. Only these of the raw palette's
/// colours are drawn anywhere, so only these can be set.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct NeutralOverride {
    pub neutral_0: Option<Color>,
    pub neutral_1: Option<Color>,
    pub neutral_2: Option<Color>,
    pub neutral_3: Option<Color>,
    pub neutral_4: Option<Color>,
    pub neutral_5: Option<Color>,
    pub neutral_6: Option<Color>,
    pub neutral_7: Option<Color>,
    pub neutral_8: Option<Color>,
    pub neutral_9: Option<Color>,
    pub neutral_10: Option<Color>,
}

/// A theme file.
///
/// Every field is optional and replaces the matching part of the built-in
/// palette. Fields left out keep whatever the built-in palette had.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ThemeFile {
    /// Shown nowhere yet; kept so a theme can identify itself
    pub name: Option<String>,
    pub accent: Option<ComponentOverride>,
    pub success: Option<ComponentOverride>,
    pub warning: Option<ComponentOverride>,
    pub destructive: Option<ComponentOverride>,
    pub button: Option<ComponentOverride>,
    pub list_button: Option<ComponentOverride>,
    /// Text drawn in the accent colour. Leaving it out keeps the built-in
    /// choice, which may be none, meaning the accent itself is used.
    pub accent_text: Option<Color>,
    /// The window's own surface
    pub bg_color: Option<Color>,
    /// The surface of panels and sidebars
    pub primary_container_bg: Option<Color>,
    /// The surface of controls inside a panel
    pub secondary_container_bg: Option<Color>,
    /// Colours the grey ramp without flattening it
    pub neutral_tint: Option<Color>,
    /// Text colour across the interface
    pub text_tint: Option<Color>,
    /// The wash drawn behind a dialog
    pub shade: Option<Color>,
    pub palette: Option<NeutralOverride>,
    pub corner_radii: Option<RadiiOverride>,
    pub spacing: Option<SpacingOverride>,
    pub is_high_contrast: Option<bool>,
}

/// The field names above, so a typo can be reported rather than ignored
const KNOWN_KEYS: &[&str] = &[
    "name",
    "accent",
    "success",
    "warning",
    "destructive",
    "button",
    "list_button",
    "accent_text",
    "bg_color",
    "primary_container_bg",
    "secondary_container_bg",
    "neutral_tint",
    "text_tint",
    "shade",
    "palette",
    "corner_radii",
    "spacing",
    "is_high_contrast",
];

impl ThemeFile {
    /// The built-in palette with this file applied on top.
    #[must_use]
    pub fn apply(&self, base: &Palette) -> Palette {
        let mut palette = base.clone();

        if let Some(name) = &self.name {
            palette.name = Box::leak(name.clone().into_boxed_str());
        }

        // The grey ramp first: the containers below are built from it
        palette.palette = self.neutral_ramp(&base.palette);

        if let Some(tint) = self.text_tint {
            tint_text(&mut palette.background, tint.0);
            tint_text(&mut palette.primary, tint.0);
            tint_text(&mut palette.secondary, tint.0);
        }

        let component_base = palette.palette.neutral_3;
        // A colour with transparency shows whatever is behind it, so it is
        // judged over that rather than on its own channels. Surfaces sit on
        // the window; the controls below are drawn on panels, dialogs and
        // header bars, which is the primary container rather than the window.
        let surface = opaque(self.bg_color.map_or(base.background.base, |c| c.0));
        // The panel may itself be see-through, in which case what shows under
        // a control is the window beneath the panel, not the panel's own
        // stored channels
        let control_surface = over(
            self.primary_container_bg.map_or(base.primary.base, |c| c.0),
            surface,
        );
        for (over, current, transparent) in [
            (
                self.bg_color,
                &mut palette.background,
                &mut palette.transparent_background,
            ),
            (
                self.primary_container_bg,
                &mut palette.primary,
                &mut palette.transparent_primary,
            ),
            (
                self.secondary_container_bg,
                &mut palette.secondary,
                &mut palette.transparent_secondary,
            ),
        ] {
            let Some(color) = over else {
                continue;
            };
            let mut built = container_from(color.0, component_base, surface);
            if let Some(tint) = self.text_tint {
                tint_text(&mut built, tint.0);
            }
            // Blur is never enabled, so the transparent twin only has to stay
            // in step with the surface actually drawn
            *transparent = built.clone();
            *current = built;
        }

        for (over, current) in [
            (&self.accent, &mut palette.accent),
            (&self.success, &mut palette.success),
            (&self.warning, &mut palette.warning),
            (&self.destructive, &mut palette.destructive),
            (&self.button, &mut palette.button),
            (&self.list_button, &mut palette.list_button),
        ] {
            if let Some(over) = over {
                *current = over.apply(current, control_surface);
            }
        }

        // A button keeps the colour of the thing it means, so these follow
        // their component rather than being set on their own
        for (over, current, source) in [
            (
                &self.accent,
                &mut palette.accent_button,
                palette.accent.clone(),
            ),
            (
                &self.success,
                &mut palette.success_button,
                palette.success.clone(),
            ),
            (
                &self.warning,
                &mut palette.warning_button,
                palette.warning.clone(),
            ),
            (
                &self.destructive,
                &mut palette.destructive_button,
                palette.destructive.clone(),
            ),
        ] {
            if over.is_some() {
                *current = source;
            }
        }

        if let Some(accent_text) = self.accent_text {
            palette.accent_text = Some(accent_text.0);
        }
        if let Some(shade) = self.shade {
            palette.shade = shade.0;
        }
        if let Some(radii) = &self.corner_radii {
            palette.corner_radii = radii.apply(base.corner_radii);
        }
        if let Some(high_contrast) = self.is_high_contrast {
            palette.is_high_contrast = high_contrast;
        }

        palette
    }

    /// The grey ramp, tinted and then overridden step by step
    fn neutral_ramp(&self, base: &Raw) -> Raw {
        let mut raw = base.clone();
        if let Some(tint) = self.neutral_tint {
            for step in [
                &mut raw.neutral_0,
                &mut raw.neutral_1,
                &mut raw.neutral_2,
                &mut raw.neutral_3,
                &mut raw.neutral_4,
                &mut raw.neutral_5,
                &mut raw.neutral_6,
                &mut raw.neutral_7,
                &mut raw.neutral_8,
                &mut raw.neutral_9,
                &mut raw.neutral_10,
            ] {
                // Mixed, not replaced, so the ramp keeps its light-to-dark
                // shape and only takes on the colour
                *step = mix(*step, tint.0, NEUTRAL_TINT_MIX);
            }
        }
        if let Some(over) = &self.palette {
            let set = |target: &mut Srgba, value: Option<Color>| {
                if let Some(value) = value {
                    *target = value.0;
                }
            };
            set(&mut raw.neutral_0, over.neutral_0);
            set(&mut raw.neutral_1, over.neutral_1);
            set(&mut raw.neutral_2, over.neutral_2);
            set(&mut raw.neutral_3, over.neutral_3);
            set(&mut raw.neutral_4, over.neutral_4);
            set(&mut raw.neutral_5, over.neutral_5);
            set(&mut raw.neutral_6, over.neutral_6);
            set(&mut raw.neutral_7, over.neutral_7);
            set(&mut raw.neutral_8, over.neutral_8);
            set(&mut raw.neutral_9, over.neutral_9);
            set(&mut raw.neutral_10, over.neutral_10);
        }
        raw
    }
}

/// Parse a theme file.
///
/// Optional fields are written plainly, `accent: "#d65d0e"` rather than
/// `accent: Some("#d65d0e")`, which is the whole point of having a friendlier
/// format than the one this replaces.
fn from_ron(text: &str) -> Result<ThemeFile, ron::error::SpannedError> {
    ron::Options::default()
        .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
        .from_str(text)
}

static DARK_OVERRIDE: OnceLock<Option<&'static Palette>> = OnceLock::new();
static LIGHT_OVERRIDE: OnceLock<Option<&'static Palette>> = OnceLock::new();
/// Spacing is not a colour, so it is not kept per mode: whichever file sets
/// it wins, with the dark one asked first.
static SPACING_OVERRIDE: OnceLock<Option<SpacingOverride>> = OnceLock::new();
/// Whether a theme file set the standard button's colours, per mode. A
/// standard button normally inherits its container's text colour, which only
/// works while its fill stays a near-neutral close to that container.
///
/// Kept apart for dark and light: a dark theme that restyles its buttons must
/// not stop the light theme's buttons from inheriting, since the light
/// palette is then still entirely the built-in one.
static DARK_BUTTON_IS_CUSTOM: OnceLock<bool> = OnceLock::new();
static LIGHT_BUTTON_IS_CUSTOM: OnceLock<bool> = OnceLock::new();

/// Whether a standard button of this mode should paint its own text rather
/// than inherit the colour of whatever it sits on.
#[must_use]
pub fn button_is_custom(is_dark: bool) -> bool {
    let flag = if is_dark {
        &DARK_BUTTON_IS_CUSTOM
    } else {
        &LIGHT_BUTTON_IS_CUSTOM
    };
    flag.get().copied().unwrap_or(false)
}

/// The spacing widgets lay out with: the steps for the configured density,
/// with any set by a theme file applied on top.
///
/// Applied on top rather than replacing, so changing the density still moves
/// everything a theme did not pin.
#[must_use]
pub fn spacing(density_steps: Spacing) -> Spacing {
    SPACING_OVERRIDE
        .get()
        .and_then(Option::as_ref)
        .map_or(density_steps, |over| over.apply(density_steps))
}

/// The directory theme files are read from
#[must_use]
pub fn themes_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| crate::home_dir().join(".config"))
        .join("earth-files")
        .join("themes")
}

/// Read `themes/dark.ron` and `themes/light.ron` and apply them to the
/// built-in palettes.
///
/// Called once during startup. A file that is absent leaves its palette
/// alone, and one that cannot be read or parsed is reported and then ignored:
/// the file belongs to the user, who will want to correct it in place, so it
/// is never moved aside the way a malformed config is.
pub fn load() {
    // Genuinely once. Every host that builds a chooser calls this, including
    // one that already called it at startup, and the body reads both files,
    // builds palettes and leaks them: repeating it would leak another pair
    // per call and then throw the result away, because the locks below only
    // accept their first value.
    static LOADED: Once = Once::new();
    LOADED.call_once(load_once);
}

/// How many times the work below has actually run. Only the test reads it.
static LOAD_RUNS: AtomicUsize = AtomicUsize::new(0);

fn load_once() {
    LOAD_RUNS.fetch_add(1, Ordering::Relaxed);
    let dir = themes_dir();
    let dark = read_file(&dir.join("dark.ron"));
    let light = read_file(&dir.join("light.ron"));

    let spacing = dark
        .as_ref()
        .and_then(|theme| theme.spacing.clone())
        .or_else(|| light.as_ref().and_then(|theme| theme.spacing.clone()));
    let _ = SPACING_OVERRIDE.set(spacing);

    let _ = DARK_BUTTON_IS_CUSTOM.set(dark.as_ref().is_some_and(|theme| theme.button.is_some()));
    let _ = LIGHT_BUTTON_IS_CUSTOM.set(light.as_ref().is_some_and(|theme| theme.button.is_some()));

    let _ = DARK_OVERRIDE
        .set(dark.map(|theme| &*Box::leak(Box::new(theme.apply(&super::palette::DARK)))));
    let _ = LIGHT_OVERRIDE
        .set(light.map(|theme| &*Box::leak(Box::new(theme.apply(&super::palette::LIGHT)))));
}

fn read_file(path: &Path) -> Option<ThemeFile> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                log::warn!("failed to read {}: {err}", path.display());
            }
            return None;
        }
    };
    let theme: ThemeFile = match from_ron(&text) {
        Ok(theme) => theme,
        Err(err) => {
            log::error!(
                "failed to parse {}: {err}. The built-in theme is used instead",
                path.display()
            );
            return None;
        }
    };
    warn_unknown_keys(path, &text);
    log::info!("applied theme {}", path.display());
    Some(theme)
}

/// Report keys the file sets that this version does not know.
///
/// They are ignored rather than refused, so a file written for a later
/// version still loads, but a typo that silently does nothing is worth a
/// line in the log.
fn warn_unknown_keys(path: &Path, text: &str) {
    let Ok(ron::Value::Map(map)) = ron::from_str::<ron::Value>(text) else {
        return;
    };
    for key in map.keys() {
        if let ron::Value::String(key) = key
            && !KNOWN_KEYS.contains(&key.as_str())
        {
            log::warn!(
                "{} sets unknown key {key:?}, which is ignored",
                path.display()
            );
        }
    }
}

/// The palette to draw a dark theme with, user file included
#[must_use]
pub fn dark() -> &'static Palette {
    DARK_OVERRIDE
        .get()
        .copied()
        .flatten()
        .unwrap_or(&super::palette::DARK)
}

/// The palette to draw a light theme with, user file included
#[must_use]
pub fn light() -> &'static Palette {
    LIGHT_OVERRIDE
        .get()
        .copied()
        .flatten()
        .unwrap_or(&super::palette::LIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::palette::{DARK, LIGHT};

    fn parse(text: &str) -> ThemeFile {
        from_ron(text).expect("the theme should parse")
    }

    fn color(hex: &str) -> Srgba {
        Color::try_from(hex.to_string()).unwrap().0
    }

    fn read_and_apply(path: &Path, base: &Palette) -> Option<Palette> {
        Some(read_file(path)?.apply(base))
    }

    #[test]
    fn colours_are_read_from_hex() {
        let rgb = color("#d65d0e");
        assert!((rgb.red - 0.839_215_7).abs() < 1e-6);
        assert!((rgb.green - 0.364_705_9).abs() < 1e-6);
        assert!((rgb.blue - 0.054_901_96).abs() < 1e-6);
        assert!((rgb.alpha - 1.0).abs() < 1e-6);

        // Short form, alpha, and a missing hash all work
        assert_eq!(color("#fff"), Srgba::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(color("000000ff"), Srgba::new(0.0, 0.0, 0.0, 1.0));
        assert!((color("#00000080").alpha - 0.501_960_8).abs() < 1e-6);

        for bad in ["", "#", "#ff", "#gggggg", "#1234567"] {
            assert!(
                Color::try_from(bad.to_string()).is_err(),
                "{bad:?} should not parse"
            );
        }
    }

    #[test]
    fn an_empty_theme_changes_nothing() {
        assert_eq!(parse("()").apply(&DARK), DARK.clone());
        assert_eq!(parse("()").apply(&LIGHT), LIGHT.clone());
    }

    #[test]
    fn a_bare_colour_sets_the_base_and_derives_the_rest() {
        let applied = parse("(accent: \"#d65d0e\")").apply(&DARK);

        assert_eq!(applied.accent.base, color("#d65d0e"));
        assert_ne!(applied.accent.hover, DARK.accent.hover);
        assert_ne!(applied.accent.pressed, DARK.accent.pressed);
        // Hover moves away from the new base, it is not left on the old accent
        assert_ne!(applied.accent.hover, applied.accent.base);
        // A button of the same meaning follows its component
        assert_eq!(applied.accent_button.base, applied.accent.base);
        // Nothing unrelated moved
        assert_eq!(applied.destructive, DARK.destructive);
        assert_eq!(applied.background, DARK.background);
        assert_eq!(applied.spacing, DARK.spacing);
        assert_eq!(applied.corner_radii, DARK.corner_radii);
    }

    #[test]
    fn named_states_win_over_derived_ones() {
        let applied = parse("(accent: (base: \"#000000\", hover: \"#ff0000\"))").apply(&DARK);
        assert_eq!(applied.accent.base, color("#000000"));
        assert_eq!(applied.accent.hover, color("#ff0000"));
        // Not named, so worked out from the new base rather than kept
        assert_eq!(applied.accent.on, color("#ffffff"));
    }

    #[test]
    fn radii_and_spacing_take_both_shapes() {
        let all = parse("(corner_radii: (all: 8.0))").apply(&DARK);
        assert_eq!(all.corner_radii.radius_s, [8.0; 4]);
        assert_eq!(all.corner_radii.radius_xl, [8.0; 4]);
        // A square corner stays square
        assert_eq!(all.corner_radii.radius_0, DARK.corner_radii.radius_0);

        let each = parse("(corner_radii: (radius_s: 4.0))").apply(&DARK);
        assert_eq!(each.corner_radii.radius_s, [4.0; 4]);
        assert_eq!(each.corner_radii.radius_m, DARK.corner_radii.radius_m);

        // Spacing is resolved against the configured density, not stored in
        // the palette, so it is checked where widgets read it
        let steps = Spacing::from(crate::ui::theme::Density::Standard);
        let compact = parse("(spacing: Compact)").spacing.unwrap().apply(steps);
        assert!(compact.space_m < steps.space_m);
        let exact = parse("(spacing: (space_m: 30))")
            .spacing
            .unwrap()
            .apply(steps);
        assert_eq!(exact.space_m, 30);
        assert_eq!(exact.space_s, steps.space_s);
    }

    #[test]
    fn a_background_colour_rebuilds_its_container() {
        let applied = parse("(bg_color: \"#ffffff\")").apply(&DARK);
        assert_eq!(applied.background.base, color("#ffffff"));
        // Text picks itself to stay legible on the new surface
        assert_eq!(applied.background.on, color("#000000"));
        // Blur is never drawn, so the transparent twin keeps in step
        assert_eq!(applied.transparent_background, applied.background);
        assert_eq!(applied.primary, DARK.primary);
    }

    #[test]
    fn the_neutral_ramp_is_tinted_but_keeps_its_shape() {
        let applied = parse("(neutral_tint: \"#ff0000\")").apply(&DARK);

        assert_ne!(applied.palette.neutral_5, DARK.palette.neutral_5);
        assert!(applied.palette.neutral_5.red > DARK.palette.neutral_5.red);
        // Still a ramp: the steps keep their order
        let rising = |raw: &Raw| {
            raw.neutral_0.red < raw.neutral_5.red && raw.neutral_5.red < raw.neutral_10.red
        };
        assert_eq!(rising(&applied.palette), rising(&DARK.palette));

        // An explicit step wins over the tint
        let pinned =
            parse("(neutral_tint: \"#ff0000\", palette: (neutral_5: \"#123456\"))").apply(&DARK);
        assert_eq!(pinned.palette.neutral_5, color("#123456"));
    }

    #[test]
    fn text_tint_reaches_every_surface_and_its_widgets() {
        let applied = parse("(text_tint: \"#ebdbb2\")").apply(&DARK);
        let tint = color("#ebdbb2");
        for container in [&applied.background, &applied.primary, &applied.secondary] {
            assert_eq!(container.on, tint);
            // What an inactive tab, a list row and a card actually read
            assert_eq!(container.component.on, tint);
            assert_eq!(container.component.selected_text, tint);
        }
        // Reaches the widgets of a surface that was itself replaced too
        let rebuilt = parse("(bg_color: \"#282828\", text_tint: \"#ebdbb2\")").apply(&DARK);
        assert_eq!(rebuilt.background.component.on, tint);
    }

    #[test]
    fn the_foreground_is_the_one_with_more_contrast() {
        // Encoded sRGB puts this just under a naive midpoint, so a brightness
        // test picks white, which is the less legible of the two
        let applied = parse("(accent: \"#888800\")").apply(&DARK);
        assert_eq!(applied.accent.on, color("#000000"));
        assert!(contrast(applied.accent.base, applied.accent.on) > 5.0);

        // Clear cases still come out the obvious way
        assert_eq!(
            text_on(color("#ffffff"), color("#ffffff")),
            color("#000000")
        );
        assert_eq!(
            text_on(color("#000000"), color("#000000")),
            color("#ffffff")
        );

        // The pressed fill moves towards the text, and must stay readable
        for hex in ["#888800", "#d65d0e", "#3c3836", "#7c6f64", "#bdae93"] {
            let component = component_from(color(hex), color("#282828"));
            assert!(
                contrast(component.pressed, component.on) > 3.0,
                "{hex} leaves its pressed state at {:.2}:1",
                contrast(component.pressed, component.on)
            );
        }
    }

    #[test]
    fn a_see_through_colour_is_judged_on_what_shows_through_it() {
        // Nothing of the fill is visible, so the surface under it decides
        let on_light = parse("(button: \"#00000000\")").apply(&LIGHT);
        assert_eq!(
            on_light.button.on,
            color("#000000"),
            "text on a clear button over a light surface must be dark"
        );
        let on_dark = parse("(button: \"#00000000\")").apply(&DARK);
        assert_eq!(on_dark.button.on, color("#ffffff"));

        // Half transparent black over a light surface reads as a mid grey.
        // Judging the stored channels alone would call it black and pick
        // white; judging what shows picks dark text and keeps it readable
        // through the pressed state, which is what the rule optimises.
        let half = parse("(button: \"#00000080\")").apply(&LIGHT);
        let panel = opaque(LIGHT.primary.base);
        let visible = over(half.button.base, panel);
        assert_eq!(half.button.on, color("#000000"));
        assert!(contrast(visible, half.button.on) > 3.0);
        let pressed = over(half.button.pressed, panel);
        assert!(contrast(pressed, half.button.on) > 3.0);

        // The panel a theme sets is the one used, not the built-in one
        let dark_panel =
            parse("(primary_container_bg: \"#101010\", button: \"#00000000\")").apply(&LIGHT);
        assert_eq!(dark_panel.button.on, color("#ffffff"));

        // A fill that hides its surface is still judged on itself
        let opaque_fill = parse("(button: \"#ffffff\")").apply(&DARK);
        assert_eq!(opaque_fill.button.on, color("#000000"));
    }

    #[test]
    fn controls_are_judged_on_the_surface_they_are_drawn_on() {
        // Dialogs, panels and header bars paint the primary container under
        // their controls, not the window background
        let applied = parse(
            "(bg_color: \"#000000\", primary_container_bg: \"#ffffff\", accent: \"#00000080\")",
        )
        .apply(&DARK);

        assert_eq!(
            applied.accent.on,
            color("#000000"),
            "a see-through control on a white panel needs dark text"
        );
        assert_eq!(applied.accent_button.on, applied.accent.on);
        // The window's own surface still decides its own text
        assert_eq!(applied.background.on, color("#ffffff"));
    }

    #[test]
    fn a_custom_button_paints_its_own_text() {
        // The style only takes the palette's button text when a theme asked
        // for it, so the built-in look is unchanged
        assert!(!button_is_custom(true));
        assert!(!button_is_custom(false));

        let applied = parse("(button: \"#ffffff\")").apply(&DARK);
        assert_eq!(
            applied.button.on,
            color("#000000"),
            "a white button needs dark text, whoever ends up drawing it"
        );
        let named = parse("(button: (base: \"#ffffff\", on: \"#112233\"))").apply(&DARK);
        assert_eq!(named.button.on, color("#112233"));
    }

    #[test]
    fn restyling_one_mode_leaves_the_other_alone() {
        // Whether a standard button paints its own text is a property of the
        // mode whose file asked for it. A dark theme restyling its buttons
        // must not stop an untouched light theme from inheriting.
        let dark = parse("(button: \"#ffffff\")");
        let light = parse("(text_tint: \"#202020\")");

        // This is what `load` records for each mode
        assert!(dark.button.is_some());
        assert!(
            light.button.is_none(),
            "the light theme touches no buttons, so its own must keep inheriting"
        );

        // And the light palette really is otherwise untouched here
        let applied = light.apply(&LIGHT);
        assert_eq!(applied.button.base, LIGHT.button.base);
        assert_eq!(applied.button.on, LIGHT.button.on);
    }

    #[test]
    fn a_see_through_panel_shows_the_window_under_its_controls() {
        // The panel hides nothing, so a clear control on it shows the black
        // window and needs light text, even though the panel's own stored
        // channels are white
        let applied = parse(
            "(bg_color: \"#000000\", primary_container_bg: \"#ffffff00\", accent: \"#00000000\")",
        )
        .apply(&DARK);
        assert_eq!(applied.accent.on, color("#ffffff"));

        // Half transparent white over the black window reads as mid grey
        let half = parse(
            "(bg_color: \"#000000\", primary_container_bg: \"#ffffff80\", accent: \"#00000000\")",
        )
        .apply(&DARK);
        let panel = over(color("#ffffff80"), color("#000000"));
        assert!(contrast(panel, half.accent.on) > 3.0);

        // An opaque panel still decides for itself
        let solid = parse(
            "(bg_color: \"#000000\", primary_container_bg: \"#ffffff\", accent: \"#00000000\")",
        )
        .apply(&DARK);
        assert_eq!(solid.accent.on, color("#000000"));
    }

    #[test]
    fn loading_repeatedly_does_the_work_once() {
        // Every chooser calls `load`, and the body leaks the palettes it
        // builds. Running it again would leak a second pair and discard them,
        // because the locks keep their first value.
        for _ in 0..5 {
            load();
        }
        assert_eq!(
            LOAD_RUNS.load(Ordering::Relaxed),
            1,
            "the theme files must be read and leaked exactly once"
        );
    }

    #[test]
    fn a_theme_never_writes_the_palette_spacing() {
        // One source of truth: layout asks `theme::spacing()`, and dialogs
        // now do too, so nothing should be reading a second copy
        let applied = parse("(spacing: (space_m: 30))").apply(&DARK);
        assert_eq!(applied.spacing, DARK.spacing);
    }

    #[test]
    fn theme_spacing_reaches_the_layout_api() {
        // What widgets lay out with, not just what the palette records
        let density_steps = Spacing::from(crate::ui::theme::Density::Standard);
        let pinned = SpacingOverride::Each(Box::new(SpacingSteps {
            space_m: Some(30),
            ..SpacingSteps::default()
        }));
        let applied = pinned.apply(density_steps);
        assert_eq!(applied.space_m, 30);
        // Everything the theme did not pin still follows the density
        assert_eq!(applied.space_s, density_steps.space_s);
        assert_ne!(
            Spacing::from(crate::ui::theme::Density::Compact).space_s,
            density_steps.space_s,
            "the density must still move what a theme leaves alone"
        );
    }

    #[test]
    fn an_unknown_key_is_ignored_rather_than_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dark.ron");
        std::fs::write(&path, b"(accent: \"#d65d0e\", from_a_later_version: 3)").unwrap();
        let applied = read_and_apply(&path, &DARK).expect("the rest of the file should still load");
        assert_eq!(applied.accent.base, color("#d65d0e"));
    }

    #[test]
    fn a_missing_file_leaves_the_built_in_palette_alone() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_and_apply(&dir.path().join("dark.ron"), &DARK).is_none());
    }

    #[test]
    fn a_malformed_file_is_reported_and_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dark.ron");
        std::fs::write(&path, b"(accent: \"not a colour\")").unwrap();
        assert!(read_and_apply(&path, &DARK).is_none());
        // The user's file is left where they wrote it, unlike a bad config
        assert!(path.exists());
    }

    #[test]
    fn a_whole_theme_file_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dark.ron");
        std::fs::write(
            &path,
            b"(\n    name: \"gruvbox\",\n    accent: \"#d65d0e\",\n    bg_color: \"#282828\",\n    text_tint: \"#ebdbb2\",\n    destructive: \"#cc241d\",\n    corner_radii: (all: 8.0),\n    spacing: Compact,\n)\n",
        )
        .unwrap();
        let applied = read_and_apply(&path, &DARK).expect("the theme should load");
        assert_eq!(applied.name, "gruvbox");
        assert_eq!(applied.accent.base, color("#d65d0e"));
        assert_eq!(applied.background.base, color("#282828"));
        assert_eq!(applied.background.on, color("#ebdbb2"));
        assert_eq!(applied.corner_radii.radius_m, [8.0; 4]);
    }
}
