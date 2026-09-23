// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/progress_bar/mod.rs

mod animation;
pub mod circular;
pub mod linear;
pub mod style;

/// A spinner / throbber widget that can be used to indicate that some operation is in progress.
pub fn indeterminate_circular() -> circular::Circular<crate::ui::Theme> {
    circular::Circular::new()
}

/// A circular progress spinner widget that can be used to indicate the progress of some operation.
pub fn determinate_circular(progress: f32) -> circular::Circular<crate::ui::Theme> {
    circular::Circular::new().progress(progress)
}

/// A linear progress bar widget that can be used to indicate the progress of some operation.
pub fn determinate_linear(progress: f32) -> linear::Linear<crate::ui::Theme> {
    linear::Linear::new().progress(progress)
}
