// Copyright 2025 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Right-click context menu for widgets with selectable text.
//!
//! Use [`context_menu_overlay`] from your widget's `overlay()` method
//! with any widget that implements
//! [`HasSelectableText`]
//! to get a context menu with Copy, Select All, and optionally Cut/Paste.
//!
//! Internally uses the [`Menu`](crate::ui::widget::menu) system for proper
//! rendering, hover effects, and positioning.
//!
//! On Wayland, [`create_text_context_popup`] can be used instead to show
//! the context menu as a native popup surface.
//!
//! Vendored from pop-os/libcosmic, src/widget/text_context_menu.rs
//!
//! The queues below are drained by [`crate::ui::shell::runner::Shell`].

pub use crate::ui::widget::text::{HasSelectableText, clipboard_has_text};

use iced_core::window;

use crate::ui::theme;
use crate::ui::widget;
use crate::ui::widget::RcElementWrapper;
use crate::ui::widget::menu::{
    self, CloseCondition, ItemHeight, ItemWidth, Menu, MenuBarState, PathHighlight, menu_roots_diff,
};

use iced_core::event;
use iced_core::layout::Limits;
use iced_core::widget::Tree;
use iced_core::{
    Clipboard, Layout, Point, Rectangle, Shell, Size, Vector, clipboard, mouse, overlay, renderer,
};
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

/// Shared state for communicating deferred context menu actions
/// from a Wayland popup back to the owning text widget.
pub(crate) type PendingAction = Arc<Mutex<Option<TextCtxAction>>>;

/// Creates a new [`PendingAction`] for use with popup-based context menus.
pub(crate) fn pending_action() -> PendingAction {
    Arc::new(Mutex::new(None))
}

/// Takes a pending action if one was set by a popup menu, and returns it.
pub(crate) fn take_pending_action(pending: &PendingAction) -> Option<TextCtxAction> {
    pending.lock().ok().and_then(|mut guard| guard.take())
}

use std::cell::Cell;

thread_local! {
    // `Option` rather than the sentinel: `window::none()` is a lazily allocated
    // id (`window::Id` has no `NONE` const, see `ui::window`), so
    // it cannot appear in a `const` initializer.
    static CURRENT_WINDOW_ID: Cell<Option<iced_core::window::Id>> = const { Cell::new(None) };
}

use crate::ui::surface::PopupSettings;

/// A request to create a text context-menu popup surface, queued by a widget
/// during `update()` and drained by the shell runner
/// ([`crate::ui::shell::runner::Shell`]) so the popup goes through the normal
/// popup Task + view pipeline.
pub(crate) struct PopupRequest {
    settings: PopupSettings,
    menu: Menu<'static, TextCtxAction>,
    selected_text: Option<String>,
    pending_action: PendingAction,
}

thread_local! {
    static PENDING_POPUP_REQUESTS: std::cell::RefCell<Vec<PopupRequest>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) fn set_current_window_id(id: iced_core::window::Id) {
    CURRENT_WINDOW_ID.set(Some(id));
}

pub(crate) fn current_window_id() -> iced_core::window::Id {
    CURRENT_WINDOW_ID
        .get()
        .unwrap_or_else(crate::ui::window::none)
}

/// Drains all popup requests queued by widgets this frame.
pub(crate) fn take_popup_requests() -> Vec<PopupRequest> {
    PENDING_POPUP_REQUESTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Consumes a [`PopupRequest`], returning the popup settings plus a view
/// builder. The builder rebuilds the menu element each frame from the
/// captured content, independent of app state.
#[allow(clippy::type_complexity)]
pub(crate) fn into_popup_view<Message: Clone + 'static>(
    req: PopupRequest,
) -> (
    PopupSettings,
    Box<dyn Fn() -> crate::ui::Element<'static, crate::ui::Action<Message>> + Send + Sync>,
) {
    let PopupRequest {
        settings,
        menu,
        selected_text,
        pending_action,
    } = req;

    let view = Box::new(move || {
        let popup_widget: TextContextMenuPopup<Message> = TextContextMenuPopup {
            menu: menu.clone(),
            selected_text: selected_text.clone(),
            pending_action: pending_action.clone(),
            _phantom: std::marker::PhantomData,
        };
        crate::ui::Element::from(
            crate::ui::widget::container(popup_widget).center(iced_core::Length::Fill),
        )
        .map(crate::ui::action::app)
    });

    (settings, view)
}

thread_local! {
    static PENDING_POPUP_DESTROYS: std::cell::RefCell<Vec<iced_core::window::Id>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Queues a context-menu popup for teardown.
///
/// Mirrors [`create_text_context_popup`]'s request queue: widgets running
/// inside `update()` can't reach the shell to issue a Task, so they push the
/// id here and the shell runner drains it into a window-removal Task that
/// flows through the normal surface pipeline.
fn queue_destroy_popup(id: iced_core::window::Id) {
    PENDING_POPUP_DESTROYS.with(|q| q.borrow_mut().push(id));
    wake_runtime();
}

static WAKE_TX: std::sync::OnceLock<iced::futures::channel::mpsc::Sender<()>> =
    std::sync::OnceLock::new();

/// Stable identity for [`wake_subscription`].
struct PopupWake;

/// Nudges the runtime so the shell runner runs and drains the popup queues.
fn wake_runtime() {
    if let Some(tx) = WAKE_TX.get() {
        let _ = tx.clone().try_send(());
    }
}

/// Subscription that backs [`wake_runtime`]: it owns the receiving end of the
/// wake channel and re-emits each ping as [`crate::ui::Action::None`]. Add it to
/// the app's subscriptions (done by the shell runner) so popup creation
/// and teardown queued from widget `update()` get drained promptly.
pub(crate) fn wake_subscription<Message: Send + 'static>()
-> iced::Subscription<crate::ui::Action<Message>> {
    use iced::futures::{SinkExt, StreamExt};
    iced::Subscription::run_with(std::any::TypeId::of::<PopupWake>(), |_| {
        iced::stream::channel(
            16,
            |mut output: iced::futures::channel::mpsc::Sender<crate::ui::Action<Message>>| async move {
                let (tx, mut rx) = iced::futures::channel::mpsc::channel(16);
                let _ = WAKE_TX.set(tx);
                while rx.next().await.is_some() {
                    let _ = output.send(crate::ui::Action::None).await;
                }
            },
        )
    })
}

/// Drains all popup teardown requests queued by widgets this frame.
///
/// Called by the shell runner, which turns each id into a window-removal
/// Task. Drained before the creation queue so a destroy-then-recreate (a
/// second right-click reusing the same id) keeps its order.
pub(crate) fn take_popup_destroys() -> Vec<iced_core::window::Id> {
    PENDING_POPUP_DESTROYS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Creates a context menu overlay for any widget implementing
/// [`HasSelectableText`].
///
/// Call this from your widget's `overlay()` method. Pass `on_input` for
/// editable widgets so Cut and Paste can publish text-change messages.
///
/// The `menu_bar_state` parameter must be a persistent [`MenuBarState`]
/// stored in the widget's tree state.
pub(crate) fn context_menu_overlay<'a, W, Message>(
    widget: &'a W,
    tree: &'a mut Tree,
    on_input: Option<&'a dyn Fn(String) -> Message>,
    translation: Vector,
    menu_bar_state: MenuBarState,
) -> Option<overlay::Element<'a, Message, crate::ui::Theme, iced::Renderer>>
where
    W: HasSelectableText + 'a,
    Message: Clone + 'static,
{
    let click_position = widget.context_menu_position(tree)?;
    let selected_text = widget.selected_text(tree);
    let is_editable = widget.is_editable();

    let mut menu_roots = build_menu_roots(
        is_editable,
        selected_text.is_some(),
        widget.has_text(tree),
        widget.clipboard_has_text(tree),
    );
    menu_roots.iter_mut().for_each(menu::Tree::set_index);

    let bounds = Rectangle {
        x: click_position.x,
        y: click_position.y,
        width: 240.0,
        height: 240.0,
    };

    let item_count = menu_roots[0].children.len();
    menu_bar_state.inner.with_data_mut(|state| {
        let stale = state.menu_states.first().is_some_and(|ms| {
            ms.menu_bounds.child_positions.len() != item_count
                || (ms.menu_bounds.parent_bounds.x - bounds.x).abs() > 0.5
                || (ms.menu_bounds.parent_bounds.y - bounds.y).abs() > 0.5
        });
        if !state.open || stale {
            state.menu_states.clear();
            state.active_root.clear();
            state.open = true;
        }
        menu_roots_diff(&menu_roots, &mut state.tree);
    });

    let menu = Menu {
        tree: menu_bar_state.clone(),
        menu_roots: Cow::Owned(menu_roots),
        bounds_expand: 16,
        menu_overlays_parent: true,
        close_condition: CloseCondition {
            leave: false,
            click_outside: true,
            click_inside: true,
        },
        item_width: ItemWidth::Uniform(240),
        item_height: ItemHeight::Dynamic(40),
        bar_bounds: bounds,
        main_offset: -(bounds.height as i32),
        cross_offset: 0,
        root_bounds_list: vec![bounds],
        path_highlight: Some(PathHighlight::MenuActive),
        style: Cow::Owned(theme::menu_bar::MenuBarStyle::Default),
        position: Point::new(translation.x, translation.y),
        is_overlay: true,
        window_id: crate::ui::window::none(),
        depth: 0,
        on_surface_action: None,
    };

    Some(overlay::Element::new(Box::new(TextMenuOverlay {
        menu,
        widget,
        tree,
        on_input,
    })))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextCtxAction {
    Copy,
    Cut,
    Paste,
    SelectAll,
}

fn build_menu_roots(
    is_editable: bool,
    has_selection: bool,
    has_text: bool,
    clipboard_has_text: bool,
) -> Vec<menu::Tree<TextCtxAction>> {
    let item = |label: &'static str, action: TextCtxAction, enabled: bool| {
        menu::Tree::from(crate::ui::Element::from(
            menu::menu_button(vec![widget::text(label).into()])
                .on_press_maybe(enabled.then_some(action)),
        ))
    };

    let mut items = Vec::with_capacity(4);
    if is_editable {
        items.push(item("Cut", TextCtxAction::Cut, has_selection));
    }
    items.push(item("Copy", TextCtxAction::Copy, has_selection));
    if is_editable {
        items.push(item("Paste", TextCtxAction::Paste, clipboard_has_text));
    }
    items.push(item("Select All", TextCtxAction::SelectAll, has_text));

    vec![menu::Tree::with_children(
        RcElementWrapper::new(crate::ui::Element::from(widget::Row::new())),
        items,
    )]
}

struct TextMenuOverlay<'a, W, Message: Clone + 'static> {
    menu: Menu<'a, TextCtxAction>,
    widget: &'a W,
    tree: &'a mut Tree,
    on_input: Option<&'a dyn Fn(String) -> Message>,
}

impl<W, Message> overlay::Overlay<Message, crate::ui::Theme, iced::Renderer>
    for TextMenuOverlay<'_, W, Message>
where
    W: HasSelectableText,
    Message: Clone + 'static,
{
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> iced_core::layout::Node {
        // Initialise the menu before the first draw so it appears at the click
        // position immediately
        let needs_init = self
            .menu
            .tree
            .inner
            .with_data(|state| state.open && state.menu_states.is_empty());

        if needs_init {
            let overlay_offset = Point::ORIGIN - self.menu.position;
            let bar_bounds = self.menu.bar_bounds;
            let main_offset = self.menu.main_offset as f32;
            let overlay_cursor = bar_bounds.center();

            let mut init_messages: Vec<TextCtxAction> = Vec::new();
            let mut init_shell = Shell::new(&mut init_messages);
            menu::init_root_menu(
                &mut self.menu,
                renderer,
                &mut init_shell,
                overlay_cursor,
                bounds,
                overlay_offset,
                bar_bounds,
                main_offset,
            );
        }

        self.menu.layout(
            renderer,
            Limits::NONE
                .min_width(bounds.width)
                .max_width(bounds.width)
                .min_height(bounds.height)
                .max_height(bounds.height),
        )
    }

    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.menu.draw(renderer, theme, style, layout, cursor);
    }

    fn update(
        &mut self,
        event: &event::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        // Right-clicks are not menu interactions. A right-click *on* the menu
        // (notably the press/release that opened it) is swallowed so it does
        // not close the menu. A right-click *outside* closes it. Clearing the
        // menu position lets the widget reopen it at the new point on the same
        // event. Initialization happens in `layout()`.
        if matches!(
            event,
            event::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
                | event::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right))
        ) {
            let over_menu = cursor
                .position()
                .is_some_and(|p| layout.bounds().contains(p));
            if !over_menu {
                self.menu.tree.inner.with_data_mut(|state| {
                    state.menu_states.clear();
                    state.active_root.clear();
                    state.open = false;
                });
                self.widget.set_context_menu_position(self.tree, None);
                shell.request_redraw();
            }
            return;
        }

        let mut local_messages = Vec::new();
        let mut local_shell = Shell::new(&mut local_messages);

        self.menu
            .update(event, layout, cursor, renderer, clipboard, &mut local_shell);

        if local_shell.is_event_captured() {
            shell.capture_event();
        }
        shell.request_redraw_at(local_shell.redraw_request());
        if local_shell.is_layout_invalid() {
            shell.invalidate_layout();
        }

        for action in local_messages {
            match action {
                TextCtxAction::Copy => {
                    self.widget.copy_to_clipboard(self.tree, clipboard);
                }
                TextCtxAction::Cut => {
                    self.widget.copy_to_clipboard(self.tree, clipboard);
                    if let Some(contents) = self.widget.delete_selection(self.tree)
                        && let Some(on_input) = self.on_input
                    {
                        shell.publish((on_input)(contents));
                    }
                }
                TextCtxAction::Paste => {
                    let content: String = clipboard
                        .read(clipboard::Kind::Standard)
                        .unwrap_or_default();
                    if let Some(contents) = self.widget.paste_text(self.tree, &content)
                        && let Some(on_input) = self.on_input
                    {
                        shell.publish((on_input)(contents));
                    }
                }
                TextCtxAction::SelectAll => {
                    self.widget.select_all(self.tree);
                }
            }
            self.widget.set_context_menu_position(self.tree, None);
            // The menu closes and the selection may have changed.
            shell.request_redraw();
        }

        let is_open = self.menu.tree.inner.with_data(|state| state.open);
        if !is_open {
            self.widget.set_context_menu_position(self.tree, None);
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Idle
        } else {
            mouse::Interaction::None
        }
    }
}

/// Queues a Wayland popup surface containing the text context menu.
///
/// Pushes a [`PopupRequest`] onto the request queue; the shell runner drains
/// it and creates the popup, so it flows through the normal Task + view
/// pipeline.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_text_context_popup(
    click_position: Point,
    selected_text: Option<String>,
    is_editable: bool,
    has_selection: bool,
    has_text: bool,
    clipboard_has_text: bool,
    menu_bar_state: &MenuBarState,
    pending_action: &PendingAction,
    renderer: &iced::Renderer,
    viewport: &Rectangle,
    cursor: mouse::Cursor,
    window_id: window::Id,
) {
    use crate::ui::surface::{PopupSettings, Positioner};

    if window_id == crate::ui::window::none() {
        return;
    }

    let mut menu_roots = build_menu_roots(is_editable, has_selection, has_text, clipboard_has_text);
    menu_roots.iter_mut().for_each(menu::Tree::set_index);

    let id = menu_bar_state.inner.with_data_mut(|state| {
        state.menu_states.clear();
        state.active_root.clear();
        menu_roots_diff(&menu_roots, &mut state.tree);
        if let Some(id) = state.popup_id.get(&window_id).copied() {
            queue_destroy_popup(id);
            state.view_cursor = cursor;
            id
        } else {
            state.open = true;
            state.view_cursor = cursor;
            iced::window::Id::unique()
        }
    });

    let bounds = Rectangle {
        x: click_position.x,
        y: click_position.y,
        width: 240.0,
        height: 240.0,
    };

    let mut popup_menu: Menu<'static, TextCtxAction> = Menu {
        tree: menu_bar_state.clone(),
        menu_roots: Cow::Owned(menu_roots),
        bounds_expand: 16,
        menu_overlays_parent: true,
        close_condition: CloseCondition {
            leave: false,
            click_outside: true,
            click_inside: true,
        },
        item_width: ItemWidth::Uniform(240),
        item_height: ItemHeight::Dynamic(40),
        bar_bounds: bounds,
        main_offset: -(bounds.height as i32),
        cross_offset: 0,
        root_bounds_list: vec![bounds],
        path_highlight: Some(PathHighlight::MenuActive),
        style: Cow::Owned(theme::menu_bar::MenuBarStyle::Default),
        position: Point::new(0., 0.),
        is_overlay: false,
        window_id: id,
        depth: 0,
        on_surface_action: None,
    };

    {
        let mut init_messages: Vec<TextCtxAction> = Vec::new();
        let mut init_shell = Shell::new(&mut init_messages);
        menu::init_root_menu(
            &mut popup_menu,
            renderer,
            &mut init_shell,
            cursor.position().unwrap_or_default(),
            viewport.size(),
            Vector::new(0., 0.),
            bounds,
            -(bounds.height),
        );
    }

    let anchor_rect = menu_bar_state.inner.with_data_mut(|state| {
        state.popup_id.insert(window_id, id);
        let pos = cursor.position().unwrap_or_default();
        iced::Rectangle {
            x: pos.x as i32,
            y: pos.y as i32,
            width: 1,
            height: 1,
        }
    });

    let menu_node = popup_menu.layout(
        renderer,
        iced_core::layout::Limits::NONE.min_width(1.).min_height(1.),
    );
    let popup_size = menu_node.size();

    let positioner = Positioner {
        size: Some((
            popup_size.width.ceil() as u32 + 2,
            popup_size.height.ceil() as u32 + 2,
        )),
        anchor_rect,
        anchor: crate::ui::surface::PopupAnchor::None,
        gravity: crate::ui::surface::PopupGravity::BottomRight,
        ..Default::default()
    };

    // Queue the request. The shell runner drains it and creates the popup, so
    // it flows through the normal Task + view pipeline (rendering and
    // teardown included).
    let settings = PopupSettings {
        parent: window_id,
        id,
        positioner,
    };

    PENDING_POPUP_REQUESTS.with(|q| {
        q.borrow_mut().push(PopupRequest {
            settings,
            menu: popup_menu,
            selected_text,
            pending_action: pending_action.clone(),
        });
    });
    wake_runtime();
}

/// Dismisses this widget's open context-menu popup on an outside click,
/// touch, or Escape.
pub(crate) fn dismiss_popup_on_event(
    menu_bar_state: &MenuBarState,
    event: &event::Event,
    window_id: window::Id,
) {
    let is_dismiss = matches!(
        event,
        event::Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left | mouse::Button::Middle
        )) | event::Event::Keyboard(iced_core::keyboard::Event::KeyPressed {
            key: iced_core::keyboard::Key::Named(iced_core::keyboard::key::Named::Escape),
            ..
        }) | event::Event::Touch(iced_core::touch::Event::FingerPressed { .. })
    );
    if !is_dismiss {
        return;
    }

    let popup_id = menu_bar_state
        .inner
        .with_data(|state| state.popup_id.get(&window_id).copied());
    if let Some(popup_id) = popup_id {
        menu_bar_state.inner.with_data_mut(|state| {
            state.popup_id.retain(|_, v| *v != popup_id);
            state.reset();
        });
        queue_destroy_popup(popup_id);
    }
}

/// Widget that wraps [`Menu`] inside a Wayland popup and intercepts
/// [`TextCtxAction`] messages for clipboard operations.
#[derive(Clone)]
struct TextContextMenuPopup<Message: Clone + 'static> {
    menu: Menu<'static, TextCtxAction>,
    selected_text: Option<String>,
    pending_action: PendingAction,
    _phantom: std::marker::PhantomData<Message>,
}

impl<Message: Clone + 'static> iced_core::widget::Widget<Message, crate::ui::Theme, iced::Renderer>
    for TextContextMenuPopup<Message>
{
    fn size(&self) -> Size<iced_core::Length> {
        use iced_core::widget::Widget;
        Widget::<TextCtxAction, crate::ui::Theme, iced::Renderer>::size(&self.menu)
    }

    fn tag(&self) -> iced_core::widget::tree::Tag {
        use iced_core::widget::Widget;
        Widget::<TextCtxAction, crate::ui::Theme, iced::Renderer>::tag(&self.menu)
    }

    fn state(&self) -> iced_core::widget::tree::State {
        use iced_core::widget::Widget;
        Widget::<TextCtxAction, crate::ui::Theme, iced::Renderer>::state(&self.menu)
    }

    fn children(&self) -> Vec<iced_core::widget::Tree> {
        use iced_core::widget::Widget;
        Widget::<TextCtxAction, crate::ui::Theme, iced::Renderer>::children(&self.menu)
    }

    fn diff(&self, tree: &mut iced_core::widget::Tree) {
        use iced_core::widget::Widget;
        Widget::<TextCtxAction, crate::ui::Theme, iced::Renderer>::diff(&self.menu, tree);
    }

    fn layout(
        &mut self,
        tree: &mut iced_core::widget::Tree,
        renderer: &iced::Renderer,
        limits: &iced_core::layout::Limits,
    ) -> iced_core::layout::Node {
        use iced_core::widget::Widget;
        Widget::<TextCtxAction, crate::ui::Theme, iced::Renderer>::layout(
            &mut self.menu,
            tree,
            renderer,
            limits,
        )
    }

    fn draw(
        &self,
        tree: &iced_core::widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced_core::widget::Widget;
        Widget::<TextCtxAction, crate::ui::Theme, iced::Renderer>::draw(
            &self.menu, tree, renderer, theme, style, layout, cursor, viewport,
        );
    }

    fn update(
        &mut self,
        tree: &mut iced_core::widget::Tree,
        event: &event::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // The compositor dismissed this popup. iced has no
        // `PlatformSpecific::Wayland` event carrying the dismissed popup's id,
        // so the shell records it and this claims it; see
        // `ui::surface::dismissal`.
        //
        // exwlshell reports only the destruction, so the surface is already
        // gone and no `queue_destroy_popup` is needed here: it would be a
        // destroy for a dead id.
        {
            let popup_id = self.menu.window_id;
            if crate::ui::surface::dismissal::claim(popup_id) {
                self.menu.tree.inner.with_data_mut(|state| {
                    state.popup_id.retain(|_, v| *v != popup_id);
                    state.reset();
                });
                return;
            }
        }

        // Escape dismisses the popup. Under the grab, keyboard input is
        // delivered to the popup surface, so handle it here.
        if matches!(
            event,
            event::Event::Keyboard(iced_core::keyboard::Event::KeyPressed {
                key: iced_core::keyboard::Key::Named(iced_core::keyboard::key::Named::Escape),
                ..
            })
        ) {
            let popup_id = self.menu.window_id;
            self.menu.tree.inner.with_data_mut(|state| {
                state.popup_id.retain(|_, v| *v != popup_id);
                state.reset();
            });
            queue_destroy_popup(popup_id);
            shell.capture_event();
            return;
        }

        let mut local_messages: Vec<TextCtxAction> = Vec::new();
        let mut local_shell = Shell::new(&mut local_messages);

        {
            use iced_core::widget::Widget;
            Widget::<TextCtxAction, crate::ui::Theme, iced::Renderer>::update(
                &mut self.menu,
                tree,
                event,
                layout,
                cursor,
                renderer,
                clipboard,
                &mut local_shell,
                viewport,
            );
        }

        if local_shell.is_event_captured() {
            shell.capture_event();
        }
        shell.request_redraw_at(local_shell.redraw_request());
        if local_shell.is_layout_invalid() {
            shell.invalidate_layout();
        }

        for action in local_messages {
            match action {
                TextCtxAction::Copy => {
                    if let Some(ref text) = self.selected_text {
                        clipboard.write(clipboard::Kind::Standard, text.clone());
                    }
                }
                TextCtxAction::Cut => {
                    if let Some(ref text) = self.selected_text {
                        clipboard.write(clipboard::Kind::Standard, text.clone());
                    }
                    if let Ok(mut guard) = self.pending_action.lock() {
                        *guard = Some(TextCtxAction::Cut);
                    }
                }
                TextCtxAction::Paste => {
                    if let Ok(mut guard) = self.pending_action.lock() {
                        *guard = Some(TextCtxAction::Paste);
                    }
                }
                TextCtxAction::SelectAll => {
                    if let Ok(mut guard) = self.pending_action.lock() {
                        *guard = Some(TextCtxAction::SelectAll);
                    }
                }
            }
        }

        // Under the popup grab the parent widget receives no events, so the
        // popup must tear itself down once its menu has closed, whether an
        // item was chosen (`click_inside`) or the user clicked away
        // (`click_outside`).
        {
            let menu_closed = self.menu.tree.inner.with_data(|state| !state.open);
            if menu_closed {
                let popup_id = self.menu.window_id;
                self.menu.tree.inner.with_data_mut(|state| {
                    state.popup_id.retain(|_, v| *v != popup_id);
                    state.reset();
                });
                queue_destroy_popup(popup_id);
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &iced_core::widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        use iced_core::widget::Widget;
        Widget::<TextCtxAction, crate::ui::Theme, iced::Renderer>::mouse_interaction(
            &self.menu, tree, layout, cursor, viewport, renderer,
        )
    }
}

impl<Message: Clone + 'static> From<TextContextMenuPopup<Message>>
    for crate::ui::Element<'static, Message>
{
    fn from(popup: TextContextMenuPopup<Message>) -> Self {
        Self::new(popup)
    }
}
