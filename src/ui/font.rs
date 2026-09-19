// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/font.rs
//!
//! Select preferred fonts.
//!
//! The families come from this app's own [`Config`]. They live in a process
//! global because the call sites take no arguments. It is set from the config
//! at startup (see [`set_families`]); read before that, it yields the defaults.

use std::collections::BTreeSet;
use std::sync::{LazyLock, RwLock};

pub use iced::Font;
use iced::font::{Family, Stretch, Style, Weight};

use crate::config::{Config, INTERFACE_FONT_DEFAULT, MONOSPACE_FONT_DEFAULT};

/// Stores static strings of the family names for `iced::Font` compatibility.
///
/// `Family::Name` wants a `&'static str`, and a family read from the config is
/// an owned `String`. It is leaked once and the leaked copy looked up here on
/// every later request; a leak per call would be a slow memory leak on the
/// draw path.
static FAMILY_MAP: LazyLock<RwLock<BTreeSet<&'static str>>> = LazyLock::new(RwLock::default);

/// The `&'static str` for `family`, leaking it only the first time it is seen.
fn intern(family: &str) -> &'static str {
    let read_guard = FAMILY_MAP.read().unwrap();
    let name: Option<&'static str> = read_guard.get(family).copied();
    drop(read_guard);

    name.unwrap_or_else(|| {
        let value: &'static str = family.to_owned().leak();
        FAMILY_MAP.write().unwrap().insert(value);
        value
    })
}

/// The families the functions below build their fonts from.
#[derive(Clone, Copy)]
struct Families {
    interface: &'static str,
    monospace: &'static str,
}

/// The configured families, or the defaults until [`set_families`]
/// has run. Both defaults are string literals, so the fallback needs no
/// interning and the global can be a `const` initialiser.
static FAMILIES: RwLock<Families> = RwLock::new(Families {
    interface: INTERFACE_FONT_DEFAULT,
    monospace: MONOSPACE_FONT_DEFAULT,
});

/// Point the fonts below at the families in `config`.
///
/// Called where the app loads its config, before any window is built.
pub fn set_families(config: &Config) {
    let interface = intern(&config.interface_font);
    let monospace = intern(&config.monospace_font);
    let mut guard = FAMILIES.write().unwrap();
    guard.interface = interface;
    guard.monospace = monospace;
}

fn families() -> Families {
    *FAMILIES.read().unwrap()
}

/// A font in `family`, with every other attribute left at Normal.
const fn font(family: &'static str) -> Font {
    Font {
        family: Family::Name(family),
        weight: Weight::Normal,
        stretch: Stretch::Normal,
        style: Style::Normal,
    }
}

#[inline]
pub fn default() -> Font {
    font(families().interface)
}

#[inline]
pub fn light() -> Font {
    Font {
        weight: Weight::Light,
        ..default()
    }
}

#[inline]
pub fn semibold() -> Font {
    Font {
        weight: Weight::Semibold,
        ..default()
    }
}

#[inline]
pub fn bold() -> Font {
    Font {
        weight: Weight::Bold,
        ..default()
    }
}

#[inline]
pub fn mono() -> Font {
    font(families().monospace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_before_initialisation() {
        // Whatever order the tests in this binary run in, the families are
        // never unset: reading them yields the defaults.
        assert!(matches!(default().family, Family::Name(_)));
        assert_eq!(bold().weight, Weight::Bold);
        assert_eq!(default().weight, Weight::Normal);
    }

    #[test]
    fn interning_is_stable() {
        let first = intern("Some Family");
        let second = intern(&String::from("Some Family"));
        assert!(std::ptr::eq(first, second), "family leaked twice");
    }
}
