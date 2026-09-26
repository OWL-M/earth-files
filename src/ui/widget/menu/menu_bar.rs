// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0 AND MIT

// From iced_aw, license MIT

//! Vendored from pop-os/libcosmic, src/widget/menu/menu_bar.rs
//!
//! A widget that handles menu trees
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::menu_inner::{
    CloseCondition, Direction, ItemHeight, ItemWidth, Menu, MenuState, PathHighlight,
};
use super::menu_tree::MenuTree;
use crate::ui::Renderer;
use crate::ui::shell::runner::{WindowingSystem, windowing_system};
use crate::ui::theme::menu_bar::StyleSheet;
use crate::ui::widget::RcWrapper;
use crate::ui::widget::menu::menu_inner::init_root_menu;

use crate::ui::convert::ToRadius;
use iced::{Point, Shadow, Vector, window};
use iced_core::Border;
use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::renderer::{self, Renderer as IcedRenderer};
use iced_core::widget::{Tree, tree};
use iced_core::{
    Alignment, Clipboard, Element, Layout, Length, Padding, Rectangle, Shell, Widget, event,
    overlay, touch,
};

/// A `MenuBar` collects `MenuTree`s and handles all the layout, event processing, and drawing.
pub fn menu_bar<Message>(menu_roots: Vec<MenuTree<Message>>) -> MenuBar<Message>
where
    Message: Clone + 'static,
{
    MenuBar::new(menu_roots)
}

#[derive(Clone, Default)]
pub(crate) struct MenuBarState {
    pub(crate) inner: RcWrapper<MenuBarStateInner>,
}

pub(crate) struct MenuBarStateInner {
    pub(crate) tree: Tree,
    pub(crate) popup_id: HashMap<window::Id, window::Id>,
    /// Popups that have been asked to go but are still on screen, playing
    /// their exit animation, keyed by parent window like `popup_id`.
    ///
    /// `popup_id` means "this window's popup is open and current". It stops
    /// meaning "a popup is on screen" the moment a teardown is deferred, and
    /// the tree freeze depends on the latter: thawed under a popup that is
    /// still rendering, the next diff lays out a mismatch and
    /// `menu_inner`'s root-count assertion fails.
    pub(crate) leaving: HashMap<window::Id, window::Id>,
    pub(crate) pressed: bool,
    /// The bar saw the press still held, so its release is the bar's too.
    pub(crate) bar_pressed: bool,
    /// A context menu opened on a right press that is still held. The
    /// release that ends it is the end of the opening click, not a click on
    /// the menu, and must neither choose an item nor close the menu.
    pub(crate) opening_press_held: bool,
    /// Fingers of the two-finger tap that opened a context menu, still down
    /// when it opened on the first lift. Their lifts end that tap, as
    /// `opening_press_held` is for a right click. The context menu takes
    /// them out; the menu only reads the set, since on the overlay path it
    /// sees the lift first.
    pub(crate) opening_fingers: HashSet<iced_core::touch::Finger>,
    pub(crate) view_cursor: Cursor,
    pub(crate) open: bool,
    pub(crate) active_root: Vec<usize>,
    pub(crate) horizontal_direction: Direction,
    pub(crate) vertical_direction: Direction,
    /// List of all menu states
    pub(crate) menu_states: Vec<MenuState>,
}
impl MenuBarStateInner {
    /// get the list of indices hovered for the menu
    pub(super) fn get_trimmed_indices(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        self.menu_states
            .iter()
            .skip(index)
            .take_while(|ms| ms.index.is_some())
            .map(|ms| ms.index.expect("No indices were found in the menu state."))
    }

    pub(crate) fn reset(&mut self) {
        self.open = false;
        self.active_root = Vec::new();
        self.menu_states.clear();
        self.opening_press_held = false;
        self.opening_fingers.clear();
    }
}
impl Default for MenuBarStateInner {
    fn default() -> Self {
        Self {
            tree: Tree::empty(),
            pressed: false,
            view_cursor: Cursor::Available([-0.5, -0.5].into()),
            open: false,
            active_root: Vec::new(),
            horizontal_direction: Direction::Positive,
            vertical_direction: Direction::Positive,
            menu_states: Vec::new(),
            popup_id: HashMap::new(),
            leaving: HashMap::new(),
            bar_pressed: false,
            opening_press_held: false,
            opening_fingers: HashSet::new(),
        }
    }
}

pub(crate) fn menu_roots_children<Message>(menu_roots: &[MenuTree<Message>]) -> Vec<Tree>
where
    Message: Clone + 'static,
{
    /*
    menu bar
        menu root 1 (stateless)
            flat tree
        menu root 2 (stateless)
            flat tree
        ...
    */

    menu_roots
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
        .collect()
}

pub(crate) fn menu_roots_diff<Message>(menu_roots: &[MenuTree<Message>], tree: &mut Tree)
where
    Message: Clone + 'static,
{
    if tree.children.len() > menu_roots.len() {
        tree.children.truncate(menu_roots.len());
    }

    tree.children
        .iter_mut()
        .zip(menu_roots.iter())
        .for_each(|(t, root)| {
            let flat = root
                .flatten()
                .iter()
                .map(|mt| &mt.item as &dyn Widget<Message, crate::ui::Theme, Renderer>)
                .collect::<Vec<_>>();

            t.diff_children(flat.as_slice());
        });

    if tree.children.len() < menu_roots.len() {
        let extended = menu_roots[tree.children.len()..].iter().map(|root| {
            let mut tree = Tree::empty();
            let flat = root
                .flatten()
                .iter()
                .map(|mt| Tree::new(mt.item.clone()))
                .collect();
            tree.children = flat;
            tree
        });
        tree.children.extend(extended);
    }
}

pub fn get_mut_or_default<T: Default>(vec: &mut Vec<T>, index: usize) -> &mut T {
    if index < vec.len() {
        &mut vec[index]
    } else {
        vec.resize_with(index + 1, T::default);
        &mut vec[index]
    }
}

/// A `MenuBar` collects `MenuTree`s and handles all the layout, event processing, and drawing.
#[allow(missing_debug_implementations)]
pub struct MenuBar<Message> {
    width: Length,
    height: Length,
    spacing: f32,
    padding: Padding,
    bounds_expand: u16,
    main_offset: i32,
    cross_offset: i32,
    close_condition: CloseCondition,
    item_width: ItemWidth,
    item_height: ItemHeight,
    path_highlight: Option<PathHighlight>,
    menu_roots: Vec<MenuTree<Message>>,
    style: <crate::ui::Theme as StyleSheet>::Style,
    window_id: window::Id,
    positioner: crate::ui::surface::Positioner,
    pub(crate) on_surface_action:
        Option<Arc<dyn Fn(crate::ui::surface::Action<Message>) -> Message + Send + Sync + 'static>>,
}

impl<Message> MenuBar<Message>
where
    Message: Clone + 'static,
{
    /// Creates a new [`MenuBar`] with the given menu roots
    #[must_use]
    pub fn new(menu_roots: Vec<MenuTree<Message>>) -> Self {
        let mut menu_roots = menu_roots;
        menu_roots.iter_mut().for_each(MenuTree::set_index);

        Self {
            width: Length::Shrink,
            height: Length::Shrink,
            spacing: 0.0,
            padding: Padding::ZERO,
            bounds_expand: 16,
            main_offset: 0,
            cross_offset: 0,
            close_condition: CloseCondition {
                leave: false,
                click_outside: true,
                click_inside: true,
            },
            item_width: ItemWidth::Uniform(150),
            item_height: ItemHeight::Uniform(30),
            path_highlight: Some(PathHighlight::MenuActive),
            menu_roots,
            style: <crate::ui::Theme as StyleSheet>::Style::default(),
            window_id: crate::ui::window::reserved(),
            positioner: crate::ui::surface::Positioner::default(),
            on_surface_action: None,
        }
    }

    /// Sets the expand value for each menu's check bounds
    ///
    /// When the cursor goes outside of a menu's check bounds,
    /// the menu will be closed automatically, this value expands
    /// the check bounds
    #[must_use]
    pub fn bounds_expand(mut self, value: u16) -> Self {
        self.bounds_expand = value;
        self
    }

    /// [`CloseCondition`]
    #[must_use]
    pub fn close_condition(mut self, close_condition: CloseCondition) -> Self {
        self.close_condition = close_condition;
        self
    }

    /// Moves each menu in the horizontal open direction
    #[must_use]
    pub fn cross_offset(mut self, value: i32) -> Self {
        self.cross_offset = value;
        self
    }

    /// Sets the height of the [`MenuBar`]
    #[must_use]
    pub fn height(mut self, height: Length) -> Self {
        self.height = height;
        self
    }

    /// [`ItemHeight`]
    #[must_use]
    pub fn item_height(mut self, item_height: ItemHeight) -> Self {
        self.item_height = item_height;
        self
    }

    /// [`ItemWidth`]
    #[must_use]
    pub fn item_width(mut self, item_width: ItemWidth) -> Self {
        self.item_width = item_width;
        self
    }

    /// Moves all the menus in the vertical open direction
    #[must_use]
    pub fn main_offset(mut self, value: i32) -> Self {
        self.main_offset = value;
        self
    }

    /// Sets the [`Padding`] of the [`MenuBar`]
    #[must_use]
    pub fn padding<P: Into<Padding>>(mut self, padding: P) -> Self {
        self.padding = padding.into();
        self
    }

    /// Sets the method for drawing path highlight
    #[must_use]
    pub fn path_highlight(mut self, path_highlight: Option<PathHighlight>) -> Self {
        self.path_highlight = path_highlight;
        self
    }

    /// Sets the spacing between menu roots
    #[must_use]
    pub fn spacing(mut self, units: f32) -> Self {
        self.spacing = units;
        self
    }

    /// Sets the style of the menu bar and its menus
    #[must_use]
    pub fn style(mut self, style: impl Into<<crate::ui::Theme as StyleSheet>::Style>) -> Self {
        self.style = style.into();
        self
    }

    /// Sets the width of the [`MenuBar`]
    #[must_use]
    pub fn width(mut self, width: Length) -> Self {
        self.width = width;
        self
    }

    pub fn with_positioner(mut self, positioner: crate::ui::surface::Positioner) -> Self {
        self.positioner = positioner;
        self
    }

    #[must_use]
    pub fn window_id(mut self, id: window::Id) -> Self {
        self.window_id = id;
        self
    }

    #[must_use]
    pub fn window_id_maybe(mut self, id: Option<window::Id>) -> Self {
        if let Some(id) = id {
            self.window_id = id;
        }
        self
    }

    #[must_use]
    pub fn on_surface_action(
        mut self,
        handler: impl Fn(crate::ui::surface::Action<Message>) -> Message + Send + Sync + 'static,
    ) -> Self {
        self.on_surface_action = Some(Arc::new(handler));
        self
    }

    /// Leaves the popup `id`, collapsing out, the state it renders from, and
    /// takes a fresh one for the menus opened after it.
    ///
    /// The two cannot share a tree. The collapsing popup lays out the menu as
    /// it was when it opened, positionally against the tree, while the next
    /// diff reshapes that tree to the menu as it is now. Frozen instead, the
    /// tree would be stale for a menu opened during the collapse.
    fn detach(&self, my_state: &mut MenuBarState, id: window::Id) {
        let mut fresh = MenuBarStateInner::default();
        menu_roots_diff(&self.menu_roots, &mut fresh.tree);
        // Still on screen until it has collapsed; see `create_popup`.
        fresh.leaving.insert(self.window_id, id);
        my_state.inner = RcWrapper::new(fresh);
    }

    #[allow(clippy::too_many_lines)]
    fn create_popup(
        &mut self,
        layout: Layout<'_>,
        view_cursor: Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
        my_state: &mut MenuBarState,
    ) {
        if self.window_id != crate::ui::window::none()
            && let Some(surface_action) = self.on_surface_action.clone()
        {
            use crate::ui::surface::action::destroy_popup;
            use crate::ui::surface::{PopupSettings, Positioner};

            let old_active_root = my_state
                .inner
                .with_data(|state| state.active_root.first().copied());

            // if position is not on menu bar button skip.
            let hovered_root = layout
                .children()
                .position(|lo| view_cursor.is_over(lo.bounds()));
            if hovered_root.is_none()
                || old_active_root
                    .zip(hovered_root)
                    .is_some_and(|r| r.0 == r.1)
            {
                return;
            }

            let (id, root_list) = my_state.inner.with_data_mut(|state| {
                // A popup still collapsing has to go now rather than finish:
                // the new one takes the grab, and a grabbing popup has to be
                // the topmost one.
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
                    layout.children().map(|lo| lo.bounds()).collect(),
                )
            });

            let mut popup_menu: Menu<'static, _> = Menu {
                tree: my_state.clone(),
                menu_roots: std::borrow::Cow::Owned(self.menu_roots.clone()),
                bounds_expand: self.bounds_expand,
                menu_overlays_parent: false,
                close_condition: self.close_condition,
                item_width: self.item_width,
                item_height: self.item_height,
                bar_bounds: layout.bounds(),
                main_offset: self.main_offset,
                cross_offset: self.cross_offset,
                root_bounds_list: root_list,
                path_highlight: self.path_highlight,
                style: std::borrow::Cow::Owned(self.style),
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
                self.main_offset as f32,
            );
            let (anchor_rect, gravity) = my_state.inner.with_data_mut(|state| {
                state.popup_id.insert(self.window_id, id);
                (
                    state
                        .menu_states
                        .iter()
                        .find(|s| s.index.is_none())
                        .map(|s| s.menu_bounds.parent_bounds)
                        .map_or_else(
                            || {
                                let bounds = layout.bounds();
                                Rectangle {
                                    x: bounds.x as i32,
                                    y: bounds.y as i32,
                                    width: bounds.width as i32,
                                    height: bounds.height as i32,
                                }
                            },
                            |r| Rectangle {
                                x: r.x as i32,
                                y: r.y as i32,
                                width: r.width as i32,
                                height: r.height as i32,
                            },
                        ),
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

            let menu_node = popup_menu.layout(renderer, Limits::NONE.min_width(1.).min_height(1.));
            let popup_size = menu_node.size();
            let positioner = Positioner {
                size: Some((
                    popup_size.width.ceil() as u32 + 2,
                    popup_size.height.ceil() as u32 + 2,
                )),
                anchor_rect,
                anchor: crate::ui::surface::PopupAnchor::BottomLeft,
                gravity,
                ..Default::default()
            };
            let parent = self.window_id;

            shell.publish((surface_action)(crate::ui::surface::action::simple_popup(
                move || PopupSettings {
                    parent,
                    id,
                    positioner,
                    // Collapses out only when dismissed, never when
                    // replaced by another root's menu: see `leaving`.
                    animate: true,
                },
                Some(move || {
                    (Element::from(
                        crate::ui::widget::container(popup_menu.clone()).center(Length::Fill),
                    ))
                    .map(crate::ui::action::app)
                }),
            )));
        }
    }
}
impl<Message> Widget<Message, crate::ui::Theme, Renderer> for MenuBar<Message>
where
    Message: Clone + 'static,
{
    fn size(&self) -> iced_core::Size<Length> {
        iced_core::Size::new(self.width, self.height)
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<MenuBarState>();
        // A popup collapsing out keeps a state of its own, so this one never
        // has to wait for it: see `detach`.
        state
            .inner
            .with_data_mut(|inner| menu_roots_diff(&self.menu_roots, &mut inner.tree));
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<MenuBarState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(MenuBarState::default())
    }

    fn children(&self) -> Vec<Tree> {
        menu_roots_children(&self.menu_roots)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        use super::flex;

        let limits = limits.width(self.width).height(self.height);
        let mut children = self
            .menu_roots
            .iter_mut()
            .map(|root| &mut root.item)
            .collect::<Vec<_>>();
        // the first children of the tree are the menu roots items
        let mut tree_children = tree
            .children
            .iter_mut()
            .map(|t| &mut t.children[0])
            .collect::<Vec<_>>();
        flex::resolve_wrapper(
            &flex::Axis::Horizontal,
            renderer,
            &limits,
            self.padding,
            self.spacing,
            Alignment::Center,
            &mut children,
            &mut tree_children,
        )
    }

    #[allow(clippy::too_many_lines)]
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &event::Event,
        layout: Layout<'_>,
        view_cursor: Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        use event::Event::{Mouse, Touch};
        use mouse::Button::Left;
        use mouse::Event::{ButtonPressed, ButtonReleased};
        use touch::Event::{FingerLifted, FingerLost, FingerPressed};

        process_root_events(
            &mut self.menu_roots,
            view_cursor,
            tree,
            event,
            layout,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        let my_state = tree.state.downcast_mut::<MenuBarState>();

        // The compositor dismissed our popup: nothing else tells this state
        // about it. iced has no `PlatformSpecific::Wayland` event
        // carrying the dismissed popup's id, so the shell records it and we
        // claim it here; see `ui::surface::dismissal`.
        my_state.inner.with_data_mut(|d| {
            if d.popup_id
                .get(&self.window_id)
                .copied()
                .is_some_and(crate::ui::surface::dismissal::claim)
            {
                // submenus were dismissed with it
                d.popup_id.clear();
                d.reset();
            }

            // A popup that was collapsing is gone once its surface is.
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
            && my_state
                .inner
                .with_data(|d| !d.open && !d.active_root.is_empty());

        let open = my_state.inner.with_data_mut(|state| {
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

        match event {
            // A menu opens on the press, as a context menu does, rather than
            // waiting for the release.
            Mouse(ButtonPressed(Left)) | Touch(FingerPressed { .. }) => {
                let over_bar = view_cursor.is_over(layout.bounds());
                let (create_popup, leaving) = my_state.inner.with_data_mut(|state| {
                    if state.menu_states.is_empty() && over_bar {
                        state.view_cursor = view_cursor;
                        state.open = true;
                        return (true, None);
                    }
                    let mut leaving = None;
                    if let Some(id) = state.popup_id.remove(&self.window_id) {
                        let was_open = state.open;
                        state.menu_states.clear();
                        state.active_root.clear();
                        state.open = false;
                        let surface_action = self.on_surface_action.as_ref().unwrap();
                        // Nothing replaces it, so it may collapse on its way
                        // out. One no longer open is only a stale id.
                        if was_open {
                            leaving = Some(id);
                            shell.publish(surface_action(
                                crate::ui::surface::action::destroy_popup_animated(id),
                            ));
                        } else {
                            shell.publish(surface_action(
                                crate::ui::surface::action::destroy_popup(id),
                            ));
                        }
                        state.view_cursor = view_cursor;
                    }
                    (false, leaving)
                });
                if let Some(id) = leaving {
                    self.detach(my_state, id);
                }
                my_state
                    .inner
                    .with_data_mut(|state| state.bar_pressed = over_bar);

                // Closing the menu is a side effect: a press elsewhere
                // belongs to what it is over. Captured, it would never reach
                // that widget.
                if over_bar {
                    shell.capture_event();
                }
                if !create_popup {
                    return;
                }
                shell.request_redraw();
                if matches!(windowing_system(), Some(WindowingSystem::Wayland)) {
                    self.create_popup(layout, view_cursor, renderer, shell, viewport, my_state);
                }
            }
            // The press already did the work; its release over the bar ends
            // the click there. A release of a press made elsewhere belongs
            // to the widget that saw that press.
            Mouse(ButtonReleased(Left)) | Touch(FingerLifted { .. } | FingerLost { .. }) => {
                if my_state
                    .inner
                    .with_data_mut(|state| std::mem::take(&mut state.bar_pressed))
                    && view_cursor.is_over(layout.bounds())
                {
                    shell.capture_event();
                }
            }
            Mouse(mouse::Event::CursorMoved { .. } | mouse::Event::CursorEntered)
                if open && view_cursor.is_over(layout.bounds()) =>
            {
                shell.request_redraw();
                shell.capture_event();
                if matches!(windowing_system(), Some(WindowingSystem::Wayland)) {
                    self.create_popup(layout, view_cursor, renderer, shell, viewport, my_state);
                }
            }
            _ => (),
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        view_cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        // The root under the pointer decides, as a row of buttons would
        self.menu_roots
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((root, t), lo)| {
                root.item.mouse_interaction(
                    &t.children[root.index],
                    lo,
                    view_cursor,
                    viewport,
                    renderer,
                )
            })
            .max()
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        view_cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<MenuBarState>();
        let cursor_pos = view_cursor.position().unwrap_or_default();
        state.inner.with_data_mut(|state| {
            let position = if state.open && (cursor_pos.x < 0.0 || cursor_pos.y < 0.0) {
                state.view_cursor
            } else {
                view_cursor
            };

            // draw path highlight
            if self.path_highlight.is_some() {
                let mut is_overlay = true;
                if matches!(windowing_system(), Some(WindowingSystem::Wayland))
                    && self.on_surface_action.is_some()
                    && self.window_id != crate::ui::window::none()
                {
                    is_overlay = true;
                };
                let styling = theme.appearance(&self.style, is_overlay);
                if let Some(active) = state.active_root.first() {
                    let active_bounds = layout
                        .children()
                        .nth(*active)
                        .expect("Active child not found in menu?")
                        .bounds();
                    let path_quad = renderer::Quad {
                        bounds: active_bounds,
                        border: Border {
                            radius: styling.bar_border_radius.to_radius(),
                            ..Default::default()
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    };

                    renderer.fill_quad(path_quad, styling.path);
                }
            }

            self.menu_roots
                .iter()
                .zip(&tree.children)
                .zip(layout.children())
                .for_each(|((root, t), lo)| {
                    root.item.draw(
                        &t.children[root.index],
                        renderer,
                        theme,
                        style,
                        lo,
                        position,
                        viewport,
                    );
                });
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        _renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, crate::ui::Theme, Renderer>> {
        if matches!(windowing_system(), Some(WindowingSystem::Wayland))
            && self.on_surface_action.is_some()
            && self.window_id != crate::ui::window::none()
        {
            return None;
        }

        let state = tree.state.downcast_ref::<MenuBarState>();
        if state.inner.with_data(|state| !state.open) {
            return None;
        }

        Some(
            Menu {
                tree: state.clone(),
                menu_roots: std::borrow::Cow::Owned(self.menu_roots.clone()),
                bounds_expand: self.bounds_expand,
                menu_overlays_parent: false,
                close_condition: self.close_condition,
                item_width: self.item_width,
                item_height: self.item_height,
                bar_bounds: layout.bounds(),
                main_offset: self.main_offset,
                cross_offset: self.cross_offset,
                root_bounds_list: layout.children().map(|lo| lo.bounds()).collect(),
                path_highlight: self.path_highlight,
                style: std::borrow::Cow::Borrowed(&self.style),
                position: Point::new(translation.x, translation.y),
                is_overlay: true,
                window_id: crate::ui::window::none(),
                depth: 0,
                on_surface_action: self.on_surface_action.clone(),
            }
            .overlay(),
        )
    }
}

impl<Message> From<MenuBar<Message>> for Element<'_, Message, crate::ui::Theme, Renderer>
where
    Message: Clone + 'static,
{
    fn from(value: MenuBar<Message>) -> Self {
        Self::new(value)
    }
}

#[allow(unused_results, clippy::too_many_arguments)]
fn process_root_events<Message>(
    menu_roots: &mut [MenuTree<Message>],
    view_cursor: Cursor,
    tree: &mut Tree,
    event: &event::Event,
    layout: Layout<'_>,
    renderer: &Renderer,
    clipboard: &mut dyn Clipboard,
    shell: &mut Shell<'_, Message>,
    viewport: &Rectangle,
) {
    for ((root, t), lo) in menu_roots
        .iter_mut()
        .zip(&mut tree.children)
        .zip(layout.children())
    {
        // assert!(t.tag == tree::Tag::stateless());
        root.item.update(
            &mut t.children[root.index],
            event,
            lo,
            view_cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::{Event, Size, clipboard};

    #[derive(Clone, Debug)]
    enum Msg {
        Surface(crate::ui::surface::Action<Msg>),
    }

    type DynWidget = dyn Widget<Msg, crate::ui::Theme, Renderer>;

    struct Harness {
        bar: MenuBar<Msg>,
        tree: Tree,
        node: Node,
        renderer: Renderer,
    }

    const WINDOW: Size = Size::new(200.0, 200.0);
    const BELOW: Point = Point::new(100.0, 150.0);

    impl Harness {
        fn new() -> Self {
            Self::with_items(1)
        }

        /// A "File" menu of `items` entries.
        fn bar(items: usize) -> MenuBar<Msg> {
            MenuBar::new(vec![MenuTree::with_children(
                crate::ui::Element::from(crate::ui::widget::text("File")),
                (0..items)
                    .map(|i| {
                        MenuTree::new(crate::ui::Element::from(crate::ui::widget::text(format!(
                            "Item {i}"
                        ))))
                    })
                    .collect::<Vec<_>>(),
            )])
            .on_surface_action(Msg::Surface)
        }

        fn with_items(items: usize) -> Self {
            let mut bar = Self::bar(items);
            let renderer = iced_texture_cache::testing::headless_tiny_skia();
            let mut tree = Tree::new(&bar as &DynWidget);
            bar.diff(&mut tree);
            let node = bar.layout(&mut tree, &renderer, &Limits::new(Size::ZERO, WINDOW));
            assert!(!Layout::new(&node).bounds().contains(BELOW));
            Self {
                bar,
                tree,
                node,
                renderer,
            }
        }

        /// The view rebuilt with a menu of `items` entries.
        fn rebuild(&mut self, items: usize) {
            self.bar = Self::bar(items);
            self.bar.diff(&mut self.tree);
            self.node = self.bar.layout(
                &mut self.tree,
                &self.renderer,
                &Limits::new(Size::ZERO, WINDOW),
            );
        }

        fn over_bar(&self) -> Point {
            Layout::new(&self.node).bounds().center()
        }

        fn inner<T>(&self, f: impl FnOnce(&mut MenuBarStateInner) -> T) -> T {
            self.tree
                .state
                .downcast_ref::<MenuBarState>()
                .inner
                .with_data_mut(f)
        }

        fn leaving(&self) -> Option<window::Id> {
            self.inner(|d| d.leaving.get(&crate::ui::window::reserved()).copied())
        }

        /// What a press on the bar does on Wayland, which a test cannot
        /// select: its messages.
        fn create_popup(&mut self) -> Vec<Msg> {
            let at = self.over_bar();
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            let state = self.tree.state.downcast_mut::<MenuBarState>();
            state.inner.with_data_mut(|d| {
                d.view_cursor = Cursor::Available(at);
                d.open = true;
            });
            self.bar.create_popup(
                Layout::new(&self.node),
                Cursor::Available(at),
                &self.renderer,
                &mut shell,
                &Rectangle::with_size(WINDOW),
                state,
            );
            messages
        }

        /// Opens the menu as a Wayland popup: its id.
        fn open_popup(&mut self) -> window::Id {
            self.open_popup_with_view().0
        }

        /// Opens the menu as a Wayland popup: its id and its view.
        fn open_popup_with_view(&mut self) -> (window::Id, PopupView) {
            let messages = self.create_popup();
            let popup = messages
                .into_iter()
                .find_map(|m| match m {
                    Msg::Surface(crate::ui::surface::Action::Popup(settings, view)) => {
                        Some((settings().id, view.expect("a popup has a view")))
                    }
                    _ => None,
                })
                .expect("a popup was requested");
            assert!(self.inner(|d| !d.menu_states.is_empty()));
            popup
        }

        /// Lays out a popup's view, as its surface does on every frame.
        fn lay_out(&self, view: &PopupView) {
            let mut element = view();
            let mut tree = Tree::new(&element);
            let _ = element.as_widget_mut().layout(
                &mut tree,
                &self.renderer,
                &Limits::new(Size::ZERO, WINDOW),
            );
        }

        /// Sends `event` at `at`: its messages, and whether it was captured.
        fn send(&mut self, at: Point, event: mouse::Event) -> (Vec<Msg>, bool) {
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            self.bar.update(
                &mut self.tree,
                &Event::Mouse(event),
                Layout::new(&self.node),
                Cursor::Available(at),
                &self.renderer,
                &mut clipboard::Null,
                &mut shell,
                &Rectangle::with_size(WINDOW),
            );
            let captured = shell.is_event_captured();
            (messages, captured)
        }

        fn press(&mut self, at: Point) -> (Vec<Msg>, bool) {
            self.send(at, mouse::Event::ButtonPressed(mouse::Button::Left))
        }

        fn release(&mut self, at: Point) -> (Vec<Msg>, bool) {
            self.send(at, mouse::Event::ButtonReleased(mouse::Button::Left))
        }
    }

    type PopupView = std::sync::Arc<
        dyn Fn() -> crate::ui::Element<'static, crate::ui::Action<Msg>> + Send + Sync,
    >;

    fn destroys(messages: &[Msg], popup: window::Id) -> Option<bool> {
        messages.iter().find_map(|m| match m {
            Msg::Surface(crate::ui::surface::Action::DestroyPopup { id, animate })
                if *id == popup =>
            {
                Some(*animate)
            }
            _ => None,
        })
    }

    #[test]
    fn a_press_on_the_bar_opens_its_menu() {
        let mut h = Harness::new();
        let (_, captured) = h.press(h.over_bar());
        assert!(captured);
        assert!(h.inner(|d| d.open));

        // The release ends that click; it neither closes nor reopens it.
        let (messages, captured) = h.release(h.over_bar());
        assert!(captured);
        assert!(messages.is_empty());
        assert!(h.inner(|d| d.open));
    }

    #[test]
    fn a_menu_opened_from_the_bar_plays_the_genie() {
        let mut h = Harness::new();
        let popup = h.create_popup().into_iter().find_map(|m| match m {
            Msg::Surface(crate::ui::surface::Action::Popup(settings, _)) => Some(settings()),
            _ => None,
        });
        assert!(popup.expect("a popup was requested").animate);
    }

    #[test]
    fn a_press_on_the_bar_while_open_collapses_the_menu() {
        let mut h = Harness::new();
        let popup = h.open_popup();
        let (messages, captured) = h.press(h.over_bar());
        assert!(captured);
        assert_eq!(destroys(&messages, popup), Some(true));
        assert!(!h.inner(|d| d.open));
        assert_eq!(h.leaving(), Some(popup));
    }

    /// A press away from the bar while its menu is open closes the menu,
    /// but belongs to whatever it is over: a file item pressed there must
    /// see it.
    #[test]
    fn a_press_elsewhere_collapses_the_menu_without_capturing() {
        let mut h = Harness::new();
        let popup = h.open_popup();
        let (messages, captured) = h.press(BELOW);
        assert!(!captured);
        assert_eq!(destroys(&messages, popup), Some(true));
        assert_eq!(h.leaving(), Some(popup));

        // Its release is no business of the bar's
        let (messages, captured) = h.release(BELOW);
        assert!(!captured);
        assert!(messages.is_empty());
    }

    /// A menu collapsing out still renders against the shared tree, so a new
    /// one cannot open beside it: it goes at once instead.
    #[test]
    fn opening_again_removes_a_menu_still_collapsing() {
        let mut h = Harness::new();
        let popup = h.open_popup();
        h.press(BELOW);
        assert_eq!(h.leaving(), Some(popup));

        assert_eq!(destroys(&h.create_popup(), popup), Some(false));
        assert_eq!(h.leaving(), None);
    }

    #[test]
    fn a_collapsing_menu_is_forgotten_once_gone() {
        let mut h = Harness::new();
        let popup = h.open_popup();
        h.press(BELOW);
        assert_eq!(h.leaving(), Some(popup));
        crate::ui::surface::dismissal::note(popup);
        h.send(BELOW, mouse::Event::CursorMoved { position: BELOW });
        assert_eq!(h.leaving(), None);
    }

    /// A release is the bar's only when the press was: one ending a gesture
    /// begun elsewhere belongs to the widget that saw that press.
    #[test]
    fn a_release_over_the_bar_of_a_press_elsewhere_is_not_captured() {
        let mut h = Harness::new();
        let (_, captured) = h.press(BELOW);
        assert!(!captured);
        let (messages, captured) = h.release(h.over_bar());
        assert!(!captured);
        assert!(messages.is_empty());
    }

    /// The menu changes while one is collapsing, then opens again: the new
    /// one is laid out from the new menu, and the collapsing one, still on
    /// screen, from the old.
    #[test]
    fn a_menu_reopened_after_its_contents_changed_mid_collapse_lays_out() {
        let mut h = Harness::with_items(1);
        let (_, old_view) = h.open_popup_with_view();
        h.press(BELOW);
        h.rebuild(2);
        h.lay_out(&old_view);

        let (_, new_view) = h.open_popup_with_view();
        h.lay_out(&new_view);
        h.lay_out(&old_view);
    }
}
