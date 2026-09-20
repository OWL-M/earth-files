// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Window-management commands.
//!
//! # What `iced_exwlshell` does and does not carry
//!
//! The original exwlshell dispatcher handled six `window::Action` variants
//! and discarded the rest in `_ => {}`, including `drag`, `maximize`,
//! `minimize` and `show_window_menu`. Commit `ad6765a` in our checkout added
//! `Drag`, `Maximize`, `ToggleMaximize`, `GetMaximized`, `GetMode`, `Minimize`,
//! `GetMinimized`, `ShowSystemMenu` and `Run`, and added the `xdg_toplevel`
//! configure `states` that drive `is_maximized`. `xdg_toplevel.move`,
//! `set_maximized` and `set_minimized` have been observed on the wire from this
//! app.
//!
//! The dispatcher still discards `Open`, `Resize`, `Move`, `SetLevel`,
//! `GainFocus`, decoration and icon changes, and pointer passthrough.
//! Create surfaces with exwlshell's `NewBaseWindow` action
//! (`crate::ui::action::exwl::base_window`). `iced::window::open` returns a
//! `Task` that is discarded without an error; using it in `src/dialog.rs`
//! caused every file-chooser window to fail to appear.
//!
//! `Minimize(false)` is dropped because xdg_shell has no `unset_minimized`.

use crate::ui::Action;
use crate::ui::iced::{Task, window};

/// Initiates a window drag.
///
/// Dropped by exwlshell. See the module note.
pub fn drag<M>(id: window::Id) -> Task<Action<M>> {
    iced_runtime::window::drag(id)
}

/// Maximizes the window.
///
/// Dropped by exwlshell. See the module note.
pub fn maximize<M>(id: window::Id, maximized: bool) -> Task<Action<M>> {
    iced_runtime::window::maximize(id, maximized)
}

/// Minimizes the window.
///
/// Dropped by exwlshell. See the module note.
pub fn minimize<M>(id: window::Id) -> Task<Action<M>> {
    iced_runtime::window::minimize(id, true)
}

/// Toggles the window's maximize state.
///
/// Dropped by exwlshell. See the module note.
pub fn toggle_maximize<M>(id: window::Id) -> Task<Action<M>> {
    iced_runtime::window::toggle_maximize(id)
}

/// Shows the compositor's window menu.
///
/// Dropped by exwlshell. See the module note.
pub fn show_window_menu<M>(id: window::Id) -> Task<Action<M>> {
    iced_runtime::window::show_system_menu(id)
}

/// Sets the title of a window.
///
/// A no-op: the title is served to the compositor from the shell's `title`
/// map through the daemon's `.title(..)` closure.
#[allow(unused_variables, clippy::needless_pass_by_value)]
pub fn set_title<M>(id: window::Id, title: String) -> Task<Action<M>> {
    Task::none()
}

/// Sets the theme every window renders with.
pub fn set_theme<M: Send + 'static>(theme: crate::ui::Theme) -> Task<Action<M>> {
    Task::done(Action::Cosmic(crate::ui::app::Action::AppThemeChange(
        theme,
    )))
}

/// Sets the scaling factor.
pub fn set_scaling_factor<M: Send + 'static>(factor: f32) -> Task<Action<M>> {
    Task::done(Action::Cosmic(crate::ui::app::Action::ScaleFactor(factor)))
}
