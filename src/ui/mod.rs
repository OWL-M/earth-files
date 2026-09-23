// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! The single seam between this app and its GUI toolkit.

pub mod action;
pub mod anim;
pub mod app;
pub mod clipboard;
pub mod command;
pub mod convert;
pub mod dnd;
pub mod font;
pub mod icon_theme;
pub mod keyboard_nav;
pub mod shell;
pub mod surface;
pub mod task;
pub mod theme;
pub mod widget;
pub mod window;

pub use iced;
// `iced::core` / `iced::runtime` are private in `iced`; the crates themselves
// are direct dependencies instead.
pub use action::Action;
pub use iced::Task;
pub use iced_core;
pub use iced_runtime;

/// The `apply` combinator: `x.apply(f)` is `f(x)`, so a value can be
/// threaded through a function in the middle of a builder chain.
pub trait Apply: Sized {
    fn apply<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}

impl<T> Apply for T {}

/// The renderer every widget in this app draws through.
///
/// `iced_texture_cache`'s renderer rather than iced's own: it is
/// `iced_renderer::fallback::Renderer` over a wgpu and a tiny-skia half,
/// each delegating every renderer trait to iced's, so drawing is unchanged
/// until a `Cached` widget appears in the tree. It is here that the app
/// gains the ability to composite a recorded subtree with an animated
/// transform and opacity, which iced 0.14 cannot express.
pub type Renderer = iced_texture_cache::Renderer;

/// The app's theme, used to style every widget. See [`theme`].
pub use theme::Theme;

/// An `iced` element parameterised by our [`Theme`].
pub type Element<'a, Message> = iced::Element<'a, Message, Theme, Renderer>;
