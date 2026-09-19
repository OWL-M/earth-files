// Copyright 2019 H�ctor Ram�n, Iced contributors
// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MIT

//! A text input widget from iced widgets plus some added details.
//!
//! Vendored from pop-os/libcosmic d9431dc, src/widget/text_input/
//!
//! `style.rs` is deliberately not vendored: `crate::ui::Theme` implements
//! `StyleSheet` inside libcosmic (`theme/style/text_input.rs`), so a vendored
//! copy of the trait would be a distinct nominal type nothing implements. The
//! trait and its `Appearance` are re-exported from `cosmic::` instead, as
//! `button`'s `Catalog`/`Style` and `dropdown::menu`'s `Appearance` are.

pub mod cursor;
pub mod editor;
// `pub(crate)` rather than private: `ui::theme::style` reuses the `ColorExt`
// restated inside, rather than keeping a second copy of the same four lines.
pub(crate) mod input;
pub mod value;

pub use crate::ui::theme::TextInput as Style;
pub use input::*;
// Vendored as of Phase 3, for the reason spelled out in
// `segmented_button::mod`'s note on its own `style`.
mod style;
pub use style::{Appearance, StyleSheet};
