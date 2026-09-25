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
            use crate::ui::widget::menu::Menu;

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

            let mut popup_menu: Menu<'static, _> = Menu {
                tree: my_state.menu_bar_state.clone(),
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
                main_offset: -(bounds.height as i32),
                cross_offset: 0,
                root_bounds_list: vec![bounds],
                path_highlight: Some(PathHighlight::MenuActive),
                style: std::borrow::Cow::Owned(crate::ui::theme::menu_bar::MenuBarStyle::Default),
                position: Point::new(0., 0.),
                is_overlay: false,
                window_id: id,
                depth: 0,
                on_surface_action: self.on_surface_action.clone(),
            };

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
                ..Default::default()
            };
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
                Some(move || {
                    (crate::ui::Element::from(
                        crate::ui::widget::container(popup_menu.clone()).center(Length::Fill),
                    ))
                    .map(crate::ui::action::app)
                }),
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
        // Any key that is not a bare modifier closes the menu: the menu has no
        // keyboard navigation, and a shortcut pressed while it is open acts on
        // the window behind it, so the menu must not stay up over the result
        let key_closes = matches!(
            event,
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) if !is_modifier_key(key)
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
                state.open = false;

                if matches!(windowing_system(), Some(WindowingSystem::Wayland))
                    && let Some(id) = state.popup_id.remove(&self.window_id)
                {
                    {
                        let surface_action = self.on_surface_action.as_ref().unwrap();

                        // A right press is about to open a replacement on its
                        // release, and two popups cannot share the one menu
                        // tree. Everything else is a plain dismissal and may
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

        if !was_open && cursor.is_over(bounds) {
            // Present a context menu on a right click event.
            if !was_open
                && self.context_menu.is_some()
                && (right_button_released(event) || (touch_lifted(event) && fingers_pressed == 2))
            {
                state.context_cursor = cursor.position().unwrap_or_default();
                let state = tree.state.downcast_mut::<LocalState>();
                state.menu_bar_state.inner.with_data_mut(|state| {
                    state.open = true;
                    state.view_cursor = cursor;
                });
                if matches!(windowing_system(), Some(WindowingSystem::Wayland)) {
                    self.create_popup(layout, cursor, renderer, shell, viewport, state);
                }

                shell.request_redraw();
                shell.capture_event();
                self.report_open_state(tree.state.downcast_mut::<LocalState>(), shell);
                return;
            } else if !was_open && right_button_released(event)
                || (touch_lifted(event))
                || left_button_released(event)
            {
                state.menu_bar_state.inner.with_data_mut(|state| {
                    was_open = true;
                    state.menu_states.clear();
                    state.active_root.clear();
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

fn right_button_released(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right,))
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
    fingers_pressed: HashSet<Finger>,
    menu_bar_state: MenuBarState,
    reported_open: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
