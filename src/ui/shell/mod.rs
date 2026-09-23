// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! This app's application shell: the code that sits between iced and
//! `crate::app::App`.
//!
//! [`Application`] and [`Shell`] drive `iced::daemon` directly.
//!
//! `src/dialog.rs` is a second, nested [`Application`]: its file-picker
//! windows embed a [`Shell`] of their own inside this app's daemon, driving it
//! through the public `app`, `init` and popup accessors on [`Shell`].

pub mod context_drawer;
pub mod core;
pub mod drawer_slide;
#[cfg(all(target_env = "gnu", not(target_os = "windows")))]
pub mod malloc;
pub(crate) mod popup_genie;
pub mod runner;
pub mod settings;

pub use core::Core;
pub use runner::{Shell, run};
pub use settings::Settings;

use crate::ui::Element;
use crate::ui::app::Task;
use crate::ui::convert::ToPadding;
use crate::ui::convert::{PushMaybe, ToColor, ToRadius};
use crate::ui::iced::{Subscription, window};
use crate::ui::widget::{nav_bar, segmented_button};

/// An interactive application driven by [`Shell`].
#[allow(unused_variables)]
pub trait Application
where
    Self: Sized + 'static,
{
    /// Argument received by [`Application::init`].
    type Flags;

    /// Message type specific to the app.
    type Message: Clone + std::fmt::Debug + Send + 'static;

    /// An ID that uniquely identifies the application.
    const APP_ID: &'static str;

    /// Grants access to the [`Core`].
    fn core(&self) -> &Core;

    /// Grants access to the [`Core`].
    fn core_mut(&mut self) -> &mut Core;

    /// Creates the application, and optionally emits a task on initialize.
    fn init(core: Core, flags: Self::Flags) -> (Self, Task<Self::Message>);

    /// Displays a context drawer on the side of the window when `Some`.
    fn context_drawer(
        &self,
    ) -> Option<crate::ui::shell::context_drawer::ContextDrawer<'_, Self::Message>> {
        None
    }

    /// Displays a dialog in the center of the window when `Some`.
    fn dialog(&self) -> Option<Element<'_, Self::Message>> {
        None
    }

    /// Displays a footer at the bottom of the window when `Some`.
    fn footer(&self) -> Option<Element<'_, Self::Message>> {
        None
    }

    /// Attaches elements to the start section of the header.
    fn header_start(&self) -> Vec<Element<'_, Self::Message>> {
        Vec::new()
    }

    /// Attaches elements to the center of the header.
    fn header_center(&self) -> Vec<Element<'_, Self::Message>> {
        Vec::new()
    }

    /// Attaches elements to the end section of the header.
    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        Vec::new()
    }

    /// The nav bar widget, when one should be drawn.
    fn nav_bar(&self) -> Option<Element<'_, crate::ui::Action<Self::Message>>> {
        None
    }

    /// Shows a context menu for the active nav bar item.
    fn nav_context_menu(
        &self,
    ) -> Option<Vec<crate::ui::widget::menu::Tree<crate::ui::Action<Self::Message>>>> {
        None
    }

    /// The app's nav bar model, if it has one.
    fn nav_model(&self) -> Option<&segmented_button::SingleSelectModel> {
        None
    }

    /// The narrowest the nav bar may be drawn, which the drag stops at.
    /// Measured from the model, so it follows an entry being added, renamed
    /// or removed with no frame's delay.
    fn nav_bar_min_width(&self) -> f32 {
        self.nav_model().map_or(0.0, nav_bar::min_width)
    }

    /// Called before closing the application. Returning a message overrides closing windows.
    fn on_app_exit(&mut self) -> Option<Self::Message> {
        None
    }

    /// Called when a window has been closed.
    fn on_close_requested(&self, id: window::Id) -> Option<Self::Message> {
        None
    }

    /// Called when the context drawer is toggled.
    fn on_context_drawer(&mut self) -> Task<Self::Message> {
        Task::none()
    }

    /// Whether the main content can follow a drawer slide by moving its
    /// list columns alone, `extent` being the width the drawer takes and
    /// `opening` which way it goes. See `crate::ui::shell::drawer_slide`.
    fn drawer_slide_fits_columns(&self, extent: f32, opening: bool) -> bool {
        let _ = (extent, opening);
        false
    }

    /// Called when the escape key is pressed.
    fn on_escape(&mut self) -> Task<Self::Message> {
        Task::none()
    }

    /// Called when a navigation item is selected.
    fn on_nav_select(&mut self, id: nav_bar::Id) -> Task<Self::Message> {
        Task::none()
    }

    /// Called when a context menu is requested for a navigation item.
    fn on_nav_context(&mut self, id: nav_bar::Id) -> Task<Self::Message> {
        Task::none()
    }

    /// Called when the nav bar has been dragged to a new width, in logical
    /// pixels. An application that remembers the width saves it here.
    fn on_nav_bar_resized(&mut self, width: u16) -> Task<Self::Message> {
        Task::none()
    }

    /// Called when the search function is requested.
    fn on_search(&mut self) -> Task<Self::Message> {
        Task::none()
    }

    /// Called when a window is resized.
    fn on_window_resize(&mut self, id: window::Id, width: f32, height: f32) {}

    /// Event sources that are to be listened to.
    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::none()
    }

    /// Respond to an application-specific message.
    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        Task::none()
    }

    /// Constructs the view for the main window.
    fn view(&self) -> Element<'_, Self::Message>;

    /// Constructs views for other windows.
    fn view_window(&self, id: window::Id) -> Element<'_, Self::Message> {
        panic!("no view for window {id:?}");
    }

    /// Composes the application chrome around [`Application::view`].
    ///
    /// Ported from pop-os/libcosmic d9431dc, src/app/mod.rs
    /// (`ApplicationExt::view_main`).
    #[allow(clippy::too_many_lines)]
    fn view_main(&self) -> Element<'_, crate::ui::Action<Self::Message>> {
        use crate::ui::app::Action;
        use crate::ui::iced::Length;
        use crate::ui::widget::{container, id_container, space};
        use crate::ui::{Apply, widget};

        let core = self.core();
        let is_condensed = core.is_condensed();
        // The outline this window draws around itself, if any. Drawing none
        // is the default: a compositor that rounds clips these square corners
        // cleanly, whereas rounding on top of that shows two curves that do
        // not meet, and a border of our own beside the compositor's.
        let outline = crate::ui::theme::custom::window_outline();
        let sharp_corners = outline.is_none();
        // exwlshell reports no `xdg_toplevel` configure states, so the window
        // never learns that it is maximized; see `src/ui/command.rs`.
        let maximized = false;
        let content_container = core.window.content_container;
        let show_context = core.window.show_context;
        let nav_bar_active = core.nav_bar_active();
        let focused = core
            .focus_chain()
            .iter()
            .any(|i| Some(*i) == core.main_window_id());

        let border_padding = core.border_padding();

        let main_content_padding = if content_container {
            let right_padding = if show_context { 0 } else { border_padding };
            let left_padding = if nav_bar_active { 0 } else { border_padding };

            [0, right_padding, 0, left_padding]
        } else {
            [0, 0, 0, 0]
        };

        let content_row = widget::Row::with_children({
            let mut widgets = Vec::with_capacity(3);

            // Insert nav bar onto the left side of the window.
            let has_nav = if let Some(nav) = self.nav_bar() {
                let nav = id_container(nav, widget::Id::new("COSMIC_nav_bar"));
                // A fixed width pins the limits, which carry it down through
                // the panel's own containers, so its entries grow with it.
                // Condensed, the panel is sized by the window instead.
                let nav = container(nav);
                let nav = if is_condensed {
                    nav
                } else {
                    nav.width(Length::Fixed(
                        core.nav_bar_effective_width(self.nav_bar_min_width()),
                    ))
                };
                // The gap to the content doubles as the outer half of the
                // divider's grab zone, so it is padding on neither side.
                let gap = if is_condensed { border_padding } else { 8 };
                let panel = widget::Row::with_children(vec![
                    container(nav)
                        .padding(([0, 0, border_padding, border_padding]).to_padding())
                        .into(),
                    widget::space::horizontal()
                        .width(Length::Fixed(f32::from(gap)))
                        .into(),
                ]);
                if is_condensed {
                    // The panel covers the window and has nothing to be
                    // dragged against.
                    widgets.push(panel.into());
                } else {
                    // Laid over the panel's trailing edge rather than beside
                    // it, so what is grabbed is the boundary itself: the
                    // handle reaches `GRAB` back over the panel and fills the
                    // gap after it. The line and its delay are the divider's
                    // own doing; see `widget::nav_bar_divider`.
                    let handle = widget::nav_bar_divider(
                        crate::mouse_area::MouseArea::new(
                            widget::space::horizontal()
                                .width(Length::Fixed(
                                    widget::nav_bar_divider::GRAB + f32::from(gap),
                                ))
                                .height(Length::Fill),
                        )
                        .interaction(crate::ui::iced_core::mouse::Interaction::ResizingHorizontally)
                        .on_press(|_| crate::ui::Action::Cosmic(Action::NavBarResizeStart))
                        // A press that lands within the double-click interval
                        // of the last one reaches `on_double_click` and
                        // nowhere else, and would otherwise begin a drag the
                        // core knows nothing about.
                        .on_double_click(|_| crate::ui::Action::Cosmic(Action::NavBarResizeStart))
                        .on_drag_delta(|delta| {
                            crate::ui::Action::Cosmic(Action::NavBarResizeDrag(delta.x))
                        })
                        .on_drag_end(|_| crate::ui::Action::Cosmic(Action::NavBarResizeEnd))
                        .on_release(|_| crate::ui::Action::Cosmic(Action::NavBarResizeEnd)),
                    );
                    widgets.push(
                        widget::Stack::with_children(vec![
                            panel.into(),
                            widget::Row::with_children(vec![
                                widget::space::horizontal().width(Length::Fill).into(),
                                handle.into(),
                            ])
                            .height(Length::Fill)
                            .into(),
                        ])
                        .into(),
                    );
                }
                true
            } else {
                false
            };

            if self.nav_model().is_none() || core.show_content() {
                let main_content = self.view();

                let context_width = core.context_width(has_nav);
                if core.window.context_is_overlay && show_context {
                    if let Some(context) = self.context_drawer() {
                        widgets.push(
                            widget::context_drawer(
                                context.title,
                                context.actions,
                                context.header,
                                context.footer,
                                context.on_close,
                                main_content,
                                context.content,
                                context_width,
                            )
                            .apply(|drawer| {
                                Element::from(id_container(
                                    drawer,
                                    widget::Id::new("COSMIC_context_drawer"),
                                ))
                            })
                            .apply(container)
                            .padding(
                                ([0, if content_container { border_padding } else { 0 }, 0, 0])
                                    .to_padding(),
                            )
                            .apply(Element::from)
                            .map(crate::ui::Action::App),
                        );
                    } else {
                        widgets.push(
                            container(main_content.map(crate::ui::Action::App))
                                .padding(main_content_padding.to_padding())
                                .into(),
                        );
                    }
                } else {
                    widgets.push(
                        container(main_content.map(crate::ui::Action::App))
                            .padding(main_content_padding.to_padding())
                            .into(),
                    );
                    if let Some(context) = self.context_drawer() {
                        widgets.push(
                            widget::ContextDrawer::new_inner(
                                context.title,
                                context.actions,
                                context.header,
                                context.footer,
                                context.content,
                                context.on_close,
                                context_width,
                            )
                            .apply(Element::from)
                            .map(crate::ui::Action::App)
                            .apply(container)
                            .width(context_width)
                            .apply(|drawer| {
                                Element::from(id_container(
                                    drawer,
                                    widget::Id::new("COSMIC_context_drawer"),
                                ))
                            })
                            .apply(container)
                            .padding(
                                (if content_container {
                                    [0, border_padding, border_padding, border_padding]
                                } else {
                                    [0, 0, 0, 0]
                                })
                                .to_padding(),
                            )
                            .into(),
                        );
                    } else {
                        // Keeps the widget tree shape stable when there is no drawer
                        widgets.push(space::horizontal().width(Length::Shrink).into());
                    }
                }
            }

            widgets
        });

        let content_col = widget::Column::with_capacity(2)
            .push(content_row)
            .push_maybe(self.footer().map(|footer| {
                container(footer.map(crate::ui::Action::App))
                    .padding(([0, border_padding, border_padding, border_padding]).to_padding())
            }));

        let content: Element<'_, crate::ui::Action<Self::Message>> = if content_container {
            content_col
                .width(Length::Fill)
                .height(Length::Fill)
                .apply(|w| id_container(w, widget::Id::new("COSMIC_content_container")))
                .into()
        } else {
            content_col.into()
        };

        // Ensures visually aligned radii for content and window corners
        let window_corner_radius = outline.map_or_else(
            || crate::ui::theme::active().cosmic().radius_0(),
            |radius| [radius; 4],
        );

        let view_column = widget::Column::with_capacity(2)
            .push_maybe(if core.window.show_headerbar {
                Some({
                    let mut header = widget::header_bar()
                        .focused(focused)
                        .maximized(maximized)
                        .sharp_corners(sharp_corners)
                        .title(&core.window.header_title)
                        .on_drag(crate::ui::Action::Cosmic(Action::Drag))
                        .on_right_click(crate::ui::Action::Cosmic(Action::ShowWindowMenu))
                        .on_double_click(crate::ui::Action::Cosmic(Action::Maximize));

                    if self.nav_model().is_some() {
                        let toggle = widget::nav_bar_toggle()
                            .active(core.nav_bar_active())
                            .selected(focused)
                            .on_toggle(if is_condensed {
                                crate::ui::Action::Cosmic(Action::ToggleNavBarCondensed)
                            } else {
                                crate::ui::Action::Cosmic(Action::ToggleNavBar)
                            });

                        header = header.start(toggle);
                    }

                    if core.window.show_close {
                        header = header.on_close(crate::ui::Action::Cosmic(Action::Close));
                    }

                    if core.window.show_maximize {
                        header = header.on_maximize(crate::ui::Action::Cosmic(Action::Maximize));
                    }

                    if core.window.show_minimize {
                        header = header.on_minimize(crate::ui::Action::Cosmic(Action::Minimize));
                    }

                    for element in self.header_start() {
                        header = header.start(element.map(crate::ui::Action::App));
                    }

                    for element in self.header_center() {
                        header = header.center(element.map(crate::ui::Action::App));
                    }

                    for element in self.header_end() {
                        header = header.end(element.map(crate::ui::Action::App));
                    }

                    let header: Element<'_, crate::ui::Action<Self::Message>> = header.into();

                    if content_container {
                        header.apply(|w| id_container(w, widget::Id::new("COSMIC_header")))
                    } else {
                        // Needed to avoid header bar corner gaps for apps without a content container
                        header
                            .apply(container)
                            .class(crate::ui::theme::Container::custom(move |theme| {
                                let cosmic = theme.cosmic();
                                container::Style {
                                    background: Some(crate::ui::iced::Background::Color(
                                        cosmic.background(theme.transparent).base.to_color(),
                                    )),
                                    border: crate::ui::iced::Border {
                                        radius: [
                                            (window_corner_radius[0] - 1.0).max(0.0),
                                            (window_corner_radius[1] - 1.0).max(0.0),
                                            cosmic.radius_0()[2],
                                            cosmic.radius_0()[3],
                                        ]
                                        .to_radius(),
                                        ..Default::default()
                                    },
                                    ..Default::default()
                                }
                            }))
                            .apply(|w| id_container(w, widget::Id::new("COSMIC_header")))
                    }
                })
            } else {
                None
            })
            // The content element contains every element beneath the header.
            .push(content)
            .apply(container)
            // The inset exists to leave room for the outline, so without one
            // the content reaches the window edge
            .padding(if outline.is_some() { 1 } else { 0 })
            .class(crate::ui::theme::Container::custom(move |theme| {
                container::Style {
                    // Painted here rather than as the surface background,
                    // which every surface shares and popups need see-through.
                    // Without an outline this is what fills the window, so it
                    // paints whether or not the content is in a container.
                    background: if content_container || outline.is_none() {
                        Some(crate::ui::iced::Background::Color(
                            theme.cosmic().background(theme.transparent).base.to_color(),
                        ))
                    } else {
                        None
                    },
                    border: crate::ui::iced::Border {
                        color: theme.cosmic().bg_divider().to_color(),
                        width: if outline.is_some() { 1.0 } else { 0.0 },
                        radius: window_corner_radius.to_radius(),
                    },
                    ..Default::default()
                }
            }));

        // Show any current dialog on top and centered over the view content
        // We have to use a popover even without a dialog to keep the tree from changing
        let mut popover = widget::popover(view_column).modal(true);
        if let Some(dialog) = self
            .dialog()
            .map(|w| Element::from(id_container(w, widget::Id::new("COSMIC_dialog"))))
        {
            popover = popover.popup(dialog.map(crate::ui::Action::App));
        }

        let view_element: Element<'_, crate::ui::Action<Self::Message>> = popover.into();
        if core.debug {
            view_element.explain(crate::ui::iced::Color::WHITE)
        } else {
            view_element
        }
    }

    /// Overrides the default window style.
    fn style(&self) -> Option<crate::ui::iced::theme::Style> {
        None
    }

    // --- provided helpers ---

    /// Initiates a window drag.
    fn drag(&mut self) -> Task<Self::Message> {
        self.core().drag(None)
    }

    /// Maximizes the window.
    fn maximize(&mut self) -> Task<Self::Message> {
        self.core().maximize(None, true)
    }

    /// Minimizes the window.
    fn minimize(&mut self) -> Task<Self::Message> {
        self.core().minimize(None)
    }

    /// Get the title of a window.
    fn title(&self, id: window::Id) -> &str {
        self.core().title.get(&id).map_or("", String::as_str)
    }

    /// Set the context drawer visibility.
    fn set_show_context(&mut self, show: bool) {
        self.core_mut().set_show_context(show);
    }

    /// Set the header bar title.
    fn set_header_title(&mut self, title: String) {
        self.core_mut().set_header_title(title);
    }

    /// Set the title of a window.
    fn set_window_title(&mut self, title: String, id: window::Id) -> Task<Self::Message> {
        self.core_mut().title.insert(id, title.clone());
        // The daemon's `.title(..)` closure supplies the compositor with the
        // title from the `title` map.
        Task::none()
    }
}
