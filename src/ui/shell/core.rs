// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! This app's `Core`: the window and nav-bar state owned by the shell.
//!
//! Ported from pop-os/libcosmic d9431dc, src/core.rs.

use crate::ui::iced::{Size, window};
use crate::ui::iced_core::layout::Limits;
use crate::ui::widget::nav_bar;
use std::collections::HashMap;

/// Status of the nav bar and its panels.
#[derive(Clone)]
pub struct NavBar {
    active: bool,
    context_id: nav_bar::Id,
    toggled: bool,
    toggled_condensed: bool,
}

/// Window chrome settings.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone)]
pub struct Window {
    /// Label to display as header bar title.
    pub header_title: String,
    pub use_template: bool,
    pub content_container: bool,
    pub context_is_overlay: bool,
    /// Whether the context drawer is shown.
    ///
    /// Three sites write it directly: `src/app.rs:2062`, `src/app.rs:3758` and
    /// `src/dialog.rs:820`. They bypass [`Core::set_show_context`] and its
    /// `is_condensed` recompute.
    pub show_context: bool,
    pub show_headerbar: bool,
    pub show_window_menu: bool,
    pub show_close: bool,
    pub show_maximize: bool,
    pub show_minimize: bool,
    pub border_padding: Option<u16>,
    height: f32,
    width: f32,
}

/// State the application shell keeps on the app's behalf.
#[derive(Clone)]
pub struct Core {
    /// Enables debug features in iced.
    pub debug: bool,

    /// Whether the window is too small for the nav bar + main content.
    is_condensed: bool,

    /// Enables built in keyboard navigation.
    pub(crate) keyboard_nav: bool,

    /// Current status of the nav bar panel.
    nav_bar: NavBar,

    /// Scaling factor used by the application.
    scale_factor: f32,

    /// Window focus state.
    pub(crate) focused_window: Vec<window::Id>,

    pub(crate) title: HashMap<window::Id, String>,

    pub window: Window,

    pub(crate) main_window: Option<window::Id>,

    pub(crate) exit_on_main_window_closed: bool,

    /// Last measurement published by a `responsive_container`, keyed by menu
    /// bar id. Written only by the runner's `ResponsiveMenuBar` arm and read by
    /// `crate::ui::widget::responsive_menu_bar`, which panics when it has
    /// decided to collapse and finds no entry here. Never cleared.
    pub(crate) menu_bars: HashMap<crate::ui::widget::Id, (Limits, Size)>,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            debug: false,
            is_condensed: false,
            keyboard_nav: true,
            nav_bar: NavBar {
                active: true,
                context_id: <nav_bar::Id as slotmap::Key>::null(),
                toggled: true,
                toggled_condensed: false,
            },
            scale_factor: 1.0,
            focused_window: Vec::new(),
            title: HashMap::new(),
            window: Window {
                header_title: String::new(),
                use_template: true,
                content_container: true,
                context_is_overlay: true,
                show_context: false,
                show_headerbar: true,
                show_close: true,
                show_maximize: true,
                show_minimize: true,
                show_window_menu: false,
                height: 0.,
                width: 0.,
                border_padding: None,
            },
            main_window: None,
            exit_on_main_window_closed: true,
            menu_bars: HashMap::new(),
        }
    }
}

impl Core {
    /// Whether the window is too small for the nav bar + main content.
    #[must_use]
    #[inline]
    pub const fn is_condensed(&self) -> bool {
        self.is_condensed
    }

    /// The scaling factor used by the application.
    #[must_use]
    #[inline]
    pub const fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Enable or disable keyboard navigation.
    #[inline]
    pub const fn set_keyboard_nav(&mut self, enabled: bool) {
        self.keyboard_nav = enabled;
    }

    /// Whether keyboard navigation is enabled.
    #[must_use]
    #[inline]
    pub const fn keyboard_nav(&self) -> bool {
        self.keyboard_nav
    }

    /// Changes the scaling factor used by the application.
    pub fn set_scale_factor(&mut self, factor: f32) {
        self.scale_factor = factor;
        self.is_condensed_update();
    }

    /// Set header bar title.
    #[inline]
    pub fn set_header_title(&mut self, title: String) {
        self.window.header_title = title;
    }

    /// Whether to show or hide the main window's content.
    #[inline]
    pub const fn show_content(&self) -> bool {
        !self.is_condensed || !self.nav_bar.toggled_condensed
    }

    /// Call this whenever the scaling factor or window width has changed.
    fn is_condensed_update(&mut self) {
        // Nav bar (280px) + padding (8px) + content (360px)
        let mut breakpoint = 280.0 + 8.0 + 360.0;
        if self.window.show_context && !self.window.context_is_overlay {
            // Context drawer min width (344px) + padding (8px)
            breakpoint += 344.0 + 8.0;
        }
        self.is_condensed = (breakpoint * self.scale_factor) > self.window.width;
        self.nav_bar_update();
    }

    /// There is a conflict if the view is condensed and both the nav bar and
    /// context drawer are open on the same layer.
    #[inline]
    fn condensed_conflict(&self) -> bool {
        self.is_condensed
            && self.nav_bar.toggled_condensed
            && self.window.show_context
            && !self.window.context_is_overlay
    }

    /// Width available to the context drawer.
    #[inline]
    pub fn context_width(&self, has_nav: bool) -> f32 {
        let window_width = self.window.width / self.scale_factor;

        // Content width (360px) + padding (8px)
        let mut reserved_width = 360.0 + 8.0;
        if has_nav {
            // Navbar width (280px) + padding (8px)
            reserved_width += 280.0 + 8.0;
        }

        #[allow(clippy::manual_clamp)]
        // Keep the content at least 360px wide until the drawer hits its 344px
        // minimum; never let the drawer exceed 480px.
        (window_width - reserved_width).min(480.0).max(344.0)
    }

    pub fn set_show_context(&mut self, show: bool) {
        self.window.show_context = show;
        self.is_condensed_update();
        // Ensure nav bar is closed if condensed view and context drawer is opened
        if self.condensed_conflict() {
            self.nav_bar.toggled_condensed = false;
            self.is_condensed_update();
        }
    }

    #[inline]
    pub fn main_window_is(&self, id: window::Id) -> bool {
        self.main_window_id().is_some_and(|main_id| main_id == id)
    }

    /// Whether the nav panel is visible or not.
    #[must_use]
    #[inline]
    pub const fn nav_bar_active(&self) -> bool {
        self.nav_bar.active
    }

    #[inline]
    pub fn nav_bar_toggle(&mut self) {
        self.nav_bar.toggled = !self.nav_bar.toggled;
        self.nav_bar_set_toggled_condensed(self.nav_bar.toggled);
    }

    #[inline]
    pub fn nav_bar_toggle_condensed(&mut self) {
        self.nav_bar_set_toggled_condensed(!self.nav_bar.toggled_condensed);
    }

    #[inline]
    pub const fn nav_bar_context(&self) -> nav_bar::Id {
        self.nav_bar.context_id
    }

    #[inline]
    pub(crate) fn nav_bar_set_context(&mut self, id: nav_bar::Id) {
        self.nav_bar.context_id = id;
    }

    #[inline]
    pub fn nav_bar_set_toggled(&mut self, toggled: bool) {
        self.nav_bar.toggled = toggled;
        self.nav_bar_set_toggled_condensed(self.nav_bar.toggled);
    }

    pub(crate) fn nav_bar_set_toggled_condensed(&mut self, toggled: bool) {
        self.nav_bar.toggled_condensed = toggled;
        self.nav_bar_update();
        // Ensure context drawer is closed if condensed view and nav bar is opened
        if self.condensed_conflict() {
            self.window.show_context = false;
            self.is_condensed_update();
            // Sync nav bar state if the view is no longer condensed after closing the context drawer
            if !self.is_condensed {
                self.nav_bar.toggled = toggled;
                self.nav_bar_update();
            }
        }
    }

    #[inline]
    pub(crate) fn nav_bar_update(&mut self) {
        self.nav_bar.active = if self.is_condensed {
            self.nav_bar.toggled_condensed
        } else {
            self.nav_bar.toggled
        };
    }

    /// Set the height of the main window.
    ///
    /// Plain assignment; `is_condensed` is not recomputed here.
    #[inline]
    pub(crate) const fn set_window_height(&mut self, new_height: f32) {
        self.window.height = new_height;
    }

    /// Set the width of the main window.
    #[inline]
    pub(crate) fn set_window_width(&mut self, new_width: f32) {
        self.window.width = new_width;
        self.is_condensed_update();
    }

    /// Get the current focused window if it exists.
    #[must_use]
    #[inline]
    pub fn focused_window(&self) -> Option<window::Id> {
        self.focused_window.last().copied()
    }

    /// Get the current focus chain of windows.
    #[must_use]
    #[inline]
    pub fn focus_chain(&self) -> &[window::Id] {
        &self.focused_window
    }

    /// The [`window::Id`] of the main window.
    #[must_use]
    #[inline]
    pub fn main_window_id(&self) -> Option<window::Id> {
        self.main_window
            .filter(|id| crate::ui::window::none() != *id)
    }

    /// Reset the tracked main window to a new value, returning the old one.
    #[inline]
    pub fn set_main_window_id(&mut self, mut id: Option<window::Id>) -> Option<window::Id> {
        std::mem::swap(&mut self.main_window, &mut id);
        id
    }

    pub fn drag<M: Send + 'static>(&self, id: Option<window::Id>) -> crate::ui::app::Task<M> {
        let Some(id) = id.or(self.main_window) else {
            return crate::ui::iced::Task::none();
        };
        crate::ui::command::drag(id)
    }

    pub fn maximize<M: Send + 'static>(
        &self,
        id: Option<window::Id>,
        maximized: bool,
    ) -> crate::ui::app::Task<M> {
        let Some(id) = id.or(self.main_window) else {
            return crate::ui::iced::Task::none();
        };
        crate::ui::command::maximize(id, maximized)
    }

    pub fn minimize<M: Send + 'static>(&self, id: Option<window::Id>) -> crate::ui::app::Task<M> {
        let Some(id) = id.or(self.main_window) else {
            return crate::ui::iced::Task::none();
        };
        crate::ui::command::minimize(id)
    }

    pub fn toggle_maximize<M: Send + 'static>(
        &self,
        id: Option<window::Id>,
    ) -> crate::ui::app::Task<M> {
        let Some(id) = id.or(self.main_window) else {
            return crate::ui::iced::Task::none();
        };
        crate::ui::command::toggle_maximize(id)
    }
}
