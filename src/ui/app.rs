// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! The shell's own internal message type, and the task alias every
//! `Application` method returns.

use crate::ui::iced::window;
use crate::ui::keyboard_nav;
use crate::ui::widget::nav_bar;

/// The task type every [`crate::ui::shell::Application`] method returns:
/// an `iced` task over the shell's message, not the app's.
pub type Task<M> = crate::ui::iced::Task<crate::ui::Action<M>>;

/// A message managed internally by the shell.
#[derive(Clone, Debug)]
pub enum Action {
    /// Application requests theme change.
    AppThemeChange(crate::ui::Theme),
    /// Requests to close the window.
    Close,
    /// Closes or shows the context drawer.
    ContextDrawer(bool),
    /// Requests to drag the window.
    Drag,
    /// Window focus changed.
    Focus(window::Id),
    /// Keyboard shortcuts managed by the shell.
    KeyboardNav(iced_core::window::Id, keyboard_nav::Action),
    /// Requests to maximize the window.
    Maximize,
    /// Requests to minimize the window.
    Minimize,
    /// Activates a navigation element from the nav bar.
    NavBar(nav_bar::Id),
    /// Activates a context menu for an item from the nav bar.
    NavBarContext(nav_bar::Id),
    /// The pointer went down on the divider beside the nav bar.
    NavBarResizeStart,
    /// The pointer moved this far from where the nav bar drag started.
    NavBarResizeDrag(f32),
    /// The nav bar drag ended, one way or the other.
    NavBarResizeEnd,
    /// A popup has finished collapsing and its surface can go.
    PopupExitFinished(window::Id),
    /// A new window was opened.
    Opened(window::Id),
    /// Set scaling factor.
    ScaleFactor(f32),
    /// Show the window menu.
    ShowWindowMenu,
    /// Notifies that a surface was closed. Any data relating to the surface
    /// should be cleaned up.
    SurfaceClosed(window::Id),
    /// Toggles visibility of the nav bar.
    ToggleNavBar,
    /// Toggles the condensed status of the nav bar.
    ToggleNavBarCondensed,
    /// Window focus lost.
    Unfocus(window::Id),
    /// Windowing system initialized.
    WindowingSystemInitialized,
    /// Updates the tracked window geometry.
    WindowResize(window::Id, f32, f32),
}
