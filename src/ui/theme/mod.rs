// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! The app's theme and layout tokens.
//!
//! [`Theme`] selects the palette, surface [`Layer`], blur setting and list
//! position. [`Palette`] holds the colours, and [`style`] implements the
//! vendored widgets' `Catalog` traits.
//!
//! The app's `Config` selects the density used by [`spacing`].

use ::palette::Srgba;
use mundy::{ColorScheme, Interest, Preferences};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use iced::Alignment;

pub mod custom;
pub mod palette;
pub mod style;

pub use palette::{Component, Container as PaletteContainer, CornerRadii, Palette};
// Style selectors for `.class(…)`.
pub use style::{
    Button, Checkbox, Container, MenuBarStyle, ProgressBar, Rule, Scrollable, SegmentedButton, Svg,
    Text, TextEditor, TextInput, menu_bar,
};

/// Straight-alpha "a over b", on non-linear sRGB.
///
/// The style code calls this to lay a translucent state colour over the
/// surface beneath it.
#[must_use]
pub fn over(a: Srgba, b: Srgba) -> Srgba {
    let alpha = (a.alpha + b.alpha * (1.0 - a.alpha)).clamp(0.0, 1.0);
    let channel = |a_c: f32, b_c: f32| {
        ((a_c * a.alpha + b_c * b.alpha * (1.0 - a.alpha)) / alpha).clamp(0.0, 1.0)
    };
    Srgba::new(
        channel(a.red, b.red),
        channel(a.green, b.green),
        channel(a.blue, b.blue),
        alpha,
    )
}

/// Which of the two built-in palettes a [`Theme`] renders with.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemeType {
    /// The built-in dark theme.
    #[default]
    Dark,
    /// The built-in light theme.
    Light,
}

impl ThemeType {
    /// Whether this theme has a dark preference.
    #[must_use]
    #[inline]
    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }

    /// The palette this theme renders with.
    #[must_use]
    #[inline]
    pub fn cosmic(self) -> &'static Palette {
        match self {
            Self::Dark => custom::dark(),
            Self::Light => custom::light(),
        }
    }
}

/// The theme a widget is drawn with.
///
/// [`Palette`] holds the colours; the remaining fields control how widgets use them. Widgets can cheaply
/// clone the theme and adjust `layer` to select a nested surface's container,
/// or `list_item_position` to round only a row's outer corners.
#[must_use]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Theme {
    /// Which built-in palette this theme renders with.
    pub theme_type: ThemeType,
    /// Which surface level the widget being drawn sits on.
    pub layer: Layer,
    /// Whether the surface is blurred. This app never enables blur.
    pub transparent: bool,
    /// Only meaningful for widgets that must be in a list; otherwise ignored.
    pub list_item_position: Option<(Alignment, usize)>,
}

impl Theme {
    /// The colours and metrics this theme renders with.
    ///
    /// The name refers to the COSMIC design system.
    #[inline]
    pub fn cosmic(&self) -> &'static Palette {
        self.theme_type.cosmic()
    }

    /// The built-in dark theme.
    #[inline]
    pub fn dark() -> Self {
        Self {
            theme_type: ThemeType::Dark,
            ..Default::default()
        }
    }

    /// The built-in light theme.
    #[inline]
    pub fn light() -> Self {
        Self {
            theme_type: ThemeType::Light,
            ..Default::default()
        }
    }

    /// The container for the layer this theme is currently drawing.
    ///
    /// Used by styles that inherit their parent surface instead of choosing
    /// a fixed surface.
    #[inline]
    pub fn current_container(&self) -> &'static PaletteContainer {
        let cosmic = self.cosmic();
        match self.layer {
            Layer::Background => cosmic.background(self.transparent),
            Layer::Primary => cosmic.primary(self.transparent),
            Layer::Secondary => cosmic.secondary(self.transparent),
        }
    }

    /// This theme, with a list position attached.
    #[inline]
    pub fn with_list_item_position(&self, position: Option<(Alignment, usize)>) -> Self {
        Self {
            list_item_position: position,
            ..self.clone()
        }
    }

    /// Set which surface level the widget being drawn sits on.
    #[inline]
    pub fn set_layer(&mut self, layer: Layer) {
        self.layer = layer;
    }
}

/// The active theme.
///
/// An atomic stores the two possible [`ThemeType`] values, as [`DENSITY`]
/// does. All other [`Theme`] fields use their defaults, so no lock is needed
/// and mutex poisoning cannot occur.
static ACTIVE: AtomicU8 = AtomicU8::new(0);

/// Set the theme returned by [`active`].
///
/// The shell calls this whenever the app's theme changes. Widgets that read
/// [`active`] during `draw`, instead of using the supplied theme, then see
/// the theme being rendered.
pub fn set_active(theme: &Theme) {
    ACTIVE.store(
        u8::from(theme.theme_type == ThemeType::Light),
        Ordering::Relaxed,
    );
}

/// The currently-active theme.
pub fn active() -> Theme {
    if ACTIVE.load(Ordering::Relaxed) == 0 {
        Theme::dark()
    } else {
        Theme::light()
    }
}

/// The inherited symbolic icon colour.
///
/// `iced_core::renderer::Style` has only `text_color`, which containers
/// propagate to their subtree. The icon colour follows `text_color` for every
/// container class in [`crate::ui::theme::style::iced`] except
/// [`Container::HeaderBar`].
///
/// Widgets with different icon and text colours call [`with_icon_color`].
/// The override records both colours. [`icon_color`] uses the override while
/// the supplied renderer style's `text_color` matches the recorded value.
/// A container that replaces `text_color` supersedes the override; its text
/// colour also supplies its icon colour.
///
/// [`crate::ui::widget::button`] installs an override around its content.
/// [`crate::ui::widget::header_bar`] installs one for `HeaderBar`, the only
/// container class with different icon and text colours. `segmented_button`
/// and `text_input` draw standalone icons with the icon colour passed as
/// `text_color`.
///
/// [`crate::ui::widget::svg::Svg::symbolic`] reads this colour, as do `button`
/// and `text_input` for icons they paint themselves.
#[must_use]
pub fn icon_color(renderer_style: &iced_core::renderer::Style) -> iced::Color {
    match ICON_COLOR_OVERRIDE.with(std::cell::Cell::get) {
        Some((icon, witness)) if witness == renderer_style.text_color => icon,
        _ => renderer_style.text_color,
    }
}

/// Runs `f` with the inherited symbolic icon colour overridden.
///
/// `text` is the `text_color` the caller passes to the same children; see
/// [`icon_color`] for why it is recorded.
pub fn with_icon_color<R>(icon: iced::Color, text: iced::Color, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<(iced::Color, iced::Color)>);

    impl Drop for Restore {
        fn drop(&mut self) {
            ICON_COLOR_OVERRIDE.with(|cell| cell.set(self.0));
        }
    }

    let _restore = Restore(ICON_COLOR_OVERRIDE.with(|cell| cell.replace(Some((icon, text)))));

    f()
}

thread_local! {
    /// `(icon colour, the `text_color` that was live when it was installed)`.
    static ICON_COLOR_OVERRIDE: std::cell::Cell<Option<(iced::Color, iced::Color)>> =
        const { std::cell::Cell::new(None) };
}

/// The theme matching the desktop's colour-scheme preference.
pub fn system_preference() -> Theme {
    if system_prefers_dark() {
        Theme::dark()
    } else {
        Theme::light()
    }
}

/// Spacing steps, in logical pixels.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Spacing {
    /// No spacing
    pub space_none: u16,
    /// smallest spacing that can be non-zero
    pub space_xxxs: u16,
    /// extra extra small spacing
    pub space_xxs: u16,
    /// extra small spacing
    pub space_xs: u16,
    /// small spacing
    pub space_s: u16,
    /// medium spacing
    pub space_m: u16,
    /// large spacing
    pub space_l: u16,
    /// extra large spacing
    pub space_xl: u16,
    /// extra extra large spacing
    pub space_xxl: u16,
    /// largest possible spacing
    pub space_xxxl: u16,
}

impl Default for Spacing {
    fn default() -> Self {
        Self::from(Density::Standard)
    }
}

/// How tightly the UI is packed.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub enum Density {
    /// Lower padding/spacing of elements
    Compact,
    /// Standard padding/spacing of elements
    #[default]
    Standard,
    /// Higher padding/spacing of elements
    Spacious,
}

impl From<Density> for Spacing {
    fn from(value: Density) -> Self {
        match value {
            Density::Compact => Self {
                space_none: 0,
                space_xxxs: 4,
                space_xxs: 4,
                space_xs: 8,
                space_s: 8,
                space_m: 16,
                space_l: 24,
                space_xl: 32,
                space_xxl: 48,
                space_xxxl: 64,
            },
            Density::Standard => Self {
                space_none: 0,
                space_xxxs: 4,
                space_xxs: 8,
                space_xs: 12,
                space_s: 16,
                space_m: 24,
                space_l: 32,
                space_xl: 48,
                space_xxl: 64,
                space_xxxl: 128,
            },
            Density::Spacious => Self {
                space_none: 4,
                space_xxxs: 8,
                space_xxs: 12,
                space_xs: 16,
                space_s: 24,
                space_m: 32,
                space_l: 48,
                space_xl: 64,
                space_xxl: 128,
                space_xxxl: 160,
            },
        }
    }
}

impl Density {
    const fn as_u8(self) -> u8 {
        match self {
            Self::Compact => 0,
            Self::Standard => 1,
            Self::Spacious => 2,
        }
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Compact,
            2 => Self::Spacious,
            // 1, and anything else, is the default.
            _ => Self::Standard,
        }
    }
}

/// The active density.
///
/// `spacing()` is called from view code that has no handle on `Config`, so the
/// density is a process global set at startup and whenever the config changes.
/// A single atomic `u8` needs no lock and cannot suffer mutex poisoning.
static DENSITY: AtomicU8 = AtomicU8::new(Density::Standard.as_u8());
static HEADER_SIZE: AtomicU8 = AtomicU8::new(Density::Standard.as_u8());

/// Set the density used by [`spacing`].
pub fn set_density(density: Density) {
    DENSITY.store(density.as_u8(), Ordering::Relaxed);
}

/// Set the header-bar size independently of [`set_density`]; see
/// `Config::header_size`.
pub fn set_header_size(density: Density) {
    HEADER_SIZE.store(density.as_u8(), Ordering::Relaxed);
}

/// The size the header bar lays itself out at.
pub fn header_size() -> Density {
    Density::from_u8(HEADER_SIZE.load(Ordering::Relaxed))
}

/// The density used by [`spacing`].
pub fn density() -> Density {
    Density::from_u8(DENSITY.load(Ordering::Relaxed))
}

/// Spacing steps widgets lay out with: those of the active density, with
/// anything a theme file pinned applied on top.
pub fn spacing() -> Spacing {
    custom::spacing(Spacing::from(density()))
}

/// How long to wait for the desktop to report its colour scheme.
///
/// This is on the startup path, so the wait has to be short enough that a
/// machine with no portal is not noticeably slower to start. The 300ms timeout
/// allows more than a local D-Bus round trip while limiting the startup delay.
const COLOR_SCHEME_TIMEOUT: Duration = Duration::from_millis(300);

/// Whether a reported colour scheme means "use the dark theme".
///
/// `NoPreference` resolves to dark.
pub fn prefers_dark(scheme: ColorScheme) -> bool {
    !matches!(scheme, ColorScheme::Light)
}

/// Ask the desktop whether it prefers dark. Falls back to dark when no
/// answer arrives in time. The portal may be absent or slow, and this runs
/// on the startup path.
/// Cached answer from [`detect_system_prefers_dark`]. 0 = unknown, 1 = light,
/// 2 = dark.
static SYSTEM_PREFERS_DARK: AtomicU8 = AtomicU8::new(0);

/// Whether the desktop prefers dark, asking it at most once.
///
/// `AppTheme::theme()` runs on every config change, not just theme changes, and
/// the answer is fetched on a blocking round trip. Caching keeps an unrelated
/// settings toggle from stalling the UI on a machine with no portal, where the
/// call costs the full timeout.
///
/// Desktop theme changes take effect after restarting the app. This also
/// applied before caching because the app does not subscribe to changes.
/// Live updates would require `mundy::Preferences::subscribe`.
pub fn system_prefers_dark() -> bool {
    match SYSTEM_PREFERS_DARK.load(Ordering::Relaxed) {
        1 => return false,
        2 => return true,
        _ => {}
    }
    let prefers_dark = detect_system_prefers_dark();
    SYSTEM_PREFERS_DARK.store(u8::from(prefers_dark) + 1, Ordering::Relaxed);
    prefers_dark
}

fn detect_system_prefers_dark() -> bool {
    // `once_blocking` builds its own current-thread tokio runtime, and
    // building one from a runtime worker thread panics; `theme()` is called
    // from inside the app's tokio executor, so the call needs its own thread.
    //
    // Dropping the D-Bus stream left by `once_blocking` makes zbus call
    // `tokio::spawn` after mundy's runtime has been dropped. Without a runtime
    // in context, this panics. Keep a runtime entered across the call for that
    // spawn. `Handle::enter` publishes the handle without marking the thread
    // as a worker, so mundy's inner `block_on` can still run.
    std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        let _guard = runtime.enter();
        Preferences::once_blocking(Interest::ColorScheme, COLOR_SCHEME_TIMEOUT)
            .map(|preferences| prefers_dark(preferences.color_scheme))
    })
    .join()
    .ok()
    .flatten()
    .unwrap_or(true)
}

/// Which theme layer a surface sits on.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum Layer {
    /// Background layer
    #[default]
    Background,
    /// Primary layer
    Primary,
    /// Secondary layer
    Secondary,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `density()` is process-global, so the two tests that read or write it
    /// must not run at the same time; the harness runs tests in parallel by
    /// default. A mutex serializes the two separately named tests.
    /// `set_density_changes_spacing` restores `Standard` before it releases
    /// the guard, so either order is fine.
    static DENSITY_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn system_preference_is_only_detected_once() {
        // Prime the cache, then confirm a second call is served from it rather
        // than making another blocking round trip.
        let first = system_prefers_dark();
        let cached = SYSTEM_PREFERS_DARK.load(Ordering::Relaxed);
        assert_ne!(cached, 0, "first call should have populated the cache");

        let started = std::time::Instant::now();
        let second = system_prefers_dark();
        assert_eq!(first, second);
        assert!(
            started.elapsed() < COLOR_SCHEME_TIMEOUT,
            "second call took {:?}, which suggests it re-queried",
            started.elapsed()
        );
    }

    #[test]
    fn over_divides_both_terms_by_the_result_alpha() {
        // Half red over half blue: the result is 3/4 opaque, and each source
        // contributes its share of that, not of the whole
        let red = Srgba::new(1.0, 0.0, 0.0, 0.5);
        let blue = Srgba::new(0.0, 0.0, 1.0, 0.5);
        let out = over(red, blue);
        assert!((out.alpha - 0.75).abs() < 1e-6);
        assert!((out.red - 2.0 / 3.0).abs() < 1e-6, "red was {}", out.red);
        assert!((out.blue - 1.0 / 3.0).abs() < 1e-6, "blue was {}", out.blue);
    }

    #[test]
    fn color_scheme_maps_to_dark_preference() {
        assert!(prefers_dark(ColorScheme::Dark));
        assert!(!prefers_dark(ColorScheme::Light));
        assert!(prefers_dark(ColorScheme::NoPreference));
    }

    #[test]
    fn spacing_steps_increase_monotonically() {
        for density in [Density::Compact, Density::Standard, Density::Spacious] {
            let s = Spacing::from(density);
            assert!(s.space_none <= s.space_xxxs, "{density:?}");
            assert!(s.space_xxxs <= s.space_xxs, "{density:?}");
            assert!(s.space_xxs <= s.space_xs, "{density:?}");
            assert!(s.space_xs <= s.space_s, "{density:?}");
            assert!(s.space_s <= s.space_m, "{density:?}");
            assert!(s.space_m <= s.space_l, "{density:?}");
            assert!(s.space_l <= s.space_xl, "{density:?}");
            assert!(s.space_xl <= s.space_xxl, "{density:?}");
            assert!(s.space_xxl <= s.space_xxxl, "{density:?}");
        }
    }

    #[test]
    fn standard_matches_libcosmic_defaults() {
        // Changing these values affects every user's layout.
        let s = Spacing::from(Density::Standard);
        assert_eq!(
            (
                s.space_none,
                s.space_xxxs,
                s.space_xxs,
                s.space_xs,
                s.space_s
            ),
            (0, 4, 8, 12, 16)
        );
        assert_eq!(
            (s.space_m, s.space_l, s.space_xl, s.space_xxl, s.space_xxxl),
            (24, 32, 48, 64, 128)
        );
    }

    #[test]
    fn default_density_is_standard() {
        let _guard = DENSITY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(Density::default(), Density::Standard);
        assert_eq!(spacing(), Spacing::from(Density::Standard));
    }

    #[test]
    fn set_density_changes_spacing() {
        let _guard = DENSITY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_density(Density::Compact);
        assert_eq!(spacing(), Spacing::from(Density::Compact));
        set_density(Density::Standard);
        assert_eq!(spacing(), Spacing::from(Density::Standard));
    }
}
