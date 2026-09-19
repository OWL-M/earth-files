// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! The single seam between this app and its GUI toolkit.
//!
//! Phase 0 moves call sites here while libcosmic is still underneath; Phase 3
//! swaps the internals to iced 0.14 without touching callers outside this
//! module.
//!
//! [`Theme`] and [`Element`] are already ours: the first step of Phase 3 gave
//! this app its own theme, and an `Element` carries its theme as a type
//! parameter, so the two move together. What is still libcosmic's underneath is
//! the toolkit, `Action`/`surface::Action`, and the `header_bar` chrome; see
//! [`theme_bridge`] for how the last of those is carried across.

pub mod action;
pub mod anim;
pub mod app;
pub mod clipboard;
pub mod command;
pub mod config;
pub mod convert;
pub mod dnd;
pub mod font;
pub mod keyboard_nav;
pub mod icon_theme;
pub mod shell;
pub mod surface;
pub mod task;
pub mod theme;
// Vendored widgets shadow libcosmic's; see `widget/mod.rs`.
pub mod widget;
pub mod window;

// Re-export whole modules so call sites only change prefixes and retain all
// exports. `ui::theme` and `ui::font` above replace `cosmic::theme` and
// `cosmic::font`, owning anything they do not re-export.
pub use iced;
// `iced::core` / `iced::runtime` are public in libcosmic's fork but private
// upstream; the crates themselves are direct dependencies instead.
pub use iced_core;
pub use iced_runtime;
pub use apply::Apply;
pub use action::Action;
/// `cosmic::Task` was `iced::Task` verbatim; so is this.
pub use iced::Task;

/// The renderer every widget in this app draws through.
///
/// libcosmic defines this as exactly `iced::Renderer` (`src/lib.rs:183`), and
/// the API delta study confirmed upstream's is the same type, so this is ours
/// already and survives the flip untouched.
pub type Renderer = iced::Renderer;

/// The app's theme, used to style every widget. See [`theme`].
pub use theme::Theme;

/// An `iced` element parameterised by our [`Theme`].
///
/// libcosmic's `Element` hard-codes its own `Theme`, so it cannot be re-exported
/// once the theme is ours. The `Renderer` is unchanged: libcosmic's is
/// `iced::Renderer`, and the API delta study confirmed upstream's is the same
/// type.
pub type Element<'a, Message> = iced::Element<'a, Message, Theme, Renderer>;
