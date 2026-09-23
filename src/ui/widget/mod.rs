// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Widgets.
//!
//! See `README.md` in this directory for the provenance of each vendored
//! widget.

// Vendored code is copied in full, including
// items this app never calls. Scoping the allow to this module keeps the
// app's own dead-code warnings meaningful.
#![allow(dead_code)]
// `button`'s vendored code warns under this crate's feature set: four
// `let mut button` bindings are reassigned only inside
// `#[cfg(feature = "a11y")]` blocks, and `button::draw`'s `is_image`
// parameter is unused. Keep both: `mut` is required when a11y is on, and
// `draw` is public API. Scoped here for the same reason as `dead_code` above:
// it keeps the app's own warnings meaningful.
#![allow(unused_mut, unused_variables)]
// `text_input::input::draw`'s `icon_layout` is assigned up to three times as it
// walks the layout children, and only the last is read. The dead writes are how
// it advances the iterator, so they cannot be removed without changing
// behaviour. Scoped here for the same reason as the allows above.
#![allow(unused_assignments)]
// `segmented_button`'s `SegmentedButton::tab_drag` is `pub(super)` while its
// `TabDragSource` type is private; neither may be widened without changing the
// module's public surface. Scoped here for the same reason as the allows above.
#![allow(private_interfaces)]

// Everything not vendored comes from `iced`.
pub use iced::widget::*;

// Vendored, shadowing the glob above. Rust gives explicit items and explicit
// `use` declarations precedence over glob imports, which is what makes this
// work.
pub mod about;
pub use about::About;

pub mod autosize;
pub use autosize::autosize;

pub mod button;
pub use button::{Button, IconButton, LinkButton, TextButton};

pub mod common;

pub mod context_drawer;
pub use context_drawer::{ContextDrawer, context_drawer};

pub mod context_menu;
pub use context_menu::{ContextMenu, context_menu};

pub mod dialog;
pub use dialog::{Dialog, dialog};

pub mod divider;

pub mod ellipsize;
pub use ellipsize::{Ellipsize, Mode as EllipsizeMode};

pub mod dropdown;
pub use dropdown::{Dropdown, dropdown};

pub mod header_bar;
pub use header_bar::{HeaderBar, header_bar};

pub mod nav_bar_toggle;
pub use nav_bar_toggle::{NavBarToggle, nav_bar_toggle};

pub mod grid;
pub use grid::{Grid, grid};

pub mod icon;
pub use icon::{Icon, icon};

pub mod id_container;
pub use id_container::{IdContainer, id_container};

pub mod layer_container;
pub use layer_container::{LayerContainer, layer_container};

pub mod list;
pub use list::{ListColumn, list_column};

pub mod menu;

pub mod nav_bar;
pub use nav_bar::{NavBar, nav_bar};

pub mod nav_bar_divider;
pub use nav_bar_divider::{NavBarDivider, nav_bar_divider};

// Not vendored: `Paragraph` queries (`cursor_position`, `highlight`, the
// affinity-carrying `Hit`) that iced lacks.
pub mod paragraph;

pub mod popover;
pub use popover::{Popover, popover};

pub mod popup_genie;
pub use popup_genie::{PopupGenie, popup_genie};

pub mod radio;

pub mod progress_bar;
pub use progress_bar::{
    circular, circular::Circular, determinate_circular, determinate_linear, indeterminate_circular,
    indeterminate_linear, linear, linear::Linear, style,
};

pub mod segmented_button;

pub mod scrollable;
pub mod svg;
pub use scrollable::{horizontal, scrollable, vertical};

pub mod settings;

pub mod selectable_text;
pub use selectable_text::{SelectableText, selectable_text};

pub mod text_context_menu;

pub mod text_editor;
pub use text_editor::{TextEditor, text_editor};

pub mod text_input;
pub use text_input::{
    TextInput, editable_input, inline_input, search_input, secure_input, text_input,
};

pub mod tab_bar;

pub mod text;
pub use text::{
    Text, Typography, body, caption, caption_heading, heading, monotext, text, title1, title2,
    title3, title4,
};

pub mod toaster;
pub use toaster::{ToastId, toaster};

pub mod toggler;
pub use toggler::{Toggler, toggler};

pub mod tooltip;
pub use tooltip::{Tooltip, tooltip};

pub mod wrapper;
pub use wrapper::{RcElementWrapper, RcWrapper};

pub mod responsive_container;
pub use responsive_container::{ResponsiveContainer, responsive_container};

pub mod responsive_menu_bar;
pub use responsive_menu_bar::{ResponsiveMenuBar, responsive_menu_bar};
