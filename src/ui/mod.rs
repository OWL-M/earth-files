// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! The single seam between this app and its GUI toolkit.

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
pub mod widget;
pub mod window;

pub use iced;
// `iced::core` / `iced::runtime` are private in `iced`; the crates themselves
// are direct dependencies instead.
pub use iced_core;
pub use iced_runtime;
pub use apply::Apply;
pub use action::Action;
pub use iced::Task;

/// The renderer every widget in this app draws through.
pub type Renderer = iced::Renderer;

/// The app's theme, used to style every widget. See [`theme`].
pub use theme::Theme;

/// An `iced` element parameterised by our [`Theme`].
pub type Element<'a, Message> = iced::Element<'a, Message, Theme, Renderer>;
