// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! `Catalog` implementations for [`crate::ui::theme::Theme`].
//!
//! Ported from libcosmic `d9431dc`, `src/theme/style/`, which is MPL-2.0 and so
//! may be carried in a GPL-3.0 work. The port is close to verbatim, with paths
//! pointing into this crate and [`crate::ui::theme::Palette`] in place of
//! `cosmic_theme::Theme`. Catalogs for unused widgets are omitted: `slider`,
//! `pane_grid`, `markdown`, `table`, `qr_code` and `combo_box`, accounting for
//! about 180 lines of `iced.rs`.
//!
//! Widgets supplied by libcosmic use its public traits. These can be
//! implemented for the local [`crate::ui::theme::Theme`] type. Phase 3 vendors
//! the trait definitions when it removes libcosmic.

// These ported `Catalog::style` implementations match upstream: `menu`,
// `pick_list`, `radio` and `toggler` ignore state, and `scrollable` ignores
// disabled flags. This produces the same unused-variable warnings as
// upstream. Keep the allow scoped here, as in `ui::widget`, so warnings
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
