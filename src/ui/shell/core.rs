// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! This app's `Core`: the window and nav-bar state owned by the shell.
//!
//! Ported from pop-os/libcosmic d9431dc, src/core.rs.

use crate::ui::iced::{Size, window};
use crate::ui::iced_core::layout::Limits;
use crate::ui::widget::nav_bar;
use std::collections::HashMap;

/// Room the main content keeps when the nav bar is dragged wider: its own
/// 360px minimum plus the padding on either side of it.
const CONTENT_RESERVE: f32 = 360.0 + 8.0 + 8.0;

/// Status of the nav bar and its panels.
#[derive(Clone)]
pub struct NavBar {
    active: bool,
    context_id: nav_bar::Id,
    toggled: bool,
    toggled_condensed: bool,
    /// Width the panel has been dragged to, in logical pixels, or `None`
    /// while it is left at the width its entries need.
    width: Option<u16>,
    /// Effective width when the drag in progress started, if there is one
    resize_from: Option<u16>,
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
    /// Several sites write it directly and bypass [`Core::set_show_context`]
    /// and its `is_condensed` recompute; the drawer's slide observes it after
    /// every update instead (`Shell::sync_drawer_slide`).
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

    /// The context drawer's slide in and out; see
    /// [`crate::ui::shell::drawer_slide`].
    pub(crate) drawer_slide: crate::ui::shell::drawer_slide::DrawerSlide,
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
                width: None,
                resize_from: None,
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
            drawer_slide: crate::ui::shell::drawer_slide::DrawerSlide::new(),
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
            // Navbar width (280px, or whatever it has been dragged to)
            // + padding (8px)
            reserved_width += self.nav_bar_effective_width(280.0) + 8.0;
        }

        #[allow(clippy::manual_clamp)]
        // Keep the content at least 360px wide until the drawer hits its 344px
        // minimum; never let the drawer exceed 480px.
        (window_width - reserved_width).min(480.0).max(344.0)
    }

    /// How much width the drawer takes from the main content, which is how
    /// far it slides.
    ///
    /// Inline, the main content gives up its right padding and the drawer
    /// brings its own on both sides (see `view_main`), so a content
    /// container adds one padding. In overlay mode the drawer sits 8px in
    /// from the edge (`context_drawer::overlay`).
    pub fn drawer_extent(&self, has_nav: bool) -> f32 {
        let width = self.context_width(has_nav);
        if self.window.context_is_overlay {
            width + 8.0
        } else if self.window.content_container {
            width + f32::from(self.window.border_padding.unwrap_or(7))
        } else {
            width
        }
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

    /// The width the nav bar has been dragged to, or `None` while it is left
    /// at the width its entries need.
    #[must_use]
    #[inline]
    pub const fn nav_bar_width(&self) -> Option<u16> {
        self.nav_bar.width
    }

    /// Restore a width saved from an earlier session.
    #[inline]
    pub const fn set_nav_bar_width(&mut self, width: Option<u16>) {
        self.nav_bar.width = width;
    }

    /// Where the panel is drawn to, for a nav bar whose entries need `min`
    /// pixels (see [`crate::ui::widget::nav_bar::min_width`]).
    ///
    /// A saved width narrower than that loses to it: the panel is never
    /// drawn clipping an entry, whatever is in the config file.
    #[must_use]
    pub fn nav_bar_effective_width(&self, min: f32) -> f32 {
        self.nav_bar
            .width
            .map_or(0.0, f32::from)
            .max(bounded_min(min))
            .min(nav_bar::MAX_WIDTH)
    }

    /// Widest the panel may be dragged: where the panel itself stops
    /// growing, or sooner if that is more than the window can spare while
    /// the main content keeps [`CONTENT_RESERVE`].
    fn nav_bar_max_width(&self) -> f32 {
        (self.window.width / self.scale_factor - CONTENT_RESERVE).min(nav_bar::MAX_WIDTH)
    }

    /// The pointer went down on the divider beside the nav bar.
    #[inline]
    pub(crate) fn nav_bar_resize_start(&mut self, min: f32) {
        self.nav_bar.resize_from = Some(width_to_px(self.nav_bar_effective_width(min)));
    }

    /// The pointer moved `delta` from where the drag started.
    ///
    /// The width follows it between what the entries need and whatever the
    /// window can spare, so the divider stops where the panel does and
    /// dragging back out moves it again straight away.
    pub(crate) fn nav_bar_resize_drag(&mut self, delta: f32, min: f32) {
        let Some(from) = self.nav_bar.resize_from else {
            return;
        };
        let min = bounded_min(min);
        let max = self.nav_bar_max_width().max(min);
        self.nav_bar.width = Some(width_to_px((f32::from(from) + delta).clamp(min, max)));
    }

    /// The drag ended. Returns the new width when it actually changed, which
    /// is what the application is asked to remember.
    pub(crate) fn nav_bar_resize_end(&mut self) -> Option<u16> {
        let from = self.nav_bar.resize_from.take()?;
        let width = self.nav_bar.width?;
        (width != from).then_some(width)
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

/// What the entries need, bounded by what the panel will actually draw.
///
/// Past [`nav_bar::MAX_WIDTH`] the panel stops growing, so reserving more
/// would leave empty space pushing the file view along and carry the resize
/// handle away from the visible edge. A label that does not fit inside the
/// maximum is ellipsized, which is what that cap already meant.
fn bounded_min(min: f32) -> f32 {
    min.min(nav_bar::MAX_WIDTH)
}

/// A laid-out width as the whole logical pixels the config file stores
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn width_to_px(width: f32) -> u16 {
    width.round().clamp(0.0, f32::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::Core;

    /// A nav bar whose entries need this much room, as
    /// `crate::ui::widget::nav_bar::min_width` would report it
    const MIN: f32 = 200.0;

    /// A core in a window wide enough that the content reserve is never what
    /// clamps a drag.
    fn core() -> Core {
        let mut core = Core::default();
        core.set_window_width(1600.0);
        core
    }

    #[test]
    fn an_undragged_nav_bar_is_as_wide_as_its_entries() {
        let core = core();
        assert_eq!(core.nav_bar_width(), None);
        assert!((core.nav_bar_effective_width(MIN) - MIN).abs() < f32::EPSILON);
    }

    #[test]
    fn a_drag_stops_at_the_widest_entry_and_moves_again_on_the_way_back() {
        let mut core = core();
        core.nav_bar_resize_start(MIN);

        core.nav_bar_resize_drag(50.0, MIN);
        assert_eq!(core.nav_bar_width(), Some(250));

        // Dragging far past the entries leaves the width on them rather than
        // letting it run on out of sight, so there is no distance to make up
        // before the divider follows the pointer out again.
        core.nav_bar_resize_drag(-500.0, MIN);
        assert_eq!(core.nav_bar_width(), Some(200));
        core.nav_bar_resize_drag(40.0, MIN);
        assert_eq!(core.nav_bar_width(), Some(240));
    }

    /// Past this the panel stops growing, so the slot it sits in must stop
    /// with it rather than pushing the file view along for nothing.
    #[test]
    fn a_drag_stops_where_the_panel_stops_growing() {
        let mut core = core();
        core.nav_bar_resize_start(MIN);
        core.nav_bar_resize_drag(5000.0, MIN);
        assert_eq!(core.nav_bar_width(), Some(280));
    }

    #[test]
    fn a_narrow_window_stops_the_drag_sooner_still() {
        let mut core = Core::default();
        core.set_window_width(600.0);
        core.nav_bar_resize_start(MIN);
        core.nav_bar_resize_drag(5000.0, MIN);
        assert_eq!(core.nav_bar_width(), Some(600 - 376));
    }

    #[test]
    fn only_a_drag_that_moved_is_worth_saving() {
        let mut core = core();

        core.nav_bar_resize_start(MIN);
        core.nav_bar_resize_drag(0.0, MIN);
        assert_eq!(core.nav_bar_resize_end(), None);

        core.nav_bar_resize_start(MIN);
        core.nav_bar_resize_drag(60.0, MIN);
        assert_eq!(core.nav_bar_resize_end(), Some(260));
        // The release arrives after the drag ended, and must not save again
        assert_eq!(core.nav_bar_resize_end(), None);
    }

    /// A bookmark whose label is wider than the panel will ever be drawn
    /// must not reserve room for itself: the panel would stop at its
    /// maximum and the rest would be empty space shoving the file view
    /// along, with the resize handle stranded away from the visible edge.
    #[test]
    fn entries_too_wide_to_draw_do_not_reserve_room_for_themselves() {
        let core = core();
        assert!((core.nav_bar_effective_width(600.0) - 280.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_drag_against_an_oversized_minimum_still_stops_at_the_maximum() {
        let mut core = core();
        core.nav_bar_resize_start(600.0);
        core.nav_bar_resize_drag(100.0, 600.0);
        assert_eq!(core.nav_bar_width(), Some(280));
    }

    /// Nothing clamps a width typed into the config file by hand
    #[test]
    fn a_saved_width_past_the_maximum_is_ignored() {
        let mut core = core();
        core.set_nav_bar_width(Some(5000));
        assert!((core.nav_bar_effective_width(MIN) - 280.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_saved_width_narrower_than_the_entries_loses_to_them() {
        let mut core = core();
        core.set_nav_bar_width(Some(120));
        assert!((core.nav_bar_effective_width(240.0) - 240.0).abs() < f32::EPSILON);
    }

    /// An entry added or renamed changes what the entries need, and the
    /// width follows it without anything having to be laid out first.
    #[test]
    fn a_wider_entry_widens_a_panel_already_at_its_minimum() {
        let core = core();
        assert!((core.nav_bar_effective_width(200.0) - 200.0).abs() < f32::EPSILON);
        assert!((core.nav_bar_effective_width(260.0) - 260.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_drawer_takes_its_width_plus_what_its_padding_adds() {
        // 1600 wide, no nav: the drawer is at its 480px maximum.
        let mut core = core();

        core.window.context_is_overlay = false;
        core.window.content_container = true;
        core.window.border_padding = None;
        assert!((core.drawer_extent(false) - 487.0).abs() < f32::EPSILON);

        core.window.content_container = false;
        assert!((core.drawer_extent(false) - 480.0).abs() < f32::EPSILON);

        core.window.context_is_overlay = true;
        assert!((core.drawer_extent(false) - 488.0).abs() < f32::EPSILON);
    }
}
