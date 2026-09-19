// Copyright 2025 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Embedded icons for platforms which do not support icon themes yet.
//!
//! Vendored from pop-os/libcosmic, src/widget/icon/bundle.rs
//!
//! The non-unix arm `include!`s `$OUT_DIR/bundled_icons.rs`, which this
//! crate's build script does not generate, so that arm does not build here.
//! It is `cfg`'d out on the platforms this app targets.

/// Icon bundling is not enabled on unix platforms.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn get(icon_name: &str) -> Option<super::Data> {
    None
}

#[cfg(any(not(unix), target_os = "macos"))]
/// Get a bundled icon on non-unix platforms.
pub fn get(icon_name: &str) -> Option<super::Data> {
    ICONS
        .get(icon_name)
        .map(|bytes| super::Data::Svg(iced::widget::svg::Handle::from_memory(*bytes)))
}

#[cfg(any(not(unix), target_os = "macos"))]
include!(concat!(env!("OUT_DIR"), "/bundled_icons.rs"));
