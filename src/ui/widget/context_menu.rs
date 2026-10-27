// Copyright 2024 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/context_menu.rs
//!
//! A context menu is a menu in a graphical user interface that appears upon user interaction, such as a right-click mouse operation.

use crate::ui::shell::runner::{WindowingSystem, windowing_system};
use crate::ui::widget::menu::{
    self, CloseCondition, Direction, ItemHeight, ItemWidth, MenuBarState, PathHighlight,
    init_root_menu, menu_roots_diff,
};
use derive_setters::Setters;
use iced::touch::Finger;
use iced::{Event, Vector, keyboard, window};
use iced_core::widget::{Tree, Widget, tree};
use iced_core::{Length, Point, Size, mouse, touch};
use std::collections::HashSet;
use std::sync::Arc;

/// A context menu is a menu in a graphical user interface that appears upon user interaction, such as a right-click mouse operation.
pub fn context_menu<'a, Message: 'static + Clone>(
    content: impl Into<crate::ui::Element<'a, Message>>,
    // on_context: Message,
    context_menu: Option<Vec<menu::Tree<Message>>>,
) -> ContextMenu<'a, Message> {
    let mut this = ContextMenu {
        content: content.into(),
        context_menu: context_menu.map(|menus| {
            vec![menu::Tree::with_children(
                crate::ui::Element::from(crate::ui::widget::Row::new()),
                menus,
            )]
        }),
        close_on_escape: true,
        window_id: crate::ui::window::reserved(),
        item_width: ItemWidth::Uniform(240),
        on_open: None,
        on_close: None,
        on_surface_action: None,
    };

    if let Some(ref mut context_menu) = this.context_menu {
        context_menu.iter_mut().for_each(menu::Tree::set_index);
    }

    this
}

/// A context menu is a menu in a graphical user interface that appears upon user interaction, such as a right-click mouse operation.
#[derive(Setters)]
#[must_use]
pub struct ContextMenu<'a, Message> {
    #[setters(skip)]
    content: crate::ui::Element<'a, Message>,
    #[setters(skip)]
    context_menu: Option<Vec<menu::Tree<Message>>>,
    pub window_id: window::Id,
    pub close_on_escape: bool,
    /// Width of each menu item, and therefore of the menu.
    pub item_width: ItemWidth,
    /// Emitted when the menu opens, so the application can mark what was right-clicked.
    #[setters(strip_option)]
    pub on_open: Option<Message>,
    /// Emitted when the menu closes by any path, including the compositor dismissing it.
    #[setters(strip_option)]
    pub on_close: Option<Message>,
    #[setters(skip)]
    pub(crate) on_surface_action:
        Option<Arc<dyn Fn(crate::ui::surface::Action<Message>) -> Message + Send + Sync + 'static>>,
}

impl<Message: Clone + 'static> ContextMenu<'_, Message> {
    /// Publish `on_open`/`on_close` when the open state changed since the last report.
    fn report_open_state(&self, state: &mut LocalState, shell: &mut iced_core::Shell<'_, Message>) {
        let open = state.menu_bar_state.inner.with_data(|d| d.open);
        if open == state.reported_open {
            return;
        }
        state.reported_open = open;
        let message = if open { &self.on_open } else { &self.on_close };
        if let Some(message) = message.clone() {
            shell.publish(message);
        }
    }

    /// Presents the menu at `cursor`.
    fn open_menu(
        &mut self,
        tree: &mut Tree,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        renderer: &crate::ui::Renderer,
        shell: &mut iced_core::Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        let state = tree.state.downcast_mut::<LocalState>();
        state.context_cursor = cursor.position().unwrap_or_default();
        let right_held = state.right_held;
        state.menu_bar_state.inner.with_data_mut(|state| {
            state.open = true;
            state.view_cursor = cursor;
            state.opening_press_held = right_held;
        });
        if matches!(windowing_system(), Some(WindowingSystem::Wayland)) {
            self.create_popup(layout, cursor, renderer, shell, viewport, state);
        }
        shell.request_redraw();
    }

    #[allow(clippy::too_many_lines)]
    fn create_popup(
        &mut self,
        layout: iced_core::Layout<'_>,
        view_cursor: iced_core::mouse::Cursor,
        renderer: &crate::ui::Renderer,
        shell: &mut iced_core::Shell<'_, Message>,
        viewport: &iced::Rectangle,
        my_state: &mut LocalState,
    ) {
        if self.window_id != crate::ui::window::none()
            && let Some(surface_action) = self.on_surface_action.clone()
        {
            use crate::ui::surface::action::destroy_popup;
            use crate::ui::surface::{PopupSettings, Positioner};

            let mut bounds = layout.bounds();
            bounds.x = my_state.context_cursor.x;
            bounds.y = my_state.context_cursor.y;

            let (id, root_list) = my_state.menu_bar_state.inner.with_data_mut(|state| {
                // A popup still collapsing has to go now rather than finish:
                // the menu about to be laid out needs this tree diffed, and
                // that one is still rendering against it.
                if let Some(id) = state.leaving.remove(&self.window_id) {
                    shell.publish(surface_action(destroy_popup(id)));
                }

                if let Some(id) = state.popup_id.get(&self.window_id).copied() {
                    // close existing popups
                    state.menu_states.clear();
                    state.active_root.clear();
                    // Nothing waited for in the last menu is for this one
                    state.pending = None;
                    state.last_cursor = None;

                    shell.publish(surface_action(destroy_popup(id)));
                    state.view_cursor = view_cursor;
                }
                // A fresh id per popup, so the old popup's Done cannot be mistaken for the new one's
                (
                    window::Id::unique(),
                    layout.children().map(|lo| lo.bounds()).collect::<Vec<_>>(),
                )
            });
            let Some(context_menu) = self.context_menu.as_mut() else {
                return;
            };

            let mut popup_menu = popup_menu(
                context_menu.clone(),
                my_state.menu_bar_state.clone(),
                self.item_width,
                bounds,
                id,
                self.on_surface_action.clone(),
            );

            init_root_menu(
                &mut popup_menu,
                renderer,
                shell,
                view_cursor.position().unwrap(),
                viewport.size(),
                Vector::new(0., 0.),
                layout.bounds(),
                -bounds.height,
            );
            let (anchor_rect, gravity) = my_state.menu_bar_state.inner.with_data_mut(|state| {
                use iced::Rectangle;

                state.popup_id.insert(self.window_id, id);
                (
                    {
                        let pos = view_cursor.position().unwrap_or_default();
                        Rectangle {
                            x: pos.x as i32,
                            y: pos.y as i32,
                            width: 1,
                            height: 1,
                        }
                    },
                    match (state.horizontal_direction, state.vertical_direction) {
                        (Direction::Positive, Direction::Positive) => {
                            crate::ui::surface::PopupGravity::BottomRight
                        }
                        (Direction::Positive, Direction::Negative) => {
                            crate::ui::surface::PopupGravity::TopRight
                        }
                        (Direction::Negative, Direction::Positive) => {
                            crate::ui::surface::PopupGravity::BottomLeft
                        }
                        (Direction::Negative, Direction::Negative) => {
                            crate::ui::surface::PopupGravity::TopLeft
                        }
                    },
                )
            });

            let menu_node = popup_menu.layout(
                renderer,
                iced_core::layout::Limits::NONE.min_width(1.).min_height(1.),
            );
            let popup_size = menu_node.size();
            // Reserve room above the rows once. Unfolding never moves or
            // resizes the surface; start the folded rows at the click.
            let room = my_state
                .menu_bar_state
                .inner
                .with_data(|state| state.menu_states.first().map_or(0.0, |ms| ms.room));
            let mut anchor_rect = anchor_rect;
            anchor_rect.y -= room.round() as i32;
            let positioner = Positioner {
                size: Some((
                    popup_size.width.ceil() as u32 + 2,
                    popup_size.height.ceil() as u32 + 2,
                )),
                anchor_rect,
                anchor: crate::ui::surface::PopupAnchor::None,
                // The direction the menu laid itself out in, not a fixed
                // guess: `init_root_menu` above decides which way there is
                // room to open, and the surface has to be placed the same
                // way. Requesting `BottomRight` regardless left the
                // compositor's constraint adjustment to correct it.
                gravity,
                // Fit the fixed surface once; unfolding only changes content.
                constraint_adjustment: crate::ui::surface::PopupConstraintAdjustment::SlideX
                    | crate::ui::surface::PopupConstraintAdjustment::SlideY
                    | crate::ui::surface::PopupConstraintAdjustment::ResizeY,
            };
            // The menu begins below the room it keeps for its groups, and
            // collapses from there: read live, as a group unfolds into the
            // room. A dismissed menu has dropped its state, and collapses
            // from where it last was.
            let menu = my_state.menu_bar_state.clone();
            let last = std::cell::Cell::new(room);
            crate::ui::surface::content_top::watch(id, move || {
                if let Some(top) = menu
                    .inner
                    .with_data(|state| state.menu_states.first().map(|ms| ms.top()))
                {
                    last.set(top);
                }
                last.get()
            });
            let parent = self.window_id;
            shell.publish(surface_action(crate::ui::surface::action::simple_popup(
                move || PopupSettings {
                    parent,
                    id,
                    positioner,
                    // Only a dismissed menu animates out, never a replaced
                    // one: see `destroy_popup_animated` and the `leaving`
                    // map. Two popups cannot share the one menu tree.
                    animate: true,
                },
                Some(move || popup_content(popup_menu.clone()).map(crate::ui::action::app)),
            )));
        }
    }

    pub fn on_surface_action(
        mut self,
        handler: impl Fn(crate::ui::surface::Action<Message>) -> Message + Send + Sync + 'static,
    ) -> Self {
        self.on_surface_action = Some(Arc::new(handler));
        self
    }
}

impl<Message: 'static + Clone> Widget<Message, crate::ui::Theme, crate::ui::Renderer>
    for ContextMenu<'_, Message>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<LocalState>()
    }

    fn state(&self) -> tree::State {
        #[allow(clippy::default_trait_access)]
        tree::State::new(LocalState {
            context_cursor: Point::default(),
            pending_open: None,
            right_held: false,
            fingers_pressed: Default::default(),
            menu_bar_state: Default::default(),
            reported_open: false,
        })
    }

    fn children(&self) -> Vec<Tree> {
        let mut children = Vec::with_capacity(if self.context_menu.is_some() { 2 } else { 1 });

        children.push(Tree::new(self.content.as_widget()));

        // Assign the context menu's elements as this widget's children.
        if let Some(ref context_menu) = self.context_menu {
            let mut tree = Tree::empty();
            tree.children = context_menu
                .iter()
                .map(|root| {
                    let mut tree = Tree::empty();
                    let flat = root
                        .flatten()
                        .iter()
                        .map(|mt| Tree::new(mt.item.clone()))
                        .collect();
                    tree.children = flat;
                    tree
                })
                .collect();

            children.push(tree);
        }

        children
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
        let state = tree.state.downcast_mut::<LocalState>();
        // This is the rebuild a pending open was waiting for: `self` now
        // carries the menu built from what the press changed.
        if let Some(pending) = state.pending_open.as_mut() {
            pending.awaiting_rebuild = false;
        }
        if let Some(context_menu) = self.context_menu.as_ref() {
            state.menu_bar_state.inner.with_data_mut(|inner| {
                // While a popup is up for this window, leave `inner.tree` alone.
                //
                // `create_popup` hands the popup surface a *snapshot* of the roots
                // (`menu_roots: Cow::Owned(context_menu.clone())`) but the *shared*
                // `MenuBarState` tree. The popup window rebuilds its own
                // `UserInterface` from that snapshot, while this window's view is
                // rebuilt from the live roots on every frame, and the live roots
                // can change shape (the file-list menu differs by location
                // and selection, the nav menu by entity). Re-diffing the shared tree
                // here would reshape it to the live roots underneath a popup that is
                // still laying out the snapshot.
                //
                // `Menu::layout` addresses menu items *positionally*
                // (`menu_inner.rs`, `tree[mt.index]`), so a shape mismatch is not a
                // misrender: index 6 of the new tree can be a stateless divider where
                // the snapshot has a `Button`, and `button/widget.rs:335` then indexes
                // `tree.children[0]` of a childless tree and panics.
                //
                // A context menu is a snapshot by nature, so freezing its tree for the
                // lifetime of the popup is also the right semantics: the popup renders
                // what was on screen when it opened. The next diff after the popup
                // closes reconciles the tree with whatever the roots are by then.
                //
                // `leaving` as well as `popup_id`: a popup playing its exit
                // is still on screen and still rendering against this tree,
                // even though it is no longer the current one.
                if inner.popup_id.contains_key(&self.window_id)
                    || inner.leaving.contains_key(&self.window_id)
                {
                    return;
                }
                menu_roots_diff(context_menu, &mut inner.tree);
            });
        }

        // if let Some(ref mut context_menus) = self.context_menu {
        //     for (menu, tree) in context_menus
        //         .iter_mut()
        //         .zip(tree.children[1].children.iter_mut())
        //     {
        //         menu.item.as_widget_mut().diff(tree);
        //     }
        // }
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &crate::ui::Renderer,
        limits: &iced_core::layout::Limits,
    ) -> iced_core::layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &iced_core::renderer::Style,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        viewport: &iced::Rectangle,
        renderer: &crate::ui::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: iced_core::Layout<'_>,
        renderer: &crate::ui::Renderer,
        operation: &mut dyn iced_core::widget::Operation<()>,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    #[allow(clippy::too_many_lines)]
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        renderer: &crate::ui::Renderer,
        clipboard: &mut dyn iced_core::Clipboard,
        shell: &mut iced_core::Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        let state = tree.state.downcast_mut::<LocalState>();
        let bounds = layout.bounds();

        // The compositor dismissed our popup: nothing else tells this state
        // about it. iced has no `PlatformSpecific::Wayland` event
        // carrying the dismissed popup's id, so the shell records it and we
        // claim it here; see `ui::surface::dismissal`.
        state.menu_bar_state.inner.with_data_mut(|d| {
            if d.popup_id
                .get(&self.window_id)
                .copied()
                .is_some_and(crate::ui::surface::dismissal::claim)
            {
                d.popup_id.remove(&self.window_id);
                d.reset();
            }

            // A popup that was animating out is gone once its surface is,
            // and only then may the tree thaw.
            if d.leaving
                .get(&self.window_id)
                .copied()
                .is_some_and(crate::ui::surface::dismissal::claim)
            {
                d.leaving.remove(&self.window_id);
            }
        });

        // XXX this should reset the state if there are no other copies of the state, which implies no dropdown menus open.
        let reset = self.window_id != crate::ui::window::none()
            && state
                .menu_bar_state
                .inner
                .with_data(|d| !d.open && !d.active_root.is_empty());

        let open = state.menu_bar_state.inner.with_data_mut(|state| {
            if reset
                && let Some(popup_id) = state.popup_id.get(&self.window_id).copied()
                && let Some(handler) = self.on_surface_action.as_ref()
            {
                shell.publish((handler)(crate::ui::surface::Action::DestroyPopup {
                    id: popup_id,
                    animate: false,
                }));
                state.reset();
            }
            state.open
        });
        let mut was_open = false;
        // Any key that is not a bare modifier closes the menu, except those the
        // menu moves through its rows with: a shortcut pressed while it is open
        // acts on the window behind it, so the menu must not stay up over the
        // result
        let key_closes = matches!(
            event,
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. })
                if !is_modifier_key(key) && !is_menu_key(key)
        );
        if open
            && (key_closes
                || matches!(
                    event,
                    Event::Mouse(mouse::Event::ButtonPressed(
                        mouse::Button::Right | mouse::Button::Left,
                    )) | Event::Touch(touch::Event::FingerPressed { .. })
                ))
        {
            state.menu_bar_state.inner.with_data_mut(|state| {
                was_open = true;
                state.menu_states.clear();
                state.active_root.clear();
                // Nothing waited for in the last menu is for this one
                state.pending = None;
                state.last_cursor = None;
                state.open = false;

                if matches!(windowing_system(), Some(WindowingSystem::Wayland))
                    && let Some(id) = state.popup_id.remove(&self.window_id)
                {
                    {
                        let surface_action = self.on_surface_action.as_ref().unwrap();

                        // A right press is about to open a replacement, and
                        // two popups cannot share the one menu tree. Everything else is a plain dismissal and may
                        // collapse on its way out, which means its surface
                        // outlives this request — so the tree has to stay
                        // frozen until it is really gone.
                        let replacing = matches!(
                            event,
                            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
                        );

                        if replacing {
                            shell.publish(surface_action(
                                crate::ui::surface::action::destroy_popup(id),
                            ));
                        } else {
                            state.leaving.insert(self.window_id, id);
                            shell.publish(surface_action(
                                crate::ui::surface::action::destroy_popup_animated(id),
                            ));
                        }
                    }
                    state.view_cursor = cursor;
                }
            });
        }

        // Counted before this event is applied, so a lift still counts its own
        // finger. Tracked for every event, not only those over the widget: a
        // finger pressed inside and lifted outside would otherwise stay in the
        // set for good, and every later single tap would count as two fingers.
        let fingers_pressed = state.fingers_pressed.len();
        track_fingers(&mut state.fingers_pressed, event);
        let ends_opening_tap = state.menu_bar_state.inner.with_data_mut(|d| match event {
            Event::Touch(
                touch::Event::FingerLifted { id, .. } | touch::Event::FingerLost { id, .. },
            ) => d.opening_fingers.remove(id),
            // A new touch is a new gesture, whatever is still down.
            Event::Touch(touch::Event::FingerPressed { .. }) => {
                d.opening_fingers.clear();
                false
            }
            _ => false,
        });

        // A right press opens the menu, but not within this event: see
        // `PendingOpen`. Nothing is captured, so the content still sees the
        // press, and that is what selects the item under the pointer.
        if right_button_pressed(event) {
            state.right_held = true;
        } else if right_button_released(event) {
            state.right_held = false;
            // Reaching this window, the release did not reach the menu.
            state
                .menu_bar_state
                .inner
                .with_data_mut(|d| d.opening_press_held = false);
        }

        if self.context_menu.is_some() && cursor.is_over(bounds) && right_button_pressed(event) {
            state.pending_open = Some(PendingOpen {
                position: cursor.position().unwrap_or_default(),
                awaiting_rebuild: true,
            });
            shell.request_redraw();
        } else if key_closes
            || matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                    | Event::Touch(touch::Event::FingerPressed { .. })
            )
        {
            // Whatever would close the menu also stops one about to open.
            state.pending_open = None;
        }

        if !was_open && cursor.is_over(bounds) {
            // A two-finger tap presents the menu when it lifts.
            if self.context_menu.is_some() && touch_lifted(event) && fingers_pressed == 2 {
                let opening_fingers = state.fingers_pressed.clone();
                self.open_menu(tree, layout, cursor, renderer, shell, viewport);
                tree.state
                    .downcast_mut::<LocalState>()
                    .menu_bar_state
                    .inner
                    .with_data_mut(|d| d.opening_fingers = opening_fingers);
                shell.capture_event();
                self.report_open_state(tree.state.downcast_mut::<LocalState>(), shell);
                return;
            } else if (touch_lifted(event) && !ends_opening_tap) || left_button_released(event) {
                state.menu_bar_state.inner.with_data_mut(|state| {
                    was_open = true;
                    state.menu_states.clear();
                    state.active_root.clear();
                    // Nothing waited for in the last menu is for this one
                    state.pending = None;
                    state.last_cursor = None;
                    state.open = false;

                    if matches!(windowing_system(), Some(WindowingSystem::Wayland))
                        && let Some(id) = state.popup_id.remove(&self.window_id)
                    {
                        {
                            let surface_action = self.on_surface_action.as_ref().unwrap();
                            state.leaving.insert(self.window_id, id);
                            shell.publish(surface_action(
                                crate::ui::surface::action::destroy_popup_animated(id),
                            ));
                        }
                        state.view_cursor = cursor;
                    }
                });
            }
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        let state = tree.state.downcast_mut::<LocalState>();
        if let Some(pending) = state.pending_open.as_mut() {
            // Messages published so far will rebuild the view, and the menu
            // may be built from what they change. With none, `self` already
            // carries the menu to show.
            if right_button_pressed(event) {
                pending.awaiting_rebuild = !shell.is_empty();
            }
            if !pending.awaiting_rebuild {
                let position = pending.position;
                state.pending_open = None;
                self.open_menu(
                    tree,
                    layout,
                    mouse::Cursor::Available(position),
                    renderer,
                    shell,
                    viewport,
                );
            }
        }
        self.report_open_state(tree.state.downcast_mut::<LocalState>(), shell);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: iced_core::Layout<'b>,
        renderer: &crate::ui::Renderer,
        viewport: &iced::Rectangle,
        translation: Vector,
    ) -> Option<iced_core::overlay::Element<'b, Message, crate::ui::Theme, crate::ui::Renderer>>
    {
        // The wrapped content's overlays (tooltips, dropdowns, ...) always pass through
        let content = self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        );

        if matches!(windowing_system(), Some(WindowingSystem::Wayland))
            && self.window_id != crate::ui::window::none()
            && self.on_surface_action.is_some()
        {
            return content;
        }

        let state = tree.state.downcast_ref::<LocalState>();
        let Some(context_menu) = self.context_menu.as_mut() else {
            return content;
        };
        if !state.menu_bar_state.inner.with_data(|state| state.open) {
            return content;
        }

        // Anchor the menu to a 1x1 rectangle at the click, like the popup path does
        let bounds = iced::Rectangle::new(state.context_cursor, Size::new(1.0, 1.0));
        let menu = crate::ui::widget::menu::Menu {
            tree: state.menu_bar_state.clone(),
            menu_roots: std::borrow::Cow::Owned(context_menu.clone()),
            bounds_expand: 16,
            menu_overlays_parent: true,
            close_condition: CloseCondition {
                leave: false,
                click_outside: true,
                click_inside: true,
            },
            item_width: self.item_width,
            item_height: ItemHeight::Dynamic(40),
            bar_bounds: bounds,
            main_offset: 0,
            cross_offset: 0,
            root_bounds_list: vec![bounds],
            path_highlight: Some(PathHighlight::MenuActive),
            style: std::borrow::Cow::Borrowed(&crate::ui::theme::menu_bar::MenuBarStyle::Default),
            position: Point::new(translation.x, translation.y),
            is_overlay: true,
            window_id: crate::ui::window::none(),
            depth: 0,
            on_surface_action: None,
        }
        .overlay();

        Some(match content {
            Some(content) => {
                iced_core::overlay::Group::with_children(vec![content, menu]).overlay()
            }
            None => menu,
        })
    }
}

impl<'a, Message: Clone + 'static> From<ContextMenu<'a, Message>>
    for crate::ui::Element<'a, Message>
{
    fn from(widget: ContextMenu<'a, Message>) -> Self {
        Self::new(widget)
    }
}

/// The keys a menu moves through its rows with.
fn is_menu_key(key: &keyboard::Key) -> bool {
    use keyboard::key::Named;
    matches!(
        key,
        keyboard::Key::Named(
            Named::ArrowUp | Named::ArrowDown | Named::ArrowLeft | Named::ArrowRight | Named::Enter
        )
    )
}

fn is_modifier_key(key: &keyboard::Key) -> bool {
    use keyboard::key::Named;
    matches!(
        key,
        keyboard::Key::Named(
            Named::Alt
                | Named::AltGraph
                | Named::CapsLock
                | Named::Control
                | Named::Fn
                | Named::FnLock
                | Named::Hyper
                | Named::Meta
                | Named::NumLock
                | Named::ScrollLock
                | Named::Shift
                | Named::Super
                | Named::Symbol
                | Named::SymbolLock
        )
    )
}

fn right_button_pressed(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
    )
}

fn right_button_released(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right))
    )
}

fn left_button_released(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left,))
    )
}

fn touch_lifted(event: &Event) -> bool {
    matches!(event, Event::Touch(touch::Event::FingerLifted { .. }))
}

/// Keeps `fingers` at the set of fingers currently down. A lost finger is
/// forgotten like a lifted one, or a cancelled touch would be counted forever.
pub(crate) fn track_fingers(fingers: &mut HashSet<Finger>, event: &Event) {
    match event {
        Event::Touch(touch::Event::FingerPressed { id, .. }) => {
            fingers.insert(*id);
        }
        Event::Touch(
            touch::Event::FingerLifted { id, .. } | touch::Event::FingerLost { id, .. },
        ) => {
            fingers.remove(id);
        }
        _ => (),
    }
}

pub struct LocalState {
    context_cursor: Point,
    pending_open: Option<PendingOpen>,
    /// Whether the right button is down, so an open can tell the menu that
    /// the release still to come ends the opening click.
    right_held: bool,
    fingers_pressed: HashSet<Finger>,
    menu_bar_state: MenuBarState,
    reported_open: bool,
}

/// What a context menu's popup shows: the menu at the top of the surface,
/// inside its one-pixel border. Never centred: while a group opens or
/// closes the surface and the rows differ in height, and centring would
/// move the whole menu.
fn popup_content<Message: Clone + 'static>(
    menu: crate::ui::widget::menu::Menu<'static, Message>,
) -> crate::ui::Element<'static, Message> {
    crate::ui::widget::container(menu)
        .center_x(Length::Fill)
        .align_top(Length::Fill)
        .padding(iced::Padding {
            top: 1.0,
            ..iced::Padding::ZERO
        })
        .into()
}

/// The menu a context menu's popup surface shows, placed at `bounds`.
fn popup_menu<Message: Clone + 'static>(
    roots: Vec<menu::Tree<Message>>,
    tree: MenuBarState,
    item_width: ItemWidth,
    bounds: iced::Rectangle,
    window_id: window::Id,
    on_surface_action: Option<
        Arc<dyn Fn(crate::ui::surface::Action<Message>) -> Message + Send + Sync + 'static>,
    >,
) -> crate::ui::widget::menu::Menu<'static, Message> {
    crate::ui::widget::menu::Menu {
        tree,
        menu_roots: std::borrow::Cow::Owned(roots),
        bounds_expand: 16,
        menu_overlays_parent: true,
        close_condition: CloseCondition {
            leave: false,
            click_outside: true,
            click_inside: true,
        },
        item_width,
        item_height: ItemHeight::Dynamic(40),
        bar_bounds: bounds,
        main_offset: -(bounds.height as i32),
        cross_offset: 0,
        root_bounds_list: vec![bounds],
        path_highlight: Some(PathHighlight::MenuActive),
        style: std::borrow::Cow::Owned(crate::ui::theme::menu_bar::MenuBarStyle::Default),
        position: Point::new(0., 0.),
        is_overlay: false,
        window_id,
        depth: 0,
        on_surface_action,
    }
}

/// A menu asked for by a right press and not yet presented.
///
/// It opens on the press rather than the release, so it does not wait for the
/// button to come up. It cannot open within the press itself: the press is
/// also what selects the item under the pointer, and the menu is built from
/// the selection. The popup takes a snapshot of `self`'s menu, which is still
/// the one built before the press. So it opens on the first event after the
/// view has been rebuilt, normally the redraw that follows.
struct PendingOpen {
    /// Where the press was, which is where the menu opens.
    position: Point,
    /// Set while a rebuild is still to come; `diff` clears it.
    awaiting_rebuild: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::clipboard;
    use iced_runtime::user_interface::{Cache, UserInterface};
    use std::time::Instant;

    const WINDOW: Size = Size::new(200.0, 200.0);
    const INSIDE: Point = Point::new(50.0, 50.0);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Msg {
        /// What the content publishes on a right press, as a file item
        /// selects itself.
        Selected,
        Opened,
        Closed,
        /// Any other surface request.
        Surface,
    }

    /// A 100×100 area with a context menu. With `selects`, a right press on
    /// it publishes `Selected`, which the application would rebuild the menu
    /// from.
    fn view(selects: bool) -> crate::ui::Element<'static, Msg> {
        let area = crate::mouse_area::MouseArea::new(
            crate::ui::widget::space::horizontal()
                .width(Length::Fixed(100.0))
                .height(Length::Fixed(100.0)),
        );
        let area = if selects {
            area.on_right_press(|_| Msg::Selected)
        } else {
            area
        };
        context_menu(
            area,
            Some(vec![menu::Tree::new(crate::ui::Element::from(
                crate::ui::widget::text("item"),
            ))]),
        )
        .on_open(Msg::Opened)
        .on_close(Msg::Closed)
        .into()
    }

    struct Harness {
        selects: bool,
        renderer: crate::ui::Renderer,
        cache: Option<Cache>,
    }

    impl Harness {
        fn new(selects: bool) -> Self {
            Self {
                selects,
                renderer: iced_texture_cache::testing::headless_tiny_skia(),
                cache: Some(Cache::default()),
            }
        }

        /// Builds the view afresh, as the runtime does after messages, and
        /// hands it `events` as one batch.
        fn send(&mut self, events: &[Event]) -> Vec<Msg> {
            let mut messages = Vec::new();
            let mut ui = UserInterface::build(
                view(self.selects),
                WINDOW,
                self.cache.take().unwrap(),
                &mut self.renderer,
            );
            let _ = ui.update(
                events,
                mouse::Cursor::Available(INSIDE),
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            self.cache = Some(ui.into_cache());
            messages
        }
    }

    fn right_press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
    }

    fn right_release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right))
    }

    fn redraw() -> Event {
        Event::Window(window::Event::RedrawRequested(Instant::now()))
    }

    /// The press selects the item under the pointer, and the menu is built
    /// from the selection. Opening before the view is rebuilt would show the
    /// menu for whatever was selected before.
    #[test]
    fn a_right_press_opens_the_menu_once_the_view_is_rebuilt() {
        let mut ui = Harness::new(true);

        // A redraw in the same batch as the press still sees the old view.
        assert_eq!(ui.send(&[right_press(), redraw()]), [Msg::Selected]);
        // The next event after the rebuild opens it, button still down.
        assert_eq!(ui.send(&[redraw()]), [Msg::Opened]);
    }

    #[test]
    fn a_right_press_that_changes_nothing_opens_at_once() {
        let mut ui = Harness::new(false);

        assert_eq!(ui.send(&[right_press()]), [Msg::Opened]);
    }

    #[test]
    fn the_release_after_the_opening_press_leaves_the_menu_open() {
        let mut ui = Harness::new(true);
        ui.send(&[right_press()]);
        assert_eq!(ui.send(&[redraw()]), [Msg::Opened]);

        assert_eq!(ui.send(&[right_release()]), []);
        assert_eq!(ui.send(&[redraw()]), []);
    }

    #[test]
    fn a_left_press_before_the_menu_opens_cancels_it() {
        let mut ui = Harness::new(true);
        ui.send(&[right_press()]);

        ui.send(&[Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        ))]);
        assert_eq!(ui.send(&[redraw()]), []);
    }

    fn finger(id: u64, pressed: bool) -> Event {
        let (id, position) = (Finger(id), INSIDE);
        Event::Touch(if pressed {
            touch::Event::FingerPressed { id, position }
        } else {
            touch::Event::FingerLifted { id, position }
        })
    }

    /// The menu opens on the first finger up; the second finger's lift is
    /// the rest of the same tap and must not close it again.
    ///
    /// Headless, the menu is an overlay `Menu`, which sees the lift before
    /// this widget, so both read `opening_fingers`; on Wayland the lift goes
    /// to the surface the finger went down on, not the popup.
    #[test]
    fn a_two_finger_tap_leaves_the_menu_open() {
        let mut ui = Harness::new(false);
        assert_eq!(ui.send(&[finger(1, true)]), []);
        assert_eq!(ui.send(&[finger(2, true)]), []);

        assert_eq!(ui.send(&[finger(1, false)]), [Msg::Opened]);
        assert_eq!(ui.send(&[finger(2, false)]), []);
        assert_eq!(ui.send(&[redraw()]), []);
    }

    /// A finger pressed over the widget and lifted elsewhere, or lost, must
    /// not be counted for the rest of the widget's life; it made every later
    /// single tap look like a two-finger tap and open the context menu.
    #[test]
    fn a_finger_lifted_or_lost_anywhere_is_forgotten() {
        let mut fingers = HashSet::new();
        let at = Point::ORIGIN;
        let (a, b) = (Finger(1), Finger(2));

        track_fingers(
            &mut fingers,
            &Event::Touch(touch::Event::FingerPressed {
                id: a,
                position: at,
            }),
        );
        track_fingers(
            &mut fingers,
            &Event::Touch(touch::Event::FingerPressed {
                id: b,
                position: at,
            }),
        );
        assert_eq!(fingers.len(), 2);

        track_fingers(
            &mut fingers,
            &Event::Touch(touch::Event::FingerLifted {
                id: a,
                position: at,
            }),
        );
        track_fingers(
            &mut fingers,
            &Event::Touch(touch::Event::FingerLost {
                id: b,
                position: at,
            }),
        );
        assert!(fingers.is_empty());
    }

    /// A context menu's popup draws the items its layout placed.
    ///
    /// The popup surface is the menu plus a one-pixel border, and the menu
    /// is centred in it. Drawing used to work out the visible items again
    /// from where it found the menu, one pixel in, so a menu starting with
    /// a one-pixel divider (a file and a folder selected) lost that divider
    /// from the range: every item was drawn into the next item's place, the
    /// dividers as grey blocks a whole row tall.
    #[test]
    fn a_popup_menu_starting_with_a_divider_draws_each_item_in_its_row() {
        use crate::ui::widget::menu::Item;
        use iced_core::renderer::Headless;

        #[derive(Clone, Copy, PartialEq, Eq)]
        struct Act;
        impl crate::ui::widget::menu::action::MenuAction for Act {
            type Message = Msg;
            fn message(&self) -> Msg {
                Msg::Selected
            }
        }

        let button = || Item::Button(String::new(), None, Act);
        let mut roots = vec![menu::Tree::with_children(
            crate::ui::Element::from(crate::ui::widget::Row::new()),
            menu::items(
                &std::collections::HashMap::new(),
                vec![Item::Divider, button(), Item::Divider, button()],
            ),
        )];
        roots.iter_mut().for_each(menu::Tree::set_index);

        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let tree = MenuBarState::default();
        tree.inner.with_data_mut(|d| {
            d.open = true;
            menu_roots_diff(&roots, &mut d.tree);
        });
        let bounds = iced::Rectangle::new(INSIDE, Size::new(1.0, 1.0));
        let mut menu = popup_menu(
            roots,
            tree,
            ItemWidth::Uniform(200),
            bounds,
            window::Id::unique(),
            None,
        );
        let mut messages = Vec::new();
        init_root_menu(
            &mut menu,
            &renderer,
            &mut iced_core::Shell::new(&mut messages),
            INSIDE,
            WINDOW,
            Vector::new(0., 0.),
            bounds,
            -bounds.height,
        );
        let size = menu
            .layout(
                &renderer,
                iced_core::layout::Limits::NONE.min_width(1.).min_height(1.),
            )
            .size();
        // Sized as `create_popup` sizes the surface.
        let (width, height) = (size.width.ceil() as u32 + 2, size.height.ceil() as u32 + 2);

        let mut ui = UserInterface::build(
            crate::ui::widget::container(menu).center(Length::Fill),
            Size::new(width as f32, height as f32),
            Cache::default(),
            &mut renderer,
        );
        ui.draw(
            &mut renderer,
            &crate::ui::Theme::default(),
            &iced_core::renderer::Style {
                text_color: iced::Color::BLACK,
            },
            mouse::Cursor::Unavailable,
        );
        let pixels = renderer.screenshot(
            iced::Size::new(width, height),
            1.0,
            iced::Color::TRANSPARENT,
        );
        let at = |y: u32| {
            let i = ((y * width + width / 2) * 4) as usize;
            pixels[i..i + 4].to_vec()
        };

        // Rows: divider, button, divider, button. Both buttons are empty, so
        // the middle of each is plain menu background.
        let first = 1 + size.height as u32 / 4;
        let second = 1 + size.height as u32 * 3 / 4;
        assert_eq!(at(first), at(second));
    }

    /// A click on a row of a popup menu runs it: the press reaches the row's
    /// button. The menu first chooses the row under the pointer for the
    /// press, and must not mark the press as taken while doing so, or the
    /// button ignores it and the release does nothing.
    #[test]
    fn a_click_on_a_popup_menu_row_runs_it() {
        use crate::ui::widget::menu::Item;

        #[derive(Clone, Copy, PartialEq, Eq)]
        struct Act;
        impl crate::ui::widget::menu::action::MenuAction for Act {
            type Message = Msg;
            fn message(&self) -> Msg {
                Msg::Selected
            }
        }

        let mut roots = vec![menu::Tree::with_children(
            crate::ui::Element::from(crate::ui::widget::Row::new()),
            menu::items(
                &std::collections::HashMap::new(),
                vec![Item::Button("Open".to_owned(), None, Act)],
            ),
        )];
        roots.iter_mut().for_each(menu::Tree::set_index);

        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let tree = MenuBarState::default();
        tree.inner.with_data_mut(|d| {
            d.open = true;
            menu_roots_diff(&roots, &mut d.tree);
        });
        let bounds = iced::Rectangle::new(INSIDE, Size::new(1.0, 1.0));
        let mut menu = popup_menu(
            roots,
            tree,
            ItemWidth::Uniform(200),
            bounds,
            window::Id::unique(),
            None,
        );
        let mut messages = Vec::new();
        init_root_menu(
            &mut menu,
            &renderer,
            &mut iced_core::Shell::new(&mut messages),
            INSIDE,
            WINDOW,
            Vector::new(0., 0.),
            bounds,
            -bounds.height,
        );
        let size = menu
            .layout(
                &renderer,
                iced_core::layout::Limits::NONE.min_width(1.).min_height(1.),
            )
            .size();
        let surface = Size::new(size.width.ceil() + 2.0, size.height.ceil() + 2.0);
        let middle = Point::new(surface.width / 2.0, surface.height / 2.0);

        let mut ui = UserInterface::build(
            crate::ui::widget::container(menu).center(Length::Fill),
            surface,
            Cache::default(),
            &mut renderer,
        );
        let mut messages = Vec::new();
        let _ = ui.update(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position: middle }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(middle),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert_eq!(messages, [Msg::Selected]);
    }

    /// A popup menu holding one folder, File, with Open in it, laid out as
    /// `create_popup` lays it out: the menu, its state, and the middle of
    /// its surface, over the folder.
    fn folder_menu() -> (
        crate::ui::widget::menu::Menu<'static, Msg>,
        MenuBarState,
        Size,
        Point,
    ) {
        use crate::ui::widget::menu::Item;

        #[derive(Clone, Copy, PartialEq, Eq)]
        struct Act;
        impl crate::ui::widget::menu::action::MenuAction for Act {
            type Message = Msg;
            fn message(&self) -> Msg {
                Msg::Selected
            }
        }

        let mut roots = vec![menu::Tree::with_children(
            crate::ui::Element::from(crate::ui::widget::Row::new()),
            menu::items(
                &std::collections::HashMap::new(),
                vec![Item::Folder(
                    "File".to_owned(),
                    vec![Item::Button("Open".to_owned(), None, Act)],
                )],
            ),
        )];
        roots.iter_mut().for_each(menu::Tree::set_index);

        let renderer = iced_texture_cache::testing::headless_tiny_skia();
        let tree = MenuBarState::default();
        tree.inner.with_data_mut(|d| {
            d.open = true;
            menu_roots_diff(&roots, &mut d.tree);
        });
        let bounds = iced::Rectangle::new(INSIDE, Size::new(1.0, 1.0));
        let mut menu = popup_menu(
            roots,
            tree.clone(),
            ItemWidth::Uniform(200),
            bounds,
            window::Id::unique(),
            None,
        );
        init_root_menu(
            &mut menu,
            &renderer,
            &mut iced_core::Shell::new(&mut Vec::new()),
            INSIDE,
            WINDOW,
            Vector::new(0., 0.),
            bounds,
            -bounds.height,
        );
        let size = menu
            .layout(
                &renderer,
                iced_core::layout::Limits::NONE.min_width(1.).min_height(1.),
            )
            .size();
        let surface = Size::new(size.width.ceil() + 2.0, size.height.ceil() + 2.0);
        let middle = Point::new(surface.width / 2.0, surface.height / 2.0);
        (menu, tree, surface, middle)
    }

    /// Sends `events` to `menu`, on a surface of `size`, with the pointer at
    /// `at`.
    fn send_to_menu(
        menu: crate::ui::widget::menu::Menu<'static, Msg>,
        size: Size,
        at: Point,
        events: &[Event],
    ) -> Vec<Msg> {
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(
            crate::ui::widget::container(menu).center(Length::Fill),
            size,
            Cache::default(),
            &mut renderer,
        );
        let mut messages = Vec::new();
        let _ = ui.update(
            events,
            mouse::Cursor::Available(at),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        messages
    }

    /// A click on a folder leaves the menu open on what it holds: the
    /// release that ends it chooses nothing, so closes nothing.
    #[test]
    fn a_click_on_a_folder_leaves_the_menu_open() {
        for (press, release) in [
            (
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ),
            (
                Event::Touch(touch::Event::FingerPressed {
                    id: Finger(1),
                    position: Point::ORIGIN,
                }),
                Event::Touch(touch::Event::FingerLifted {
                    id: Finger(1),
                    position: Point::ORIGIN,
                }),
            ),
        ] {
            let (menu, tree, size, middle) = folder_menu();
            send_to_menu(
                menu,
                size,
                middle,
                &[
                    Event::Mouse(mouse::Event::CursorMoved { position: middle }),
                    press,
                    release,
                ],
            );
            assert!(
                tree.inner.with_data(|d| d.open),
                "the click closed the menu"
            );
        }
    }

    /// A touch the compositor takes back ends its press: the menu goes on
    /// following the pointer instead of waiting for a lift that never comes.
    #[test]
    fn a_lost_touch_ends_the_menus_press() {
        let (menu, tree, size, middle) = folder_menu();
        send_to_menu(
            menu,
            size,
            middle,
            &[
                Event::Touch(touch::Event::FingerPressed {
                    id: Finger(1),
                    position: middle,
                }),
                Event::Touch(touch::Event::FingerLost {
                    id: Finger(1),
                    position: middle,
                }),
            ],
        );
        assert!(
            !tree.inner.with_data(|d| d.pressed),
            "the press is still held"
        );
    }

    /// A popup menu of Open, Copy (with Copy path and Copy to folded under
    /// it) and Paste, laid out as `create_popup` lays it out: what the
    /// surface shows, its size, and the state to look at.
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Act;
    impl crate::ui::widget::menu::action::MenuAction for Act {
        type Message = Msg;
        fn message(&self) -> Msg {
            Msg::Selected
        }
    }

    type Items = Vec<menu::Item<Act, String>>;

    fn button(label: &str) -> menu::Item<Act, String> {
        menu::Item::Button(label.to_owned(), None, Act)
    }

    fn split(label: &str, variants: &[&str]) -> menu::Item<Act, String> {
        menu::Item::Split(
            menu::Entry::new(label.to_owned(), Act),
            variants.iter().map(|v| button(v)).collect(),
        )
    }

    fn copy_popup() -> (menu::Menu<'static, Msg>, MenuBarState, Size) {
        popup_of(vec![
            button("Open"),
            split("Copy", &["Copy path", "Copy to"]),
            button("Paste"),
        ])
    }

    fn popup_of(items: Items) -> (menu::Menu<'static, Msg>, MenuBarState, Size) {
        popup_with(items, None)
    }

    type SurfaceHandler =
        Option<Arc<dyn Fn(crate::ui::surface::Action<Msg>) -> Msg + Send + Sync + 'static>>;

    fn popup_with(
        items: Items,
        on_surface_action: SurfaceHandler,
    ) -> (menu::Menu<'static, Msg>, MenuBarState, Size) {
        let mut roots = vec![menu::Tree::with_children(
            crate::ui::Element::from(crate::ui::widget::Row::new()),
            menu::items(&std::collections::HashMap::new(), items),
        )];
        roots.iter_mut().for_each(menu::Tree::set_index);

        let renderer = iced_texture_cache::testing::headless_tiny_skia();
        let tree = MenuBarState::default();
        tree.inner.with_data_mut(|d| {
            d.open = true;
            menu_roots_diff(&roots, &mut d.tree);
        });
        let bounds = iced::Rectangle::new(INSIDE, Size::new(1.0, 1.0));
        let mut menu = popup_menu(
            roots,
            tree.clone(),
            ItemWidth::Static(360),
            bounds,
            window::Id::unique(),
            on_surface_action.clone(),
        );
        let mut messages = Vec::new();
        init_root_menu(
            &mut menu,
            &renderer,
            &mut iced_core::Shell::new(&mut messages),
            INSIDE,
            WINDOW,
            Vector::new(0., 0.),
            bounds,
            -bounds.height,
        );
        let size = menu
            .layout(
                &renderer,
                iced_core::layout::Limits::NONE.min_width(1.).min_height(1.),
            )
            .size();
        let surface = Size::new(size.width.ceil() + 2.0, size.height.ceil() + 2.0);
        (menu, tree, surface)
    }

    /// Sends `events` to the popup showing `menu`, with the pointer at
    /// `at`; the messages that came out.
    fn send_to(
        menu: menu::Menu<'static, Msg>,
        surface: Size,
        at: Point,
        events: &[Event],
    ) -> Vec<Msg> {
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(
            popup_content(menu),
            surface,
            Cache::default(),
            &mut renderer,
        );
        let mut events = events.to_vec();
        events.insert(0, Event::Mouse(mouse::Event::CursorMoved { position: at }));
        let mut messages = Vec::new();
        let _ = ui.update(
            &events,
            mouse::Cursor::Available(at),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        messages
    }

    fn click() -> [Event; 2] {
        [
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ]
    }

    /// The room reserved above the rows for Copy's
    /// two rows to open into, upward.
    const ROOM: f32 = 2.0 * 32.0;
    /// Copy's row, in the surface: below the room and Open, inside the
    /// one-pixel border.
    const COPY_Y: f32 = 1.0 + ROOM + 36.0 + 18.0;
    /// On Copy's count, at the row's right end.
    const ON_COUNT: Point = Point::new(1.0 + 340.0, COPY_Y);

    fn unfolded(tree: &MenuBarState) -> Option<usize> {
        tree.inner.with_data(|d| d.menu_states[0].unfolded)
    }

    /// A popup opens tall enough for its biggest
    /// group: three rows and room for Copy's two.
    #[test]
    fn a_popup_opens_with_room_for_its_biggest_group() {
        let (_, _, surface) = copy_popup();
        assert_eq!(surface.height, 2.0 + 3.0 * 36.0 + 2.0 * 32.0);
    }

    /// Frames 16 ms apart from now, enough for any spring to rest.
    fn frames() -> Vec<Event> {
        let start = Instant::now();
        (0..120)
            .map(|i| {
                Event::Window(window::Event::RedrawRequested(
                    start + std::time::Duration::from_millis(16 * i),
                ))
            })
            .collect()
    }

    /// Where row `row` is in the popup's surface, below its border.
    fn row_y(tree: &MenuBarState, row: usize) -> f32 {
        tree.inner.with_data(|d| {
            let bounds = &d.menu_states[0].menu_bounds;
            bounds.children_bounds.y + bounds.child_positions[row]
        })
    }

    const COPY: usize = 1;
    const PASTE: usize = 4;

    /// Pointing at a row's count unfolds its group, with nothing run and
    /// the menu still open. It opens upward over a few frames: Copy and
    /// the rows above it move up as its rows open under it, and the rows
    /// below never move.
    #[test]
    fn pointing_at_the_count_unfolds_the_group_upward() {
        let (menu, tree, surface) = copy_popup();
        assert_eq!(unfolded(&tree), None);
        assert_eq!(row_y(&tree, COPY), ROOM + 36.0);
        assert_eq!(row_y(&tree, PASTE), ROOM + 72.0);
        assert_eq!(send_to(menu.clone(), surface, ON_COUNT, &[]), []);
        assert_eq!(unfolded(&tree), Some(1));
        assert_eq!(row_y(&tree, COPY), ROOM + 36.0, "not open yet");

        let frames = frames();
        send_to(menu.clone(), surface, ON_COUNT, &frames[..4]);
        let copy = row_y(&tree, COPY);
        assert!(copy > 36.0 && copy < ROOM + 36.0, "on its way up: {copy}");
        assert_eq!(row_y(&tree, PASTE), ROOM + 72.0, "Paste stays");

        send_to(menu, surface, ON_COUNT, &frames[4..]);
        assert_eq!(row_y(&tree, COPY), 36.0, "Copy moved up by its rows");
        assert_eq!(row_y(&tree, PASTE), ROOM + 72.0, "Paste stays");
    }

    /// The menu says where it begins in its surface, which is where the
    /// genie collapses it from: below the room while its groups are folded,
    /// at the top once one fills the room.
    #[test]
    fn the_menu_begins_below_the_room_its_groups_do_not_take() {
        let (menu, tree, surface) = copy_popup();
        let top = |tree: &MenuBarState| tree.inner.with_data(|d| d.menu_states[0].top());
        assert_eq!(top(&tree), ROOM);
        send_to(menu, surface, ON_COUNT, &frames());
        assert_eq!(top(&tree), 0.0, "Copy's rows take the room");
    }

    /// Folding plays the other way: the rows close as Copy and the rows
    /// above come back down, to exactly where they started; Paste never
    /// moves.
    #[test]
    fn leaving_folds_the_group_back_down() {
        let (menu, tree, surface) = copy_popup();
        send_to(menu.clone(), surface, ON_COUNT, &frames());
        assert_eq!(row_y(&tree, COPY), 36.0);
        // Open, at the top now
        let on_open = Point::new(100.0, 1.0 + 18.0);
        send_to(menu.clone(), surface, on_open, &[]);
        assert_eq!(unfolded(&tree), None);
        let frames = frames();
        send_to(menu.clone(), surface, on_open, &frames[..4]);
        let copy = row_y(&tree, COPY);
        assert!(copy > 36.0 && copy < ROOM + 36.0, "on its way down: {copy}");
        assert_eq!(row_y(&tree, PASTE), ROOM + 72.0, "Paste stays");
        send_to(menu, surface, on_open, &frames[4..]);
        assert_eq!(row_y(&tree, COPY), ROOM + 36.0, "back where it started");
        assert_eq!(row_y(&tree, PASTE), ROOM + 72.0);
    }

    /// A click on the count runs nothing and keeps the menu open; the
    /// group stays unfolded, as pointing unfolded it, and the next click
    /// folds it.
    #[test]
    fn a_click_on_the_count_runs_nothing_and_folds_the_second_time() {
        let (menu, tree, surface) = copy_popup();
        assert_eq!(send_to(menu.clone(), surface, ON_COUNT, &click()), []);
        assert!(tree.inner.with_data(|d| d.open));
        assert_eq!(unfolded(&tree), Some(1));

        assert_eq!(send_to(menu, surface, ON_COUNT, &click()), []);
        assert_eq!(unfolded(&tree), None);
        assert!(tree.inner.with_data(|d| d.open));
    }

    /// A click on the rest of the row still runs the row's own action, and
    /// the menu closes as after any.
    #[test]
    fn a_click_beside_the_count_runs_the_row() {
        let (menu, tree, surface) = copy_popup();
        let on_label = Point::new(1.0 + 40.0, COPY_Y);
        assert_eq!(send_to(menu, surface, on_label, &click()), [Msg::Selected]);
        assert!(
            tree.inner.with_data(|d| d.menu_states.is_empty()),
            "the menu closed, as after any action"
        );
    }

    /// The reserved room above the popup rows is empty:
    /// pointing there chooses no row, and a click there runs nothing and
    /// dismisses the menu.
    #[test]
    fn clicking_the_empty_room_dismisses_the_menu() {
        let (menu, tree, surface) = copy_popup();
        let above = Point::new(surface.width / 2.0, 10.0);
        assert_eq!(send_to(menu, surface, above, &click()), []);
        assert!(!tree.inner.with_data(|d| d.open));
        assert!(tree.inner.with_data(|d| d.menu_states.is_empty()));
    }

    fn key(named: keyboard::key::Named) -> Event {
        use keyboard::{Key, Location, Modifiers, key::NativeCode, key::Physical};
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Named(named),
            modified_key: Key::Named(named),
            physical_key: Physical::Unidentified(NativeCode::Unidentified),
            location: Location::Standard,
            modifiers: Modifiers::default(),
            text: None,
            repeat: false,
        })
    }

    fn chosen(tree: &MenuBarState) -> Option<usize> {
        tree.inner.with_data(|d| d.menu_states[0].index)
    }

    /// Only the count itself unfolds: not the rest of the row's end, nor
    /// the row above or below it.
    #[test]
    fn pointing_beside_the_count_does_not_unfold() {
        for at in [
            Point::new(1.0 + 355.0, COPY_Y),
            Point::new(1.0 + 300.0, COPY_Y),
            Point::new(ON_COUNT.x, COPY_Y - 14.0),
        ] {
            let (menu, tree, surface) = copy_popup();
            assert_eq!(send_to(menu, surface, at, &[]), []);
            assert_eq!(unfolded(&tree), None, "at {at:?}");
        }
    }

    /// An unfolded group stays while the pointer is on its row or its rows,
    /// and folds once it is on any other row, above or below.
    #[test]
    fn leaving_the_group_folds_it() {
        let (menu, tree, surface) = copy_popup();
        let move_to = |at: Point| send_to(menu.clone(), surface, at, &frames());
        move_to(ON_COUNT);
        assert_eq!(unfolded(&tree), Some(1));
        // Open now: Open 0-36, Copy 36-72, Copy path 72-104, Copy to
        // 104-136, Paste 136-172
        move_to(Point::new(100.0, 1.0 + 72.0 + 16.0));
        assert_eq!(unfolded(&tree), Some(1), "on Copy path");
        move_to(Point::new(100.0, 1.0 + 36.0 + 18.0));
        assert_eq!(unfolded(&tree), Some(1), "back on Copy");
        move_to(Point::new(100.0, 1.0 + 18.0));
        assert_eq!(unfolded(&tree), None, "up onto Open");

        move_to(ON_COUNT);
        assert_eq!(unfolded(&tree), Some(1));
        move_to(Point::new(100.0, 1.0 + 136.0 + 18.0));
        assert_eq!(unfolded(&tree), None, "down onto Paste");
        assert!(tree.inner.with_data(|d| d.open));
    }

    /// The pointer leaving the menu folds its group too.
    #[test]
    fn leaving_the_menu_folds_the_group() {
        let (menu, tree, surface) = copy_popup();
        send_to(menu.clone(), surface, ON_COUNT, &[]);
        assert_eq!(unfolded(&tree), Some(1));
        send_to(
            menu,
            surface,
            ON_COUNT,
            &[Event::Mouse(mouse::Event::CursorLeft)],
        );
        assert_eq!(unfolded(&tree), None);
        assert!(tree.inner.with_data(|d| d.open));
    }

    /// A tap on a count is the group's from touch down to lift.
    #[test]
    fn a_tap_on_the_count_runs_nothing_and_keeps_the_menu() {
        let (menu, tree, surface) = copy_popup();
        let tap = [
            Event::Touch(touch::Event::FingerPressed {
                id: Finger(1),
                position: ON_COUNT,
            }),
            Event::Touch(touch::Event::FingerLifted {
                id: Finger(1),
                position: ON_COUNT,
            }),
        ];
        assert_eq!(send_to(menu, surface, ON_COUNT, &tap), []);
        assert!(tree.inner.with_data(|d| d.open));
        assert_eq!(unfolded(&tree), Some(1));
    }

    /// The arrow keys pass a folded group's rows by; Space unfolds the
    /// chosen row's group and goes into it, then folds it again.
    #[test]
    fn keys_skip_folded_rows_and_space_unfolds() {
        use keyboard::key::Named;
        let (menu, tree, surface) = copy_popup();
        let away = Point::new(-10.0, -10.0);
        let send = |events: &[Event]| send_to(menu.clone(), surface, away, events);

        send(&[key(Named::ArrowDown), key(Named::ArrowDown)]);
        assert_eq!(chosen(&tree), Some(1), "Copy");
        send(&[key(Named::ArrowDown)]);
        assert_eq!(chosen(&tree), Some(4), "Paste, past Copy's folded rows");

        send(&[key(Named::ArrowUp), key(Named::Space)]);
        assert_eq!(unfolded(&tree), Some(1));
        assert_eq!(chosen(&tree), Some(2), "Copy path");
        send(&[key(Named::ArrowDown)]);
        assert_eq!(chosen(&tree), Some(3), "Copy to");

        send(&[key(Named::Space)]);
        assert_eq!(unfolded(&tree), None);
        assert_eq!(chosen(&tree), Some(1), "back on Copy");

        // Space on a row of its own does nothing in a menu with groups
        send(&[key(Named::ArrowDown), key(Named::Space)]);
        assert!(tree.inner.with_data(|d| d.open));
    }

    /// In a menu without groups Space still closes the menu, as before.
    #[test]
    fn space_closes_a_menu_without_groups() {
        let (menu, tree, surface) = popup_of(vec![button("Open"), button("Paste")]);
        send_to(
            menu,
            surface,
            Point::new(-10.0, -10.0),
            &[key(keyboard::key::Named::Space)],
        );
        assert!(!tree.inner.with_data(|d| d.open));
    }

    /// The room kept is for the biggest group, wherever it sits.
    #[test]
    fn the_room_kept_is_for_the_biggest_group() {
        let (_, _, surface) = popup_of(vec![
            split("Cut", &["Move to"]),
            split("Copy", &["Copy path", "Copy to", "Copy name"]),
        ]);
        assert_eq!(surface.height, 2.0 + 2.0 * 36.0 + 3.0 * 32.0);
    }

    fn copy_popup_with_surface_handler() -> (menu::Menu<'static, Msg>, MenuBarState, Size) {
        popup_with(
            vec![
                button("Open"),
                split("Copy", &["Copy path", "Copy to"]),
                button("Paste"),
            ],
            Some(Arc::new(|_| Msg::Surface)),
        )
    }

    #[test]
    fn unfolding_with_a_surface_handler_never_repositions_the_popup() {
        let (menu, tree, surface) = copy_popup_with_surface_handler();
        let at = Point::new(341.0, 1.0 + row_y(&tree, COPY) + 18.0);
        let messages = send_to(menu.clone(), surface, at, &frames());
        assert!(
            messages.is_empty(),
            "unfold must not resize or move the surface: {messages:?}"
        );
        assert_eq!(unfolded(&tree), Some(COPY));
        let messages = send_to(menu, surface, Point::new(100.0, 19.0), &frames());
        assert!(
            messages.is_empty(),
            "fold must not resize or move the surface: {messages:?}"
        );
        assert_eq!(unfolded(&tree), None);
    }
    /// A popup given less height than it asked for gives its room up to its
    /// rows as it is laid out, and the crop follows on that same, first
    /// layout: every row is drawn, from the top of the surface.
    #[test]
    fn a_constrained_popup_is_cropped_where_it_fits_on_its_first_layout() {
        use iced_core::renderer::Headless;
        let (menu, tree, surface) = copy_popup_with_surface_handler();
        assert_eq!(tree.inner.with_data(|d| d.menu_states[0].top()), ROOM);
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let state = tree.clone();
        let top =
            crate::ui::widget::Top::new(move || state.inner.with_data(|d| d.menu_states[0].top()));
        let crop = std::cell::Cell::new(iced::Rectangle::default());
        let view = top.place(
            iced_texture_cache::cached(
                iced_texture_cache::TextureCache::new(),
                crate::ui::widget::measure_bounds(top.crop(popup_content(menu)), &crop),
            )
            .border_radius(8.0),
        );
        // Three rows and the border, without the room
        let fitted = Size::new(surface.width, 110.0);
        let mut ui = UserInterface::build(view, fitted, Cache::default(), &mut renderer);
        let theme = crate::ui::theme::active();
        let pixels = Size::new(fitted.width as u32, fitted.height as u32);
        iced_core::Renderer::reset(&mut renderer, iced::Rectangle::with_size(fitted));
        ui.draw(
            &mut renderer,
            &theme,
            &iced_core::renderer::Style {
                text_color: iced::Color::WHITE,
            },
            mouse::Cursor::Unavailable,
        );
        let image = renderer.screenshot(pixels, 1.0, iced::Color::TRANSPARENT);
        assert_eq!(tree.inner.with_data(|d| d.menu_states[0].top()), 0.0);
        assert_eq!(crop.get().y, 0.0, "cropped from the top of the surface");
        // Open, the first row, now at the top
        let (x, y) = (100, (1.0 + 18.0) as usize);
        assert_ne!(
            image[(y * pixels.width as usize + x) * 4 + 3],
            0,
            "Open is drawn"
        );
    }

    /// The shell's popup composition, built once: a group unfolding over
    /// frames moves the crop with it, so the rows moving up into the room
    /// are drawn, not cut off at where the menu began when it was built.
    #[test]
    fn rows_unfolding_into_the_room_are_drawn_in_the_cropped_popup() {
        use iced_core::renderer::Headless;
        let (menu, tree, surface) = copy_popup_with_surface_handler();
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let state = tree.clone();
        let top =
            crate::ui::widget::Top::new(move || state.inner.with_data(|d| d.menu_states[0].top()));
        let view = top.place(
            iced_texture_cache::cached(
                iced_texture_cache::TextureCache::new(),
                top.crop(popup_content(menu)),
            )
            .border_radius(8.0),
        );
        let mut ui = UserInterface::build(view, surface, Cache::default(), &mut renderer);
        let theme = crate::ui::theme::active();
        let pixels = Size::new(surface.width as u32, surface.height as u32);
        let mut draw = |ui: &mut UserInterface<'_, Msg, crate::ui::Theme, crate::ui::Renderer>,
                        renderer: &mut crate::ui::Renderer| {
            iced_core::Renderer::reset(renderer, iced::Rectangle::with_size(surface));
            ui.draw(
                renderer,
                &theme,
                &iced_core::renderer::Style {
                    text_color: iced::Color::WHITE,
                },
                mouse::Cursor::Available(ON_COUNT),
            );
            renderer.screenshot(pixels, 1.0, iced::Color::TRANSPARENT)
        };
        // Copy's row once it has moved up by its group's rows: in the room
        let (x, y) = (100, (1.0 + 36.0 + 18.0) as usize);
        let alpha = |rgba: &[u8]| rgba[(y * pixels.width as usize + x) * 4 + 3];
        assert_eq!(alpha(&draw(&mut ui, &mut renderer)), 0, "the room is empty");

        let mut messages = Vec::new();
        let cursor = mouse::Cursor::Available(ON_COUNT);
        let now = Instant::now();
        let events = std::iter::once(Event::Mouse(mouse::Event::CursorMoved {
            position: ON_COUNT,
        }))
        .chain((0..60).map(|frame| {
            Event::Window(window::Event::RedrawRequested(
                now + std::time::Duration::from_millis(16 * frame),
            ))
        }));
        for event in events {
            let _ = ui.update(
                &[event],
                cursor,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            let _ = draw(&mut ui, &mut renderer);
        }
        assert_eq!(unfolded(&tree), Some(COPY));
        assert_eq!(row_y(&tree, COPY), 36.0, "Copy moved up by its rows");
        assert_ne!(
            alpha(&draw(&mut ui, &mut renderer)),
            0,
            "Copy's row is drawn in the room"
        );
    }

    #[test]
    fn cached_unfold_frames_keep_lower_rows_still_and_leave_no_trails() {
        use iced_core::renderer::Headless;
        let (menu, tree, surface) = copy_popup_with_surface_handler();
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let cache = iced_texture_cache::TextureCache::new();
        let mut ui = UserInterface::build(
            iced_texture_cache::cached(cache.clone(), popup_content(menu)).border_radius(8.0),
            surface,
            Cache::default(),
            &mut renderer,
        );
        let theme = crate::ui::theme::active();
        let pixels = Size::new(surface.width as u32, surface.height as u32);
        let draw = |ui: &mut UserInterface<'_, Msg, crate::ui::Theme, crate::ui::Renderer>,
                    renderer: &mut crate::ui::Renderer,
                    cursor| {
            iced_core::Renderer::reset(renderer, iced::Rectangle::with_size(surface));
            ui.draw(
                renderer,
                &theme,
                &iced_core::renderer::Style {
                    text_color: iced::Color::WHITE,
                },
                cursor,
            );
            renderer.screenshot(pixels, 1.0, iced::Color::TRANSPARENT)
        };
        let baseline = draw(&mut ui, &mut renderer, mouse::Cursor::Unavailable);
        let pixel = |rgba: &[u8], x: usize, y: usize| {
            let i = (y * pixels.width as usize + x) * 4;
            [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
        };
        assert_ne!(
            pixel(&baseline, 335, COPY_Y as usize),
            pixel(&baseline, 300, COPY_Y as usize),
            "the count is drawn above its row"
        );
        let lower = ((surface.height as usize - 35) * pixels.width as usize) * 4;
        let mut messages = Vec::new();
        let now = Instant::now();
        for opening in [true, false] {
            let cursor = if opening {
                mouse::Cursor::Available(ON_COUNT)
            } else {
                mouse::Cursor::Unavailable
            };
            let event = if opening {
                Event::Mouse(mouse::Event::CursorMoved { position: ON_COUNT })
            } else {
                Event::Mouse(mouse::Event::CursorLeft)
            };
            let _ = ui.update(
                &[event],
                cursor,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            for frame in 0..60 {
                let offset = if opening { frame } else { frame + 60 };
                let event = Event::Window(window::Event::RedrawRequested(
                    now + std::time::Duration::from_millis(16 * offset),
                ));
                let _ = ui.update(
                    &[event],
                    cursor,
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
                let image = draw(&mut ui, &mut renderer, cursor);
                assert_eq!(
                    &image[lower..],
                    &baseline[lower..],
                    "lower row changed on frame {offset}"
                );
                if opening && frame == 3 {
                    let paste_y = row_y(&tree, PASTE) + 1.0;
                    cache.invalidate();
                    let away = draw(&mut ui, &mut renderer, mouse::Cursor::Unavailable);
                    cache.invalidate();
                    let over_paste = draw(
                        &mut ui,
                        &mut renderer,
                        mouse::Cursor::Available(Point::new(100.0, paste_y + 8.0)),
                    );
                    let above_paste = paste_y as usize * pixels.width as usize * 4;
                    assert_eq!(
                        &away[..above_paste],
                        &over_paste[..above_paste],
                        "hidden member geometry must not borrow Paste's hover"
                    );
                    cache.invalidate();
                }
                let top = row_y(&tree, 0).floor() as usize;
                if top > 1 {
                    for y in 0..top - 1 {
                        for x in 0..pixels.width as usize {
                            assert_eq!(
                                pixel(&image, x, y)[3],
                                0,
                                "trail above the menu at ({x}, {y}) on frame {offset}"
                            );
                        }
                    }
                }
            }
        }
        assert_eq!(
            draw(&mut ui, &mut renderer, mouse::Cursor::Unavailable),
            baseline,
            "folding must restore the original image"
        );
        assert!(messages.is_empty());
    }
    #[test]
    fn edge_groups_keep_the_border_visible_during_every_frame() {
        check_edge_group_borders(false);
    }

    #[test]
    #[ignore = "requires a WGPU adapter"]
    fn wgpu_edge_groups_keep_the_border_visible_during_every_frame() {
        check_edge_group_borders(true);
    }

    fn check_edge_group_borders(wgpu: bool) {
        for scale in [1.0, 1.25, 2.0] {
            check_edge_group_borders_at_scale(wgpu, scale);
        }
    }

    fn check_edge_group_borders_at_scale(wgpu: bool, scale: f32) {
        use crate::ui::convert::ToRadius as _;
        use crate::ui::theme::style::menu_bar::StyleSheet as _;
        use iced_core::Renderer as _;
        use iced_core::renderer::Headless;
        for first in [true, false] {
            let items = if first {
                vec![split("Group", &["One", "Two"]), button("Other")]
            } else {
                vec![button("Other"), split("Group", &["One", "Two"])]
            };
            let (menu, tree, surface) = popup_of(items);
            let mut renderer = if wgpu {
                crate::ui::iced::futures::executor::block_on(
                    <crate::ui::Renderer as Headless>::new(
                        iced_core::Font::DEFAULT,
                        iced_core::Pixels(16.0),
                        Some("wgpu"),
                    ),
                )
                .expect("WGPU adapter")
            } else {
                iced_texture_cache::testing::headless_tiny_skia()
            };
            let mut ui = UserInterface::build(
                popup_content(menu),
                surface,
                Cache::default(),
                &mut renderer,
            );
            let theme = crate::ui::theme::active();
            let appearance = theme.appearance(&Default::default(), false);
            let pixels = Size::new(
                (surface.width * scale).ceil() as u32,
                (surface.height * scale).ceil() as u32,
            );
            let pointer = Point::new(341.0, 64.0 + 1.0 + if first { 18.0 } else { 54.0 });
            let mut messages = Vec::new();
            let now = Instant::now();
            for opening in [true, false] {
                let cursor = if opening {
                    mouse::Cursor::Available(pointer)
                } else {
                    mouse::Cursor::Unavailable
                };
                let event = if opening {
                    Event::Mouse(mouse::Event::CursorMoved { position: pointer })
                } else {
                    Event::Mouse(mouse::Event::CursorLeft)
                };
                let _ = ui.update(
                    &[event],
                    cursor,
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
                for frame in 0..60 {
                    let tick = if opening { frame } else { frame + 60 };
                    let _ = ui.update(
                        &[Event::Window(window::Event::RedrawRequested(
                            now + std::time::Duration::from_millis(tick * 16),
                        ))],
                        cursor,
                        &mut renderer,
                        &mut clipboard::Null,
                        &mut messages,
                    );
                    renderer.reset(iced::Rectangle::with_size(surface));
                    ui.draw(
                        &mut renderer,
                        &theme,
                        &iced_core::renderer::Style {
                            text_color: iced::Color::WHITE,
                        },
                        cursor,
                    );
                    let actual = renderer.screenshot(pixels, scale, iced::Color::TRANSPARENT);
                    let bounds = tree
                        .inner
                        .with_data(|d| d.menu_states[0].menu_bounds.children_bounds);
                    let outline = iced::Rectangle {
                        x: 0.0,
                        y: bounds.y,
                        width: surface.width,
                        height: bounds.height + 2.0,
                    };
                    renderer.reset(iced::Rectangle::with_size(surface));
                    // Reference: an empty rounded menu with its outline on
                    // top, independent of row geometry and reveal clipping.
                    let quad = iced_core::renderer::Quad {
                        bounds: outline,
                        border: iced::Border {
                            radius: appearance.menu_border_radius.to_radius(),
                            width: appearance.border_width,
                            color: appearance.border_color,
                        },
                        shadow: Default::default(),
                        snap: true,
                    };
                    renderer.fill_quad(
                        iced_core::renderer::Quad {
                            border: iced::Border {
                                width: 0.0,
                                ..quad.border
                            },
                            ..quad
                        },
                        appearance.background,
                    );
                    renderer.fill_quad(quad, iced::Color::TRANSPARENT);
                    let expected = renderer.screenshot(pixels, scale, iced::Color::TRANSPARENT);
                    for y in 0..pixels.height as usize {
                        for x in 0..pixels.width as usize {
                            let i = (y * pixels.width as usize + x) * 4;
                            if x != 0 && x != pixels.width as usize - 1 && expected[i + 3] == 255 {
                                continue;
                            }
                            assert_eq!(
                                &actual[i..i + 4],
                                &expected[i..i + 4],
                                "border at ({x}, {y}), first={first}, opening={opening}, frame={frame}, scale={scale}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn constrained_popup_gives_rows_priority_over_empty_room() {
        let (menu, tree, surface) = copy_popup();
        let short = Size::new(surface.width, 110.0);
        send_to(menu, short, Point::new(-10.0, -10.0), &[]);
        assert_eq!(row_y(&tree, 0), 0.0);
        assert_eq!(row_y(&tree, PASTE), 72.0);
    }

    #[test]
    fn keyboard_unfold_scrolls_to_the_chosen_member_after_animation() {
        use keyboard::key::Named;
        let mut items: Items = (0..20).map(|_| button("Row")).collect();
        items.push(split("Copy", &["Variant"; 10]));
        let (menu, tree, surface) = popup_of(items);
        let short = Size::new(surface.width, 200.0);
        let mut events = vec![key(Named::ArrowUp), key(Named::ArrowRight)];
        events.extend(frames());
        send_to(menu.clone(), short, Point::new(-10.0, -10.0), &events);
        assert_eq!(chosen(&tree), Some(21));
        let renderer = iced_texture_cache::testing::headless_tiny_skia();
        let node = menu.layout(
            &renderer,
            iced_core::layout::Limits::new(Size::ZERO, Size::new(short.width, short.height - 1.0)),
        );
        let rows = node.children()[0].children();
        // A partially visible standalone row may precede the member.
        let first = rows
            .iter()
            .map(|row| row.bounds())
            .find(|bounds| bounds.height == 32.0)
            .expect("selected member is laid out");
        let y = first.y + node.children()[0].bounds().y;
        assert!(
            first.height == 32.0 && y >= 0.0 && y + first.height <= short.height,
            "first selected member must be fully visible, got {first:?} at {y}"
        );
    }

    #[test]
    fn pointer_selection_follows_rows_moving_under_a_still_pointer() {
        let (menu, tree, surface) = copy_popup();
        send_to(menu, surface, ON_COUNT, &frames());
        assert_eq!(unfolded(&tree), Some(COPY));
        assert_eq!(chosen(&tree), Some(3));
    }
}
