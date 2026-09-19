// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! `Catalog` implementations for [`crate::ui::theme::Theme`].
//!
//! Ported from pop-os/libcosmic d9431dc, src/theme/style/ (MPL-2.0).

// The `menu`, `pick_list`, `radio` and `toggler` styles ignore state, and
// `scrollable` ignores disabled flags, which produces unused-variable
// warnings. Keep the allow scoped here, as in `ui::widget`, so warnings
// remain enabled in the rest of the app.
#![allow(unused_variables)]

mod button;
pub use self::button::Button;

mod dropdown;

pub mod iced;
#[doc(inline)]
pub use self::iced::{Checkbox, Container, ProgressBar, Rule, Scrollable, Svg, Text, TextEditor};

pub mod menu_bar;
#[doc(inline)]
pub use self::menu_bar::MenuBarStyle;

mod segmented_button;
#[doc(inline)]
pub use self::segmented_button::SegmentedButton;

mod text_input;
#[doc(inline)]
pub use self::text_input::TextInput;
