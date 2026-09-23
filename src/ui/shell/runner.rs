// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Application runner.
//!
//! Ported from pop-os/libcosmic d9431dc, src/app/mod.rs and src/app/cosmic.rs.
//!
//! [`run`] uses [`iced_exwlshell::build_pattern::daemon`]. Its base window is
//! an `xdg_toplevel` opened with `NewBaseWindow`, and its popups are
//! `xdg_popup`s attached to that toplevel. Upstream `iced::daemon` uses `winit`
//! and does not support Wayland popups.
//!
//! ## Runtime limits
//!
//! `iced_exwlshell` handles six `window::Action` variants and discards the rest
//! in `_ => {}`. This affects three parts of the runner:
//!
//! - Two `run_with_handle` calls read `RawWindowHandle` to identify the
//!   windowing system. exwlshell does not dispatch
//!   `WindowAction::RunWithHandle`, so [`WINDOWING_SYSTEM`] is set at boot.
//!   Its only backend is Wayland.
//! - Window dragging, maximizing, minimizing and the window menu use
//!   [`crate::ui::command`].
//! - exwlshell reports no `xdg_toplevel` configure states, so nothing sets
//!   `Core::window.is_maximized` or `sharp_corners`.

use super::{Application, Core};
use crate::ui::app::Task as AppTask;
use crate::ui::convert::ToColor;
use crate::ui::iced::{self, Subscription, window};
use crate::ui::{Element, Theme};
use std::borrow::Cow;
use std::cell::RefCell;

/// Builds a popup's content, capturing everything it needs
type PopupView<M> = Box<dyn Fn() -> Element<'static, crate::ui::Action<M>> + Send + Sync>;

/// The windowing system the app is running under.
///
/// Read by [`crate::ui::widget::responsive_menu_bar`] and by every widget that
/// decides between a Wayland popup and an in-window overlay.
pub(crate) static WINDOWING_SYSTEM: std::sync::OnceLock<WindowingSystem> =
    std::sync::OnceLock::new();

/// The windowing backends.
///
/// Only [`WindowingSystem::Wayland`] is reachable because `iced_exwlshell`
/// has no other backend. Readers use
/// `matches!(windowing_system(), Some(WindowingSystem::Wayland))`, preserving
/// the runtime distinction if a non-Wayland runner is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowingSystem {
    Wayland,
    Xlib,
    Xcb,
    Drm,
    Gbm,
}

/// The currently detected windowing system, if the main window has opened.
#[must_use]
pub fn windowing_system() -> Option<WindowingSystem> {
    WINDOWING_SYSTEM.get().copied()
}

const EMBEDDED_FONTS: &[&[u8]] = &[
    include_bytes!("../../../res/fonts/open-sans/OpenSans-Light.ttf"),
    include_bytes!("../../../res/fonts/open-sans/OpenSans-Regular.ttf"),
    include_bytes!("../../../res/fonts/open-sans/OpenSans-Semibold.ttf"),
    include_bytes!("../../../res/fonts/open-sans/OpenSans-Bold.ttf"),
    include_bytes!("../../../res/fonts/open-sans/OpenSans-ExtraBold.ttf"),
    include_bytes!("../../../res/fonts/noto/NotoSansMono-Regular.ttf"),
    include_bytes!("../../../res/fonts/noto/NotoSansMono-Bold.ttf"),
];

/// Load the interface fonts into the shared font system.
///
/// The fonts are carried in `res/fonts/`. Without this the app renders in
/// whatever fontconfig picks for "Open Sans", which on most systems is not
/// Open Sans.
fn preload_fonts() {
    let mut font_system = iced::advanced::graphics::text::font_system()
        .write()
        .unwrap();

    EMBEDDED_FONTS
        .iter()
        .for_each(move |font| font_system.load_font(Cow::Borrowed(font)));
}

/// Split [`super::Settings`] into what the daemon takes and what the shell keeps.
///
/// `IcedXdgWindowSettings` carries a size and a decoration mode and nothing
/// else. The application id becomes the daemon's namespace.
fn split_settings<App: Application>(
    settings: super::Settings,
) -> (
    iced_exwlshell::settings::Settings,
    Core,
    iced_exwlshell::actions::IcedXdgWindowSettings,
    Theme,
) {
    preload_fonts();

    let mut core = Core::default();
    core.debug = settings.debug;
    core.set_scale_factor(settings.scale_factor);
    crate::ui::dnd::set_scale_factor(settings.scale_factor);
    core.set_window_width(settings.size.width);
    core.set_window_height(settings.size.height);

    // `ui::icon_theme` resolves the icon theme from the desktop portal, so the
    // setting is only consulted as an override.
    if let Some(icon_theme) = settings.default_icon_theme.clone() {
        crate::ui::icon_theme::set_default(icon_theme);
    }

    if settings.no_main_window {
        core.main_window = Some(crate::ui::window::none());
    }

    let mut exwl = iced_exwlshell::settings::Settings {
        id: Some(App::APP_ID.to_owned()),
        antialiasing: settings.antialiasing,
        default_font: settings.default_font,
        default_text_size: iced::Pixels(settings.default_text_size),
        layer_settings: iced_exwlshell::settings::LayerShellSettings {
            // This is a normal windowed application, not a panel.
            //
            // The daemon unconditionally builds a root surface before any of
            // our windows exist (`multi_window.rs`, `.build().expect("Cannot
            // create layershell")`). Under the default `StartMode::Active` it
            // gives that surface the layer-shell role, so the app came up
            // as an overlay across the screen instead of an ordinary window,
            // with our `NewBaseWindow` xdg_toplevel behind it.
            //
            // `StartMode::Background` makes it create a plain `wl_surface`
            // with no shell role at all (`exwlshellev/src/lib.rs`, the
            // `is_background()` branch), leaving the xdg_toplevel that
            // `ui::action::exwl::base_window` opens as the only real surface.
            start_mode: iced_exwlshell::settings::StartMode::Background,
            ..Default::default()
        },
        ..Default::default()
    };
    // The fonts are already in the shared font system via `preload_fonts`;
    // handing them over twice would only re-parse them.
    exwl.fonts = Vec::new();

    core.exit_on_main_window_closed = settings.exit_on_close;

    let window_settings = iced_exwlshell::actions::IcedXdgWindowSettings {
        size: Some(iced_exwlshell::reexport::PixelSize::px(
            settings.size.width.max(1.0) as u32,
            settings.size.height.max(1.0) as u32,
        )),
        client_side_decorations: settings.client_decorations,
    };

    (exwl, core, window_settings, settings.theme)
}

/// Launch an application with the given [`Settings`](super::Settings).
///
/// # Errors
///
/// Returns an error on application failure.
pub fn run<App: Application>(
    settings: super::Settings,
    flags: App::Flags,
) -> Result<(), iced_exwlshell::Error> {
    image_extras::register();

    #[cfg(all(target_env = "gnu", not(target_os = "windows")))]
    if let Some(threshold) = settings.default_mmap_threshold {
        super::malloc::limit_mmap_threshold(threshold);
    }

    // exwlshell only supports Wayland, so set this before creating surfaces.
    // The previous `run_with_handle` round trip left widgets on their
    // non-Wayland paths for the first few frames.
    _ = WINDOWING_SYSTEM.set(WindowingSystem::Wayland);

    let (mut exwl_settings, mut core, window_settings, theme) = split_settings::<App>(settings);

    // Own the connection rather than letting exwlshell open one, so
    // `ui::dnd` can put a second event queue on the same connection and bind
    // the `wl_data_device_manager` exwlshell does not bind at all. Tab
    // drag-to-reorder is the only thing that needs it; if this fails the app
    // runs exactly as before, minus the drag.
    match wayland_client::Connection::connect_to_env() {
        Ok(connection) => {
            crate::ui::dnd::init(&connection);
            exwl_settings.with_connection =
                Some(iced_exwlshell::reexport::WithConnection::Value(connection));
        }
        Err(err) => log::warn!("could not open a Wayland connection of our own: {err}"),
    }

    if core.main_window.is_none() {
        core.main_window = Some(crate::ui::window::reserved());
    }

    // One-shot: `boot` takes `&self`, so the flags and `Core` are moved out on
    // the first call and a second call panics.
    let boot_data = RefCell::new(Some((core, flags, window_settings, theme)));
    let boot = move || {
        let (mut core, flags, window_settings, theme) = boot_data
            .borrow_mut()
            .take()
            .expect("the application was booted twice");

        let mut tasks = Vec::new();
        if core.main_window_id().is_some() {
            core.set_main_window_id(Some(crate::ui::window::reserved()));
            tasks.push(iced::Task::done(crate::ui::action::exwl::base_window(
                crate::ui::window::reserved(),
                window_settings,
            )));
        }

        let (shell, task) = Shell::<App>::init(core, flags, theme);
        tasks.push(task);
        (shell, iced::Task::batch(tasks))
    };

    iced_exwlshell::build_pattern::daemon(
        boot,
        App::APP_ID,
        Shell::<App>::update,
        Shell::<App>::view,
    )
    .settings(exwl_settings)
    .subscription(Shell::<App>::subscription)
    .title(|shell: &Shell<App>, id| Some(shell.title(id)))
    .style(|shell: &Shell<App>, theme: &Theme| shell.style(theme))
    .theme(|shell: &Shell<App>, id| shell.theme(id))
    .run()
}

/// The shell: owns the app, the theme and the popup surfaces.
pub struct Shell<App: Application> {
    pub app: App,
    /// The theme every window renders with.
    theme: Theme,
    /// View builders for the Wayland popup surfaces this shell has opened:
    /// the ones [`crate::ui::widget::text_context_menu`] queues, and the ones
    /// [`crate::ui::surface::Action::Popup`] carries in from `menu`,
    /// `context_menu`, `segmented_button` and `dropdown`.
    ///
    /// Each builder captures its content and takes no `&App`.
    popup_views: std::collections::HashMap<window::Id, PopupView<App::Message>>,
    /// Refcount of open surfaces per id. Text context menus can reuse an id on
    /// a second right-click before the previous surface's `SurfaceClosed` arrives
    /// (`drain_text_context_popups`). Counting open surfaces prevents that
    /// stale close from deleting the new popup's view.
    opened_surfaces: std::collections::HashMap<window::Id, u32>,
    /// Animation state for the popups that asked for it, keyed the same way
    /// as `popup_views`.
    popup_genies: crate::ui::shell::popup_genie::PopupGenies,
}

impl<App: Application> Shell<App> {
    pub fn init(core: Core, flags: App::Flags, theme: Theme) -> (Self, AppTask<App::Message>) {
        // Widgets that read `theme::active()` during `draw` must see the right
        // theme from the first frame, not from the first `AppThemeChange`.
        crate::ui::theme::set_active(&theme);
        let (app, task) = App::init(core, flags);

        let shell = Self {
            app,
            theme,
            popup_views: std::collections::HashMap::new(),
            opened_surfaces: std::collections::HashMap::new(),
            popup_genies: crate::ui::shell::popup_genie::PopupGenies::new(),
        };

        (shell, task)
    }

    pub fn title(&self, id: window::Id) -> String {
        self.app.title(id).to_string()
    }

    pub fn theme(&self, _id: window::Id) -> Theme {
        self.theme.clone()
    }

    pub fn style(&self, _theme: &Theme) -> iced::theme::Style {
        if let Some(style) = self.app.style() {
            style
        } else {
            // Every surface shares this, popups included, and a popup's
            // rounded corners are drawn inside a square surface
            // (`crate::ui::surface`), so it has to stay see-through. A window
            // with no outline is filled by its own root container instead;
            // see `view_main`.
            iced::theme::Style {
                background_color: iced::Color::TRANSPARENT,
                text_color: self.theme.cosmic().on_bg_color().to_color(),
            }
        }
    }

    pub fn view(&self, id: window::Id) -> Element<'_, crate::ui::Action<App::Message>> {
        // Text widgets read this to learn which window they are being laid out
        // in, so a right-click can anchor its popup to the right parent surface.
        crate::ui::widget::text_context_menu::set_current_window_id(id);

        // A popup surface renders the view its creator handed us, not the app.
        if let Some(view) = self.popup_views.get(&id) {
            let content = view();
            let Some(genie) = self.popup_genies.get(id) else {
                return content;
            };

            // The genie composites the recorded menu; the wrapper reports
            // when an exit has finished; the host is the clock that ticks
            // the engine, and must be the outermost of the three.
            // The menu's own corner radius, read live so a theme change
            // reaches an open popup. The genie keeps that radius on screen
            // however far it squeezes; without it the recorded corner is
            // squeezed with the texture and reads as a straight cut.
            let corner_radius = self.theme.cosmic().radius_s()[0];
            let collapsing = iced_texture_cache::cached(genie.cache(), content)
                // A menu on its way out is a picture, not a menu. Dismissal
                // has already cleared the state it draws from — `Menu::draw`
                // returns at once with `open` false — so what keeps it on
                // screen is the texture recorded while it was still live. Any
                // re-record during the collapse would replace that picture
                // with an empty one and the menu would vanish mid-animation
                // instead of collapsing.
                //
                // Auto-invalidation is what would order that re-record: it
                // fires when the content reacts to an event or its pointer
                // appearance changes, both of which a dismissed menu under
                // the cursor does. That is why the collapse was lost only
                // sometimes, and only ever with the pointer over the menu.
                .auto_invalidate(!genie.exiting())
                .genie(genie.progress(), genie.shape(corner_radius));
            let watched = crate::ui::widget::popup_genie(
                genie.motion().clone(),
                genie.key(),
                genie.exiting(),
                crate::ui::Action::Cosmic(crate::ui::app::Action::PopupExitFinished(id)),
                collapsing,
            );

            return genie.motion().host(watched).into();
        }

        if self
            .app
            .core()
            .main_window_id()
            .is_none_or(|main_id| main_id != id)
        {
            return self.app.view_window(id).map(crate::ui::Action::App);
        }

        // `use_template` switches between the composed chrome and a bare view;
        // this app never sets it false.
        let view = if self.app.core().window.use_template {
            self.app.view_main()
        } else {
            self.app.view().map(crate::ui::Action::App)
        };

        #[cfg(all(target_env = "gnu", not(target_os = "windows")))]
        super::malloc::trim(0);

        view
    }

    pub fn update(&mut self, message: crate::ui::Action<App::Message>) -> AppTask<App::Message> {
        #[allow(unused_mut)]
        let mut task = match message {
            crate::ui::Action::App(message) => self.app.update(message),
            crate::ui::Action::Cosmic(message) => self.cosmic_update(message),
            crate::ui::Action::Surface(action) => self.surface_update(action),
            // Intercepted by `TryInto` before the daemon ever calls `update`;
            // this arm exists only because the match must be total.
            crate::ui::Action::Exwl(_) | crate::ui::Action::None => iced::Task::none(),
        };

        {
            task = self.drain_text_context_popups(task);
        }

        self.sync_drawer_slide();

        #[cfg(all(target_env = "gnu", not(target_os = "windows")))]
        super::malloc::trim(0);

        task
    }

    /// Starts, turns round or latches the drawer's slide from
    /// `show_context`, which several sites write directly.
    ///
    /// `is_condensed` is passed as stored, even where a direct write has
    /// left it stale: `view_main` lays out from the same value, and the
    /// slide's path has to agree with that layout, not with what the value
    /// ought to be.
    fn sync_drawer_slide(&mut self) {
        let core = self.app.core();
        let shown = core.window.show_context;
        let condensed = core.is_condensed();
        let (extent, columns_fit) = if core.drawer_slide.starts_slide(shown) {
            let extent = core.drawer_extent(self.app.nav_bar().is_some());
            let columns_fit = !core.window.context_is_overlay
                && self.app.drawer_slide_fits_columns(extent, shown);
            (extent, columns_fit)
        } else {
            (0.0, false)
        };
        self.app
            .core_mut()
            .drawer_slide
            .sync(shown, condensed, extent, columns_fit);
    }

    /// Drain the text context-menu popup queues.
    ///
    /// `crate::ui::widget::text_context_menu` cannot issue a Task from inside a
    /// widget's `update()`, so it pushes onto two thread-local queues and pings
    /// [`text_context_menu::wake_subscription`] to make this run. Without a
    /// drainer the queues just grow and no popup ever appears.
    ///
    /// Drain teardowns first to preserve destroy-then-recreate order when a
    /// second right-click reuses the same id.
    fn drain_text_context_popups(
        &mut self,
        mut task: AppTask<App::Message>,
    ) -> AppTask<App::Message> {
        use crate::ui::widget::text_context_menu;

        for id in text_context_menu::take_popup_destroys() {
            // The view stays in `popup_views` until `SurfaceClosed`. Dropping it
            // here leaves the
            // still-live surface without a view, and `Shell::view` then falls
            // through to `App::view_window`, which renders the main view into
            // the popup's tiny limits.
            task = task.chain(iced::Task::done(crate::ui::action::exwl::remove_window(id)));
        }

        for req in text_context_menu::take_popup_requests() {
            let (settings, view) = text_context_menu::into_popup_view::<App::Message>(req);
            let id = settings.id;

            *self.opened_surfaces.entry(id).or_insert(0) += 1;
            self.popup_views.insert(id, view);
            task = task.chain(iced::Task::done(crate::ui::action::exwl::popup(
                id,
                settings.to_exwlshell(),
            )));
        }

        task
    }

    /// Close the main window.
    pub fn close(&mut self) -> AppTask<App::Message> {
        if let Some(id) = self.app.core().main_window_id() {
            iced::Task::done(crate::ui::action::exwl::remove_window(id))
        } else {
            iced::Task::none()
        }
    }

    /// Whether `id` names a popup surface this shell opened.
    ///
    /// The embedded case (`crate::dialog`) needs this to route a window id to
    /// the nested shell that owns it.
    #[must_use]
    pub fn has_popup_view(&self, id: &window::Id) -> bool {
        self.popup_views.contains_key(id)
    }

    /// The ids of every popup surface this shell has open.
    pub fn popup_view_ids(&self) -> impl ExactSizeIterator<Item = window::Id> + '_ {
        self.popup_views.keys().copied()
    }

    fn surface_update(
        &mut self,
        action: crate::ui::surface::Action<App::Message>,
    ) -> AppTask<App::Message> {
        match action {
            // The measurement channel behind `responsive_menu_bar`: the bar
            // renders expanded, `responsive_container` measures it, publishes
            // `(limits, size)` here, and on the next frame the bar sees it no
            // longer fits and collapses to a hamburger sized from this entry,
            // which it unwraps, so the map must be kept and never cleared.
            crate::ui::surface::Action::ResponsiveMenuBar {
                menu_bar,
                limits,
                size,
            } => {
                self.app
                    .core_mut()
                    .menu_bars
                    .insert(menu_bar, (limits, size));
                iced::Task::none()
            }
            // `menu`, `context_menu`, `segmented_button` and `dropdown` all
            // build their popups with `surface::action::simple_popup`.
            crate::ui::surface::Action::Popup(settings, view) => {
                let settings = settings();
                let id = settings.id;

                if settings.animate {
                    // The popup's own width decides how wide a band it
                    // collapses into, so the band is a constant size on
                    // screen rather than a constant fraction of the menu.
                    let menu_width = settings
                        .positioner
                        .size
                        .map_or(0.0, |(width, _)| width as f32);

                    self.popup_genies.insert(
                        id,
                        crate::ui::surface::collapse_corner(settings.positioner.gravity),
                        menu_width,
                    );
                }

                *self.opened_surfaces.entry(id).or_insert(0) += 1;
                if let Some(view) = view {
                    self.popup_views.insert(id, Box::new(move || view()));
                }

                iced::Task::done(crate::ui::action::exwl::popup(id, settings.to_exwlshell()))
            }
            crate::ui::surface::Action::DestroyPopup { id, animate } => {
                // An animated popup keeps its surface until it has finished
                // collapsing; `widget::popup_genie` publishes
                // `PopupExitFinished` and the removal happens there instead.
                if animate && self.popup_genies.begin_exit(id) {
                    return iced::Task::none();
                }

                // The view is dropped in `Action::SurfaceClosed`, once the
                // surface is really gone. The
                // popup keeps drawing between the two, and a popup without a
                // view falls through to `App::view_window`; see the comment in
                // `drain_text_context_popups`.
                iced::Task::done(crate::ui::action::exwl::remove_window(id))
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn cosmic_update(&mut self, message: crate::ui::app::Action) -> AppTask<App::Message> {
        use crate::ui::app::Action;

        match message {
            Action::WindowResize(id, width, height) => {
                if self.app.core().main_window_is(id) {
                    self.app.core_mut().set_window_width(width);
                    self.app.core_mut().set_window_height(height);
                }

                self.app.on_window_resize(id, width, height);
            }

            // A nested shell subscribes alongside its host, so each one acts
            // only on the shortcuts aimed at its own window
            Action::KeyboardNav(window_id, message)
                if self.app.core().main_window_id() == Some(window_id) =>
            {
                match message {
                    crate::ui::keyboard_nav::Action::FocusNext => {
                        return iced::widget::operation::focus_next()
                            .map(crate::ui::Action::Cosmic);
                    }
                    crate::ui::keyboard_nav::Action::FocusPrevious => {
                        return iced::widget::operation::focus_previous()
                            .map(crate::ui::Action::Cosmic);
                    }
                    crate::ui::keyboard_nav::Action::Escape => return self.app.on_escape(),
                    crate::ui::keyboard_nav::Action::Search => return self.app.on_search(),
                    crate::ui::keyboard_nav::Action::Fullscreen => {
                        return self.app.core().toggle_maximize(None);
                    }
                }
            }
            // Nothing to do. For `DrawerSlideSettled` the update itself is
            // what rebuilds the view, which then lays the drawer out at rest.
            Action::KeyboardNav(..) | Action::DrawerSlideSettled => {}

            Action::ContextDrawer(show) => {
                self.app.core_mut().set_show_context(show);
                return self.app.on_context_drawer();
            }

            Action::Drag => return self.app.core().drag(None),

            Action::Minimize => return self.app.core().minimize(None),

            Action::Maximize => return self.app.core().toggle_maximize(None),

            Action::NavBar(key) => {
                self.app.core_mut().nav_bar_set_toggled_condensed(false);
                return self.app.on_nav_select(key);
            }

            Action::NavBarContext(key) => {
                self.app.core_mut().nav_bar_set_context(key);
                return self.app.on_nav_context(key);
            }

            Action::NavBarResizeStart => {
                let min = self.app.nav_bar_min_width();
                self.app.core_mut().nav_bar_resize_start(min);
            }

            Action::NavBarResizeDrag(delta_x) => {
                let min = self.app.nav_bar_min_width();
                self.app.core_mut().nav_bar_resize_drag(delta_x, min);
            }

            // Both the drag and the button release end it, so the app is only
            // told once: the second one finds no drag in progress.
            Action::NavBarResizeEnd => {
                if let Some(width) = self.app.core_mut().nav_bar_resize_end() {
                    return self.app.on_nav_bar_resized(width);
                }
            }

            Action::PopupExitFinished(id) => {
                // The collapse is over; let the surface go. `SurfaceClosed`
                // drops the view and the animation state.
                return iced::Task::done(crate::ui::action::exwl::remove_window(id));
            }

            Action::ToggleNavBar => {
                self.app.core_mut().nav_bar_toggle();
            }

            Action::ToggleNavBarCondensed => {
                self.app.core_mut().nav_bar_toggle_condensed();
            }

            // Widgets that read `theme::active()` for colours rather than taking
            // them from the theme passed to `draw`, notably `segmented_button`'s
            // dividers, follow the theme being rendered only because
            // `set_active` is updated here too.
            Action::AppThemeChange(theme) => {
                self.theme = theme;
                crate::ui::theme::set_active(&self.theme);
            }

            Action::ScaleFactor(factor) => {
                self.app.core_mut().set_scale_factor(factor);
                // `ui::dnd` compares wl_data_device coordinates against iced
                // widget bounds, so it needs the same factor.
                crate::ui::dnd::set_scale_factor(factor);
            }

            Action::Close => {
                return match self.app.on_app_exit() {
                    Some(message) => self.app.update(message),
                    None => self.close(),
                };
            }

            Action::SurfaceClosed(id) => {
                // A widget running in the parent surface has no other way to
                // learn that its popup is gone; see `ui::surface::dismissal`.
                crate::ui::surface::dismissal::note(id);

                // The compositor has already torn down one surface for this id,
                // but if the id was already reused (a text context menu can
                // reopen with the same id before this late close arrives, see
                // `drain_text_context_popups`), a still-live surface is sharing
                // it. Only drop the view once every opened surface for this id
                // has been closed.
                if self.opened_surfaces.get_mut(&id).is_some_and(|v| {
                    *v = v.saturating_sub(1);
                    *v == 0
                }) {
                    self.opened_surfaces.remove(&id);
                    self.popup_views.remove(&id);
                    self.popup_genies.remove(id);
                }

                let mut ret = if let Some(msg) = self.app.on_close_requested(id) {
                    self.app.update(msg)
                } else {
                    iced::Task::none()
                };
                let core = self.app.core();
                if core.exit_on_main_window_closed && core.main_window_is(id) {
                    ret = iced::Task::batch([iced::exit::<crate::ui::Action<App::Message>>()]);
                }
                return ret;
            }

            Action::ShowWindowMenu => {
                if let Some(id) = self.app.core().main_window_id() {
                    return crate::ui::command::show_window_menu(id);
                }
            }

            Action::Focus(id) => {
                self.app.core_mut().focused_window = vec![id];
            }

            Action::Unfocus(id) => {
                let core = self.app.core_mut();
                if core.focused_window().is_some_and(|cur| cur == id) {
                    core.focused_window.pop();
                }
            }

            // Both carry no state this shell keeps. `Opened` used to trigger the
            // `run_with_handle` round trip that classified the windowing
            // system; that is decided in `run` now.
            Action::Opened(_) | Action::WindowingSystemInitialized => (),
        }

        iced::Task::none()
    }

    pub fn subscription(&self) -> Subscription<crate::ui::Action<App::Message>> {
        use crate::ui::app::Action;

        // Every Wayland-specific arm this used to carry is gone with
        // `event::wayland`, and all but one of them was replaceable: a popup the
        // compositor dismisses arrives as an ordinary `window::Event::Closed`
        // for the popup's own id (verified in the exwlshell spike), which
        // `SurfaceClosed` already handled. The one with no replacement is
        // `WindowEvent::WindowState`: exwlshell reports no `xdg_toplevel`
        // configure states, so nothing can tell the shell that the window is
        // maximized. The fields that used to carry it are gone; wiring the
        // compositor state through here is what would bring them back.
        let window_events = iced::event::listen_with(|event, _, id| match event {
            iced::Event::Window(window::Event::Resized(iced::Size { width, height })) => {
                Some(Action::WindowResize(id, width, height))
            }
            iced::Event::Window(window::Event::Opened { .. }) => Some(Action::Opened(id)),
            iced::Event::Window(window::Event::Closed) => Some(Action::SurfaceClosed(id)),
            iced::Event::Window(window::Event::Focused) => Some(Action::Focus(id)),
            iced::Event::Window(window::Event::Unfocused) => Some(Action::Unfocus(id)),
            _ => None,
        });

        let mut subscriptions = vec![
            self.app.subscription().map(crate::ui::Action::App),
            window_events.map(crate::ui::Action::Cosmic),
        ];

        if self.app.core().keyboard_nav() {
            subscriptions.push(
                crate::ui::keyboard_nav::subscription()
                    .map(|(window_id, action)| Action::KeyboardNav(window_id, action))
                    .map(crate::ui::Action::Cosmic),
            );
        }

        // Drives the text context-menu popup queues: a right-click queues a
        // popup but publishes no message, so this re-emits `Action::None` to
        // make `update()` run and drain the queue.
        subscriptions.push(crate::ui::widget::text_context_menu::wake_subscription::<
            App::Message,
        >());

        Subscription::batch(subscriptions)
    }
}
