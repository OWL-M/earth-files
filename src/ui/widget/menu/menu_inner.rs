// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0 AND MIT

// From iced_aw, license MIT

//! Vendored from pop-os/libcosmic, src/widget/menu/menu_inner.rs
//!
//! Menu tree overlay
use std::borrow::Cow;
use std::sync::Arc;

use super::aim::{AIM_TIME, ChildOpen, Opening, aims_at, next_selectable};
use super::menu_bar::{MenuBarState, MenuBarStateInner, Pending, PendingKind};
use super::menu_tree::MenuTree;
use super::unfold::{self, FoldKey, Group};
use crate::ui::shell::runner::{WindowingSystem, windowing_system};
use crate::ui::theme::menu_bar::StyleSheet;
use std::time::Instant;

use crate::ui::convert::{ToColor, ToPadding, ToRadius};
use iced::{Alignment, window};
use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::widget::Tree;
use iced_core::{Border, Renderer as IcedRenderer, Shadow, Widget};
use iced_core::{
    Clipboard, Layout, Length, Padding, Point, Rectangle, Shell, Size, Vector, event, overlay,
    renderer, touch,
};

/// The condition of when to close a menu
#[derive(Debug, Clone, Copy)]
pub struct CloseCondition {
    /// Close menus when the cursor moves outside the check bounds
    pub leave: bool,

    /// Close menus when the cursor clicks outside the check bounds
    pub click_outside: bool,

    /// Close menus when the cursor clicks inside the check bounds
    pub click_inside: bool,
}

/// The width of an item
#[derive(Debug, Clone, Copy)]
pub enum ItemWidth {
    /// Use uniform width
    Uniform(u16),
    /// Static tries to use the width value of each menu(menu tree with children),
    /// the widths of items(menu tree with empty children) will be the same as the menu they're in,
    /// if that value is None,
    /// the default value will be used instead,
    /// which is the value of the Static variant
    Static(u16),
}

/// The height of an item
#[derive(Debug, Clone, Copy)]
pub enum ItemHeight {
    /// Use uniform height.
    Uniform(u16),
    /// Static tries to use `MenuTree.height` as item height,
    /// when it's `None` it'll fallback to the value of the `Static` variant.
    Static(u16),
    /// Dynamic tries to automatically choose the proper item height for you,
    /// but it only works in certain cases:
    ///
    /// - Fixed height
    /// - Shrink height
    /// - Menu tree height
    ///
    /// If none of these is the case, it'll fallback to the value of the `Dynamic` variant.
    Dynamic(u16),
}

/// Methods for drawing path highlight
#[derive(Debug, Clone, Copy)]
pub enum PathHighlight {
    /// Draw the full path,
    Full,
    /// Omit the active item(the last item in the path)
    OmitActive,
    /// Omit the active item if it's not a menu
    MenuActive,
}

/// X+ goes right and Y+ goes down
#[derive(Debug, Clone, Copy)]
pub(crate) enum Direction {
    Positive,
    Negative,
}

/// Adaptive open direction
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
struct Aod {
    // whether or not to use aod
    horizontal: bool,
    vertical: bool,

    // whether or not to use overlap
    horizontal_overlap: bool,
    vertical_overlap: bool,

    // default direction
    horizontal_direction: Direction,
    vertical_direction: Direction,

    // Offset of the child in the default direction
    horizontal_offset: f32,
    vertical_offset: f32,
}
impl Aod {
    /// Returns child position and offset position
    #[allow(clippy::too_many_arguments)]
    fn adaptive(
        parent_pos: f32,
        parent_size: f32,
        child_size: f32,
        max_size: f32,
        offset: f32,
        on: bool,
        overlap: bool,
        direction: Direction,
    ) -> (f32, f32) {
        /*
        Imagine there're two sticks, parent and child
        parent: o-----o
        child:  o----------o

        Now we align the child to the parent in one dimension
        There are 4 possibilities:

        1. to the right
                    o-----oo----------o

        2. to the right but allow overlapping
                    o-----o
                    o----------o

        3. to the left
        o----------oo-----o

        4. to the left but allow overlapping
                    o-----o
               o----------o

        The child goes to the default direction by default,
        if the space on the default direction runs out it goes to the the other,
        whether to use overlap is the caller's decision

        This can be applied to any direction
        */

        match direction {
            Direction::Positive => {
                let space_negative = parent_pos;
                let space_positive = max_size - parent_pos - parent_size;

                if overlap {
                    let overshoot = child_size - parent_size;
                    if on && space_negative > space_positive && overshoot > space_positive {
                        (parent_pos - overshoot, parent_pos - overshoot)
                    } else {
                        (parent_pos, parent_pos)
                    }
                } else {
                    let overshoot = child_size + offset;
                    if on && space_negative > space_positive && overshoot > space_positive {
                        (parent_pos - overshoot, parent_pos - offset)
                    } else {
                        (parent_pos + parent_size + offset, parent_pos + parent_size)
                    }
                }
            }
            Direction::Negative => {
                let space_positive = parent_pos;
                let space_negative = max_size - parent_pos - parent_size;

                if overlap {
                    let overshoot = child_size - parent_size;
                    if on && space_negative > space_positive && overshoot > space_positive {
                        (parent_pos, parent_pos)
                    } else {
                        (parent_pos - overshoot, parent_pos - overshoot)
                    }
                } else {
                    let overshoot = child_size + offset;
                    if on && space_negative > space_positive && overshoot > space_positive {
                        (parent_pos + parent_size + offset, parent_pos + parent_size)
                    } else {
                        (parent_pos - overshoot, parent_pos - offset)
                    }
                }
            }
        }
    }

    /// Returns child position and offset position
    fn resolve(
        &self,
        parent_bounds: Rectangle,
        children_size: Size,
        viewport_size: Size,
    ) -> (Point, Point) {
        let (x, ox) = Self::adaptive(
            parent_bounds.x,
            parent_bounds.width,
            children_size.width,
            viewport_size.width,
            self.horizontal_offset,
            self.horizontal,
            self.horizontal_overlap,
            self.horizontal_direction,
        );
        let (y, oy) = Self::adaptive(
            parent_bounds.y,
            parent_bounds.height,
            children_size.height,
            viewport_size.height,
            self.vertical_offset,
            self.vertical,
            self.vertical_overlap,
            self.vertical_direction,
        );

        ([x, y].into(), [ox, oy].into())
    }
}

/// A part of a menu where items are displayed.
///
/// When the bounds of a menu exceed the viewport,
/// only items inside the viewport will be displayed,
/// when scrolling happens, this should be updated
#[derive(Debug, Clone, Copy)]
pub(super) struct MenuSlice {
    pub(super) start_index: usize,
    pub(super) end_index: usize,
}

#[derive(Debug, Clone)]
/// Menu bounds in overlay space
pub struct MenuBounds {
    pub(crate) child_positions: Vec<f32>,
    child_sizes: Vec<Size>,
    pub(crate) children_bounds: Rectangle,
    pub parent_bounds: Rectangle,
    check_bounds: Rectangle,
    offset_bounds: Rectangle,
}
impl MenuBounds {
    #[allow(clippy::too_many_arguments)]
    fn new<Message>(
        menu_tree: &MenuTree<Message>,
        renderer: &crate::ui::Renderer,
        item_width: ItemWidth,
        item_height: ItemHeight,
        viewport_size: Size,
        overlay_offset: Vector,
        aod: &Aod,
        bounds_expand: u16,
        parent_bounds: Rectangle,
        tree: &mut [Tree],
        is_overlay: bool,
    ) -> Self {
        let (children_size, child_positions, child_sizes) =
            get_children_layout(menu_tree, renderer, item_width, item_height, tree, None);

        // viewport space parent bounds
        let view_parent_bounds = parent_bounds + overlay_offset;

        // overlay space children position
        let (children_position, offset_position) = {
            let (cp, op) = aod.resolve(view_parent_bounds, children_size, viewport_size);
            if is_overlay {
                (cp - overlay_offset, op - overlay_offset)
            } else {
                (Point::ORIGIN, op - overlay_offset)
            }
        };

        // calc offset bounds
        let delta = children_position - offset_position;
        let offset_size = if delta.x.abs() > delta.y.abs() {
            Size::new(delta.x, children_size.height)
        } else {
            Size::new(children_size.width, delta.y)
        };
        let offset_bounds = Rectangle::new(offset_position, offset_size);

        let children_bounds = Rectangle::new(children_position, children_size);
        let check_bounds = pad_rectangle(children_bounds, bounds_expand.into());

        Self {
            child_positions,
            child_sizes,
            children_bounds,
            parent_bounds,
            check_bounds,
            offset_bounds,
        }
    }
}

#[derive(Clone)]
pub(crate) struct MenuState {
    /// The index of the active menu item
    pub(crate) index: Option<usize>,
    scroll_offset: f32,
    keyboard_selection: bool,
    pub menu_bounds: MenuBounds,
    /// The items the last layout made nodes for, first and last.
    ///
    /// `draw` pairs items with those nodes, so it has to take the same
    /// range rather than work it out again: it sees the menu where its
    /// parent placed it, which in a popup is inset by the surface's border,
    /// and that inset alone can drop a one-pixel divider at the top.
    laid_out: Option<(usize, usize)>,
    /// The group row whose group is unfolded.
    pub(crate) unfolded: Option<usize>,
    /// Whether that group was unfolded by pointing at its count, and the
    /// pointer has not left the count since: a click there then keeps it
    /// unfolded rather than folding it straight back.
    fresh: bool,
    /// Fixed popup height, including room for the largest group.
    min_height: f32,
    /// Space fitted by the last layout, reused between layout passes.
    fitted_height: f32,
    /// Unused reserved room above the rows, keeping rows below a group still.
    settle: f32,
    /// The group opening or closing, while it moves.
    reveal: Option<unfold::Reveal>,
    /// Room reserved above the rows for the largest group to unfold upward.
    pub(crate) room: f32,
    /// A group asked to open while another one was closing: it opens once
    /// that one has.
    queued: Option<usize>,
    /// Whether `queued` was asked for with the keys, which choose its first
    /// row once it opens, and which moving the pointer does not cancel.
    queued_keys: bool,
}
impl MenuState {
    fn new(menu_bounds: MenuBounds) -> Self {
        Self {
            index: None,
            scroll_offset: 0.0,
            keyboard_selection: false,
            menu_bounds,
            laid_out: None,
            unfolded: None,
            fresh: false,
            min_height: 0.0,
            fitted_height: f32::INFINITY,
            settle: 0.0,
            reveal: None,
            room: 0.0,
            queued: None,
            queued_keys: false,
        }
    }

    /// Unfolds the group of row `unfolded`, or folds every group, at once,
    /// and lays the rows of `menu_tree`, this menu, out again.
    #[allow(clippy::too_many_arguments)]
    fn refold<Message>(
        &mut self,
        menu_tree: &MenuTree<Message>,
        unfolded: Option<usize>,
        renderer: &crate::ui::Renderer,
        item_width: ItemWidth,
        item_height: ItemHeight,
        tree: &mut [Tree],
        bounds_expand: u16,
    ) {
        self.unfolded = unfolded;
        self.reveal = None;
        self.relayout(
            menu_tree,
            renderer,
            item_width,
            item_height,
            tree,
            bounds_expand,
        );
    }

    /// Lays the rows of `menu_tree`, this menu, out as they show now: the
    /// unfolded group's rows, and those of a group opening or closing as
    /// far as it has got, with the rows after it moved up by the rest.
    #[allow(clippy::too_many_arguments)]
    fn relayout<Message>(
        &mut self,
        menu_tree: &MenuTree<Message>,
        renderer: &crate::ui::Renderer,
        item_width: ItemWidth,
        item_height: ItemHeight,
        tree: &mut [Tree],
        bounds_expand: u16,
    ) {
        let laid_open = self.reveal.as_ref().map(|r| r.head).or(self.unfolded);
        let (mut size, mut positions, sizes) = get_children_layout(
            menu_tree,
            renderer,
            item_width,
            item_height,
            tree,
            laid_open,
        );
        let mut shown = 0.0;
        if let Some(reveal) = &self.reveal {
            let hidden = reveal.hidden();
            for position in positions.iter_mut().skip(reveal.head + reveal.rows + 1) {
                *position -= hidden;
            }
            size.height -= hidden;
            shown = reveal.shown();
        } else if let Some(head) = self.unfolded
            && let Group::Head { rows, .. } = menu_tree.children[head].group
        {
            shown = sizes
                .iter()
                .skip(head + 1)
                .take(rows)
                .map(|s| s.height)
                .sum();
        }
        // The rows below a group never move: what the group takes, the rows
        // above it give, by moving up
        let settle = (self.room - shown).max(0.0);
        let origin = self.menu_bounds.children_bounds.y - self.settle;
        self.settle = settle;
        self.menu_bounds.children_bounds.y = origin + settle;
        self.menu_bounds.child_positions = positions;
        self.menu_bounds.child_sizes = sizes;
        self.menu_bounds.children_bounds.width = size.width;
        self.menu_bounds.children_bounds.height = size.height;
        self.menu_bounds.check_bounds =
            pad_rectangle(self.menu_bounds.children_bounds, bounds_expand.into());
        self.fit_viewport(self.fitted_height);
        self.laid_out = None;
        // A chosen row that folded away gives way to its group row
        if let Some(index) = self.index
            && !self.shows(index)
        {
            let groups: Vec<Group> = menu_tree.children.iter().map(|mt| mt.group).collect();
            self.index = unfold::heads(&groups)[index];
        }
    }

    /// Whether row `index` shows: a folded group's rows do not, nor do
    /// those of a group closing.
    fn shows(&self, index: usize) -> bool {
        let closing = self
            .reveal
            .as_ref()
            .is_some_and(|reveal| reveal.folding && reveal.contains(index));
        self.drawn(index) && !closing
    }

    /// Whether row `index` is drawn: it has height, so it shows or is part
    /// of a group opening or closing.
    fn drawn(&self, index: usize) -> bool {
        self.menu_bounds
            .child_sizes
            .get(index)
            .is_some_and(|size| size.height > 0.0)
    }

    /// The rows of a group opening or closing that can be seen, in the
    /// space of `layout`, the menu laid out: the rest of them is cut off.
    fn reveal_clip(&self, layout: Layout<'_>) -> Option<(usize, Rectangle)> {
        let reveal = self.reveal.as_ref()?;
        let menu = layout.bounds();
        Some((
            reveal.head,
            Rectangle {
                x: menu.x,
                y: menu.y + self.menu_bounds.child_positions[reveal.head + 1] + self.scroll_offset,
                width: menu.width,
                height: reveal.shown(),
            },
        ))
    }

    /// Whether row `index` is one of the rows of the group opening or
    /// closing: those are drawn cut to the part that shows.
    fn moving(&self, index: usize) -> bool {
        self.reveal
            .as_ref()
            .is_some_and(|reveal| reveal.contains(index))
    }
    /// Scrolls the menu so row `index` is inside a viewport `height` tall,
    /// as little as it takes; `overlay_offset` takes the menu to the
    /// viewport's space. Whether it moved.
    fn scroll_into_view(&mut self, index: usize, height: f32, overlay_offset: Vector) -> bool {
        let bounds = self.menu_bounds.children_bounds + overlay_offset;
        let top = bounds.y + self.menu_bounds.child_positions[index];
        let bottom = top + self.menu_bounds.child_sizes[index].height;
        let mut offset = self.scroll_offset;
        if bottom + offset > height {
            offset = height - bottom;
        }
        if top + offset < 0.0 {
            offset = -top;
        }
        // The same bounds the wheel keeps to
        let max_offset = (0.0 - bounds.y).max(0.0);
        let min_offset = (height - (bounds.y + bounds.height)).min(0.0);
        let offset = offset.clamp(min_offset, max_offset);
        let moved = offset != self.scroll_offset;
        self.scroll_offset = offset;
        moved
    }

    /// Give rows priority over the reserved unfolding space when constrained.
    fn fit_viewport(&mut self, height: f32) {
        self.fitted_height = height;
        if self.min_height == 0.0 {
            return;
        }
        let settle = (self.min_height.min(height.max(0.0))
            - self.menu_bounds.children_bounds.height)
            .max(0.0);
        let delta = settle - self.settle;
        self.menu_bounds.children_bounds.y += delta;
        self.menu_bounds.check_bounds.y += delta;
        self.settle = settle;
    }

    /// The bounds of row `index`, in overlay space.
    fn row_bounds(&self, index: usize) -> Rectangle {
        Rectangle::new(
            Point::new(
                0.0,
                self.menu_bounds.child_positions[index] + self.scroll_offset,
            ),
            self.menu_bounds.child_sizes[index],
        ) + (self.menu_bounds.children_bounds.position() - Point::ORIGIN)
    }

    /// Visible geometry shared by pointer targeting and viewport slicing.
    fn visible_row_bounds(&self, index: usize) -> Option<Rectangle> {
        if !self.drawn(index) {
            return None;
        }
        let row = self.row_bounds(index);
        if let Some(reveal) = &self.reveal
            && reveal.contains(index)
        {
            let clip = Rectangle {
                y: self.row_bounds(reveal.head + 1).y,
                height: reveal.shown(),
                ..self.menu_bounds.children_bounds
            };
            return row.intersection(&clip).filter(|r| r.height > 0.0);
        }
        Some(row)
    }

    fn row_at(&self, cursor: Point) -> Option<usize> {
        (0..self.menu_bounds.child_sizes.len()).find(|&i| {
            self.shows(i)
                && self.visible_row_bounds(i).is_some_and(|row| {
                    cursor.x >= row.x
                        && cursor.x < row.x + row.width
                        && cursor.y >= row.y
                        && cursor.y < row.y + row.height
                })
        })
    }

    pub(super) fn layout<Message>(
        &mut self,
        overlay_offset: Vector,
        slice: MenuSlice,
        renderer: &crate::ui::Renderer,
        menu_tree: &[MenuTree<Message>],
        tree: &mut [Tree],
    ) -> Node {
        let MenuSlice {
            start_index,
            end_index,
        } = slice;

        debug_assert_eq!(menu_tree.len(), self.menu_bounds.child_positions.len());
        self.laid_out = Some((start_index, end_index));

        // viewport space children bounds
        let children_bounds = self.menu_bounds.children_bounds + overlay_offset;
        let child_nodes = self.menu_bounds.child_positions[start_index..=end_index]
            .iter_mut()
            .zip(self.menu_bounds.child_sizes[start_index..=end_index].iter_mut())
            .zip(menu_tree[start_index..=end_index].iter())
            .map(|((cp, size), mt)| {
                // Keep natural row geometry. Clipping belongs to drawing, otherwise
                // a partially visible row re-centres its text as it is revealed.
                let position = *cp;
                let size = *size;

                let limits = Limits::new(size, size);

                mt.item
                    .element
                    .with_data_mut(|e| {
                        e.as_widget_mut()
                            .layout(&mut tree[mt.index], renderer, &limits)
                    })
                    .move_to(Point::new(0.0, position + self.scroll_offset))
            })
            .collect::<Vec<_>>();

        Node::with_children(children_bounds.size(), child_nodes).move_to(children_bounds.position())
    }

    fn layout_single<Message>(
        &self,
        overlay_offset: Vector,
        index: usize,
        renderer: &crate::ui::Renderer,
        menu_tree: &mut MenuTree<Message>,
        tree: &mut Tree,
    ) -> Node {
        // viewport space children bounds
        let children_bounds = self.menu_bounds.children_bounds + overlay_offset;

        let position = self.menu_bounds.child_positions[index];
        let limits = Limits::new(Size::ZERO, self.menu_bounds.child_sizes[index]);
        let parent_offset = children_bounds.position() - Point::ORIGIN;
        let node = menu_tree.item.layout(tree, renderer, &limits);
        node.move_to(Point::new(
            parent_offset.x,
            parent_offset.y + position + self.scroll_offset,
        ))
    }

    /// returns a slice of the menu items that are inside the viewport
    pub(super) fn slice(
        &self,
        viewport_size: Size,
        overlay_offset: Vector,
        _item_height: ItemHeight,
    ) -> MenuSlice {
        let max_index = self.menu_bounds.child_positions.len().saturating_sub(1);

        // Reveal members keep their natural positions while the following
        // rows move upward, so positions are not sorted during animation.
        let mut visible = (0..=max_index).filter(|&i| {
            self.visible_row_bounds(i).is_some_and(|row| {
                let row = row + overlay_offset;
                row.y < viewport_size.height && row.y + row.height > 0.0
            })
        });
        let start_index = visible.next().unwrap_or(0);
        let end_index = visible.next_back().unwrap_or(start_index);

        MenuSlice {
            start_index,
            end_index,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Menu<'b, Message: std::clone::Clone> {
    pub(crate) tree: MenuBarState,
    // Flattened menu tree
    pub(crate) menu_roots: Cow<'b, [MenuTree<Message>]>,
    pub(crate) bounds_expand: u16,
    /// Allows menu overlay items to overlap the parent
    pub(crate) menu_overlays_parent: bool,
    pub(crate) close_condition: CloseCondition,
    pub(crate) item_width: ItemWidth,
    pub(crate) item_height: ItemHeight,
    pub(crate) bar_bounds: Rectangle,
    pub(crate) main_offset: i32,
    pub(crate) cross_offset: i32,
    pub(crate) root_bounds_list: Vec<Rectangle>,
    pub(crate) path_highlight: Option<PathHighlight>,
    pub(crate) style: Cow<'b, <crate::ui::Theme as StyleSheet>::Style>,
    pub(crate) position: Point,
    pub(crate) is_overlay: bool,
    /// window id for this popup
    pub(crate) window_id: window::Id,
    pub(crate) depth: usize,
    pub(crate) on_surface_action:
        Option<Arc<dyn Fn(crate::ui::surface::Action<Message>) -> Message + Send + Sync + 'static>>,
}
impl<'b, Message: Clone + 'static> Menu<'b, Message> {
    pub(crate) fn overlay(
        self,
    ) -> overlay::Element<'b, Message, crate::ui::Theme, crate::ui::Renderer> {
        overlay::Element::new(Box::new(self))
    }

    pub(crate) fn layout(&self, renderer: &crate::ui::Renderer, limits: Limits) -> Node {
        // layout children;
        let position = self.position;
        let mut intrinsic_size = Size::ZERO;

        let empty = Vec::new();
        self.tree.inner.with_data_mut(|data| {
            if data.active_root.len() < self.depth + 1 || data.menu_states.len() < self.depth + 1 {
                return Node::new(limits.min());
            }

            let overlay_offset = Point::ORIGIN - position;
            let tree_children: &mut Vec<Tree> = &mut data.tree.children;

            let children = (if self.is_overlay { 0 } else { self.depth }..=if self.is_overlay {
                data.active_root.len() - 1
            } else {
                self.depth
            })
                .map(|active_root| {
                    if self.menu_roots.is_empty() {
                        return (&empty, vec![]);
                    }
                    let (active_tree, roots) =
                        data.active_root[..=active_root].iter().skip(1).fold(
                            (
                                &mut tree_children[data.active_root[0]].children,
                                &self.menu_roots[data.active_root[0]].children,
                            ),
                            |(tree, mt), next_active_root| (tree, &mt[*next_active_root].children),
                        );

                    data.menu_states[if self.is_overlay { 0 } else { self.depth }
                        ..=if self.is_overlay {
                            data.active_root.len() - 1
                        } else {
                            self.depth
                        }]
                        .iter_mut()
                        .enumerate()
                        .filter(|ms| self.is_overlay || ms.0 < 1)
                        .fold(
                            (roots, Vec::new()),
                            |(menu_root, mut nodes), (_i, ms)| {
                                if !self.is_overlay {
                                    ms.fit_viewport(limits.max().height - 1.0);
                                }
                                let slice =
                                    ms.slice(limits.max(), overlay_offset, self.item_height);
                                let _start_index = slice.start_index;
                                let _end_index = slice.end_index;
                                let children_node = ms.layout(
                                    overlay_offset,
                                    slice,
                                    renderer,
                                    menu_root,
                                    active_tree,
                                );
                                let node_size = children_node.size();
                                intrinsic_size.height +=
                                    (ms.settle + node_size.height).max(ms.min_height);

                                intrinsic_size.width = intrinsic_size.width.max(node_size.width);

                                nodes.push(children_node);
                                // if popup just use len 1?
                                // only the last menu can have a None active index
                                (
                                    ms.index
                                        .map_or(menu_root, |active| &menu_root[active].children),
                                    nodes,
                                )
                            },
                        )
                })
                .map(|(_, l)| l)
                .next()
                .unwrap_or_default();

            // overlay space viewport rectangle
            Node::with_children(
                limits.resolve(Length::Shrink, Length::Shrink, intrinsic_size),
                children,
            )
            .translate(Point::ORIGIN - position)
        })
    }

    #[allow(clippy::too_many_lines)]
    fn update(
        &mut self,
        event: &event::Event,
        layout: Layout<'_>,
        view_cursor: Cursor,
        renderer: &crate::ui::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) -> Option<(usize, MenuState)> {
        use event::Event::{Mouse, Touch};
        use mouse::Button::Left;
        use mouse::Event::{ButtonPressed, ButtonReleased, CursorMoved, WheelScrolled};
        use touch::Event::{FingerLifted, FingerMoved, FingerPressed};

        if !self
            .tree
            .inner
            .with_data(|data| data.open || data.active_root.len() <= self.depth)
        {
            return None;
        }

        let viewport = layout.bounds();

        let viewport_size = viewport.size();
        let overlay_offset = Point::ORIGIN - viewport.position();
        let overlay_cursor = view_cursor.position().unwrap_or_default() - overlay_offset;
        let menu_roots = match &mut self.menu_roots {
            Cow::Borrowed(_) => panic!(),
            Cow::Owned(o) => o.as_mut_slice(),
        };
        // A press is for the row under the pointer, so that row is chosen
        // first: during the wait for the pointer to reach an open child the
        // row chosen is still the old one, and a press on a split row's
        // arrow opens its variants at once
        let mut pressed_root = None;
        if !self.is_overlay
            && matches!(
                event,
                Mouse(ButtonPressed(Left)) | Touch(FingerPressed { .. })
            )
            && view_cursor.is_over(viewport)
        {
            // In a shell of its own: choosing the row takes the event, and a
            // button ignores a press that is taken, so the row would never
            // run. What it asks for besides is passed on.
            let mut messages = Vec::new();
            let mut own = Shell::new(&mut messages);
            pressed_root = process_overlay_events(
                self,
                renderer,
                viewport_size,
                overlay_offset,
                view_cursor,
                overlay_cursor,
                self.cross_offset as f32,
                &mut own,
                false,
            );
            let (layout_invalid, widgets_invalid, redraw) = (
                own.is_layout_invalid(),
                own.are_widgets_invalid(),
                own.redraw_request(),
            );
            for message in messages {
                shell.publish(message);
            }
            if layout_invalid {
                shell.invalidate_layout();
            }
            if widgets_invalid {
                shell.invalidate_widgets();
            }
            shell.request_redraw_at(redraw);
        }
        process_menu_events(
            self,
            event,
            view_cursor,
            renderer,
            clipboard,
            shell,
            overlay_offset,
        );

        init_root_menu(
            self,
            renderer,
            shell,
            overlay_cursor,
            viewport_size,
            overlay_offset,
            self.bar_bounds,
            self.main_offset as f32,
        );

        match event {
            // The pointer left the menu: its unfolded group folds
            Mouse(mouse::Event::CursorLeft) => {
                let _ = fold_all(self, renderer, shell);
            }

            event::Event::Window(window::Event::RedrawRequested(now)) => {
                let moving = self.tree.inner.with_data(|state| {
                    state
                        .menu_states
                        .get(self.depth)
                        .is_some_and(|ms| ms.reveal.is_some())
                });
                advance_reveal(self, *now, renderer, shell);
                if moving {
                    self.tree.inner.with_data_mut(|state| {
                        let Some(ms) = state.menu_states.get_mut(self.depth) else {
                            return;
                        };
                        if ms.keyboard_selection {
                            if ms.reveal.is_none()
                                && let Some(index) = ms.index
                                && ms.scroll_into_view(index, viewport_size.height, overlay_offset)
                            {
                                shell.invalidate_layout();
                            }
                        } else if !state.pressed {
                            let index = view_cursor
                                .position()
                                .and_then(|point| ms.row_at(point - overlay_offset));
                            if ms.index != index {
                                ms.index = index;
                                state.pending = None;
                                shell.request_redraw();
                            }
                        }
                    });
                }
                return process_pending(self, *now, renderer, viewport_size, overlay_offset, shell);
            }

            event::Event::Keyboard(iced_core::keyboard::Event::KeyPressed { key, .. }) => {
                return process_key(self, key, renderer, viewport_size, overlay_offset, shell);
            }

            Mouse(WheelScrolled { delta }) => process_scroll_events(
                self,
                shell,
                *delta,
                overlay_cursor,
                viewport_size,
                overlay_offset,
            ),

            Mouse(ButtonPressed(Left)) | Touch(FingerPressed { .. }) => {
                let group_press = press_group(self, overlay_cursor, renderer, shell);
                self.tree.inner.with_data_mut(|data| {
                    data.group_press = group_press;
                    data.pressed = true;
                    data.view_cursor = view_cursor;
                });
                return pressed_root;
            }

            Mouse(CursorMoved { position }) | Touch(FingerMoved { position, .. }) => {
                self.tree.inner.with_data_mut(|state| {
                    if let Some(ms) = state.menu_states.get_mut(self.depth) {
                        ms.keyboard_selection = false;
                    }
                });
                let view_cursor = Cursor::Available(*position);
                let overlay_cursor = view_cursor.position().unwrap_or_default() - overlay_offset;
                if !self.is_overlay && !view_cursor.is_over(viewport) {
                    return None;
                }
                let new_root = process_overlay_events(
                    self,
                    renderer,
                    viewport_size,
                    overlay_offset,
                    view_cursor,
                    overlay_cursor,
                    self.cross_offset as f32,
                    shell,
                    true,
                );

                if view_cursor.is_over(viewport) {
                    shell.request_redraw();
                }

                if self.is_overlay && view_cursor.is_over(viewport) {
                    shell.capture_event();
                }

                return new_root;
            }

            Mouse(ButtonReleased(_)) | Touch(FingerLifted { .. }) => {
                self.tree.inner.with_data_mut(|state| {
                    state.pressed = false;

                    if matches!(event, Mouse(ButtonReleased(mouse::Button::Right)))
                        && std::mem::take(&mut state.opening_press_held)
                    {
                        return;
                    }
                    if let Touch(FingerLifted { id, .. }) = event
                        && state.opening_fingers.contains(id)
                    {
                        return;
                    }

                    // The release of a press for a group is the group's
                    if std::mem::take(&mut state.group_press) {
                        return;
                    }

                    // A reason only shows something: a click on it does not
                    // close the menu
                    let depth_index = if self.is_overlay {
                        state.menu_states.len().saturating_sub(1)
                    } else {
                        self.depth
                    };
                    if state.active_root.len() > depth_index
                        && let Some(ms) = state.menu_states.get(depth_index)
                        && let Some(index) = ms.index
                    {
                        let rows = rows_at(&self.menu_roots, &state.active_root, depth_index);
                        if rows.iter().all(|row| row.passive) {
                            return;
                        }
                    }

                    // Reserved transparent space is outside the visible menu:
                    // dismiss it just like an outside click.
                    if !self.is_overlay
                        && view_cursor.is_over(viewport)
                        && state.menu_states.get(self.depth).is_some_and(|ms| {
                            let rows = ms.menu_bounds.children_bounds;
                            let top = rows.y + ms.scroll_offset;
                            overlay_cursor.y < top || overlay_cursor.y >= top + rows.height
                        })
                    {
                        close_all(self, state, shell);
                        return;
                    }

                    // process close condition
                    if state.open
                        && state
                            .view_cursor
                            .position()
                            .unwrap_or_default()
                            .distance(view_cursor.position().unwrap_or_default())
                            < 2.0
                    {
                        let is_inside = state.menu_states[..=if self.is_overlay {
                            state.active_root.len().saturating_sub(1)
                        } else {
                            self.depth
                        }]
                            .iter()
                            .any(|ms| ms.menu_bounds.check_bounds.contains(overlay_cursor));
                        let mut needs_reset = false;
                        needs_reset |= self.close_condition.click_inside
                            && is_inside
                            && matches!(
                                event,
                                Mouse(ButtonReleased(Left)) | Touch(FingerLifted { .. })
                            );

                        needs_reset |= self.close_condition.click_outside && !is_inside;

                        if needs_reset {
                            if matches!(windowing_system(), Some(WindowingSystem::Wayland))
                                && let Some(handler) = self.on_surface_action.as_ref()
                            {
                                let mut root = self.window_id;
                                let mut depth = self.depth;
                                while let Some(parent) =
                                    state.popup_id.iter().find(|(_, v)| **v == root)
                                {
                                    // parent of root popup is the window, so we stop.
                                    if depth == 0 {
                                        break;
                                    }
                                    root = *parent.0;
                                    depth = depth.saturating_sub(1);
                                }
                                shell.publish((handler)(
                                    crate::ui::surface::Action::DestroyPopup {
                                        id: root,
                                        animate: false,
                                    },
                                ));
                            }

                            state.reset();
                        }
                    }

                    // close all menus when clicking inside the menu bar
                    if self.bar_bounds.contains(overlay_cursor) {
                        state.reset();
                    }
                });
            }

            _ => {}
        }
        None
    }

    #[allow(unused_results, clippy::too_many_lines)]
    fn draw(
        &self,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        view_cursor: Cursor,
    ) {
        self.tree.inner.with_data(|state| {
            if !state.open || state.active_root.len() <= self.depth {
                return;
            }
            let active_root = &state.active_root[..=if self.is_overlay { 0 } else { self.depth }];
            let viewport = layout.bounds();
            let viewport_size = viewport.size();
            let overlay_offset = Point::ORIGIN - viewport.position();

            let render_bounds = if self.is_overlay {
                Rectangle::new(Point::ORIGIN, viewport.size())
            } else {
                Rectangle::new(Point::ORIGIN, Size::INFINITE)
            };

            let styling = theme.appearance(&self.style, self.is_overlay);
            let roots = active_root.iter().skip(1).fold(
                &self.menu_roots[active_root[0]].children,
                |mt, next_active_root| &mt[*next_active_root].children,
            );
            let indices = state.get_trimmed_indices(self.depth).collect::<Vec<_>>();
            state.menu_states[if self.is_overlay { 0 } else { self.depth }..=if self.is_overlay {
                state.menu_states.len() - 1
            } else {
                self.depth
            }]
                .iter()
                .zip(layout.children())
                .enumerate()
                .filter(|ms: &(usize, (&MenuState, Layout<'_>))| self.is_overlay || ms.0 < 1)
                .fold(
                    roots,
                    |menu_roots: &Vec<MenuTree<Message>>, (i, (ms, children_layout))| {
                        let draw_path = self.path_highlight.as_ref().is_some_and(|ph| match ph {
                            PathHighlight::Full => true,
                            PathHighlight::OmitActive => {
                                !indices.is_empty() && i < indices.len() - 1
                            }
                            PathHighlight::MenuActive => {
                                !indices.is_empty()
                                    && i < indices.len()
                                    && menu_roots.len() > indices[i]
                                    && (i < indices.len() - 1
                                        || !menu_roots[indices[i]].children.is_empty())
                            }
                        });

                        // react only to the last menu; a popup draws only its own
                        // menu, which the pointer is over even with a child open
                        let view_cursor = if !self.is_overlay
                            || self.depth == state.active_root.len() - 1
                            || i == state.menu_states.len() - 1
                        {
                            view_cursor
                        } else {
                            Cursor::Available([-1.0; 2].into())
                        };

                        let draw_menu = |r: &mut crate::ui::Renderer| {
                            // calc slice
                            let (start_index, end_index) = ms.laid_out.unwrap_or_else(|| {
                                let slice =
                                    ms.slice(viewport_size, overlay_offset, self.item_height);
                                (slice.start_index, slice.end_index)
                            });

                            let children_bounds = children_layout.bounds();
                            // The rows that show: the first and last of them
                            // take the menu's corners
                            let shown = |i: usize| ms.drawn(i);
                            // The first and last rows that show take the
                            // corners, not those of a group closing
                            let cornered = |i: usize| ms.shows(i);
                            let clip = ms.reveal_clip(children_layout);
                            let first_shown = (start_index..=end_index)
                                .find(|i| cornered(*i))
                                .unwrap_or(start_index);
                            let last_shown = (start_index..=end_index)
                                .rev()
                                .find(|i| cornered(*i))
                                .unwrap_or(end_index);

                            // draw menu background
                            // let bounds = pad_rectangle(children_bounds, styling.background_expand.into());
                            // println!("cursor: {:?}", view_cursor);
                            // println!("bg_bounds: {:?}", bounds);
                            // println!("color: {:?}\n", styling.background);
                            let menu_quad = renderer::Quad {
                                bounds: pad_rectangle(
                                    children_bounds.intersection(&viewport).unwrap_or_default(),
                                    styling.background_expand.to_padding(),
                                ),
                                border: Border {
                                    radius: styling.menu_border_radius.to_radius(),
                                    width: styling.border_width,
                                    color: styling.border_color,
                                },
                                shadow: Shadow::default(),
                                snap: true,
                            };
                            let menu_color = styling.background;
                            r.fill_quad(
                                renderer::Quad {
                                    border: Border {
                                        width: 0.0,
                                        ..menu_quad.border
                                    },
                                    ..menu_quad
                                },
                                menu_color,
                            );
                            // draw path highlight
                            if let (true, Some(active)) = (draw_path, ms.index)
                                && (start_index..=end_index).contains(&active)
                                && let Some(active_layout) = children_layout
                                    .children()
                                    .nth(active.saturating_sub(start_index))
                            {
                                let mut rad = styling.menu_border_radius;
                                let rad_0 = theme.cosmic().radius_0();
                                if first_shown != last_shown {
                                    if active == first_shown {
                                        rad[2] = rad_0[0];
                                        rad[3] = rad_0[1];
                                    } else if active == last_shown {
                                        rad[0] = rad_0[2];
                                        rad[1] = rad_0[3];
                                    } else {
                                        rad = rad_0;
                                    }
                                }
                                let path_quad = renderer::Quad {
                                    bounds: active_layout
                                        .bounds()
                                        .intersection(&viewport)
                                        .unwrap_or_default(),
                                    border: Border {
                                        radius: rad.to_radius(),
                                        ..Default::default()
                                    },
                                    shadow: Shadow::default(),
                                    snap: true,
                                };

                                let path_clip = if ms.moving(active) {
                                    clip.and_then(|(_, clip)| path_quad.bounds.intersection(&clip))
                                } else {
                                    Some(path_quad.bounds)
                                };
                                if let Some(path_clip) = path_clip.filter(|r| r.height > 0.0) {
                                    r.with_layer(path_clip, |r| {
                                        r.fill_quad(path_quad, styling.path)
                                    });
                                }
                            }
                            if start_index < menu_roots.len() {
                                // draw item
                                menu_roots[start_index..=end_index]
                                    .iter()
                                    .enumerate()
                                    .zip(children_layout.children())
                                    .for_each(|((i, mt), clo)| {
                                        let row = start_index + i;
                                        // A folded group's rows are not drawn
                                        if !shown(row) {
                                            return;
                                        }
                                        let mut t = theme.with_list_item_position(
                                            if first_shown == last_shown {
                                                Some((Alignment::Center, i))
                                            } else if row == first_shown {
                                                Some((Alignment::Start, i))
                                            } else if row == last_shown {
                                                Some((Alignment::End, i))
                                            } else {
                                                None
                                            },
                                        );
                                        t.menu_bounds = children_bounds.intersection(&viewport);
                                        // An unfolded group's row stays lit
                                        let bounds = clo.bounds();
                                        let view_cursor = if ms.unfolded == Some(row) {
                                            Cursor::Available(Point::new(
                                                bounds.x + 8.0,
                                                bounds.center_y(),
                                            ))
                                        } else {
                                            view_cursor
                                        };

                                        let mut row_clip = bounds.intersection(&viewport);
                                        if ms.moving(row) {
                                            row_clip = row_clip.and_then(|bounds| {
                                                clip.and_then(|(_, clip)| {
                                                    bounds.intersection(&clip)
                                                })
                                            });
                                        }
                                        let Some(row_clip) = row_clip.filter(|r| r.height > 0.0)
                                        else {
                                            return;
                                        };
                                        // Hidden portions of a member cannot make its
                                        // visible fragment hover when the pointer is on
                                        // the following row.
                                        let view_cursor = if view_cursor.is_over(row_clip) {
                                            view_cursor
                                        } else {
                                            Cursor::Unavailable
                                        };
                                        r.with_layer(row_clip, |r| {
                                            mt.item.draw(
                                                &state.tree.children[active_root[0]].children
                                                    [mt.index],
                                                r,
                                                &t,
                                                style,
                                                clo,
                                                view_cursor,
                                                &row_clip,
                                            );
                                        });
                                    });
                                // A later layer keeps badges above the row layers.
                                r.with_layer(viewport, |r| {
                                    draw_groups(
                                        r,
                                        theme,
                                        menu_roots,
                                        ms,
                                        (start_index, end_index),
                                        children_layout,
                                        view_cursor,
                                        viewport,
                                        clip.map(|(_, clip)| clip),
                                    )
                                });
                            }
                            // The outline belongs above all row layers.
                            r.with_layer(render_bounds, |r| {
                                r.fill_quad(menu_quad, iced_core::Color::TRANSPARENT);
                            });
                        };

                        renderer.with_layer(render_bounds, draw_menu);

                        // only the last menu can have a None active index
                        ms.index
                            .map_or(menu_roots, |active| &menu_roots[active].children)
                    },
                );
        });
    }
}
impl<Message: Clone + 'static> overlay::Overlay<Message, crate::ui::Theme, crate::ui::Renderer>
    for Menu<'_, Message>
{
    fn layout(&mut self, renderer: &crate::ui::Renderer, bounds: Size) -> iced_core::layout::Node {
        Menu::layout(
            self,
            renderer,
            Limits::NONE
                .min_width(bounds.width)
                .max_width(bounds.width)
                .min_height(bounds.height)
                .max_height(bounds.height),
        )
    }

    fn update(
        &mut self,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &crate::ui::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        self.update(event, layout, cursor, renderer, clipboard, shell);
    }

    fn draw(
        &self,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.draw(renderer, theme, style, layout, cursor);
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &crate::ui::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Idle
        } else {
            mouse::Interaction::None
        }
    }
}

impl<Message: std::clone::Clone + 'static> Widget<Message, crate::ui::Theme, crate::ui::Renderer>
    for Menu<'_, Message>
{
    fn size(&self) -> Size<Length> {
        Size {
            width: Length::Shrink,
            height: Length::Shrink,
        }
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        renderer: &crate::ui::Renderer,
        limits: &iced_core::layout::Limits,
    ) -> iced_core::layout::Node {
        Menu::layout(self, renderer, *limits)
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        Menu::draw(self, renderer, theme, style, layout, cursor);
    }

    #[allow(clippy::too_many_lines)]
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &crate::ui::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let new_root = self.update(event, layout, cursor, renderer, clipboard, shell);

        if matches!(windowing_system(), Some(WindowingSystem::Wayland))
            && let Some((new_root, new_ms)) = new_root
        {
            use crate::ui::surface::{PopupSettings, Positioner};

            let overlay_offset = Point::ORIGIN - viewport.position();

            let overlay_cursor = cursor.position().unwrap_or_default() - overlay_offset;

            let Some((mut menu, popup_id)) = self.tree.inner.with_data_mut(|state| {
                let popup_id = *state
                    .popup_id
                    .entry(self.window_id)
                    .or_insert_with(window::Id::unique);
                let active_roots = state
                    .active_root
                    .get(self.depth)
                    .cloned()
                    .unwrap_or_default();

                let root_bounds_list = layout
                    .children()
                    .next()
                    .unwrap()
                    .children()
                    .map(|lo| lo.bounds())
                    .collect();

                let mut popup_menu = Menu {
                    tree: self.tree.clone(),
                    menu_roots: Cow::Owned(Cow::into_owned(self.menu_roots.clone())),
                    bounds_expand: self.bounds_expand,
                    menu_overlays_parent: false,
                    close_condition: self.close_condition,
                    item_width: self.item_width,
                    item_height: self.item_height,
                    bar_bounds: layout.bounds(),
                    main_offset: self.main_offset,
                    cross_offset: self.cross_offset,
                    root_bounds_list,
                    path_highlight: self.path_highlight,
                    style: Cow::Owned(Cow::into_owned(self.style.clone())),
                    position: Point::new(0., 0.),
                    is_overlay: false,
                    window_id: popup_id,
                    depth: self.depth + 1,
                    on_surface_action: self.on_surface_action.clone(),
                };

                state.active_root.push(new_root);

                Some((popup_menu, popup_id))
            }) else {
                return;
            };
            // XXX we push a new active root manually instead
            init_root_popup_menu(
                &mut menu,
                renderer,
                shell,
                cursor.position().unwrap_or_default(),
                layout.bounds().size(),
                Vector::new(0., 0.),
                layout.bounds(),
                self.main_offset as f32,
            );
            let (anchor_rect, gravity) = self.tree.inner.with_data_mut(|state| {
                (
                    state
                        .menu_states
                        .get(self.depth + 1)
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

            let menu_node = Widget::layout(
                &mut menu,
                &mut Tree::empty(),
                renderer,
                &Limits::NONE.min_width(1.).min_height(1.),
            );

            let popup_size = menu_node.size();
            let mut positioner = Positioner {
                size: Some((
                    popup_size.width.ceil() as u32 + 2,
                    popup_size.height.ceil() as u32 + 2,
                )),
                anchor_rect,
                anchor: crate::ui::surface::PopupAnchor::TopRight,
                gravity,
                ..Default::default()
            };
            // Disable slide_x if it is set in the default.
            positioner
                .constraint_adjustment
                .remove(crate::ui::surface::PopupConstraintAdjustment::SlideX);
            let parent = self.window_id;

            shell.publish((self.on_surface_action.as_ref().unwrap())(
                crate::ui::surface::action::simple_popup(
                    move || PopupSettings {
                        parent,
                        id: popup_id,
                        positioner,
                        animate: false,
                    },
                    Some(move || {
                        (crate::ui::Element::from(
                            crate::ui::widget::container(menu.clone()).center(Length::Fill),
                        ))
                        .map(crate::ui::action::app)
                    }),
                ),
            ));
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &crate::ui::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Idle
        } else {
            mouse::Interaction::None
        }
    }
}

impl<'a, Message> From<Menu<'a, Message>>
    for iced::Element<'a, Message, crate::ui::Theme, crate::ui::Renderer>
where
    Message: std::clone::Clone + 'static,
{
    fn from(value: Menu<'a, Message>) -> Self {
        Self::new(value)
    }
}

/// Draws what the menu adds to its group rows: each one's count, and the
/// guide line beside an unfolded group's rows.
#[allow(clippy::too_many_arguments)]
fn draw_groups<Message>(
    renderer: &mut crate::ui::Renderer,
    theme: &crate::ui::Theme,
    rows: &[MenuTree<Message>],
    ms: &MenuState,
    (start_index, end_index): (usize, usize),
    layout: Layout<'_>,
    cursor: Cursor,
    viewport: Rectangle,
    clip: Option<Rectangle>,
) {
    use iced_core::text::{self, Renderer as _};

    let cosmic = theme.cosmic();
    let bounds_of = |row: usize| {
        (start_index..=end_index)
            .contains(&row)
            .then(|| layout.children().nth(row - start_index))
            .flatten()
            .map(|l| l.bounds())
    };
    let quad = |bounds: Rectangle, radius: f32| renderer::Quad {
        bounds,
        border: Border {
            radius: radius.into(),
            ..Default::default()
        },
        shadow: Shadow::default(),
        snap: true,
    };

    let open = ms.reveal.as_ref().map(|reveal| reveal.head).or(ms.unfolded);
    if let Some(head) = open
        && let Some(Group::Head { rows: members, .. }) = rows.get(head).map(|row| row.group)
        && let (Some(first), Some(last)) = (bounds_of(head + 1), bounds_of(head + members))
    {
        let mut line = Rectangle {
            x: layout.bounds().x + unfold::GUIDE_X,
            y: first.y + 4.0,
            width: 1.0,
            height: (last.y + last.height - 4.0) - (first.y + 4.0),
        };
        // As far as the rows show
        if let Some(clip) = clip {
            line.height = line
                .height
                .min(clip.y + clip.height - 4.0 - line.y)
                .max(0.0);
        }
        renderer.fill_quad(
            quad(line, 0.0),
            cosmic.background(theme.transparent).divider.to_color(),
        );
    }

    for row in start_index..=end_index {
        let Some(Group::Head { count, .. }) = rows.get(row).map(|row| row.group) else {
            continue;
        };
        if !ms.shows(row) {
            continue;
        }
        let Some(bounds) = bounds_of(row) else {
            continue;
        };
        let unfolded = ms.unfolded == Some(row);
        let chip = unfold::chip(bounds, count, unfolded);
        let hot = unfolded || cursor.position().is_some_and(|at| chip.contains(at));
        let label = unfold::chip_label(count, unfolded);
        let font = crate::ui::font::default();
        let (fill, ink) = if hot {
            let mut fill = cosmic.accent_color();
            fill.alpha = 0.15;
            (fill, cosmic.accent_text_color())
        } else {
            let mut fill = cosmic.control_5();
            fill.alpha = 0.2;
            let mut ink = cosmic.background(theme.transparent).component.on;
            ink.alpha *= 0.75;
            (fill, ink)
        };
        renderer.fill_quad(quad(chip, unfold::CHIP_HEIGHT / 2.0), fill.to_color());
        renderer.fill_text(
            text::Text {
                content: label,
                bounds: chip.size(),
                size: iced::Pixels(unfold::CHIP_TEXT),
                line_height: text::LineHeight::default(),
                font,
                align_x: text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::None,
            },
            chip.center(),
            ink.to_color(),
            viewport,
        );
    }
}

/// Where `child`, a popup, sits in its parent menu's space: beside the row
/// it opened from, on the side menus open towards.
fn beside(child: &MenuState, direction: Direction) -> Rectangle {
    let row = child.menu_bounds.parent_bounds;
    let size = child.menu_bounds.children_bounds.size();
    let x = match direction {
        Direction::Positive => row.x + row.width,
        Direction::Negative => row.x - size.width,
    };
    Rectangle::new(Point::new(x, row.y), size)
}

/// The rows of the menu at `depth`, following `active_root` down from the
/// menu's root.
fn rows_at<'a, Message>(
    menu_roots: &'a [MenuTree<Message>],
    active_root: &[usize],
    depth: usize,
) -> &'a Vec<MenuTree<Message>> {
    active_root[..=depth]
        .iter()
        .skip(1)
        .fold(&menu_roots[active_root[0]].children, |mt, next| {
            &mt[*next].children
        })
}

/// The state of the child opened from row `index` of the last menu in
/// `state`, placed beside that row.
#[allow(clippy::too_many_arguments)]
fn child_menu_state<Message: Clone>(
    menu: &Menu<'_, Message>,
    state: &mut MenuBarStateInner,
    menu_depth: usize,
    index: usize,
    renderer: &crate::ui::Renderer,
    viewport_size: Size,
    overlay_offset: Vector,
    cross_offset: f32,
) -> MenuState {
    let active_root = if menu.is_overlay {
        state.active_root.clone()
    } else {
        state.active_root[..=menu.depth].to_vec()
    };
    let item = &rows_at(&menu.menu_roots, &active_root, active_root.len() - 1)[index];
    let parent = &state.menu_states[menu_depth];
    let item_bounds = parent.row_bounds(index);
    let aod = Aod {
        horizontal: true,
        vertical: true,
        horizontal_overlap: false,
        vertical_overlap: true,
        horizontal_direction: state.horizontal_direction,
        vertical_direction: state.vertical_direction,
        horizontal_offset: cross_offset,
        vertical_offset: 0.0,
    };
    let tree = &mut state.tree.children[active_root[0]].children;
    MenuState::new(MenuBounds::new(
        item,
        renderer,
        menu.item_width,
        menu.item_height,
        viewport_size,
        overlay_offset,
        &aod,
        menu.bounds_expand,
        item_bounds,
        tree,
        menu.is_overlay,
    ))
}

/// Closes every menu of the chain `menu` belongs to.
fn close_all<Message: Clone>(
    menu: &Menu<'_, Message>,
    state: &mut MenuBarStateInner,
    shell: &mut Shell<'_, Message>,
) {
    if matches!(windowing_system(), Some(WindowingSystem::Wayland))
        && let Some(handler) = menu.on_surface_action.as_ref()
    {
        let mut root = menu.window_id;
        let mut depth = menu.depth;
        while let Some(parent) = state.popup_id.iter().find(|(_, v)| **v == root) {
            // parent of root popup is the window, so we stop.
            if depth == 0 {
                break;
            }
            root = *parent.0;
            depth = depth.saturating_sub(1);
        }
        shell.publish((handler)(crate::ui::surface::Action::DestroyPopup {
            id: root,
            animate: false,
        }));
    }
    state.reset();
}

/// Closes `menu`, a child popup, handing the keys back to its parent.
fn close_own<Message: Clone>(
    menu: &Menu<'_, Message>,
    state: &mut MenuBarStateInner,
    shell: &mut Shell<'_, Message>,
) {
    if let Some(parent) = state
        .popup_id
        .iter()
        .find(|(_, child)| **child == menu.window_id)
        .map(|(parent, _)| *parent)
    {
        state.popup_id.remove(&parent);
        shell.publish((menu.on_surface_action.as_ref().unwrap())(
            crate::ui::surface::action::destroy_popup(menu.window_id),
        ));
    }
    state.active_root.truncate(menu.depth);
    state.menu_states.truncate(menu.depth);
}

/// Closes the child popup of the menu at `menu.depth`, if one is open.
fn close_child<Message: Clone>(
    menu: &Menu<'_, Message>,
    state: &mut MenuBarStateInner,
    shell: &mut Shell<'_, Message>,
) {
    if matches!(windowing_system(), Some(WindowingSystem::Wayland))
        && let Some(id) = state.popup_id.remove(&menu.window_id)
    {
        shell.publish((menu.on_surface_action.as_ref().unwrap())(
            crate::ui::surface::action::destroy_popup(id),
        ));
    }
    state.active_root.truncate(menu.depth + 1);
    state.menu_states.truncate(menu.depth + 1);
}

fn pad_rectangle(rect: Rectangle, padding: Padding) -> Rectangle {
    Rectangle {
        x: rect.x - padding.left,
        y: rect.y - padding.top,
        width: rect.width + padding.x(),
        height: rect.height + padding.y(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn init_root_menu<Message: Clone>(
    menu: &mut Menu<'_, Message>,
    renderer: &crate::ui::Renderer,
    shell: &mut Shell<'_, Message>,
    overlay_cursor: Point,
    viewport_size: Size,
    overlay_offset: Vector,
    bar_bounds: Rectangle,
    main_offset: f32,
) {
    menu.tree.inner.with_data_mut(|state| {
        if !(state.menu_states.get(menu.depth).is_none()
            && (!menu.is_overlay || bar_bounds.contains(overlay_cursor)))
            || menu.depth > 0
            || !state.open
        {
            return;
        }

        for (i, (&root_bounds, mt)) in menu
            .root_bounds_list
            .iter()
            .zip(menu.menu_roots.iter())
            .enumerate()
        {
            if mt.children.is_empty() {
                continue;
            }

            if root_bounds.contains(overlay_cursor) {
                let view_center = viewport_size.width * 0.5;
                let rb_center = root_bounds.center_x();

                state.horizontal_direction = if menu.is_overlay && rb_center > view_center {
                    Direction::Negative
                } else {
                    Direction::Positive
                };

                let aod = Aod {
                    horizontal: true,
                    vertical: true,
                    horizontal_overlap: true,
                    vertical_overlap: false,
                    horizontal_direction: state.horizontal_direction,
                    vertical_direction: state.vertical_direction,
                    horizontal_offset: 0.0,
                    vertical_offset: main_offset,
                };
                let menu_bounds = MenuBounds::new(
                    mt,
                    renderer,
                    menu.item_width,
                    menu.item_height,
                    viewport_size,
                    overlay_offset,
                    &aod,
                    menu.bounds_expand,
                    root_bounds,
                    &mut state.tree.children[0].children,
                    menu.is_overlay,
                );
                state.active_root.push(i);
                let mut ms = MenuState::new(menu_bounds);
                // Allocate room once, so animation cannot race a configure.
                if !menu.is_overlay {
                    let room = fold_reserve(
                        mt,
                        renderer,
                        menu.item_width,
                        menu.item_height,
                        &mut state.tree.children[0].children,
                    );
                    ms.room = room;
                    ms.min_height = ms.menu_bounds.children_bounds.height + room;
                    ms.settle = room;
                    ms.menu_bounds.children_bounds.y += room;
                    ms.menu_bounds.check_bounds.y += room;
                }
                state.menu_states.push(ms);
                // Hack to ensure menu opens properly
                shell.invalidate_layout();

                break;
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn init_root_popup_menu<Message>(
    menu: &mut Menu<'_, Message>,
    renderer: &crate::ui::Renderer,
    shell: &mut Shell<'_, Message>,
    overlay_cursor: Point,
    viewport_size: Size,
    overlay_offset: Vector,
    bar_bounds: Rectangle,
    main_offset: f32,
) where
    Message: std::clone::Clone,
{
    menu.tree.inner.with_data_mut(|state| {
        if !(state.menu_states.get(menu.depth).is_none()
            && (!menu.is_overlay || bar_bounds.contains(overlay_cursor)))
        {
            return;
        }

        let active_roots = &state.active_root[..=menu.depth];

        let mt = active_roots
            .iter()
            .skip(1)
            .fold(&menu.menu_roots[active_roots[0]], |mt, next_active_root| {
                &mt.children[*next_active_root]
            });
        let i = active_roots.last().unwrap();
        let root_bounds = menu.root_bounds_list[*i];

        assert!(!mt.children.is_empty(), "skipping menu with no children");
        let aod = Aod {
            horizontal: true,
            vertical: true,
            horizontal_overlap: true,
            vertical_overlap: false,
            horizontal_direction: state.horizontal_direction,
            vertical_direction: state.vertical_direction,
            horizontal_offset: 0.0,
            vertical_offset: main_offset,
        };
        let menu_bounds = MenuBounds::new(
            mt,
            renderer,
            menu.item_width,
            menu.item_height,
            viewport_size,
            overlay_offset,
            &aod,
            menu.bounds_expand,
            root_bounds,
            &mut state.tree.children[0].children,
            menu.is_overlay,
        );

        let view_center = viewport_size.width * 0.5;
        let rb_center = root_bounds.center_x();

        state.horizontal_direction = if rb_center > view_center {
            Direction::Negative
        } else {
            Direction::Positive
        };

        let ms = MenuState::new(menu_bounds);
        state.menu_states.push(ms);

        // Hack to ensure menu opens properly
        shell.invalidate_layout();
    });
}

#[allow(clippy::too_many_arguments)]
fn process_menu_events<Message: std::clone::Clone>(
    menu: &mut Menu<Message>,
    event: &event::Event,
    view_cursor: Cursor,
    renderer: &crate::ui::Renderer,
    clipboard: &mut dyn Clipboard,
    shell: &mut Shell<'_, Message>,
    overlay_offset: Vector,
) {
    let my_state = &mut menu.tree;
    let menu_roots = match &mut menu.menu_roots {
        Cow::Borrowed(_) => panic!(),
        Cow::Owned(o) => o.as_mut_slice(),
    };
    my_state.inner.with_data_mut(|state| {
        if state.active_root.len() <= menu.depth {
            return;
        }

        // A popup's own menu gets its events, even with a child open beside
        // it: a split row's left part is pressed with its variants showing
        let depth = if menu.is_overlay {
            state.menu_states.len().saturating_sub(1)
        } else {
            menu.depth
        };
        if state.active_root.len() <= depth {
            return;
        }
        let Some(hover) = state.menu_states.get_mut(depth) else {
            return;
        };

        let Some(hover_index) = hover.index else {
            return;
        };

        let mt = state.active_root[..=depth].iter().skip(1).fold(
            // then use menu states for each open menu
            &mut menu_roots[state.active_root[0]],
            |mt, next_active_root| &mut mt.children[*next_active_root],
        );

        let mt = &mut mt.children[hover_index];
        // A press for a group is not the row's, and nor is its release,
        // wherever the rows have moved since
        let cursor = view_cursor.position().unwrap_or_default() - overlay_offset;
        let for_group = match event {
            event::Event::Mouse(mouse::Event::ButtonPressed(_))
            | event::Event::Touch(touch::Event::FingerPressed { .. }) => presses_group(
                mt,
                hover.row_bounds(hover_index),
                hover.unfolded == Some(hover_index),
                cursor,
            ),
            event::Event::Mouse(mouse::Event::ButtonReleased(_))
            | event::Event::Touch(touch::Event::FingerLifted { .. }) => state.group_press,
            _ => false,
        };
        if for_group {
            return;
        }
        let tree = &mut state.tree.children[state.active_root[0]].children[mt.index];

        // get layout
        let child_node = hover.layout_single(
            overlay_offset,
            hover.index.expect("missing index within menu state."),
            renderer,
            mt,
            tree,
        );
        let child_layout = Layout::new(&child_node);

        // process only the last widget
        mt.item.update(
            tree,
            event,
            child_layout,
            view_cursor,
            renderer,
            clipboard,
            shell,
            &Rectangle::default(),
        );
    });
}

#[allow(unused_results, clippy::too_many_lines, clippy::too_many_arguments)]
fn process_overlay_events<Message>(
    menu: &mut Menu<Message>,
    renderer: &crate::ui::Renderer,
    viewport_size: Size,
    overlay_offset: Vector,
    view_cursor: Cursor,
    overlay_cursor: Point,
    cross_offset: f32,
    shell: &mut Shell<'_, Message>,
    aim: bool,
) -> Option<(usize, MenuState)>
where
    Message: std::clone::Clone,
{
    /*
    if no active root || pressed:
        return
    else:
        remove invalid menus // overlay space
        update active item
        if active item is a menu:
            add menu // viewport space
    */
    let mut new_menu_root = None;

    menu.tree.inner.with_data_mut(|state| {

        /* When overlay is running, cursor_position in any widget method will go negative
        but I still want Widget::draw() to react to cursor movement */
        state.view_cursor = view_cursor;

        // * remove invalid menus
        if state.open {
            let mut prev_bounds = std::iter::once(menu.bar_bounds)
                .chain(
                    if menu.is_overlay {
                        state.menu_states[..state.menu_states.len().saturating_sub(1)].iter()
                    } else {
                        state.menu_states[..menu.depth].iter()
                    }
                    .map(|s| s.menu_bounds.children_bounds),
                )
                .collect::<Vec<_>>();

            if menu.is_overlay && menu.close_condition.leave {
                for i in (0..state.menu_states.len()).rev() {
                    let mb = &state.menu_states[i].menu_bounds;

                    if mb.parent_bounds.contains(overlay_cursor)
                        || menu.is_overlay && mb.children_bounds.contains(overlay_cursor)
                        || mb.offset_bounds.contains(overlay_cursor)
                        || (mb.check_bounds.contains(overlay_cursor)
                            && prev_bounds.iter().all(|pvb| !pvb.contains(overlay_cursor)))
                    {
                        break;
                    }
                    prev_bounds.pop();
                    state.active_root.pop();
                    state.menu_states.pop();
                }
            } else if menu.is_overlay {
                for i in (0..state.menu_states.len()).rev() {
                    let mb = &state.menu_states[i].menu_bounds;

                    if mb.parent_bounds.contains(overlay_cursor)
                        || mb.children_bounds.contains(overlay_cursor)
                        || prev_bounds.iter().all(|pvb| !pvb.contains(overlay_cursor))
                    {
                        break;
                    }
                    prev_bounds.pop();
                    state.active_root.pop();
                    state.menu_states.pop();
                }
            }
        }

        // * update active item
        let menu_states_len = state.menu_states.len();
        let depth_index = if menu.is_overlay {
            menu_states_len.saturating_sub(1)
        } else {
            menu.depth
        };
        // The open child of this menu, which the pointer may be heading for,
        // where it sits beside its row: a popup's own bounds are in its own
        // space, not this menu's
        let child_bounds = (!menu.is_overlay)
            .then(|| state.menu_states.get(depth_index + 1))
            .flatten()
            .map(|child| beside(child, state.horizontal_direction));
        // Where the pointer was over this same menu: each popup has its own
        // space
        let last_cursor = state
            .last_cursor
            .replace((menu.depth, overlay_cursor))
            .filter(|(depth, _)| *depth == menu.depth)
            .map(|(_, point)| point);
        // A move over a child menu settles the aim of the menus above it
        if state.pending.is_some_and(|p| p.depth < menu.depth) {
            state.pending = None;
        }

        let Some(last_menu_state) = state.menu_states.get_mut(depth_index) else {
            if menu.is_overlay {
                // no menus left
                // keep state.open when the cursor is still inside the menu bar
                // this allows the overlay to keep drawing when the cursor is
                // moving around the menu bar
                if !menu.bar_bounds.contains(overlay_cursor) {
                    state.open = false;
                }
            }
            shell.capture_event();
            return new_menu_root;
        };

        let last_menu_bounds = &last_menu_state.menu_bounds;
        let last_parent_bounds = last_menu_bounds.parent_bounds;
        let last_children_bounds = last_menu_bounds.children_bounds;

        // The room a popup keeps for a group to unfold into, and whatever of
        // the surface the rows do not cover while they move
        let top = last_children_bounds.y + last_menu_state.scroll_offset;
        let below_rows = !menu.is_overlay
            && (overlay_cursor.y < top || overlay_cursor.y >= top + last_children_bounds.height);
        if (menu.is_overlay && !menu.menu_overlays_parent && last_parent_bounds.contains(overlay_cursor))
        // cursor is in the parent part
        || menu.is_overlay && !last_children_bounds.contains(overlay_cursor)
        // cursor is outside
        || below_rows
        {

            if last_menu_state.index.take().is_some() {
                shell.request_redraw();
            }
            last_menu_state.fresh = false;
            if last_menu_state.unfolded.is_some() {
                set_unfolded(menu, state, depth_index, None, renderer, shell);
            }
            shell.capture_event();
            return new_menu_root;
        }

        let active_root = if menu.is_overlay {
            &state.active_root
        } else {
            &state.active_root[..=menu.depth]
        };

        if state.pressed {
            return new_menu_root;
        }
        let active_menu = rows_at(&menu.menu_roots, active_root, active_root.len() - 1);
        let Some(new_index) = last_menu_state.row_at(overlay_cursor) else {
            if last_menu_state.index.take().is_some() {
                shell.request_redraw();
            }
            shell.capture_event();
            return new_menu_root;
        };
        // Leaving an unfolded group, its row and its rows, folds it: the
        // rows above it come down, the rows below stay
        let groups: Vec<Group> = active_menu.iter().map(|row| row.group).collect();
        if let Some(head) = last_menu_state.unfolded
            && !unfold::in_group(&groups, head, new_index)
        {
            set_unfolded(menu, state, depth_index, None, renderer, shell);
        }
        let Some(last_menu_state) = state.menu_states.get_mut(depth_index) else {
            return new_menu_root;
        };
        let active_root = if menu.is_overlay {
            &state.active_root
        } else {
            &state.active_root[..=menu.depth]
        };
        let active_menu = rows_at(&menu.menu_roots, active_root, active_root.len() - 1);

        let now = Instant::now();
        let entered = last_menu_state.index != Some(new_index);

        // Heading for the open child across other rows: the child stays
        // until the pointer rests on one of them
        if aim
            && entered
            && last_menu_state.index.is_some()
            && let (Some(child), Some(from)) = (child_bounds, last_cursor)
            && aims_at(from, overlay_cursor, child)
        {
            let at = now + AIM_TIME;
            state.pending = Some(Pending {
                depth: menu.depth,
                at,
                kind: PendingKind::Switch,
            });
            shell.request_redraw_at(at);
            shell.capture_event();
            return new_menu_root;
        }

        let remove = last_menu_state
            .index
            .as_ref()
            .is_some_and(|i| *i != new_index && !active_menu[*i].children.is_empty());

        if matches!(windowing_system(), Some(WindowingSystem::Wayland)) && remove
            && let Some(id) = state.popup_id.remove(&menu.window_id) {
                state.active_root.truncate(menu.depth + 1);
                shell.publish((menu.on_surface_action.as_ref().unwrap())({
                    crate::ui::surface::action::destroy_popup(id)
                }));
            }
        let item = &active_menu[new_index];
        // set new index
        let old_index = last_menu_state.index.replace(new_index);

        if old_index != Some(new_index) {
            shell.request_redraw();
            state.pending = None;
        }

        let open = !remove && child_bounds.is_some();
        let opening = if item.children.is_empty() {
            Opening::Not
        } else {
            item.child_open.opening(entered, open)
        };
        // Pointing at a group row's count, exactly, unfolds its group at once
        let on_count = match item.group {
            Group::Head { count, .. } => unfold::on_chip(
                state.menu_states[depth_index].row_bounds(new_index),
                count,
                state.menu_states[depth_index].unfolded == Some(new_index),
                overlay_cursor,
            ),
            _ => false,
        };
        match opening {
            // * add new menu if the new item is a menu
            Opening::Now => {
                let ms = child_menu_state(
                    menu,
                    state,
                    depth_index,
                    new_index,
                    renderer,
                    viewport_size,
                    overlay_offset,
                    cross_offset,
                );
                new_menu_root = Some((new_index, ms.clone()));
                if menu.is_overlay {
                    state.active_root.push(new_index);
                } else {
                    state.menu_states.truncate(menu.depth + 1);
                }
                state.menu_states.push(ms);
            }
            Opening::After(delay) => {
                let at = now + delay;
                state.pending = Some(Pending {
                    depth: menu.depth,
                    at,
                    kind: PendingKind::Open(new_index),
                });
                shell.request_redraw_at(at);
                if !menu.is_overlay && remove {
                    state.menu_states.truncate(menu.depth + 1);
                }
            }
            Opening::Not => {
                if !menu.is_overlay && remove {
                    state.menu_states.truncate(menu.depth + 1);
                }
            }
        }

        // A group closing is not opened again by pointing: its row comes
        // back under the pointer as it closes, and it would flap open and
        // shut. A click or the keys can.
        let closing = state.menu_states[depth_index]
            .reveal
            .as_ref()
            .is_some_and(|reveal| reveal.folding && reveal.head == new_index);
        if on_count && !closing && state.menu_states[depth_index].unfolded != Some(new_index) {
            set_unfolded(
                menu,
                state,
                depth_index,
                Some(new_index),
                renderer,
                shell,
            );
            state.menu_states[depth_index].fresh = true;
        } else if !on_count && let Some(ms) = state.menu_states.get_mut(depth_index) {
            ms.fresh = false;
            // A group waiting its turn opens only while it is pointed at,
            // unless the keys asked for it
            if !ms.queued_keys {
                ms.queued = None;
            }
        }

        shell.capture_event();
        new_menu_root
    })
}

/// Does what the menu at `menu.depth` waited for, once its time has come:
/// opens a row's child, or lets the row under the pointer take over from an
/// open child it was not heading for after all.
fn process_pending<Message: Clone>(
    menu: &mut Menu<'_, Message>,
    now: Instant,
    renderer: &crate::ui::Renderer,
    viewport_size: Size,
    overlay_offset: Vector,
    shell: &mut Shell<'_, Message>,
) -> Option<(usize, MenuState)> {
    let due = menu.tree.inner.with_data_mut(|state| match state.pending {
        Some(pending) if pending.depth == menu.depth => {
            if now >= pending.at {
                state.pending = None;
                Some(pending.kind)
            } else {
                shell.request_redraw_at(pending.at);
                None
            }
        }
        _ => None,
    })?;
    match due {
        PendingKind::Open(index) => menu.tree.inner.with_data_mut(|state| {
            let depth_index = menu.depth;
            let still_there = state.menu_states.get(depth_index)?.index == Some(index);
            if !still_there || state.menu_states.len() > depth_index + 1 {
                return None;
            }
            let ms = child_menu_state(
                menu,
                state,
                depth_index,
                index,
                renderer,
                viewport_size,
                overlay_offset,
                menu.cross_offset as f32,
            );
            state.menu_states.push(ms.clone());
            shell.request_redraw();
            Some((index, ms))
        }),
        PendingKind::Switch => {
            let cursor = menu
                .tree
                .inner
                .with_data(|state| state.last_cursor)
                .filter(|(depth, _)| *depth == menu.depth)
                .map(|(_, point)| point)?;
            process_overlay_events(
                menu,
                renderer,
                viewport_size,
                overlay_offset,
                Cursor::Available(cursor + overlay_offset),
                cursor,
                menu.cross_offset as f32,
                shell,
                false,
            )
        }
    }
}

/// The keyboard in the menu that has it, the deepest one open: Up and Down
/// move between rows the keys stop on, Right opens a row's child and Left
/// closes this menu if it is a child, Enter chooses the row and Escape
/// closes every menu. Any other key closes them too and goes on to the
/// window, as before the menus had keys. A reason's popup gives the keys
/// back to its menu by closing.
fn process_key<Message: Clone>(
    menu: &mut Menu<'_, Message>,
    key: &iced_core::keyboard::Key,
    renderer: &crate::ui::Renderer,
    viewport_size: Size,
    overlay_offset: Vector,
    shell: &mut Shell<'_, Message>,
) -> Option<(usize, MenuState)> {
    use iced_core::keyboard::Key;
    use iced_core::keyboard::key::Named;

    let Key::Named(named) = key else {
        menu.tree
            .inner
            .with_data_mut(|state| close_all(menu, state, shell));
        return None;
    };
    if matches!(
        named,
        Named::Shift | Named::Control | Named::Alt | Named::Super | Named::Meta
    ) {
        return None;
    }
    let mut depth = menu.depth;
    let mut new_root = None;
    let mut press = None;
    menu.tree.inner.with_data_mut(|state| {
        if !state.open || state.active_root.len() <= depth || state.menu_states.len() <= depth {
            return;
        }
        // The reason's popup has the keys while it shows, as every popup
        // takes them: it closes, and the key is for the menu it belongs to
        let forwarded = depth > 0
            && rows_at(&menu.menu_roots, &state.active_root, depth)
                .iter()
                .all(|row| row.passive);
        if forwarded {
            close_own(menu, state, shell);
            depth -= 1;
        }
        state.menu_states[depth].keyboard_selection = true;
        let rows = rows_at(&menu.menu_roots, &state.active_root, depth);
        let index = state.menu_states[depth].index;
        // What Enter does on the chosen row: run its left part, or open its
        // child when the row has nothing else to run
        let row = index.map(|i| &rows[i]);
        let opens = row.is_some_and(|row| {
            !row.children.is_empty()
                && match row.child_open {
                    ChildOpen::Row => true,
                    ChildOpen::Rest(_) => false,
                }
        });
        // → ← and Space fold and unfold groups; Enter on a group row that
        // runs nothing unfolds it
        let groups: Vec<Group> = rows.iter().map(|row| row.group).collect();
        let selectable: Vec<bool> = rows.iter().map(|row| row.selectable).collect();
        let fold_key = match named {
            Named::ArrowRight => Some(FoldKey::Right),
            Named::ArrowLeft => Some(FoldKey::Left),
            Named::Space => Some(FoldKey::Space),
            Named::Enter if row.is_some_and(|row| !row.pressable) => Some(FoldKey::Right),
            _ => None,
        };
        let folding = (!forwarded)
            .then_some(fold_key.zip(index))
            .flatten()
            .and_then(|(key, index)| {
                unfold::on_key(
                    &groups,
                    &selectable,
                    index,
                    state.menu_states[depth].unfolded,
                    key,
                )
            });
        if let Some((unfolded, chosen)) = folding {
            set_unfolded(menu, state, depth, unfolded, renderer, shell);
            let ms = &mut state.menu_states[depth];
            // Waiting its turn, the group keeps its row chosen and chooses
            // its first row once it opens
            let waits = unfolded.is_some() && ms.queued == unfolded;
            ms.queued_keys = waits;
            ms.index = Some(if waits {
                unfolded.unwrap_or(chosen)
            } else {
                chosen
            });
            ms.fresh = false;
            // Do not scroll to the natural position of a still-hidden member.
            if ms.reveal.is_none()
                && ms.scroll_into_view(chosen, viewport_size.height, overlay_offset)
            {
                shell.invalidate_layout();
            }
            state.pending = None;
            shell.request_redraw();
            shell.capture_event();
            return;
        }
        match named {
            // From the reason's popup the menu cannot be scrolled to show
            // another row, so these only close the reason, as Right and
            // Left do
            Named::ArrowDown | Named::ArrowUp if forwarded => {}
            Named::ArrowDown | Named::ArrowUp => {
                // A folded group's rows are not stopped on
                let ms = &state.menu_states[depth];
                let selectable: Vec<bool> = selectable
                    .iter()
                    .enumerate()
                    .map(|(i, stops)| *stops && ms.shows(i))
                    .collect();
                let next = next_selectable(&selectable, index, *named == Named::ArrowDown);
                if next != index {
                    close_child(menu, state, shell);
                    state.menu_states[depth].index = next;
                    // A menu taller than its surface scrolls, so the row
                    // chosen is one that can be seen
                    if let Some(next) = next
                        && state.menu_states[depth].scroll_into_view(
                            next,
                            viewport_size.height,
                            overlay_offset,
                        )
                    {
                        shell.invalidate_layout();
                    }
                    state.pending = None;
                    shell.request_redraw();
                }
            }
            // A child is opened by the popup of its parent menu, so not
            // from the reason's popup
            Named::ArrowRight | Named::Enter if opens && !forwarded => {
                let index = index.unwrap_or_default();
                if state.menu_states.len() > depth + 1 {
                    return;
                }
                let mut ms = child_menu_state(
                    menu,
                    state,
                    depth,
                    index,
                    renderer,
                    viewport_size,
                    overlay_offset,
                    menu.cross_offset as f32,
                );
                let child_rows = &rows[index].children;
                let selectable: Vec<bool> = child_rows.iter().map(|row| row.selectable).collect();
                ms.index = next_selectable(&selectable, None, true);
                state.menu_states.push(ms.clone());
                state.pending = None;
                new_root = Some((index, ms));
            }
            Named::ArrowRight => {}
            Named::ArrowLeft | Named::Escape if depth > 0 && !forwarded => {
                close_own(menu, state, shell);
            }
            Named::ArrowLeft => {}
            Named::Enter => {
                press = index.filter(|i| rows[*i].pressable);
            }
            Named::Escape => close_all(menu, state, shell),
            // Space on a row without a group does nothing in a menu with
            // groups; elsewhere it closes the menu and goes on, as before
            Named::Space if groups.iter().any(|g| matches!(g, Group::Head { .. })) => {}
            _ => {
                close_all(menu, state, shell);
                return;
            }
        }
        shell.capture_event();
    });
    if let Some(index) = press {
        press_row(menu, depth, index, renderer, overlay_offset, shell);
        menu.tree
            .inner
            .with_data_mut(|state| close_all(menu, state, shell));
    }
    new_root
}

/// Presses and releases the left part of row `index` of the menu at
/// `depth`, as a click there would: its button sends its message.
fn press_row<Message: Clone>(
    menu: &mut Menu<'_, Message>,
    depth: usize,
    index: usize,
    renderer: &crate::ui::Renderer,
    overlay_offset: Vector,
    shell: &mut Shell<'_, Message>,
) {
    use iced_core::mouse::{Button, Event as MouseEvent};

    let menu_roots = match &mut menu.menu_roots {
        Cow::Borrowed(_) => return,
        Cow::Owned(o) => o.as_mut_slice(),
    };
    menu.tree.inner.with_data_mut(|state| {
        let row = state.active_root[..=depth]
            .iter()
            .skip(1)
            .fold(&mut menu_roots[state.active_root[0]], |mt, next| {
                &mut mt.children[*next]
            });
        let row = &mut row.children[index];
        let tree = &mut state.tree.children[state.active_root[0]].children[row.index];
        let node =
            state.menu_states[depth].layout_single(overlay_offset, index, renderer, row, tree);
        let layout = Layout::new(&node);
        let bounds = layout.bounds();
        let cursor = Cursor::Available(Point::new(bounds.x + 24.0, bounds.center_y()));
        let mut clipboard = iced_core::clipboard::Null;
        for event in [
            MouseEvent::ButtonPressed(Button::Left),
            MouseEvent::ButtonReleased(Button::Left),
        ] {
            row.item.update(
                tree,
                &event::Event::Mouse(event),
                layout,
                cursor,
                renderer,
                &mut clipboard,
                shell,
                &Rectangle::default(),
            );
        }
    });
}

fn process_scroll_events<Message>(
    menu: &mut Menu<'_, Message>,
    shell: &mut Shell<'_, Message>,
    delta: mouse::ScrollDelta,
    overlay_cursor: Point,
    viewport_size: Size,
    overlay_offset: Vector,
) where
    Message: Clone,
{
    use mouse::ScrollDelta;

    menu.tree.inner.with_data_mut(|state| {
        let delta_y = match delta {
            ScrollDelta::Lines { y, .. } => y * 60.0,
            ScrollDelta::Pixels { y, .. } => y,
        };

        let calc_offset_bounds = |menu_state: &MenuState, viewport_size: Size| -> (f32, f32) {
            // viewport space children bounds
            let children_bounds = menu_state.menu_bounds.children_bounds + overlay_offset;

            let max_offset = (0.0 - children_bounds.y).max(0.0);
            let min_offset =
                (viewport_size.height - (children_bounds.y + children_bounds.height)).min(0.0);
            (max_offset, min_offset)
        };

        // update
        if state.menu_states.is_empty() {
            return;
        } else if state.menu_states.len() == 1 {
            let last_ms = &mut state.menu_states[0];

            if last_ms.index.is_none() {
                return;
            }

            let (max_offset, min_offset) = calc_offset_bounds(last_ms, viewport_size);
            last_ms.scroll_offset = (last_ms.scroll_offset + delta_y).clamp(min_offset, max_offset);
        } else {
            // >= 2
            let max_index = state.menu_states.len() - 1;
            let last_two = &mut state.menu_states[max_index - 1..=max_index];

            if last_two[1].index.is_some() {
                // scroll the last one
                let (max_offset, min_offset) = calc_offset_bounds(&last_two[1], viewport_size);
                last_two[1].scroll_offset =
                    (last_two[1].scroll_offset + delta_y).clamp(min_offset, max_offset);
            } else {
                if !last_two[0]
                    .menu_bounds
                    .children_bounds
                    .contains(overlay_cursor)
                {
                    shell.capture_event();
                    return;
                }

                // scroll the second last one
                let (max_offset, min_offset) = calc_offset_bounds(&last_two[0], viewport_size);
                let scroll_offset =
                    (last_two[0].scroll_offset + delta_y).clamp(min_offset, max_offset);
                let clamped_delta_y = scroll_offset - last_two[0].scroll_offset;
                last_two[0].scroll_offset = scroll_offset;

                // update the bounds of the last one
                last_two[1].menu_bounds.parent_bounds.y += clamped_delta_y;
                last_two[1].menu_bounds.children_bounds.y += clamped_delta_y;
                last_two[1].menu_bounds.check_bounds.y += clamped_delta_y;
            }
        }
        shell.capture_event();
        shell.request_redraw();
    });
}

#[allow(clippy::pedantic)]
/// Returns (children_size, child_positions, child_sizes)
fn get_children_layout<Message>(
    menu_tree: &MenuTree<Message>,
    renderer: &crate::ui::Renderer,
    item_width: ItemWidth,
    item_height: ItemHeight,
    tree: &mut [Tree],
    unfolded: Option<usize>,
) -> (Size, Vec<f32>, Vec<Size>) {
    let width = match item_width {
        ItemWidth::Uniform(u) => f32::from(u),
        ItemWidth::Static(s) => f32::from(menu_tree.width.unwrap_or(s)),
    };

    let mut child_sizes: Vec<Size> = match item_height {
        ItemHeight::Uniform(u) => {
            let count = menu_tree.children.len();
            vec![Size::new(width, f32::from(u)); count]
        }
        ItemHeight::Static(s) => menu_tree
            .children
            .iter()
            .map(|mt| Size::new(width, f32::from(mt.height.unwrap_or(s))))
            .collect(),
        ItemHeight::Dynamic(d) => menu_tree
            .children
            .iter()
            .map(|mt| {
                mt.item
                    .element
                    .with_data_mut(|w| match w.as_widget_mut().size().height {
                        Length::Fixed(f) => Size::new(width, f),
                        Length::Shrink => {
                            let l_height = w
                                .as_widget_mut()
                                .layout(
                                    &mut tree[mt.index],
                                    renderer,
                                    &Limits::new(Size::ZERO, Size::new(width, f32::MAX)),
                                )
                                .size()
                                .height;

                            let height = if (f32::MAX - l_height) < 0.001 {
                                f32::from(d)
                            } else {
                                l_height
                            };

                            Size::new(width, height)
                        }
                        _ => mt.height.map_or_else(
                            || Size::new(width, f32::from(d)),
                            |h| Size::new(width, f32::from(h)),
                        ),
                    })
            })
            .collect(),
    };

    // A folded group's rows take no room
    let groups: Vec<Group> = menu_tree.children.iter().map(|mt| mt.group).collect();
    for (size, shown) in child_sizes.iter_mut().zip(unfold::shown(&groups, unfolded)) {
        if !shown {
            size.height = 0.0;
        }
    }

    let max_index = menu_tree.children.len().saturating_sub(1);
    let child_positions: Vec<f32> = std::iter::once(0.0)
        .chain(child_sizes[0..max_index].iter().scan(0.0, |acc, x| {
            *acc += x.height;
            Some(*acc)
        }))
        .collect();

    let height = child_sizes.iter().fold(0.0, |acc, x| acc + x.height);

    (Size::new(width, height), child_positions, child_sizes)
}

/// How much taller than folded the menu `menu_tree` gets with its biggest
/// group unfolded.
fn fold_reserve<Message>(
    menu_tree: &MenuTree<Message>,
    renderer: &crate::ui::Renderer,
    item_width: ItemWidth,
    item_height: ItemHeight,
    tree: &mut [Tree],
) -> f32 {
    let folded = get_children_layout(menu_tree, renderer, item_width, item_height, tree, None)
        .0
        .height;
    menu_tree
        .children
        .iter()
        .enumerate()
        .filter(|(_, mt)| matches!(mt.group, Group::Head { .. }))
        .map(|(head, _)| {
            get_children_layout(
                menu_tree,
                renderer,
                item_width,
                item_height,
                tree,
                Some(head),
            )
            .0
            .height
                - folded
        })
        .fold(0.0, f32::max)
}

/// The menu at `depth`, following `active_root` down from the menu's root.
fn menu_at<'a, Message>(
    menu_roots: &'a [MenuTree<Message>],
    active_root: &[usize],
    depth: usize,
) -> &'a MenuTree<Message> {
    active_root[..=depth]
        .iter()
        .skip(1)
        .fold(&menu_roots[active_root[0]], |mt, next| &mt.children[*next])
}

/// Animates within the reserved space; another group waits for the current fold.
fn set_unfolded<Message: Clone>(
    menu: &Menu<'_, Message>,
    state: &mut MenuBarStateInner,
    depth: usize,
    unfolded: Option<usize>,
    renderer: &crate::ui::Renderer,
    shell: &mut Shell<'_, Message>,
) {
    let ms = &mut state.menu_states[depth];
    if unfolded.is_none() {
        ms.queued = None;
    }
    if ms.unfolded == unfolded {
        return;
    }
    if let Some(head) = unfolded {
        let busy =
            ms.unfolded.is_some() || ms.reveal.as_ref().is_some_and(|reveal| reveal.head != head);
        if busy {
            if ms.unfolded.is_some() {
                set_unfolded(menu, state, depth, None, renderer, shell);
            }
            state.menu_states[depth].queued = Some(head);
            state.menu_states[depth].queued_keys = false;
            return;
        }
    }
    close_child(menu, state, shell);
    let active_root = state.active_root.clone();
    let menu_tree = menu_at(&menu.menu_roots, &active_root, depth);
    let tree = &mut state.tree.children[active_root[0]].children;
    let ms = &mut state.menu_states[depth];
    let Some(head) = unfolded.or(ms.unfolded) else {
        return;
    };
    let Group::Head { rows, .. } = menu_tree.children[head].group else {
        return;
    };
    let full = get_children_layout(
        menu_tree,
        renderer,
        menu.item_width,
        menu.item_height,
        tree,
        Some(head),
    )
    .2
    .iter()
    .skip(head + 1)
    .take(rows)
    .map(|size| size.height)
    .sum();
    let from = ms.reveal.as_ref().filter(|r| r.head == head).map_or(
        if unfolded.is_some() { 0.0 } else { full },
        unfold::Reveal::shown,
    );
    ms.unfolded = unfolded;
    ms.reveal = Some(if unfolded.is_some() {
        unfold::Reveal::unfold(head, rows, full, from)
    } else {
        unfold::Reveal::fold(head, rows, full, from)
    });
    ms.relayout(
        menu_tree,
        renderer,
        menu.item_width,
        menu.item_height,
        tree,
        menu.bounds_expand,
    );
    shell.invalidate_layout();
    shell.request_redraw();
}

/// Opens the group that asked to open while another one closed; asked for
/// with the keys, its first row is chosen.
fn open_queued<Message: Clone>(
    menu: &Menu<'_, Message>,
    state: &mut MenuBarStateInner,
    depth: usize,
    renderer: &crate::ui::Renderer,
    shell: &mut Shell<'_, Message>,
) {
    let ms = &mut state.menu_states[depth];
    let Some(head) = ms.queued.take() else {
        return;
    };
    let keys = std::mem::take(&mut ms.queued_keys);
    set_unfolded(menu, state, depth, Some(head), renderer, shell);
    if keys {
        let rows = rows_at(&menu.menu_roots, &state.active_root, depth);
        let groups: Vec<Group> = rows.iter().map(|row| row.group).collect();
        let selectable: Vec<bool> = rows.iter().map(|row| row.selectable).collect();
        if let Some((_, chosen)) = unfold::on_key(
            &groups,
            &selectable,
            head,
            Some(head),
            unfold::FoldKey::Right,
        ) {
            state.menu_states[depth].index = Some(chosen);
        }
    }
}

/// Lays the menu at `depth` out again as it shows now.
fn relayout_menu<Message: Clone>(
    menu: &Menu<'_, Message>,
    state: &mut MenuBarStateInner,
    depth: usize,
    renderer: &crate::ui::Renderer,
    shell: &mut Shell<'_, Message>,
) {
    let active_root = state.active_root.clone();
    let menu_tree = menu_at(&menu.menu_roots, &active_root, depth);
    let tree = &mut state.tree.children[active_root[0]].children;
    let ms = &mut state.menu_states[depth];
    ms.relayout(
        menu_tree,
        renderer,
        menu.item_width,
        menu.item_height,
        tree,
        menu.bounds_expand,
    );
    shell.invalidate_layout();
    shell.request_redraw();
}

/// Advances the reveal without changing the popup surface.
fn advance_reveal<Message: Clone>(
    menu: &Menu<'_, Message>,
    now: Instant,
    renderer: &crate::ui::Renderer,
    shell: &mut Shell<'_, Message>,
) {
    menu.tree.inner.with_data_mut(|state| {
        let depth = if menu.is_overlay {
            state.menu_states.len().saturating_sub(1)
        } else {
            menu.depth
        };
        if state.active_root.len() <= depth {
            return;
        }
        let Some(ms) = state.menu_states.get_mut(depth) else {
            return;
        };
        let Some(reveal) = ms.reveal.as_mut() else {
            return;
        };
        let changed = reveal.advance(now);
        let done = !reveal.is_moving();
        let closed = done && reveal.folding;
        if done {
            ms.reveal = None;
        }
        if changed || done {
            relayout_menu(menu, state, depth, renderer, shell);
        }
        if !done {
            shell.request_redraw();
        }
        if closed {
            open_queued(menu, state, depth, renderer, shell);
        }
    });
}

/// Folds the unfolded group of the menu, as the pointer leaves the menu.
fn fold_all<Message: Clone>(
    menu: &Menu<'_, Message>,
    renderer: &crate::ui::Renderer,
    shell: &mut Shell<'_, Message>,
) -> bool {
    menu.tree.inner.with_data_mut(|state| {
        let depth = if menu.is_overlay {
            state.menu_states.len().saturating_sub(1)
        } else {
            menu.depth
        };
        if state.active_root.len() <= depth
            || !state
                .menu_states
                .get(depth)
                .is_some_and(|ms| ms.unfolded.is_some())
        {
            return false;
        }
        set_unfolded(menu, state, depth, None, renderer, shell);
        state.menu_states[depth].fresh = false;
        true
    })
}
/// Whether a press at `cursor` on `row`, at `bounds`, is for its group
/// rather than the row: on its count, or anywhere on a group row that runs
/// nothing itself.
fn presses_group<Message>(
    row: &MenuTree<Message>,
    bounds: Rectangle,
    unfolded: bool,
    cursor: Point,
) -> bool {
    match row.group {
        Group::Head { count, .. } => {
            !row.pressable || unfold::on_chip(bounds, count, unfolded, cursor)
        }
        _ => false,
    }
}

/// A press on a group row's count, or on a group row that runs nothing:
/// unfolds its group, or folds it if it was unfolded before the pointer
/// came. Whether the press was for a group.
fn press_group<Message: Clone>(
    menu: &Menu<'_, Message>,
    overlay_cursor: Point,
    renderer: &crate::ui::Renderer,
    shell: &mut Shell<'_, Message>,
) -> bool {
    menu.tree.inner.with_data_mut(|state| {
        let depth = if menu.is_overlay {
            state.menu_states.len().saturating_sub(1)
        } else {
            menu.depth
        };
        if state.active_root.len() <= depth {
            return false;
        }
        let Some(ms) = state.menu_states.get(depth) else {
            return false;
        };
        let Some(index) = ms.index else {
            return false;
        };
        let rows = rows_at(&menu.menu_roots, &state.active_root, depth);
        if !presses_group(
            &rows[index],
            ms.row_bounds(index),
            ms.unfolded == Some(index),
            overlay_cursor,
        ) {
            return false;
        }
        let unfolded = if ms.unfolded == Some(index) && !ms.fresh {
            None
        } else {
            Some(index)
        };
        set_unfolded(menu, state, depth, unfolded, renderer, shell);
        state.menu_states[depth].fresh = false;
        true
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A child popup opened from the row at `row`, of `size`: its own
    /// bounds start at its own origin, as a popup's do.
    fn child(row: Rectangle, size: Size) -> MenuState {
        MenuState {
            index: None,
            scroll_offset: 0.0,
            keyboard_selection: false,
            menu_bounds: MenuBounds {
                child_positions: vec![0.0],
                child_sizes: vec![size],
                children_bounds: Rectangle::new(Point::ORIGIN, size),
                parent_bounds: row,
                check_bounds: Rectangle::new(Point::ORIGIN, size),
                offset_bounds: Rectangle::default(),
            },
            laid_out: None,
            unfolded: None,
            fresh: false,
            min_height: 0.0,
            fitted_height: f32::INFINITY,
            settle: 0.0,
            reveal: None,
            room: 0.0,
            queued: None,
            queued_keys: false,
        }
    }

    /// A row chosen with the keys in a menu taller than its viewport is
    /// scrolled into view, and only as far as it takes.
    #[test]
    fn a_row_chosen_with_the_keys_is_scrolled_into_view() {
        let rows = 10;
        let row = Size::new(360.0, 36.0);
        let mut menu = MenuState {
            index: None,
            scroll_offset: 0.0,
            keyboard_selection: false,
            menu_bounds: MenuBounds {
                child_positions: (0..rows).map(|i| i as f32 * row.height).collect(),
                child_sizes: vec![row; rows],
                children_bounds: Rectangle::new(
                    Point::ORIGIN,
                    Size::new(row.width, rows as f32 * row.height),
                ),
                parent_bounds: Rectangle::default(),
                check_bounds: Rectangle::default(),
                offset_bounds: Rectangle::default(),
            },
            laid_out: None,
            unfolded: None,
            fresh: false,
            min_height: 0.0,
            fitted_height: f32::INFINITY,
            settle: 0.0,
            reveal: None,
            room: 0.0,
            queued: None,
            queued_keys: false,
        };
        let height = 200.0;
        assert!(
            !menu.scroll_into_view(2, height, Vector::ZERO),
            "already in view"
        );
        assert!(menu.scroll_into_view(8, height, Vector::ZERO));
        // Row 8 ends at 324; its bottom is now the viewport's
        assert_eq!(menu.scroll_offset, height - 324.0);
        assert!(
            !menu.scroll_into_view(7, height, Vector::ZERO),
            "still in view"
        );
        assert!(menu.scroll_into_view(0, height, Vector::ZERO));
        assert_eq!(menu.scroll_offset, 0.0);
        assert!(menu.scroll_into_view(9, height, Vector::ZERO));
        assert_eq!(
            menu.scroll_offset,
            height - 360.0,
            "the last row at the bottom"
        );
    }

    /// The pointer leaving a row for its child popup beside the menu (a
    /// folded menu bar's submenu, a dimmed row's reason) is heading for it:
    /// the child is placed in the parent's space, not at the origin of its
    /// own.
    #[test]
    fn a_child_popup_sits_beside_its_row_for_the_aim() {
        let row = Rectangle::new(Point::new(0.0, 100.0), Size::new(360.0, 36.0));
        let size = Size::new(280.0, 300.0);
        let right = beside(&child(row, size), Direction::Positive);
        assert_eq!(right, Rectangle::new(Point::new(360.0, 100.0), size));
        let left = beside(&child(row, size), Direction::Negative);
        assert_eq!(left, Rectangle::new(Point::new(-280.0, 100.0), size));

        let on_row = Point::new(340.0, 118.0);
        assert!(aims_at(on_row, Point::new(350.0, 140.0), right));
        assert!(!aims_at(on_row, Point::new(300.0, 140.0), right));
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Act;
    impl crate::ui::widget::menu::action::MenuAction for Act {
        type Message = ();
        fn message(&self) {}
    }

    /// Open, Copy with two variants, Paste: five rows.
    fn copy_menu() -> MenuTree<()> {
        use crate::ui::widget::menu::{Entry, Item};
        let items = crate::ui::widget::menu::items(
            &std::collections::HashMap::new(),
            vec![
                Item::Button("Open", None, Act),
                Item::Split(
                    Entry::new("Copy", Act),
                    vec![
                        Item::Button("Copy path", None, Act),
                        Item::Button("Copy to", None, Act),
                    ],
                ),
                Item::Button("Paste", None, Act),
            ],
        );
        let mut menu = MenuTree::with_children(
            crate::ui::Element::from(crate::ui::widget::Row::new()),
            items,
        );
        menu.set_index();
        menu
    }

    fn layout_of(menu: &MenuTree<()>, unfolded: Option<usize>) -> (Size, Vec<f32>, Vec<Size>) {
        let renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut tree: Vec<Tree> = (0..8).map(|_| Tree::empty()).collect();
        get_children_layout(
            menu,
            &renderer,
            ItemWidth::Uniform(360),
            ItemHeight::Dynamic(40),
            &mut tree,
            unfolded,
        )
    }

    /// A folded group's rows take no room; unfolded, they sit under their
    /// row and push the rows after them down.
    #[test]
    fn a_group_takes_room_only_when_unfolded() {
        let menu = copy_menu();
        let (size, positions, sizes) = layout_of(&menu, None);
        let heights: Vec<f32> = sizes.iter().map(|s| s.height).collect();
        assert_eq!(heights, [36.0, 36.0, 0.0, 0.0, 36.0]);
        assert_eq!(size.height, 108.0);
        assert_eq!(positions[4], 72.0, "Paste right under Copy");

        let (size, positions, sizes) = layout_of(&menu, Some(1));
        let heights: Vec<f32> = sizes.iter().map(|s| s.height).collect();
        assert_eq!(heights, [36.0, 36.0, 32.0, 32.0, 36.0]);
        assert_eq!(size.height, 172.0);
        assert_eq!(positions[2], 72.0, "Copy path right under Copy");
        assert_eq!(positions[4], 136.0, "Paste moved down");
    }

    /// Unfolding lays the menu out again, and a row that folds away while
    /// chosen hands the choice to its group row.
    #[test]
    fn refolding_moves_the_rows_and_the_choice() {
        let menu = copy_menu();
        let renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut tree: Vec<Tree> = (0..8).map(|_| Tree::empty()).collect();
        let (size, positions, sizes) = layout_of(&menu, None);
        let mut state = MenuState::new(MenuBounds {
            child_positions: positions,
            child_sizes: sizes,
            children_bounds: Rectangle::new(Point::ORIGIN, size),
            parent_bounds: Rectangle::default(),
            check_bounds: Rectangle::default(),
            offset_bounds: Rectangle::default(),
        });
        let refold = |state: &mut MenuState, unfolded, tree: &mut Vec<Tree>| {
            state.refold(
                &menu,
                unfolded,
                &renderer,
                ItemWidth::Uniform(360),
                ItemHeight::Dynamic(40),
                tree,
                16,
            );
        };
        refold(&mut state, Some(1), &mut tree);
        assert_eq!(state.unfolded, Some(1));
        assert_eq!(state.menu_bounds.children_bounds.height, 172.0);
        assert_eq!(state.menu_bounds.check_bounds.height, 172.0 + 32.0);
        assert!(state.shows(3));

        state.index = Some(3);
        refold(&mut state, None, &mut tree);
        assert_eq!(state.index, Some(1), "Copy to folded away");
        assert_eq!(state.menu_bounds.children_bounds.height, 108.0);
        assert!(!state.shows(3));
    }

    /// Reserve enough room for the largest group.
    #[test]
    fn the_room_kept_is_the_biggest_groups() {
        let menu = copy_menu();
        let renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut tree: Vec<Tree> = (0..8).map(|_| Tree::empty()).collect();
        let reserve = fold_reserve(
            &menu,
            &renderer,
            ItemWidth::Uniform(360),
            ItemHeight::Dynamic(40),
            &mut tree,
        );
        assert_eq!(reserve, 64.0);
    }

    /// A folded menu without a reserved margin in this geometry fixture.
    fn folded_state(menu: &MenuTree<()>) -> MenuState {
        let (size, positions, sizes) = layout_of(menu, None);
        MenuState::new(MenuBounds {
            child_positions: positions,
            child_sizes: sizes,
            children_bounds: Rectangle::new(Point::ORIGIN, size),
            parent_bounds: Rectangle::default(),
            check_bounds: Rectangle::default(),
            offset_bounds: Rectangle::default(),
        })
    }

    fn relayout(state: &mut MenuState, menu: &MenuTree<()>) {
        let renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut tree: Vec<Tree> = (0..8).map(|_| Tree::empty()).collect();
        state.relayout(
            menu,
            &renderer,
            ItemWidth::Uniform(360),
            ItemHeight::Dynamic(40),
            &mut tree,
            16,
        );
    }

    /// While a group closes, its rows no longer count as showing, but the
    /// rows below it, moved up over them, still do: the pointer and the
    /// keys keep them. Only the group's own rows are cut to the part that
    /// shows.
    #[test]
    fn only_the_closing_groups_rows_are_hidden_and_cut() {
        let menu = copy_menu();
        let mut state = folded_state(&menu);
        state.unfolded = None;
        let mut reveal = unfold::Reveal::fold(1, 2, 64.0, 20.0);
        reveal.advance(Instant::now());
        state.reveal = Some(reveal);
        state.index = Some(4);
        relayout(&mut state, &menu);
        assert!(!state.shows(2) && !state.shows(3), "closing rows");
        assert!(state.shows(4), "Paste");
        assert_eq!(state.index, Some(4), "Paste stays chosen");
        assert!(state.moving(2) && state.moving(3));
        assert!(!state.moving(1) && !state.moving(4));
    }
    #[test]
    fn reveal_slice_keeps_the_visible_row_below_hidden_members() {
        let menu = copy_menu();
        let mut state = folded_state(&menu);
        state.unfolded = Some(1);
        state.reveal = Some(unfold::Reveal::unfold(1, 2, 64.0, 10.0));
        relayout(&mut state, &menu);
        state.menu_bounds.children_bounds.y = 0.0;
        state.scroll_offset = -90.0;
        let slice = state.slice(
            Size::new(360.0, 10.0),
            Vector::ZERO,
            ItemHeight::Dynamic(40),
        );
        assert_eq!((slice.start_index, slice.end_index), (4, 4));
        assert_eq!(state.row_at(Point::new(100.0, 5.0)), Some(4));
    }

    #[test]
    fn reveal_clip_survives_the_first_member_scrolling_away() {
        let menu = copy_menu();
        let mut state = folded_state(&menu);
        state.unfolded = Some(1);
        state.reveal = Some(unfold::Reveal::unfold(1, 2, 64.0, 48.0));
        relayout(&mut state, &menu);
        state.scroll_offset = -110.0;
        let node = Node::new(Size::new(360.0, 156.0));
        let (_, clip) = state
            .reveal_clip(Layout::new(&node))
            .expect("remaining member needs a clip");
        assert_eq!(clip.y, -38.0);
        assert_eq!(clip.height, 48.0);
    }
}
