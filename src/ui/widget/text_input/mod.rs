// Copyright 2019 H�ctor Ram�n, Iced contributors
// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MIT

//! A text input widget from iced widgets plus some added details.
//!
//! Vendored from pop-os/libcosmic, src/widget/text_input/

pub mod cursor;
pub mod editor;
// `pub(crate)` rather than private: `ui::theme::style` reuses the `ColorExt`
// restated inside, rather than keeping a second copy of the same four lines.
pub(crate) mod input;
pub mod value;

pub use crate::ui::theme::TextInput as Style;
pub use input::*;
mod style;
pub use style::{Appearance, StyleSheet};
