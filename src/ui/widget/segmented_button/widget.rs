// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/segmented_button/widget.rs

use super::model::{Entity, Model, Selectable};
use super::nav_drag::{self, Change, NavState};
use super::tab_drag::{self, TabState};
use super::{InsertPosition, NavDrop, ReorderEvent};
use crate::ui::Renderer;
use crate::ui::dnd::LocalPayload;
use crate::ui::shell::runner::{WindowingSystem, windowing_system};
use crate::ui::theme::SegmentedButton as Style;
use crate::ui::widget::menu::{
    self, CloseCondition, ItemHeight, ItemWidth, MenuBarState, PathHighlight, menu_roots_children,
    menu_roots_diff,
};
use crate::ui::widget::{Icon, icon};

use crate::ui::Element;
use derive_setters::Setters;
use iced::touch::Finger;
use iced::{
    Alignment, Background, Color, Event, Length, Padding, Rectangle, Size, Task, Vector, alignment,
    keyboard, mouse, touch, window,
};
use iced_core::mouse::ScrollDelta;
use iced_core::text::{self, LineHeight, Renderer as TextRenderer, Shaping, Wrapping};
use iced_core::widget::operation::Focusable;
use iced_core::widget::{self, Tree, operation, tree};
use iced_core::{
    Border, Clipboard, Layout, Point, Renderer as IcedRenderer, Shadow, Shell, Text, Widget,
    layout, renderer,
};
use iced_runtime::{Action, task};
use slotmap::{Key, SecondaryMap};
use std::cell::{Cell, LazyCell};
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ui::convert::{ToColor, ToRadius};

/// The message for a drop target changing: the button and whether the drop is
/// a file drop, or `None` when nothing is hovered
type OnDropHint<Message> = Box<dyn Fn(Option<(Entity, bool)>) -> Message + 'static>;
type CanPin = Box<dyn Fn(&Path) -> bool + 'static>;

type Plain = iced_core::text::paragraph::Plain<
    <crate::ui::Renderer as iced_core::text::Renderer>::Paragraph,
>;

thread_local! {
    // Prevents two segmented buttons from being focused at the same time.
    static LAST_FOCUS_UPDATE: LazyCell<Cell<Instant>> = LazyCell::new(|| Cell::new(Instant::now()));
}

/// How opaque a sidebar entry is drawn while it is dragged.
const DRAGGED_ALPHA: f32 = 0.4;

// Under `earth_files` so `RUST_LOG=earth_files=trace` reaches it.
const TAB_REORDER_LOG_TARGET: &str = "earth_files::widget::tab_reorder";

/// A command that focuses a segmented item stored in a widget.
pub fn focus<Message: 'static>(id: Id) -> Task<Message> {
    task::effect(Action::Widget(Box::new(operation::focusable::focus(id.0))))
}

pub enum ItemBounds {
    Button(Entity, Rectangle),
    Divider(Rectangle, bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DropSide {
    Before,
    After,
}

impl From<DropSide> for InsertPosition {
    fn from(side: DropSide) -> Self {
        match side {
            DropSide::Before => InsertPosition::Before,
            DropSide::After => InsertPosition::After,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DropHint {
    entity: Entity,
    side: DropSide,
}

/// Isolates variant-specific behaviors from [`SegmentedButton`].
pub trait SegmentedVariant {
    const VERTICAL: bool;

    /// Get the appearance for this variant of the widget.
    fn variant_appearance(
        theme: &crate::ui::Theme,
        style: &crate::ui::theme::SegmentedButton,
    ) -> super::Appearance;

    /// Calculates the bounds for visible buttons.
    fn variant_bounds<'b>(
        &'b self,
        state: &'b LocalState,
        bounds: Rectangle,
    ) -> Box<dyn Iterator<Item = ItemBounds> + 'b>;

    /// Calculates the layout of this variant.
    fn variant_layout(
        &self,
        state: &mut LocalState,
        renderer: &crate::ui::Renderer,
        limits: &layout::Limits,
    ) -> Size;
}

/// A conjoined group of items that function together as a button.
#[derive(Setters)]
#[must_use]
pub struct SegmentedButton<'a, Variant, SelectionMode, Message: Clone + 'static>
where
    Model<SelectionMode>: Selectable,
    SelectionMode: Default,
{
    /// The model borrowed from the application create this widget.
    #[setters(skip)]
    pub(super) model: &'a Model<SelectionMode>,
    /// iced widget ID
    pub(super) id: Id,
    /// The icon used for the close button.
    pub(super) close_icon: Icon,
    /// Scrolling switches focus between tabs.
    pub(super) scrollable_focus: bool,
    /// Show the close icon only when item is hovered.
    pub(super) show_close_icon_on_hover: bool,
    /// Padding of the whole widget.
    #[setters(into)]
    pub(super) padding: Padding,
    /// Whether to place dividers between buttons.
    pub(super) dividers: bool,
    /// Alignment of button contents.
    pub(super) button_alignment: Alignment,
    /// Padding around a button.
    pub(super) button_padding: [u16; 4],
    /// Desired height of a button.
    pub(super) button_height: u16,
    /// Spacing between icon and text in button.
    pub(super) button_spacing: u16,
    /// Maximum width of a button.
    pub(super) maximum_button_width: u16,
    /// Minimum width of a button.
    pub(super) minimum_button_width: u16,
    /// Spacing for each indent.
    pub(super) indent_spacing: u16,
    /// Desired font for active tabs.
    pub(super) font_active: crate::ui::font::Font,
    /// Desired font for hovered tabs.
    pub(super) font_hovered: crate::ui::font::Font,
    /// Desired font for inactive tabs.
    pub(super) font_inactive: crate::ui::font::Font,
    /// Size of the font.
    pub(super) font_size: f32,
    /// Desired width of the widget.
    pub(super) width: Length,
    /// Desired height of the widget.
    pub(super) height: Length,
    /// Desired spacing between items.
    pub(super) spacing: u16,
    /// LineHeight of the font.
    pub(super) line_height: LineHeight,
    /// Style to draw the widget in.
    #[setters(into)]
    pub(super) style: Style,
    /// The context menu to display when a context is activated
    #[setters(skip)]
    pub(super) context_menu: Option<Vec<menu::Tree<Message>>>,
    /// Emits the ID of the item that was activated.
    #[setters(skip)]
    pub(super) on_activate: Option<Box<dyn Fn(Entity) -> Message + 'static>>,
    #[setters(skip)]
    pub(super) on_close: Option<Box<dyn Fn(Entity) -> Message + 'static>>,
    #[setters(skip)]
    pub(super) on_context: Option<Box<dyn Fn(Entity) -> Message + 'static>>,
    #[setters(skip)]
    pub(super) on_middle_press: Option<Box<dyn Fn(Entity) -> Message + 'static>>,
    // `on_file_drop` below needs no mime list (`ui::dnd` negotiates those), no
    // drop action (move or copy is decided by the modifier at the drop), and
    // no enter/leave pair: this widget tracks the hovered destination itself
    // in `LocalState::file_drop_target` and draws it with the hover style.
    #[setters(skip)]
    pub(super) tab_drag: Option<TabDragSource<Message>>,
    #[setters(skip)]
    pub(super) on_drop_hint: Option<OnDropHint<Message>>,
    /// What to do when *files* are dropped on one of these buttons, as opposed
    /// to a tab being dragged among them. `None` for a button that is not a
    /// destination, which is also how the drop hint knows not to light it up.
    #[setters(skip)]
    pub(super) on_file_drop: Option<Box<dyn Fn(Entity) -> Option<Message> + 'static>>,
    /// What to do when a file drag is held over one of these buttons for
    /// [`crate::ui::dnd::HOVER_OPEN`]: switch to the tab, show the place.
    /// Only buttons `on_file_drop` accepts are held over, and `None` opens
    /// nothing: the tab or place already showing, which still takes a drop.
    #[setters(skip)]
    pub(super) on_drag_hover_open: Option<Box<dyn Fn(Entity) -> Option<Message> + 'static>>,

    #[setters(skip)]
    pub(super) on_reorder: Option<Box<dyn Fn(ReorderEvent) -> Message + 'static>>,
    /// Which entries a sidebar lets the user drag to a new place; see
    /// [`Self::reorderable`].
    #[setters(skip)]
    pub(super) reorderable: Option<Box<dyn Fn(Entity) -> bool + 'static>>,
    /// Which folders dragged from the file view can be pinned.
    #[setters(skip)]
    pub(super) can_pin: Option<CanPin>,
    #[setters(skip)]
    pub(super) on_nav_drop: Option<Box<dyn Fn(NavDrop) -> Message + 'static>>,
    /// The widget sits at the top of a vertical scrollable, which is how a
    /// drag's window coordinates are turned into its own; see
    /// [`Self::to_content`].
    pub(super) at_scrollable_top: bool,
    #[setters(skip)]
    window_id: window::Id,
    positioner: crate::ui::surface::Positioner,
    #[setters(skip)]
    pub(crate) on_surface_action:
        Option<Arc<dyn Fn(crate::ui::surface::Action<Message>) -> Message + Send + Sync + 'static>>,

    /// Defines the implementation of this struct
    variant: PhantomData<Variant>,
}

impl<'a, Variant, SelectionMode, Message: Clone + 'static>
    SegmentedButton<'a, Variant, SelectionMode, Message>
where
    Self: SegmentedVariant,
    Model<SelectionMode>: Selectable,
    SelectionMode: Default,
{
    #[inline]
    pub fn new(model: &'a Model<SelectionMode>) -> Self {
        Self {
            model,
            id: Id::unique(),
            close_icon: icon::from_name("window-close-symbolic").size(16).icon(),
            scrollable_focus: false,
            show_close_icon_on_hover: false,
            button_alignment: Alignment::Start,
            padding: Padding::from(0.0),
            dividers: false,
            button_padding: [0, 0, 0, 0],
            button_height: 32,
            button_spacing: 0,
            minimum_button_width: u16::MIN,
            maximum_button_width: u16::MAX,
            indent_spacing: 16,
            font_active: crate::ui::font::semibold(),
            font_hovered: crate::ui::font::default(),
            font_inactive: crate::ui::font::default(),
            font_size: 14.0,
            height: Length::Shrink,
            width: Length::Fill,
            spacing: 0,
            line_height: LineHeight::default(),
            style: Style::default(),
            context_menu: None,
            on_activate: None,
            on_close: None,
            on_context: None,
            on_middle_press: None,
            variant: PhantomData,
            tab_drag: None,
            on_drop_hint: None,
            on_file_drop: None,
            on_drag_hover_open: None,
            on_reorder: None,
            reorderable: None,
            can_pin: None,
            on_nav_drop: None,
            at_scrollable_top: false,
            window_id: crate::ui::window::reserved(),
            positioner: crate::ui::surface::Positioner::default(),
            on_surface_action: None,
        }
    }

    fn update_entity_paragraph(&self, state: &mut LocalState, key: Entity) {
        if let Some(text) = self.model.text.get(key) {
            let font = if self.button_is_focused(state, key)
                || state.show_context == Some(key)
                || self.model.is_active(key)
            {
                self.font_active
            } else if self.button_is_hovered(state, key) {
                self.font_hovered
            } else {
                self.font_inactive
            };

            let mut hasher = DefaultHasher::new();
            text.hash(&mut hasher);
            font.hash(&mut hasher);
            let text_hash = hasher.finish();

            if let Some(prev_hash) = state.text_hashes.insert(key, text_hash)
                && prev_hash == text_hash
            {
                return;
            }

            if let Some(paragraph) = state.paragraphs.get_mut(key) {
                let text = Text {
                    content: text.as_ref(),
                    size: iced::Pixels(self.font_size),
                    bounds: Size::INFINITE,
                    font,
                    align_x: text::Alignment::Left,
                    align_y: alignment::Vertical::Center,
                    shaping: Shaping::Advanced,
                    wrapping: Wrapping::None,
                    line_height: self.line_height,
                };
                paragraph.update(text);
            } else {
                let text = Text {
                    content: text.to_string(),
                    size: iced::Pixels(self.font_size),
                    bounds: Size::INFINITE,
                    font,
                    align_x: text::Alignment::Left,
                    align_y: alignment::Vertical::Center,
                    shaping: Shaping::Advanced,
                    wrapping: Wrapping::None,
                    line_height: self.line_height,
                };
                state.paragraphs.insert(key, Plain::new(text));
            }
        }
    }

    pub fn context_menu(mut self, context_menu: Option<Vec<menu::Tree<Message>>>) -> Self
    where
        Message: Clone + 'static,
    {
        self.context_menu = context_menu;

        if let Some(ref mut context_menu) = self.context_menu {
            context_menu.iter_mut().for_each(menu::Tree::set_index);
        }

        self
    }

    /// Emitted when a tab is pressed.
    pub fn on_activate<T>(mut self, on_activate: T) -> Self
    where
        T: Fn(Entity) -> Message + 'static,
    {
        self.on_activate = Some(Box::new(on_activate));
        self
    }

    /// Emitted when a tab close button is pressed.
    pub fn on_close<T>(mut self, on_close: T) -> Self
    where
        T: Fn(Entity) -> Message + 'static,
    {
        self.on_close = Some(Box::new(on_close));
        self
    }

    /// Emitted when a button is right-clicked.
    pub fn on_context<T>(mut self, on_context: T) -> Self
    where
        T: Fn(Entity) -> Message + 'static,
    {
        self.on_context = Some(Box::new(on_context));
        self
    }

    /// Emitted when the middle mouse button is pressed on a button.
    pub fn on_middle_press<T>(mut self, on_middle_press: T) -> Self
    where
        T: Fn(Entity) -> Message + 'static,
    {
        self.on_middle_press = Some(Box::new(on_middle_press));
        self
    }

    /// Enable drag-and-drop support for tabs using the provided payload builder.
    pub fn enable_tab_drag(mut self, mime: String) -> Self {
        self.tab_drag = Some(TabDragSource::new(mime));
        self
    }

    /// Receive drop hint updates during drag-and-drop.
    pub fn on_drop_hint(
        mut self,
        callback: impl Fn(Option<(Entity, bool)>) -> Message + 'static,
    ) -> Self {
        self.on_drop_hint = Some(Box::new(callback));
        self
    }

    /// Make these buttons destinations for a file drag.
    ///
    /// The callback answers for one button at a time: `Some(message)` makes it a
    /// destination. Return `None` for the current tab, a bookmark pointing at the
    /// directory already on screen, or anything that cannot be pasted into.
    /// The button then neither lights up under the drag nor accepts the drop.
    ///
    /// This is a *destination* only. The tab drag this widget can start is a
    /// separate feature that happens to ride the same `wl_data_device`, and the
    /// two are told apart by [`crate::ui::dnd::Drag::files`].
    pub fn on_file_drop<T>(mut self, on_file_drop: T) -> Self
    where
        T: Fn(Entity) -> Option<Message> + 'static,
    {
        self.on_file_drop = Some(Box::new(on_file_drop));
        self
    }

    /// Emit a message when a file drag is held over a button that accepts it
    /// (see [`Self::on_file_drop`]) for [`crate::ui::dnd::HOVER_OPEN`], if
    /// there is one for that button.
    pub fn on_drag_hover_open<T>(mut self, on_drag_hover_open: T) -> Self
    where
        T: Fn(Entity) -> Option<Message> + 'static,
    {
        self.on_drag_hover_open = Some(Box::new(on_drag_hover_open));
        self
    }

    /// Emit a message when a tab drag is dropped inside this widget.
    pub fn on_reorder(mut self, callback: impl Fn(ReorderEvent) -> Message + 'static) -> Self {
        self.on_reorder = Some(Box::new(callback));
        self
    }

    /// Let the user drag the entries `reorderable` answers `true` for to a
    /// new place among themselves, as in the sidebar. A drag of one starts
    /// once the pointer has moved 8 px, and opens a gap half an entry tall
    /// where it would land; how it ends goes to [`Self::on_nav_drop`].
    ///
    /// Vertical only: the gap is drawn by the vertical layout.
    pub fn reorderable(mut self, reorderable: impl Fn(Entity) -> bool + 'static) -> Self {
        self.reorderable = Some(Box::new(reorderable));
        self.tab_drag = Some(TabDragSource {
            payload: LocalPayload::NavEntry,
            ..TabDragSource::new(String::from(crate::ui::dnd::NAV_DRAG_MIME))
        });
        self
    }

    /// Let a single folder dragged from this app's file view be pinned, when
    /// `can_pin` answers `true` for it: near the top or bottom edge of an
    /// entry that can move, a gap opens and a drop there pins it.
    pub fn can_pin(mut self, can_pin: impl Fn(&Path) -> bool + 'static) -> Self {
        self.can_pin = Some(Box::new(can_pin));
        self
    }

    /// Emitted when a drag of an entry, or of a folder to pin, ends in a
    /// change to the sidebar; see [`NavDrop`].
    pub fn on_nav_drop(mut self, on_nav_drop: impl Fn(NavDrop) -> Message + 'static) -> Self {
        self.on_nav_drop = Some(Box::new(on_nav_drop));
        self
    }

    /// Set the pointer distance threshold before a drag is started.
    pub fn tab_drag_threshold(mut self, threshold: f32) -> Self {
        if let Some(tab_drag) = self.tab_drag.as_mut() {
            tab_drag.threshold = threshold.max(1.0);
        }
        self
    }

    fn reorder_event_for_drop(&self, state: &LocalState, target: Entity) -> Option<ReorderEvent> {
        let dragged = state.dragging_tab?;
        if dragged == target
            || !self.model.contains_item(dragged)
            || !self.model.contains_item(target)
        {
            return None;
        }
        let position = state
            .drop_hint
            .filter(|hint| hint.entity == target)
            .map(|hint| InsertPosition::from(hint.side))
            .unwrap_or_else(|| self.default_insert_position(dragged, target));
        Some(ReorderEvent {
            dragged,
            target,
            position,
        })
    }

    fn default_insert_position(&self, dragged: Entity, target: Entity) -> InsertPosition {
        let len = self.model.len();
        let target_pos = self
            .model
            .position(target)
            .map(|pos| pos as usize)
            .unwrap_or(len);
        let from_pos = self
            .model
            .position(dragged)
            .map(|pos| pos as usize)
            .unwrap_or(target_pos);
        if from_pos < target_pos {
            InsertPosition::After
        } else {
            InsertPosition::Before
        }
    }

    /// Check if an item is enabled.
    fn is_enabled(&self, key: Entity) -> bool {
        self.model.items.get(key).is_some_and(|item| item.enabled)
    }

    /// Item the previous item in the widget.
    fn focus_previous(&mut self, state: &mut LocalState, shell: &mut Shell<'_, Message>) {
        match state.focused_item {
            Item::Tab(entity) => {
                let mut keys = self.iterate_visible_tabs(state).rev();

                while let Some(key) = keys.next() {
                    if key == entity {
                        for key in keys {
                            // Skip disabled buttons.
                            if !self.is_enabled(key) {
                                continue;
                            }

                            state.focused_item = Item::Tab(key);
                            shell.capture_event();
                            return;
                        }

                        break;
                    }
                }

                if self.prev_tab_sensitive(state) {
                    state.focused_item = Item::PrevButton;
                    shell.capture_event();
                    return;
                }
            }

            Item::NextButton => {
                if let Some(last) = self.last_tab(state) {
                    state.focused_item = Item::Tab(last);
                    shell.capture_event();
                    return;
                }
            }

            Item::None => {
                if self.next_tab_sensitive(state) {
                    state.focused_item = Item::NextButton;
                    shell.capture_event();
                    return;
                } else if let Some(last) = self.last_tab(state) {
                    state.focused_item = Item::Tab(last);
                    shell.capture_event();
                    return;
                }
            }

            Item::PrevButton | Item::Set => (),
        }

        state.focused_item = Item::None;
    }

    /// Item the next item in the widget.
    fn focus_next(&mut self, state: &mut LocalState, shell: &mut Shell<'_, Message>) {
        match state.focused_item {
            Item::Tab(entity) => {
                let mut keys = self.iterate_visible_tabs(state);
                while let Some(key) = keys.next() {
                    if key == entity {
                        for key in keys {
                            // Skip disabled buttons.
                            if !self.is_enabled(key) {
                                continue;
                            }

                            state.focused_item = Item::Tab(key);
                            shell.capture_event();
                            return;
                        }

                        break;
                    }
                }

                if self.next_tab_sensitive(state) {
                    state.focused_item = Item::NextButton;
                    shell.capture_event();
                    return;
                }
            }

            Item::PrevButton => {
                if let Some(first) = self.first_tab(state) {
                    state.focused_item = Item::Tab(first);
                    shell.capture_event();
                    return;
                }
            }

            Item::None => {
                if self.prev_tab_sensitive(state) {
                    state.focused_item = Item::PrevButton;
                    shell.capture_event();
                    return;
                } else if let Some(first) = self.first_tab(state) {
                    state.focused_item = Item::Tab(first);
                    shell.capture_event();
                    return;
                }
            }

            Item::NextButton | Item::Set => (),
        }

        state.focused_item = Item::None;
    }

    fn iterate_visible_tabs<'b>(
        &'b self,
        state: &LocalState,
    ) -> impl DoubleEndedIterator<Item = Entity> + 'b {
        self.model
            .order
            .iter()
            .copied()
            .skip(state.buttons_offset)
            .take(state.buttons_visible)
    }

    fn first_tab(&self, state: &LocalState) -> Option<Entity> {
        self.model.order.get(state.buttons_offset).copied()
    }

    fn last_tab(&self, state: &LocalState) -> Option<Entity> {
        self.model
            .order
            .get(state.buttons_offset + state.buttons_visible)
            .copied()
    }

    #[allow(clippy::unused_self)]
    fn prev_tab_sensitive(&self, state: &LocalState) -> bool {
        state.buttons_offset > 0
    }

    fn next_tab_sensitive(&self, state: &LocalState) -> bool {
        state.buttons_offset < self.model.order.len() - state.buttons_visible
    }

    pub(super) fn button_dimensions(
        &self,
        state: &mut LocalState,
        font: crate::ui::font::Font,
        button: Entity,
    ) -> (f32, f32) {
        let mut width = 0.0f32;
        let mut icon_spacing = 0.0f32;

        // Add text to measurement if text was given.
        if let Some((text, entry)) = self
            .model
            .text
            .get(button)
            .zip(state.paragraphs.entry(button))
            && !text.is_empty()
        {
            icon_spacing = f32::from(self.button_spacing);
            let paragraph = entry.or_insert_with(|| {
                Plain::new(Text {
                    content: text.to_string(),
                    size: iced::Pixels(self.font_size),
                    bounds: Size::INFINITE,
                    font,
                    align_x: text::Alignment::Left,
                    align_y: alignment::Vertical::Center,
                    shaping: Shaping::Advanced,
                    wrapping: Wrapping::default(),
                    line_height: self.line_height,
                })
            });

            let size = paragraph.min_bounds();
            width += size.width;
        }

        // Add indent to measurement if found.
        if let Some(indent) = self.model.indent(button) {
            width = f32::from(indent).mul_add(f32::from(self.indent_spacing), width);
        }

        // Add icon to measurement if icon was given.
        if let Some(icon) = self.model.icon(button) {
            width += f32::from(icon.size) + icon_spacing;
        } else if self.model.is_active(button) {
            // Add selection icon measurements when widget is a selection widget.
            if let crate::ui::theme::SegmentedButton::Control = self.style {
                width += 16.0 + icon_spacing;
            }
        }

        // Add close button to measurement if found.
        if self.model.is_closable(button) {
            width += f32::from(self.close_icon.size) + f32::from(self.button_spacing);
        }

        // Add button padding to the max size found
        width += f32::from(self.button_padding[0]) + f32::from(self.button_padding[2]);
        width = width.min(f32::from(self.maximum_button_width));

        (width, f32::from(self.button_height))
    }

    pub(super) fn max_button_dimensions(
        &self,
        state: &mut LocalState,
        renderer: &Renderer,
    ) -> (f32, f32) {
        let mut width = 0.0f32;
        let mut height = 0.0f32;
        let font = renderer.default_font();

        for key in self.model.order.iter().copied() {
            let (button_width, button_height) = self.button_dimensions(state, font, key);

            state.internal_layout.push((
                Size::new(button_width, button_height),
                Size::new(
                    button_width
                        - f32::from(self.button_padding[0])
                        - f32::from(self.button_padding[2]),
                    button_height,
                ),
            ));

            height = height.max(button_height);
            width = width.max(button_width);
        }

        for (size, actual) in &mut state.internal_layout {
            size.height = height;
            actual.height = height;
        }

        (width, height)
    }

    fn button_is_focused(&self, state: &LocalState, key: Entity) -> bool {
        state.focused.is_some()
            && self.on_activate.is_some()
            && Item::Tab(key) == state.focused_item
    }

    fn button_is_hovered(&self, state: &LocalState, key: Entity) -> bool {
        // The button a file drag is over is drawn hovered: during a Wayland
        // drag no pointer event reaches iced at all, so `state.hovered` alone
        // would leave the whole bar looking untouched while a drag crossed it.
        if state.file_drop_target == Some(key) {
            return true;
        }
        self.on_activate.is_some() && state.hovered == Item::Tab(key)
    }

    fn button_is_pressed(&self, state: &LocalState, key: Entity) -> bool {
        state.pressed_item == Some(Item::Tab(key))
    }

    fn emit_drop_hint(&self, shell: &mut Shell<'_, Message>, hint: Option<DropHint>) {
        if let Some(on_hint) = self.on_drop_hint.as_ref() {
            let mapped = hint.map(|hint| (hint.entity, matches!(hint.side, DropSide::After)));
            shell.publish(on_hint(mapped));
        }
    }

    fn drop_hint_for_position(
        &self,
        state: &LocalState,
        bounds: Rectangle,
        cursor: Point,
    ) -> Option<DropHint> {
        let _ = state.dragging_tab?;

        self.variant_bounds(state, bounds)
            .filter_map(|item| match item {
                ItemBounds::Button(entity, rect) if rect.contains(cursor) => Some((entity, rect)),
                _ => None,
            })
            .map(|(entity, rect)| {
                let before = if Self::VERTICAL {
                    cursor.y < rect.center_y()
                } else {
                    cursor.x < rect.center_x()
                };
                DropHint {
                    entity,
                    side: if before {
                        DropSide::Before
                    } else {
                        DropSide::After
                    },
                }
            })
            .next()
    }

    /// The rows' tops as laid out with no gap, relative to the widget's top,
    /// in model order. Mirrors the vertical `variant_bounds`.
    pub(super) fn nav_tops(&self, state: &LocalState) -> Vec<f32> {
        let spacing = f32::from(self.spacing);
        let mut y = 0.0;
        let mut tops = Vec::with_capacity(self.model.order.len());
        for (nth, key) in self.model.order.iter().copied().enumerate() {
            if nth > 0 && self.model.divider_above(key).unwrap_or(false) {
                y += 1.0 + spacing;
            }
            tops.push(y);
            y += state
                .internal_layout
                .get(nth)
                .map_or(f32::from(self.button_height), |(size, _)| size.height)
                + spacing;
        }
        tops
    }

    /// A fingerprint of the labels in order; see [`nav_drag::labels`].
    pub(super) fn nav_labels(&self) -> u64 {
        nav_drag::labels(self.model.order.iter().map(|&key| self.model.text(key)))
    }

    fn nav_row_height(&self, state: &LocalState) -> f32 {
        state
            .internal_layout
            .first()
            .map_or(f32::from(self.button_height), |(size, _)| size.height)
    }

    /// The gap is half an entry tall.
    fn nav_gap_height(&self, state: &LocalState) -> f32 {
        self.nav_row_height(state) / 2.0
    }

    /// The rows that can move, by position.
    fn nav_run(&self) -> Vec<usize> {
        let Some(reorderable) = self.reorderable.as_ref() else {
            return Vec::new();
        };
        self.model
            .order
            .iter()
            .enumerate()
            .filter(|(_, key)| reorderable(**key))
            .map(|(nth, _)| nth)
            .collect()
    }

    /// The entry a gap opens above, if it can move; `None` for the gap after
    /// the last entry that can.
    fn nav_before(&self, gap: usize) -> Option<Entity> {
        let reorderable = self.reorderable.as_ref()?;
        self.model
            .order
            .get(gap)
            .copied()
            .filter(|key| reorderable(*key))
    }

    /// A drag's position, in window coordinates, in this widget's own.
    ///
    /// Inside a scrollable a widget is laid out as if nothing were scrolled,
    /// and the scrollable hands its content a viewport moved by the scroll.
    /// At the top of the scrollable the widget's top is the scrollable's, so
    /// the scroll is how far the viewport's top is below it.
    fn to_content(&self, state: &LocalState, bounds: Rectangle, point: Point) -> Point {
        if self.at_scrollable_top {
            Point::new(point.x, point.y + state.nav.viewport.y - bounds.y)
        } else {
            point
        }
    }

    /// Whether a drag at `point`, in window coordinates, is over the sidebar:
    /// the whole scrollable it sits in, entries or not.
    fn over_sidebar(&self, state: &LocalState, bounds: Rectangle, point: Point) -> bool {
        if self.at_scrollable_top {
            let viewport = state.nav.viewport;
            Rectangle {
                y: bounds.y,
                ..viewport
            }
            .contains(point)
        } else {
            bounds.contains(point)
        }
    }

    /// Open the gap above row `gap`, or close it, and lay out again when the
    /// sidebar's height changed.
    fn set_nav_gap(
        &self,
        state: &mut LocalState,
        gap: Option<usize>,
        shell: &mut Shell<'_, Message>,
    ) {
        let height = self.nav_gap_height(state);
        if state.nav.set_gap(gap, height) {
            shell.invalidate_layout();
            shell.request_redraw();
        }
    }

    /// The folder a file drag carries, if it is the one folder, dragged from
    /// this app, that can be pinned. Worked out once per drag.
    fn pinnable(&self, state: &mut LocalState, drag: &crate::ui::dnd::Drag) -> Option<PathBuf> {
        let can_pin = self.can_pin.as_ref()?;
        self.on_nav_drop.as_ref()?;
        if let Some((id, path)) = &state.nav.pin_checked
            && *id == drag.id
        {
            return path.clone();
        }
        let path = match crate::ui::dnd::local_payload() {
            Some(LocalPayload::Paths(paths)) if paths.len() == 1 => paths.into_iter().next(),
            _ => None,
        }
        .filter(|path| can_pin(path));
        state.nav.pin_checked = Some((drag.id, path.clone()));
        path
    }

    /// Advance a live drag of one of the sidebar's own entries.
    ///
    /// Like [`Self::poll_tab_drag`], and for the same reason, polled on the
    /// redraw it requests. Over the sidebar, the gap follows the pointer; a
    /// drop there moves the entry to it. A drop anywhere else in the window,
    /// or outside it where the compositor says the drop was made, unpins it.
    /// A cancel, such as Esc, leaves everything where it was.
    fn poll_nav_drag(
        &self,
        state: &mut LocalState,
        bounds: Rectangle,
        shell: &mut Shell<'_, Message>,
    ) {
        let Some(dragged) = state.dragging_tab else {
            return;
        };
        let drag = crate::ui::dnd::drag().filter(|drag| {
            !drag.files && crate::ui::dnd::local_payload() == Some(LocalPayload::NavEntry)
        });
        let Some(drag) = drag else {
            // `ui::dnd` lost the drag underneath us: put everything back.
            state.dragging_tab = None;
            self.set_nav_gap(state, None, shell);
            return;
        };

        if !drag.ended {
            shell.request_redraw();
        }

        let over = drag
            .position
            .map(|(x, y)| Point::new(x, y))
            .filter(|point| self.over_sidebar(state, bounds, *point));
        let tops = self.nav_tops(state);
        let gap = over.and_then(|point| {
            nav_drag::nearest_gap(
                &self.nav_run(),
                &tops,
                self.nav_row_height(state),
                state.nav.gap(),
                self.nav_gap_height(state),
                self.to_content(state, bounds, point).y - bounds.y,
            )
        });
        self.set_nav_gap(state, gap, shell);

        if !drag.ended {
            return;
        }

        if let Some(on_nav_drop) = self.on_nav_drop.as_ref()
            && let Some(from) = self.model.position(dragged).map(usize::from)
        {
            let now = Instant::now();
            if drag.dropped && over.is_some() {
                if let Some(gap) = gap
                    && gap != from
                    && gap != from + 1
                {
                    state
                        .nav
                        .expect(Change::Move { from, gap }, &tops, self.nav_labels(), now);
                    shell.publish(on_nav_drop(NavDrop::Move {
                        dragged,
                        before: self.nav_before(gap),
                    }));
                }
            } else if (drag.dropped && drag.position.is_some()) || drag.performed {
                state
                    .nav
                    .expect(Change::Remove { from }, &tops, self.nav_labels(), now);
                shell.publish(on_nav_drop(NavDrop::Unpin(dragged)));
            }
        }

        // Nothing reads an entry drag's placeholder, and until the drop on
        // our surface is finished the compositor may keep the drag going;
        // see `finish_drop_unread`.
        if drag.dropped {
            crate::ui::dnd::finish_drop_unread();
        }
        state.dragging_tab = None;
        self.set_nav_gap(state, None, shell);
        crate::ui::dnd::end_drag();
        shell.request_redraw();
    }

    fn start_tab_drag(
        &self,
        state: &mut LocalState,
        entity: Entity,
        bounds: Rectangle,
        cursor: Point,
        clipboard: &mut dyn Clipboard,
    ) -> bool {
        let Some(tab_drag) = self.tab_drag.as_ref() else {
            return false;
        };

        log::trace!(
            target: TAB_REORDER_LOG_TARGET,
            "start_tab_drag requested entity={:?} cursor=({:.2},{:.2}) bounds=({:.2},{:.2},{:.2},{:.2}) threshold={}",
            entity,
            cursor.x,
            cursor.y,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            tab_drag.threshold
        );

        // `ui::dnd` speaks `wl_data_device` directly; the clipboard is not
        // involved.
        let _ = clipboard;
        state.tab_drag_candidate = None;

        if crate::ui::dnd::start_drag(&tab_drag.mime, tab_drag.payload.clone()) {
            state.dragging_tab = Some(entity);
            state.drop_hint = None;
            // The compositor keeps the release, so nothing else would let go
            // of the entry's pressed look while it is dragged.
            if self.reorderable.is_some() {
                state.pressed_item = None;
            }
            log::debug!(
                target: TAB_REORDER_LOG_TARGET,
                "tab drag started entity={entity:?}"
            );
            return true;
        }

        false
    }

    /// Advance a live tab drag from what `ui::dnd` has seen on the wire.
    ///
    /// Once the compositor hands the pointer to the data device, iced receives
    /// the whole drag through this poll and gets no pointer events. The redraw
    /// requested below is the only event that drives the poll.
    fn poll_tab_drag(
        &self,
        state: &mut LocalState,
        bounds: Rectangle,
        shell: &mut Shell<'_, Message>,
    ) {
        // `Self::poll_file_drop` or the file view handles drags carrying files.
        // Only a tab drag, which carries no file list, can reorder tabs.
        let Some(drag) = crate::ui::dnd::drag().filter(|drag| !drag.files) else {
            // `ui::dnd` dropped the drag underneath us (init failed, or another
            // widget finished it). Leave no dangling hint behind.
            state.dragging_tab = None;
            state.drop_hint = None;
            self.emit_drop_hint(shell, None);
            if self.uses_tab_gap(state) {
                let visible = self.visible_tabs(state);
                let (_, row_width) = self.tab_row(state, bounds);
                state
                    .tabs
                    .unhide(&visible, row_width, f32::from(self.spacing));
                shell.request_redraw();
            }
            return;
        };

        if !drag.ended {
            shell.request_redraw();
        }

        let point = drag.position.map(|(x, y)| Point::new(x, y));
        // A tab bar of tabs sharing its width takes the dragged tab out of the
        // row and opens a gap among the rest where it would land; see
        // `tab_drag`. Any other draws the drop line.
        let gap = self.uses_tab_gap(state).then(|| {
            let visible = self.visible_tabs(state);
            let shown = state.tabs.shown(&visible);
            let (row_x, row_width) = self.tab_row(state, bounds);
            let spacing = f32::from(self.spacing);
            let slot = point.filter(|point| bounds.contains(*point)).map(|point| {
                state
                    .tabs
                    .slot_for(shown.len(), row_width, spacing, point.x - row_x)
            });
            (visible, shown, row_width, spacing, slot)
        });
        let hint = match &gap {
            Some((_, shown, row_width, spacing, slot)) => {
                // Half as wide as a tab is with no gap.
                let width = tab_drag::tab_width(*row_width, *spacing, shown.len(), 0.0) / 2.0;
                if state.tabs.set_open(*slot, shown.len(), width) {
                    shell.request_redraw();
                }
                slot.and_then(|slot| hint_for_slot(shown, slot))
            }
            None => point.and_then(|point| self.drop_hint_for_position(state, bounds, point)),
        };

        if hint != state.drop_hint {
            state.drop_hint = hint;
            self.emit_drop_hint(shell, hint);
            shell.request_redraw();
        }

        if !drag.ended {
            return;
        }

        // With the tab out of the row, the slot it left is where it already is.
        let at_own_place = gap.as_ref().is_some_and(|(visible, _, _, _, slot)| {
            slot.is_some()
                && *slot
                    == visible
                        .iter()
                        .position(|entity| Some(*entity) == state.dragging_tab)
        });
        let mut reordered = false;
        if drag.dropped
            && !at_own_place
            && let Some(hint) = hint
            && let Some(on_reorder) = self.on_reorder.as_ref()
            && let Some(reorder) = self.reorder_event_for_drop(state, hint.entity)
        {
            if let Some((visible, _, row_width, spacing, _)) = gap.as_ref() {
                state
                    .tabs
                    .expect(visible.clone(), *row_width, *spacing, Instant::now());
            }
            reordered = true;
            log::debug!(
                target: TAB_REORDER_LOG_TARGET,
                "tab drop reorders {:?} {:?} {:?}",
                reorder.dragged,
                reorder.position,
                reorder.target
            );
            shell.publish(on_reorder(reorder));
        }

        // Nothing reads a tab drag's placeholder, wherever on our surface it
        // was dropped, and until that drop is finished the compositor may keep
        // the drag going; see `finish_drop_unread`.
        if drag.dropped {
            crate::ui::dnd::finish_drop_unread();
        }
        // The tab comes back where it was, unless the reorder about to happen
        // puts it in the gap; see `TabState::sync`.
        if !reordered && let Some((visible, _, row_width, spacing, _)) = gap.as_ref() {
            state.tabs.unhide(visible, *row_width, *spacing);
        }
        state.dragging_tab = None;
        state.drop_hint = None;
        self.emit_drop_hint(shell, None);
        crate::ui::dnd::end_drag();
        shell.request_redraw();
    }

    /// Whether this is a tab bar of tabs sharing its width, which opens a gap
    /// for a dragged tab; see `tab_drag`. A bar of tabs as wide as their
    /// labels keeps the drop line.
    pub(super) fn uses_tab_gap(&self, state: &LocalState) -> bool {
        !Self::VERTICAL
            && self.reorderable.is_none()
            && (Length::Shrink != self.width || state.collapsed)
    }

    /// The tabs on screen, in order: all of them, or one page of a paged bar.
    pub(super) fn visible_tabs(&self, state: &LocalState) -> Vec<Entity> {
        self.model
            .order
            .iter()
            .copied()
            .skip(state.buttons_offset)
            .take(state.buttons_visible)
            .collect()
    }

    /// What to clip the tabs to while they move on springs, which overshoot
    /// their places: their row, between the ‹ › buttons of a paged bar. `None`
    /// at rest, when every tab is inside it anyway.
    pub(super) fn tab_clip(&self, state: &LocalState, bounds: Rectangle) -> Option<Rectangle> {
        (self.uses_tab_gap(state) && state.tabs.is_active()).then(|| {
            let (x, width) = self.tab_row(state, bounds);
            Rectangle { x, width, ..bounds }
        })
    }

    /// Where the row of tabs starts within `bounds`, and how wide it is:
    /// between the ‹ › buttons of a paged bar.
    pub(super) fn tab_row(&self, state: &LocalState, bounds: Rectangle) -> (f32, f32) {
        let paging = f32::from(self.button_height);
        if state.collapsed {
            (bounds.x + paging, paging.mul_add(-2.0, bounds.width))
        } else {
            (bounds.x, bounds.width)
        }
    }

    /// Advance a live file drag over this bar, from what `ui::dnd` has seen.
    ///
    /// Like [`Self::poll_tab_drag`], this poll runs on the redraw it requests.
    /// That is its only trigger once the compositor hands the pointer to the
    /// data device and iced stops receiving pointer events. The two polls share
    /// only this mechanism and never both act: `Drag::files` selects one.
    fn poll_file_drop(
        &self,
        state: &mut LocalState,
        bounds: Rectangle,
        shell: &mut Shell<'_, Message>,
    ) {
        let Some(on_file_drop) = self.on_file_drop.as_ref() else {
            return;
        };
        let Some(drag) = crate::ui::dnd::drag().filter(|drag| drag.files) else {
            // Clear the highlight when there is no active file drag.
            if state.file_drop_target.take().is_some() {
                shell.request_redraw();
            }
            state.drag_hover = crate::ui::dnd::HoverOpen::default();
            // The gap of an entry drag is `poll_nav_drag`'s to close.
            if state.dragging_tab.is_none() {
                self.set_nav_gap(state, None, shell);
            }
            return;
        };

        if !drag.ended {
            shell.request_redraw();
        }

        // An ended drag stays readable until the next one starts (see
        // `ui::dnd::end_drag`), so widget visitation order does not matter.
        // Handle the end once, rather than on each subsequent pass, and leave
        // it alone after that: working out its gap and highlight again would
        // put them back on screen until the next drag. By `id`, because the
        // source's own `dnd_finished` or `cancelled` still bumps `generation`
        // after the drop.
        if drag.ended && state.file_drag_done == Some(drag.id) {
            return;
        }

        // `Drag::position` is in window coordinates; `to_content` takes them
        // to this widget's, which differ only inside a scrolled sidebar.
        let point = drag
            .position
            .map(|(x, y)| Point::new(x, y))
            .filter(|point| self.over_sidebar(state, bounds, *point))
            .map(|point| self.to_content(state, bounds, point));

        // Near the edge of an entry that can move, a folder that can be
        // pinned opens the gap, and the entry is not a destination.
        let pin = self.pinnable(state, &drag);
        let tops = self.nav_tops(state);
        let pin_gap = pin.as_ref().and(point).and_then(|point| {
            nav_drag::pin_gap(
                &self.nav_run(),
                &tops,
                self.nav_row_height(state),
                state.nav.gap(),
                self.nav_gap_height(state),
                point.y - bounds.y,
            )
        });
        self.set_nav_gap(state, pin_gap, shell);

        let target = point
            .filter(|point| pin_gap.is_none() && bounds.contains(*point))
            .and_then(|point| {
                self.variant_bounds(state, bounds)
                    .find_map(|item| match item {
                        ItemBounds::Button(entity, rect) if rect.contains(point) => Some(entity),
                        _ => None,
                    })
            })
            // A button the callback has no message for is not a destination,
            // so it must not be drawn as one either.
            .filter(|entity| on_file_drop(*entity).is_some());

        if target != state.file_drop_target {
            state.file_drop_target = target;
            shell.request_redraw();
        }

        if !drag.ended {
            // Held over a button long enough, it opens
            if let Some(on_drag_hover_open) = self.on_drag_hover_open.as_ref()
                && let Some(entity) = state.drag_hover.step(target, Instant::now())
                && let Some(message) = on_drag_hover_open(entity)
            {
                shell.publish(message);
            }
            return;
        }
        state.drag_hover = crate::ui::dnd::HoverOpen::default();
        state.file_drag_done = Some(drag.id);

        if drag.dropped
            && let Some(gap) = pin_gap
            && let Some(path) = pin
            && let Some(on_nav_drop) = self.on_nav_drop.as_ref()
        {
            state.nav.expect(
                Change::Add { gap },
                &tops,
                self.nav_labels(),
                Instant::now(),
            );
            shell.publish(on_nav_drop(NavDrop::Pin {
                path,
                before: self.nav_before(gap),
            }));
            // Pinning needs only the path already known, not the drop's
            // payload; see `finish_drop_unread`.
            crate::ui::dnd::finish_drop_unread();
        } else if drag.dropped
            && let Some(entity) = target
            && let Some(message) = on_file_drop(entity)
        {
            shell.publish(message);
        }

        self.set_nav_gap(state, None, shell);
        state.file_drop_target = None;
        // This bar ends the drag rather than leaving it to the file view,
        // because the bar is drawn in states with no file list, such as an
        // empty folder or the gallery. An unfinished drag blocks the next one.
        // `end_drag` is idempotent and leaves the dropped offer available for
        // the read that the published message is about to start.
        crate::ui::dnd::end_drag();
        shell.request_redraw();
    }

    pub fn with_positioner(mut self, positioner: crate::ui::surface::Positioner) -> Self {
        self.positioner = positioner;
        self
    }

    pub fn window_id(mut self, id: window::Id) -> Self {
        self.window_id = id;
        self
    }

    pub fn window_id_maybe(mut self, id: Option<window::Id>) -> Self {
        if let Some(id) = id {
            self.window_id = id;
        }
        self
    }

    pub fn on_surface_action(
        mut self,
        handler: impl Fn(crate::ui::surface::Action<Message>) -> Message + Send + Sync + 'static,
    ) -> Self {
        self.on_surface_action = Some(Arc::new(handler));
        self
    }

    #[allow(clippy::too_many_lines)]
    fn create_popup<'b>(
        &mut self,
        layout: Layout<'b>,
        view_cursor: mouse::Cursor,
        renderer: &'b Renderer,
        shell: &'b mut Shell<'_, Message>,
        viewport: &'b Rectangle,
        tree: &'b mut Tree,
    ) {
        let state = tree.state.downcast_mut::<LocalState>();
        let my_state = state.menu_state.clone();

        if self.window_id != crate::ui::window::none() {
            use crate::ui::surface::action::destroy_popup;
            use crate::ui::surface::{PopupSettings, Positioner};

            let Some(surface_action) = self.on_surface_action.as_ref() else {
                return;
            };

            let id = my_state.inner.with_data_mut(|state| {
                // A popup still collapsing has to go now rather than finish:
                // the menu about to be laid out shares its tree.
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
                window::Id::unique()
            });
            let Some(entity) = state.show_context else {
                return;
            };

            let Some((mut bounds, i)) = self
                .variant_bounds(state, layout.bounds())
                .filter_map(|item| match item {
                    ItemBounds::Button(entity, bounds) => Some((bounds, entity)),
                    _ => None,
                })
                .enumerate()
                .find_map(|(i, (bounds, e))| if e == entity { Some((bounds, i)) } else { None })
            else {
                return;
            };

            assert!(
                self.context_menu
                    .as_ref()
                    .is_none_or(|m| m[0].children.len() == self.model.len()),
                "model length must match the number of context menus"
            );
            let menu = self
                .context_menu
                .as_mut()
                .map(|m| m[0].children[i].clone())
                .unwrap();

            bounds.x = state.context_cursor.x;
            bounds.y = state.context_cursor.y;

            let mut popup_menu: menu::Menu<'static, _> = menu::Menu {
                tree: my_state.clone(),
                menu_roots: std::borrow::Cow::Owned(vec![menu]),
                bounds_expand: 0,
                menu_overlays_parent: false,
                close_condition: CloseCondition {
                    leave: false,
                    click_outside: true,
                    click_inside: true,
                },
                item_width: ItemWidth::Uniform(240),
                item_height: ItemHeight::Dynamic(40),
                bar_bounds: bounds,
                main_offset: 0,
                cross_offset: 0,
                root_bounds_list: vec![bounds],
                path_highlight: Some(PathHighlight::MenuActive),
                style: std::borrow::Cow::Borrowed(
                    &crate::ui::theme::menu_bar::MenuBarStyle::Default,
                ),
                position: Point::new(0., 0.),
                is_overlay: false,
                window_id: id,
                depth: 0,
                on_surface_action: self.on_surface_action.clone(),
            };

            menu::init_root_menu(
                &mut popup_menu,
                renderer,
                shell,
                view_cursor.position().unwrap(),
                viewport.size(),
                Vector::new(0., 0.),
                bounds,
                0.,
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
                                    width: 1,
                                    height: 1,
                                }
                            },
                            |r| Rectangle {
                                x: r.x as i32,
                                y: r.y as i32,
                                width: 1,
                                height: 1,
                            },
                        ),
                    match (state.horizontal_direction, state.vertical_direction) {
                        (menu::Direction::Positive, menu::Direction::Positive) => {
                            crate::ui::surface::PopupGravity::BottomRight
                        }
                        (menu::Direction::Positive, menu::Direction::Negative) => {
                            crate::ui::surface::PopupGravity::TopRight
                        }
                        (menu::Direction::Negative, menu::Direction::Positive) => {
                            crate::ui::surface::PopupGravity::BottomLeft
                        }
                        (menu::Direction::Negative, menu::Direction::Negative) => {
                            crate::ui::surface::PopupGravity::TopLeft
                        }
                    },
                )
            });

            let menu_node =
                popup_menu.layout(renderer, layout::Limits::NONE.min_width(1.).min_height(1.));
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
                    // replaced: see the `leaving` map.
                    animate: true,
                },
                Some(move || {
                    Element::from(
                        crate::ui::widget::container(popup_menu.clone()).center(Length::Fill),
                    )
                    .map(crate::ui::Action::App)
                }),
            )));
        }
    }
}

impl<Variant, SelectionMode, Message> Widget<Message, crate::ui::Theme, Renderer>
    for SegmentedButton<'_, Variant, SelectionMode, Message>
where
    Self: SegmentedVariant,
    Model<SelectionMode>: Selectable,
    SelectionMode: Default,
    Message: 'static + Clone,
{
    fn children(&self) -> Vec<Tree> {
        let mut children = Vec::new();

        // Assign the context menu's elements as this widget's children.
        if let Some(ref context_menu) = self.context_menu {
            let mut tree = Tree::empty();
            tree.state = tree::State::new(MenuBarState::default());
            tree.children = menu_roots_children(context_menu);
            children.push(tree);
        }

        children
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<LocalState>()
    }

    fn state(&self) -> tree::State {
        #[allow(clippy::default_trait_access)]
        tree::State::new(LocalState {
            menu_state: Default::default(),
            paragraphs: SecondaryMap::new(),
            text_hashes: SecondaryMap::new(),
            buttons_visible: Default::default(),
            buttons_offset: Default::default(),
            collapsed: Default::default(),
            focused: Default::default(),
            focused_item: Default::default(),
            focused_visible: false,
            hovered: Default::default(),
            known_length: Default::default(),
            middle_clicked: Default::default(),
            internal_layout: Default::default(),
            context_cursor: Point::default(),
            show_context: Default::default(),
            wheel_timestamp: Default::default(),
            fingers_pressed: Default::default(),
            pressed_item: None,
            close_pressed: None,
            tab_drag_candidate: None,
            dragging_tab: None,
            drop_hint: None,
            file_drop_target: None,
            file_drag_done: None,
            drag_hover: crate::ui::dnd::HoverOpen::default(),
            nav: NavState::default(),
            tabs: TabState::default(),
        })
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<LocalState>();
        for key in self.model.order.iter().copied() {
            self.update_entity_paragraph(state, key);
        }

        // Diff the context menu
        if let Some(context_menu) = &self.context_menu {
            state.menu_state.inner.with_data_mut(|inner| {
                // A popup collapsing out is still on screen and still
                // rendering against this tree; see `context_menu::diff`.
                if inner.leaving.contains_key(&self.window_id) {
                    return;
                }
                menu_roots_diff(context_menu, &mut inner.tree);
            });
        }

        // Unfocus if another segmented control was focused.
        if let Some(f) = state.focused.as_ref()
            && f.updated_at != LAST_FOCUS_UPDATE.with(|f| f.get())
        {
            state.unfocus();
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<LocalState>();
        let limits = limits.shrink(self.padding);
        let size = self
            .variant_layout(state, renderer, &limits)
            .expand(self.padding);
        layout::Node::new(size)
    }

    #[allow(clippy::too_many_lines)]
    fn update(
        &mut self,
        tree: &mut Tree,
        mut event: &Event,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        let my_bounds = layout.bounds();
        let state = tree.state.downcast_mut::<LocalState>();

        // See `Self::to_content`.
        state.nav.viewport = *viewport;
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            // Both advance, whichever is moving.
            let nav = state.nav.tick(*now);
            let tabs = state.tabs.tick(*now);
            if nav || tabs {
                shell.request_redraw();
            }
        }

        // The compositor dismissed our context menu popup: nothing else tells this state
        // about it. iced has no `PlatformSpecific::Wayland` event
        // carrying the dismissed popup's id, so the shell records it and we
        // claim it here; see `ui::surface::dismissal`.
        {
            let dismissed = state.menu_state.inner.with_data_mut(|data| {
                if data
                    .popup_id
                    .get(&self.window_id)
                    .copied()
                    .is_some_and(crate::ui::surface::dismissal::claim)
                {
                    data.popup_id.clear();
                    data.reset();
                    true
                } else {
                    // A popup that was collapsing is gone once its surface
                    // is, and only then may the tree thaw.
                    if data
                        .leaving
                        .get(&self.window_id)
                        .copied()
                        .is_some_and(crate::ui::surface::dismissal::claim)
                    {
                        data.leaving.remove(&self.window_id);
                    }
                    false
                }
            });
            if dismissed {
                state.show_context = None;
                for key in self.model.order.iter().copied() {
                    self.update_entity_paragraph(state, key);
                }
            }
        }

        // A Wayland drag steals the pointer, so the drag is polled rather than
        // delivered. See `Self::poll_tab_drag`.
        if state.dragging_tab.is_some() {
            if self.reorderable.is_some() {
                self.poll_nav_drag(state, my_bounds, shell);
            } else {
                self.poll_tab_drag(state, my_bounds, shell);
            }
        }
        // Unconditional, unlike the tab drag's: this widget is the destination,
        // so it has no state of its own saying a drag has begun and the poll is
        // what discovers one. It returns immediately when there is nothing to
        // do, and when this bar is not a file destination at all.
        self.poll_file_drop(state, my_bounds, shell);

        // Clear the drag candidate when its gesture ends, before any early
        // return in this function.
        //
        // This clear used to live at the bottom of `update`, which a plain click
        // on a tab never reaches: the release is consumed by the `on_activate`
        // arm above, which publishes, captures and returns. The candidate then
        // survived the click. A later `CursorMoved` batch could jump hundreds
        // of pixels, meet the threshold, and start an unintended tab drag. Its
        // grab serial came from a released press (wlroots rejects it), and
        // the attempt also cancelled any subsequent file drag.
        //
        // `CursorLeft` is included for the reason `src/mouse_area.rs` handles it
        // (see 1188e18): when something else takes the pointer (a popup grab or
        // an interactive move), the client is sent `wl_pointer.leave` and the
        // release never arrives at all, so a release-only clear is unreachable
        // for exactly the cases that leave a candidate lying around.
        if left_button_released(event)
            || touch_lifted(event)
            || matches!(event, Event::Mouse(mouse::Event::CursorLeft))
        {
            state.tab_drag_candidate = None;
        }

        let hovered_before = state.hovered;

        // `Self::poll_tab_drag`, called above, turns a tab-drag drop into
        // `on_reorder` using `ui::dnd`'s own `wl_data_device`.

        // Counted before this event is applied, so a lift still counts its own
        // finger. Tracked for every event, not only those over the widget; see
        // `context_menu::track_fingers`.
        let fingers_pressed = state.fingers_pressed.len();
        crate::ui::widget::context_menu::track_fingers(&mut state.fingers_pressed, event);

        // Every press, release and leave ends whatever gesture was on a close
        // button, wherever it happens; a press on one starts a new gesture
        // below. Taken here, before any early return, so a release that is
        // not over this widget still clears it.
        let close_pressed = if is_pressed(event)
            || is_lifted(event)
            || matches!(event, Event::Mouse(mouse::Event::CursorLeft))
        {
            state.close_pressed.take()
        } else {
            state.close_pressed
        };

        if cursor_position.is_over(my_bounds) {
            // Check for clicks on the previous and next tab buttons, when tabs are collapsed.
            if state.collapsed {
                // Check if the prev tab button was clicked.
                if cursor_position
                    .is_over(prev_tab_bounds(&my_bounds, f32::from(self.button_height)))
                    && self.prev_tab_sensitive(state)
                {
                    state.hovered = Item::PrevButton;
                    for key in self.model.order.iter().copied() {
                        self.update_entity_paragraph(state, key);
                    }
                    if let Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                    | Event::Touch(touch::Event::FingerLifted { .. }) = event
                    {
                        state.buttons_offset -= 1;
                    }
                } else {
                    // Check if the next tab button was clicked.
                    if cursor_position
                        .is_over(next_tab_bounds(&my_bounds, f32::from(self.button_height)))
                        && self.next_tab_sensitive(state)
                    {
                        state.hovered = Item::NextButton;
                        for key in self.model.order.iter().copied() {
                            self.update_entity_paragraph(state, key);
                        }
                        if let Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                        | Event::Touch(touch::Event::FingerLifted { .. }) = event
                        {
                            state.buttons_offset += 1;
                        }
                    }
                }
            }

            for (key, bounds) in self
                .variant_bounds(state, my_bounds)
                .filter_map(|item| match item {
                    ItemBounds::Button(entity, bounds) => Some((entity, bounds)),
                    _ => None,
                })
                .collect::<Vec<_>>()
            {
                if cursor_position.is_over(bounds) {
                    if self.model.items[key].enabled {
                        // Record that the mouse is hovering over this button.
                        if state.hovered != Item::Tab(key) {
                            state.hovered = Item::Tab(key);
                            for key in self.model.order.iter().copied() {
                                self.update_entity_paragraph(state, key);
                            }
                        }

                        let close_button_bounds =
                            close_bounds(bounds, f32::from(self.close_icon.size));
                        let over_close_button = self.model.items[key].closable
                            && cursor_position.is_over(close_button_bounds);

                        // If marked as closable, show a close icon.
                        if self.model.items[key].closable {
                            // Emit close message if the close button is pressed.
                            if let Some(on_close) = self.on_close.as_ref() {
                                if over_close_button && is_pressed(event) {
                                    state.close_pressed = Some(key);
                                }

                                // Only when the press began on this close
                                // button. A release that ends another widget's
                                // gesture here (a rubber band in the file
                                // list) is that widget's, and capturing it
                                // would leave that gesture armed.
                                if over_close_button
                                    && close_pressed == Some(key)
                                    && (left_button_released(event)
                                        || (touch_lifted(event) && fingers_pressed == 1))
                                {
                                    shell.publish(on_close(key));
                                    shell.capture_event();
                                    return;
                                }

                                if self.on_middle_press.is_none() {
                                    // Emit close message if the tab is middle clicked.
                                    if let Event::Mouse(mouse::Event::ButtonReleased(
                                        mouse::Button::Middle,
                                    )) = event
                                    {
                                        if state.middle_clicked == Some(Item::Tab(key)) {
                                            shell.publish(on_close(key));
                                            shell.capture_event();
                                            return;
                                        }

                                        state.middle_clicked = None;
                                    }
                                }
                            }
                        }

                        if self.tab_drag.is_some()
                            && self
                                .reorderable
                                .as_ref()
                                .is_none_or(|reorderable| reorderable(key))
                            && matches!(
                                event,
                                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                            )
                            && !over_close_button
                            && let Some(position) = cursor_position.position()
                        {
                            state.tab_drag_candidate = Some(TabDragCandidate {
                                entity: key,
                                bounds,
                                origin: position,
                            });
                            if let Some(tab_drag) = self.tab_drag.as_ref() {
                                log::trace!(
                                    target: TAB_REORDER_LOG_TARGET,
                                    "tab drag candidate entity={:?} origin=({:.2},{:.2}) bounds=({:.2},{:.2},{:.2},{:.2}) threshold={}",
                                    key,
                                    position.x,
                                    position.y,
                                    bounds.x,
                                    bounds.y,
                                    bounds.width,
                                    bounds.height,
                                    tab_drag.threshold
                                );
                            }
                        }

                        if is_lifted(event) {
                            state.unfocus();
                        }

                        if is_pressed(event)
                            && let Some(on_context) = self.on_context.as_ref()
                        {
                            let (was_open, id) = state.menu_state.inner.with_data_mut(|data| {
                                let was_open = data.open;
                                data.reset();
                                data.open = false;
                                data.view_cursor = cursor_position;
                                let root = data.popup_id.remove(&self.window_id);
                                data.popup_id.clear();
                                (was_open, root)
                            });
                            if let Some(w) = id
                                && let Some(surface_action) = self.on_surface_action.as_ref()
                                && was_open
                            {
                                use crate::ui::surface::action::destroy_popup_animated;

                                state
                                    .menu_state
                                    .inner
                                    .with_data_mut(|data| data.leaving.insert(self.window_id, w));
                                shell.publish((surface_action)(destroy_popup_animated(w)));
                                return;
                            }
                        }

                        if let Some(on_activate) = self.on_activate.as_ref() {
                            if is_pressed(event) {
                                state.pressed_item = Some(Item::Tab(key));
                            } else if is_lifted(event) && self.button_is_pressed(state, key) {
                                shell.publish(on_activate(key));
                                state.set_focused();
                                state.focused_item = Item::Tab(key);
                                state.pressed_item = None;
                                shell.capture_event();
                                return;
                            }
                        }

                        // Present a context menu on a right press, without waiting
                        // for the release. Every entity's menu is already in
                        // `self.context_menu`, so nothing the press publishes
                        // changes the one shown and it can open right away.
                        if self.context_menu.is_some()
                            && let Some(on_context) = self.on_context.as_ref()
                            && (right_button_pressed(event)
                                || (touch_lifted(event) && fingers_pressed == 2))
                        {
                            state.show_context = Some(key);
                            state.context_cursor = cursor_position.position().unwrap_or_default();

                            state.menu_state.inner.with_data_mut(|data| {
                                // Clear stale MenuBounds from any previous context menu before opening a new one.
                                data.reset();
                                data.open = true;
                                data.view_cursor = cursor_position;
                                data.opening_press_held = right_button_pressed(event);
                            });

                            shell.publish(on_context(key));
                            shell.capture_event();

                            if matches!(windowing_system(), Some(WindowingSystem::Wayland)) {
                                self.create_popup(
                                    layout,
                                    cursor_position,
                                    renderer,
                                    shell,
                                    viewport,
                                    tree,
                                );
                            }
                            return;
                        }
                        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle)) =
                            event
                        {
                            state.middle_clicked = Some(Item::Tab(key));
                            if let Some(on_middle_press) = self.on_middle_press.as_ref() {
                                shell.publish(on_middle_press(key));
                                shell.capture_event();
                                return;
                            }
                        }
                    }

                    break;
                } else if state.hovered == Item::Tab(key) {
                    state.hovered = Item::None;
                    self.update_entity_paragraph(state, key);
                }
            }

            if self.scrollable_focus
                && let Some(on_activate) = self.on_activate.as_ref()
                && let Event::Mouse(mouse::Event::WheelScrolled { delta }) = event
            {
                let current = Instant::now();

                // Permit successive scroll wheel events only after a given delay.
                if state.wheel_timestamp.is_none_or(|previous| {
                    current.duration_since(previous) > Duration::from_millis(250)
                }) {
                    state.wheel_timestamp = Some(current);

                    match delta {
                        ScrollDelta::Lines { y, .. } | ScrollDelta::Pixels { y, .. } => {
                            let mut activate_key = None;

                            if *y < 0.0 {
                                let mut prev_key = Entity::null();

                                for key in self.model.order.iter().copied() {
                                    if self.model.is_active(key) && !prev_key.is_null() {
                                        activate_key = Some(prev_key);
                                    }

                                    if self.model.is_enabled(key) {
                                        prev_key = key;
                                    }
                                }
                            } else if *y > 0.0 {
                                let mut buttons = self.model.order.iter().copied();
                                while let Some(key) = buttons.next() {
                                    if self.model.is_active(key) {
                                        for key in buttons {
                                            if self.model.is_enabled(key) {
                                                activate_key = Some(key);
                                                break;
                                            }
                                        }
                                        break;
                                    }
                                }
                            }

                            if let Some(key) = activate_key {
                                shell.publish(on_activate(key));
                                state.set_focused();
                                state.focused_item = Item::Tab(key);
                                shell.capture_event();
                                return;
                            }
                        }
                    }
                }
            }
        } else {
            if let Item::Tab(_key) = std::mem::replace(&mut state.hovered, Item::None) {
                for key in self.model.order.iter().copied() {
                    self.update_entity_paragraph(state, key);
                }
            }
            if state.is_focused() {
                // Unfocus on clicks outside of the boundaries of the segmented button.
                if is_pressed(event) {
                    state.unfocus();
                    state.pressed_item = None;
                    return;
                }
            } else if is_lifted(event) {
                state.pressed_item = None;
            }
        }

        if let (Some(tab_drag), Some(candidate)) =
            (self.tab_drag.as_ref(), state.tab_drag_candidate)
            && let Event::Mouse(mouse::Event::CursorMoved { .. }) = event
            && let Some(position) = cursor_position.position()
            && position.distance(candidate.origin) >= tab_drag.threshold
            && let Some(candidate) = state.tab_drag_candidate.take()
        {
            log::trace!(
                target: TAB_REORDER_LOG_TARGET,
                "tab drag threshold met entity={:?} distance={:.2} threshold={}",
                candidate.entity,
                position.distance(candidate.origin),
                tab_drag.threshold
            );
            if self.start_tab_drag(
                state,
                candidate.entity,
                candidate.bounds,
                position,
                clipboard,
            ) {
                // The dragged tab leaves the row; see `tab_drag`.
                if self.uses_tab_gap(state) {
                    let visible = self.visible_tabs(state);
                    let (_, row_width) = self.tab_row(state, my_bounds);
                    state.tabs.hide(
                        candidate.entity,
                        &visible,
                        row_width,
                        f32::from(self.spacing),
                    );
                }
                // Bootstraps the poll loop: nothing else will ask for a frame
                // once the compositor has the pointer.
                shell.request_redraw();
                shell.capture_event();
                return;
            }
        }

        // The release that ends the press which opened the menu is not a
        // click away from it.
        let ends_opening_press = right_button_released(event)
            && state
                .menu_state
                .inner
                .with_data_mut(|ms| std::mem::take(&mut ms.opening_press_held));

        // A right press reaching here did not open this menu, and may be
        // opening another widget's: close now rather than on its release, or
        // both popups would be up while the button is held.
        if !ends_opening_press
            && (matches!(event, Event::Mouse(mouse::Event::ButtonReleased(_)))
                || right_button_pressed(event)
                || (touch_lifted(event)))
            && let Some(_id) = state
                .menu_state
                .inner
                .with_data_mut(|ms| ms.popup_id.remove(&self.window_id))
        {
            {
                let surface_action = self.on_surface_action.as_ref().unwrap();
                // Closing the menu is a side effect: a click elsewhere
                // belongs to what it is over. Captured, it would never reach
                // that widget — a file pressed after a nav menu opened
                // never saw its release, and started a drag on the next
                // pointer movement.
                if cursor_position.is_over(my_bounds) {
                    shell.capture_event();
                }

                // Nothing here replaces it with another popup of this widget,
                // which opens through `create_popup` and returns before this,
                // so it may collapse on its way out.
                shell.publish(surface_action(
                    crate::ui::surface::action::destroy_popup_animated(_id),
                ));
            }
            state.show_context = None;

            state.menu_state.inner.with_data_mut(|data| {
                // Clear stale MenuBounds from any previous context menu before opening a new one.
                data.reset();
                data.open = false;
                data.view_cursor = cursor_position;
                data.leaving.insert(self.window_id, _id);
            });
        }

        if state.is_focused() {
            if let Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Tab),
                modifiers,
                ..
            }) = event
            {
                shell.request_redraw();
                state.focused_visible = true;
                return if *modifiers == keyboard::Modifiers::SHIFT {
                    self.focus_previous(state, shell);
                } else if modifiers.is_empty() {
                    self.focus_next(state, shell);
                };
            }

            if let Some(on_activate) = self.on_activate.as_ref()
                && let Event::Keyboard(keyboard::Event::KeyReleased {
                    key: keyboard::Key::Named(keyboard::key::Named::Enter),
                    ..
                }) = event
            {
                match state.focused_item {
                    Item::Tab(entity) => {
                        shell.publish(on_activate(entity));
                    }

                    Item::PrevButton => {
                        if self.prev_tab_sensitive(state) {
                            state.buttons_offset -= 1;

                            // If the change would cause it to be insensitive, focus the first tab.
                            if !self.prev_tab_sensitive(state)
                                && let Some(first) = self.first_tab(state)
                            {
                                state.focused_item = Item::Tab(first);
                            }
                        }
                    }

                    Item::NextButton => {
                        if self.next_tab_sensitive(state) {
                            state.buttons_offset += 1;

                            // If the change would cause it to be insensitive, focus the last tab.
                            if !self.next_tab_sensitive(state)
                                && let Some(last) = self.last_tab(state)
                            {
                                state.focused_item = Item::Tab(last);
                            }
                        }
                    }

                    Item::None | Item::Set => (),
                }

                shell.capture_event();
            }
        }

        if hovered_before != state.hovered {
            shell.request_redraw();
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn iced_core::widget::Operation<()>,
    ) {
        let state = tree.state.downcast_mut::<LocalState>();
        operation.focusable(Some(&self.id.0), layout.bounds(), state);
        operation.custom(Some(&self.id.0), layout.bounds(), state);

        if let Item::Set = state.focused_item {
            if self.prev_tab_sensitive(state) {
                state.focused_item = Item::PrevButton;
            } else if let Some(first) = self.first_tab(state) {
                state.focused_item = Item::Tab(first);
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        _viewport: &iced::Rectangle,
        _renderer: &Renderer,
    ) -> iced_core::mouse::Interaction {
        if self.on_activate.is_none() {
            return iced_core::mouse::Interaction::default();
        }
        let state = tree.state.downcast_ref::<LocalState>();
        let bounds = layout.bounds();

        if cursor_position.is_over(bounds) {
            let hovered_button = self
                .variant_bounds(state, bounds)
                .filter_map(|item| match item {
                    ItemBounds::Button(entity, bounds) => Some((entity, bounds)),
                    _ => None,
                })
                .find(|(_key, bounds)| cursor_position.is_over(*bounds));

            if let Some((key, _bounds)) = hovered_button {
                return if self.model.items[key].enabled {
                    iced_core::mouse::Interaction::Pointer
                } else {
                    iced_core::mouse::Interaction::Idle
                };
            }
        }

        iced_core::mouse::Interaction::default()
    }

    #[allow(clippy::too_many_lines)]
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        let state = tree.state.downcast_ref::<LocalState>();
        let appearance = Self::variant_appearance(theme, &self.style);
        let bounds: Rectangle = layout.bounds();
        let button_amount = self.model.items.len();
        // A tab bar with a gap shows the tab's landing place with it instead.
        let show_drop_hint = state.dragging_tab.is_some() && !self.uses_tab_gap(state);
        let drop_hint = if show_drop_hint {
            state.drop_hint
        } else {
            None
        };

        // Draw the background, if a background was defined.
        if let Some(background) = appearance.background {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: appearance.border,
                    shadow: Shadow::default(),
                    snap: true,
                },
                background,
            );
        }

        // Draw previous and next tab buttons if there is a need to paginate tabs.
        if state.collapsed {
            let mut tab_bounds = prev_tab_bounds(&bounds, f32::from(self.button_height));

            // Previous tab button
            let mut background_appearance =
                if self.on_activate.is_some() && Item::PrevButton == state.focused_item {
                    Some(appearance.active)
                } else if self.on_activate.is_some() && Item::PrevButton == state.hovered {
                    Some(appearance.hover)
                } else {
                    None
                };

            if let Some(background_appearance) = background_appearance.take() {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: tab_bounds,
                        border: Border {
                            radius: theme.cosmic().radius_s().to_radius(),
                            ..Default::default()
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    background_appearance
                        .background
                        .unwrap_or(Background::Color(Color::TRANSPARENT)),
                );
            }

            draw_icon::<Message>(
                renderer,
                theme,
                style,
                cursor,
                viewport,
                if state.buttons_offset == 0 {
                    appearance.inactive.text_color
                } else {
                    appearance.active.text_color
                },
                Rectangle {
                    x: tab_bounds.x + 8.0,
                    y: tab_bounds.y + f32::from(self.button_height) / 4.0,
                    width: 16.0,
                    height: 16.0,
                },
                icon::from_name("go-previous-symbolic").size(16).icon(),
            );

            tab_bounds = next_tab_bounds(&bounds, f32::from(self.button_height));

            // Next tab button
            background_appearance =
                if self.on_activate.is_some() && Item::NextButton == state.focused_item {
                    Some(appearance.active)
                } else if self.on_activate.is_some() && Item::NextButton == state.hovered {
                    Some(appearance.hover)
                } else {
                    None
                };

            if let Some(background_appearance) = background_appearance {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: tab_bounds,
                        border: Border {
                            radius: theme.cosmic().radius_s().to_radius(),
                            ..Default::default()
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    background_appearance
                        .background
                        .unwrap_or(Background::Color(Color::TRANSPARENT)),
                );
            }

            draw_icon::<Message>(
                renderer,
                theme,
                style,
                cursor,
                viewport,
                if self.next_tab_sensitive(state) {
                    appearance.active.text_color
                } else if let Item::NextButton = state.focused_item {
                    appearance.active.text_color
                } else {
                    appearance.inactive.text_color
                },
                Rectangle {
                    x: tab_bounds.x + 8.0,
                    y: tab_bounds.y + f32::from(self.button_height) / 4.0,
                    width: 16.0,
                    height: 16.0,
                },
                icon::from_name("go-next-symbolic").size(16).icon(),
            );
        }

        let rad_0 = crate::ui::theme::active().cosmic().corner_radii.radius_0;

        let divider_background = Background::Color(
            crate::ui::theme::active()
                .cosmic()
                .primary_component_divider()
                .to_color(),
        );

        // Draw each of the items in the widget.
        let mut nth = 0;
        let drop_hint_marker = drop_hint;
        let show_drop_hint_marker = show_drop_hint;
        let clip = self.tab_clip(state, bounds);
        if let Some(clip) = clip {
            renderer.start_layer(clip);
        }
        // Reborrowed, so the layer can be ended once the items are drawn.
        let items = &mut *renderer;
        self.variant_bounds(state, bounds).for_each(move |item| {
            let renderer = &mut *items;
            let (key, mut bounds) = match item {
                // Draw a button
                ItemBounds::Button(entity, bounds) => (entity, bounds),

                // Draw a divider between buttons
                ItemBounds::Divider(bounds, accented) => {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds,
                            border: Border::default(),
                            shadow: Shadow::default(),
                            snap: true,
                        },
                        {
                            let theme = crate::ui::theme::active();
                            if accented {
                                Background::Color(theme.cosmic().small_widget_divider().to_color())
                            } else {
                                Background::Color(
                                    theme.cosmic().primary_container_divider().to_color(),
                                )
                            }
                        },
                    );

                    return;
                }
            };

            let original_bounds = bounds;
            let center_y = bounds.center_y();

            if show_drop_hint_marker
                && matches!(
                    drop_hint_marker,
                    Some(DropHint {
                        entity,
                        side: DropSide::Before
                    }) if entity == key
                )
            {
                draw_drop_indicator(
                    renderer,
                    original_bounds,
                    DropSide::Before,
                    Self::VERTICAL,
                    appearance.active.text_color,
                );
            }

            let menu_open = || {
                state.show_context == Some(key)
                    && state.menu_state.inner.with_data(|data| data.open)
            };

            let key_is_active = self.model.is_active(key);
            let key_is_focused = state.focused_visible && self.button_is_focused(state, key);
            let key_is_hovered = self.button_is_hovered(state, key);
            let mut status_appearance = if self.button_is_pressed(state, key) {
                appearance.pressed
            } else if key_is_hovered || menu_open() {
                appearance.hover
            } else if key_is_active {
                appearance.active
            } else {
                appearance.inactive
            };
            // A tab or sidebar entry being dragged stays in place, faded.
            if self.tab_drag.is_some() && state.dragging_tab == Some(key) {
                status_appearance.text_color.a *= DRAGGED_ALPHA;
            }

            let button_appearance = if nth == 0 {
                status_appearance.first
            } else if nth + 1 == button_amount {
                status_appearance.last
            } else {
                status_appearance.middle
            };

            // Draw the active hint on tabs
            if appearance.active_width > 0.0 {
                let active_width = if key_is_active {
                    appearance.active_width
                } else {
                    1.0
                };

                renderer.fill_quad(
                    renderer::Quad {
                        bounds: if Self::VERTICAL {
                            Rectangle {
                                x: bounds.x + bounds.width - active_width,
                                width: active_width,
                                ..bounds
                            }
                        } else {
                            Rectangle {
                                y: bounds.y + bounds.height - active_width,
                                height: active_width,
                                ..bounds
                            }
                        },
                        border: Border {
                            radius: rad_0.to_radius(),
                            ..Default::default()
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    appearance.active.text_color,
                );
            }

            bounds.x += f32::from(self.button_padding[0]);
            bounds.width -= f32::from(self.button_padding[0]) - f32::from(self.button_padding[2]);
            let mut indent_padding = 0.0;

            // Adjust bounds by indent
            if let Some(indent) = self.model.indent(key)
                && indent > 0
            {
                let adjustment = f32::from(indent) * f32::from(self.indent_spacing);
                bounds.x += adjustment;
                bounds.width -= adjustment;

                // Draw indent line
                if let crate::ui::theme::SegmentedButton::FileNav = self.style
                    && indent > 1
                {
                    indent_padding = 7.0;

                    for level in 1..indent {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: (level as f32)
                                        .mul_add(-(self.indent_spacing as f32), bounds.x)
                                        + indent_padding,
                                    width: 1.0,
                                    ..bounds
                                },
                                border: Border {
                                    radius: rad_0.to_radius(),
                                    ..Default::default()
                                },
                                shadow: Shadow::default(),
                                snap: true,
                            },
                            divider_background,
                        );
                    }

                    indent_padding += 4.0;
                }
            }

            // Render the background of the button.
            if key_is_focused || status_appearance.background.is_some() {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            x: bounds.x - f32::from(self.button_padding[0]) + indent_padding,
                            width: bounds.width + f32::from(self.button_padding[0])
                                - f32::from(self.button_padding[2])
                                - indent_padding,
                            ..bounds
                        },
                        border: if key_is_focused {
                            Border {
                                width: 1.0,
                                color: appearance.active.text_color,
                                radius: button_appearance.border.radius,
                            }
                        } else {
                            button_appearance.border
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    status_appearance
                        .background
                        .unwrap_or(Background::Color(Color::TRANSPARENT)),
                );
            }

            // Align contents of the button to the requested `button_alignment`.
            {
                // Avoid shifting content outside the left edge when the measured content is
                // wider than the available button bounds (for example, non-ellipsized text).
                let actual_width = state.internal_layout[nth].1.width.min(bounds.width);

                let offset = match self.button_alignment {
                    Alignment::Start => None,
                    Alignment::Center => Some((bounds.width - actual_width) / 2.0),
                    Alignment::End => Some(bounds.width - actual_width),
                };

                if let Some(offset) = offset {
                    bounds.x += offset - f32::from(self.button_padding[0]);
                    bounds.width = actual_width;
                }
            }

            // Draw the image beside the text.
            if let Some(icon) = self.model.icon(key) {
                let mut image_bounds = bounds;
                let width = f32::from(icon.size);
                let offset = width + f32::from(self.button_spacing);
                image_bounds.y = center_y - width / 2.0;

                draw_icon::<Message>(
                    renderer,
                    theme,
                    style,
                    cursor,
                    viewport,
                    status_appearance.text_color,
                    Rectangle {
                        width,
                        height: width,
                        ..image_bounds
                    },
                    icon.clone(),
                );

                bounds.x += offset;
            } else {
                // Draw the selection indicator if widget is a segmented selection, and the item is selected.
                if key_is_active && let crate::ui::theme::SegmentedButton::Control = self.style {
                    let mut image_bounds = bounds;
                    image_bounds.y = center_y - 8.0;

                    draw_icon::<Message>(
                        renderer,
                        theme,
                        style,
                        cursor,
                        viewport,
                        status_appearance.text_color,
                        Rectangle {
                            width: 16.0,
                            height: 16.0,
                            ..image_bounds
                        },
                        crate::ui::widget::icon(
                            match crate::ui::widget::common::object_select().data() {
                                iced_core::svg::Data::Bytes(bytes) => {
                                    crate::ui::widget::icon::from_svg_bytes(bytes.as_ref())
                                        .symbolic(true)
                                }
                                iced_core::svg::Data::Path(path) => {
                                    crate::ui::widget::icon::from_path(path.clone())
                                }
                            },
                        ),
                    );

                    let offset = 16.0 + f32::from(self.button_spacing);

                    bounds.x += offset;
                }
            }

            // Whether to show the close button on this tab.
            let show_close_button =
                (key_is_active || !self.show_close_icon_on_hover || key_is_hovered)
                    && self.model.is_closable(key);

            // Width of the icon used by the close button, which we will subtract from the text bounds.
            let close_icon_width = if show_close_button {
                f32::from(self.close_icon.size)
            } else {
                0.0
            };

            bounds.width = original_bounds.width
                - (bounds.x - original_bounds.x)
                - close_icon_width
                - f32::from(self.button_padding[2]);

            bounds.y = center_y;

            if self.model.text(key).is_some_and(|text| !text.is_empty()) {
                // FIXME why has this behavior changed? Does the center alignment not work with infinite bounds now?
                bounds.y -= state.paragraphs[key].min_height() / 2.;

                // Draw the text for this segmented button or tab.
                renderer.fill_paragraph(
                    state.paragraphs[key].raw(),
                    bounds.position(),
                    status_appearance.text_color,
                    Rectangle {
                        x: bounds.x,
                        width: bounds.width,
                        height: original_bounds.height,
                        y: bounds.y,
                        //  ..original_bounds,
                    },
                );
            }

            // Draw a close button if set.
            if show_close_button {
                let close_button_bounds = close_bounds(original_bounds, close_icon_width);

                draw_icon::<Message>(
                    renderer,
                    theme,
                    style,
                    cursor,
                    viewport,
                    status_appearance.text_color,
                    close_button_bounds,
                    self.close_icon.clone(),
                );
            }

            if show_drop_hint_marker
                && matches!(
                    drop_hint_marker,
                    Some(DropHint {
                        entity,
                        side: DropSide::After
                    }) if entity == key
                )
            {
                draw_drop_indicator(
                    renderer,
                    original_bounds,
                    DropSide::After,
                    Self::VERTICAL,
                    appearance.active.text_color,
                );
            }

            nth += 1;
        });
        if clip.is_some() {
            renderer.end_layer();
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: iced_core::Layout<'b>,
        _renderer: &Renderer,
        _viewport: &iced_core::Rectangle,
        translation: Vector,
    ) -> Option<iced_core::overlay::Element<'b, Message, crate::ui::Theme, Renderer>> {
        if matches!(windowing_system(), Some(WindowingSystem::Wayland))
            && self.on_surface_action.is_some()
            && self.window_id != crate::ui::window::none()
        {
            return None;
        }

        let state = tree.state.downcast_mut::<LocalState>();
        let menu_state = state.menu_state.clone();

        let entity = state.show_context?;

        let (mut bounds, i) = self
            .variant_bounds(state, layout.bounds())
            .filter_map(|item| match item {
                ItemBounds::Button(entity, bounds) => Some((bounds, entity)),
                _ => None,
            })
            .enumerate()
            .find_map(|(i, (bounds, e))| if e == entity { Some((bounds, i)) } else { None })?;

        assert!(
            self.context_menu
                .as_ref()
                .is_none_or(|m| m[0].children.len() == self.model.len())
        );
        let menu = self
            .context_menu
            .as_mut()
            .map(|m| m[0].children[i].clone())?;

        if !menu_state.inner.with_data(|data| data.open) {
            // If the menu is not open, we don't need to show it.
            // We also clear the context entity and update the text
            // cache so that the item is not bold when the context menu is closed
            state.show_context = None;
            for key in self.model.order.iter().copied() {
                self.update_entity_paragraph(state, key);
            }
            return None;
        }
        bounds.x = state.context_cursor.x;
        bounds.y = state.context_cursor.y;

        Some(
            crate::ui::widget::menu::Menu {
                tree: menu_state,
                menu_roots: std::borrow::Cow::Owned(vec![menu]),
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
                main_offset: -bounds.height as i32,
                cross_offset: 0,
                root_bounds_list: vec![bounds],
                path_highlight: Some(PathHighlight::MenuActive),
                style: std::borrow::Cow::Borrowed(
                    &crate::ui::theme::menu_bar::MenuBarStyle::Default,
                ),
                position: Point::new(translation.x, translation.y),
                is_overlay: true,
                window_id: crate::ui::window::none(),
                depth: 0,
                on_surface_action: None,
            }
            .overlay(),
        )
    }
}

impl<'a, Variant, SelectionMode, Message> From<SegmentedButton<'a, Variant, SelectionMode, Message>>
    for Element<'a, Message>
where
    SegmentedButton<'a, Variant, SelectionMode, Message>: SegmentedVariant,
    Variant: 'static,
    Model<SelectionMode>: Selectable,
    SelectionMode: Default,
    Message: 'static + Clone,
{
    fn from(mut widget: SegmentedButton<'a, Variant, SelectionMode, Message>) -> Self {
        if widget.model.items.is_empty() {
            widget.spacing = 0;
        }

        Self::new(widget)
    }
}

struct TabDragSource<Message> {
    mime: String,
    threshold: f32,
    /// What the drag carries as far as this app is concerned.
    payload: crate::ui::dnd::LocalPayload,
    _marker: PhantomData<Message>,
}

impl<Message> TabDragSource<Message> {
    fn new(mime: String) -> Self {
        Self {
            mime,
            threshold: 8.0,
            payload: crate::ui::dnd::LocalPayload::Tab,
            _marker: PhantomData,
        }
    }
}

#[derive(Clone, Copy)]
struct TabDragCandidate {
    entity: Entity,
    bounds: Rectangle,
    origin: Point,
}

#[derive(Debug, Clone, Copy)]
struct Focus {
    updated_at: Instant,
    now: Instant,
}

/// State that is maintained by each individual widget.
pub struct LocalState {
    /// Menu state
    pub(crate) menu_state: MenuBarState,
    /// Defines how many buttons to show at a time.
    pub(super) buttons_visible: usize,
    /// Button visibility offset, when collapsed.
    pub(super) buttons_offset: usize,
    /// Whether buttons need to be collapsed to preserve minimum width
    pub(super) collapsed: bool,
    /// Visibility of focus state
    focused_visible: bool,
    /// If the widget is focused or not.
    focused: Option<Focus>,
    /// The key inside the widget that is currently focused.
    focused_item: Item,
    /// The ID of the button that is being hovered. Defaults to null.
    hovered: Item,
    /// The ID of the button that was middle-clicked, but not yet released.
    middle_clicked: Option<Item>,
    /// Last known length of the model.
    pub(super) known_length: usize,
    /// Dimensions of internal buttons when shrinking
    pub(super) internal_layout: Vec<(Size, Size)>,
    /// The paragraphs for each text.
    paragraphs: SecondaryMap<Entity, Plain>,
    /// Used to detect changes in text.
    text_hashes: SecondaryMap<Entity, u64>,
    /// Location of cursor when context menu was opened.
    context_cursor: Point,
    /// Track whether an item is currently showing a context menu.
    show_context: Option<Entity>,
    /// The button a live *file* drag is over and would drop into. Drawn
    /// hovered; see `button_is_hovered`.
    file_drop_target: Option<Entity>,
    /// The [`id`](crate::ui::dnd::Drag::id) of the file drag this bar has
    /// already finished with, so a retired drag, which stays readable until
    /// the next one starts, is acted on exactly once rather than on every
    /// pass that follows it.
    file_drag_done: Option<u64>,
    /// The button a file drag is held over, to open it; see
    /// [`SegmentedButton::on_drag_hover_open`].
    drag_hover: crate::ui::dnd::HoverOpen<Entity>,
    /// Time since last tab activation from wheel movements.
    wheel_timestamp: Option<Instant>,
    /// Tracks multi-touch events
    fingers_pressed: HashSet<Finger>,
    /// The currently pressed item
    pressed_item: Option<Item>,
    /// The entity whose close button the current left press or touch began
    /// on, so only a click on that button closes it.
    close_pressed: Option<Entity>,
    /// Pending tab drag candidate data
    tab_drag_candidate: Option<TabDragCandidate>,
    /// Currently dragging tab entity
    dragging_tab: Option<Entity>,
    /// Current drop hint for drag-and-drop indicator
    drop_hint: Option<DropHint>,
    /// The sidebar's gap and moving rows; see [`Self::reorderable`](SegmentedButton::reorderable).
    pub(super) nav: NavState,
    /// The tab bar's gap and moving tabs; see `tab_drag`.
    pub(super) tabs: TabState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Item {
    NextButton,
    #[default]
    None,
    PrevButton,
    Set,
    Tab(Entity),
}

impl LocalState {
    fn set_focused(&mut self) {
        let now = Instant::now();
        LAST_FOCUS_UPDATE.with(|x| x.set(now));

        self.focused = Some(Focus {
            updated_at: now,
            now,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::widget::segmented_button::{self, Appearance as SegAppearance};
    use iced::Size;
    use slotmap::SecondaryMap;
    use std::collections::HashSet;

    #[derive(Clone, Debug)]
    enum TestMessage {}

    struct TestVariant;

    impl<SelectionMode, Message> SegmentedVariant
        for SegmentedButton<'_, TestVariant, SelectionMode, Message>
    where
        Model<SelectionMode>: Selectable,
        SelectionMode: Default,
        Message: Clone,
    {
        const VERTICAL: bool = false;

        fn variant_appearance(
            _theme: &crate::ui::Theme,
            _style: &crate::ui::theme::SegmentedButton,
        ) -> SegAppearance {
            SegAppearance::default()
        }

        fn variant_bounds<'b>(
            &'b self,
            _state: &'b LocalState,
            bounds: Rectangle,
        ) -> Box<dyn Iterator<Item = ItemBounds> + 'b> {
            let len = self.model.order.len();
            if len == 0 {
                return Box::new(std::iter::empty());
            }
            let width = bounds.width / len as f32;
            Box::new(
                self.model
                    .order
                    .iter()
                    .copied()
                    .enumerate()
                    .map(move |(idx, entity)| {
                        let rect = Rectangle {
                            x: bounds.x + (idx as f32) * width,
                            y: bounds.y,
                            width,
                            height: bounds.height,
                        };
                        ItemBounds::Button(entity, rect)
                    }),
            )
        }

        /// Fills its limits, so a test can point at the evenly split items.
        fn variant_layout(
            &self,
            _state: &mut LocalState,
            _renderer: &crate::ui::Renderer,
            limits: &layout::Limits,
        ) -> Size {
            limits.max()
        }
    }

    fn sample_model() -> (
        segmented_button::SingleSelectModel,
        Vec<segmented_button::Entity>,
    ) {
        let mut entities = Vec::new();
        let model = segmented_button::Model::builder()
            .insert(|b| b.text("One").with_id(|id| entities.push(id)))
            .insert(|b| b.text("Two").with_id(|id| entities.push(id)))
            .insert(|b| b.text("Three").with_id(|id| entities.push(id)))
            .build();
        (model, entities)
    }

    fn test_state(dragging: segmented_button::Entity, len: usize) -> LocalState {
        let mut state = LocalState {
            menu_state: MenuBarState::default(),
            paragraphs: SecondaryMap::new(),
            text_hashes: SecondaryMap::new(),
            buttons_visible: 0,
            buttons_offset: 0,
            collapsed: false,
            focused: None,
            focused_item: Item::default(),
            focused_visible: false,
            hovered: Item::default(),
            known_length: 0,
            middle_clicked: None,
            internal_layout: Vec::new(),
            context_cursor: Point::ORIGIN,
            show_context: None,
            wheel_timestamp: None,
            fingers_pressed: HashSet::new(),
            pressed_item: None,
            close_pressed: None,
            tab_drag_candidate: None,
            dragging_tab: Some(dragging),
            drop_hint: None,
            file_drop_target: None,
            file_drag_done: None,
            drag_hover: crate::ui::dnd::HoverOpen::default(),
            nav: NavState::default(),
            tabs: TabState::default(),
        };
        state.buttons_visible = len;
        state.known_length = len;
        state
    }

    #[test]
    fn drop_hint_reports_before_and_after() {
        let (model, ids) = sample_model();
        let button =
            SegmentedButton::<TestVariant, segmented_button::SingleSelect, TestMessage>::new(
                &model,
            );
        let state = test_state(ids[0], model.order.len());
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 30.0,
        };
        let before = button
            .drop_hint_for_position(&state, bounds, Point::new(10.0, 15.0))
            .expect("hint");
        assert_eq!(before.entity, ids[0]);
        assert!(matches!(before.side, DropSide::Before));

        let after = button
            .drop_hint_for_position(&state, bounds, Point::new(290.0, 15.0))
            .expect("hint");
        assert_eq!(after.entity, ids[2]);
        assert!(matches!(after.side, DropSide::After));
    }

    /// Sends `events` as one batch to a button over `model` with a context
    /// menu per entity, and returns what it published.
    fn send_to_button_with_menu(
        model: &segmented_button::SingleSelectModel,
        cache: iced_runtime::user_interface::Cache,
        cursor: Point,
        events: &[Event],
    ) -> (
        Vec<segmented_button::Entity>,
        iced_runtime::user_interface::Cache,
    ) {
        let menus = model
            .order
            .iter()
            .map(|_| menu::Tree::new(crate::ui::Element::from(crate::ui::widget::text("item"))))
            .collect::<Vec<_>>();
        let button = SegmentedButton::<TestVariant, segmented_button::SingleSelect, _>::new(model)
            .on_context(|entity| entity)
            .context_menu(Some(vec![menu::Tree::with_children(
                crate::ui::Element::from(crate::ui::widget::Row::new()),
                menus,
            )]));
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = iced_runtime::user_interface::UserInterface::build(
            crate::ui::Element::from(button),
            Size::new(300.0, 30.0),
            cache,
            &mut renderer,
        );
        let mut published = Vec::new();
        let _ = ui.update(
            events,
            mouse::Cursor::Available(cursor),
            &mut renderer,
            &mut iced_core::clipboard::Null,
            &mut published,
        );
        (published, ui.into_cache())
    }

    #[test]
    fn a_right_press_opens_the_context_menu_without_waiting_for_the_release() {
        let (model, ids) = sample_model();
        let over_first = Point::new(10.0, 15.0);

        let (published, cache) = send_to_button_with_menu(
            &model,
            iced_runtime::user_interface::Cache::default(),
            over_first,
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Right,
            ))],
        );
        assert_eq!(published, [ids[0]]);

        // The release ends the opening click: it neither opens the menu a
        // second time nor dismisses it.
        let (published, _) = send_to_button_with_menu(
            &model,
            cache,
            over_first,
            &[Event::Mouse(mouse::Event::ButtonReleased(
                mouse::Button::Right,
            ))],
        );
        assert_eq!(published, []);
    }

    /// Sends `events` as one batch to a button over `model` that closes its
    /// entities, and returns what it published and whether each event was
    /// captured.
    fn send_to_closable_button(
        model: &segmented_button::SingleSelectModel,
        cursor: Point,
        events: &[Event],
    ) -> (Vec<segmented_button::Entity>, Vec<iced_core::event::Status>) {
        let button = SegmentedButton::<TestVariant, segmented_button::SingleSelect, _>::new(model)
            .on_close(|entity| entity);
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = iced_runtime::user_interface::UserInterface::build(
            crate::ui::Element::from(button),
            Size::new(300.0, 30.0),
            iced_runtime::user_interface::Cache::default(),
            &mut renderer,
        );
        let mut published = Vec::new();
        let (_, statuses) = ui.update(
            events,
            mouse::Cursor::Available(cursor),
            &mut renderer,
            &mut iced_core::clipboard::Null,
            &mut published,
        );
        (published, statuses)
    }

    /// The centre of the first entity's close button in a 300x30 button over
    /// three entities, mirroring `TestVariant::variant_bounds`.
    fn first_close_button_centre(model: &segmented_button::SingleSelectModel) -> Point {
        let button =
            SegmentedButton::<TestVariant, segmented_button::SingleSelect, TestMessage>::new(model);
        let padding = button.padding;
        let first = Rectangle {
            x: padding.left,
            y: padding.top,
            width: (300.0 - padding.left - padding.right) / model.order.len() as f32,
            height: 30.0 - padding.top - padding.bottom,
        };
        close_bounds(first, f32::from(button.close_icon.size)).center()
    }

    /// What the sidebar tests' widget publishes.
    #[derive(Clone, Debug, PartialEq)]
    enum SidebarMsg {
        Nav(NavDrop),
        /// Files dropped into an entry.
        Into(segmented_button::Entity),
    }

    /// A sidebar of four 32 px rows: three that can move and a fixed one
    /// below, like Trash. It can pin `/pin` and nothing else.
    struct Sidebar {
        model: segmented_button::SingleSelectModel,
        ids: Vec<segmented_button::Entity>,
        tree: Tree,
        renderer: crate::ui::Renderer,
    }

    /// The sidebar's bounds; wider than laid out, as the panel is.
    const SIDEBAR: Rectangle = Rectangle {
        x: 0.0,
        y: 0.0,
        width: 200.0,
        height: 400.0,
    };

    impl Sidebar {
        fn new() -> Self {
            let mut ids = Vec::new();
            let model = segmented_button::Model::builder()
                .insert(|b| b.text("Recents").with_id(|id| ids.push(id)))
                .insert(|b| b.text("Home").with_id(|id| ids.push(id)))
                .insert(|b| b.text("Music").with_id(|id| ids.push(id)))
                .insert(|b| b.text("Trash").with_id(|id| ids.push(id)))
                .build();
            let mut sidebar = Self {
                model,
                ids,
                tree: Tree::empty(),
                renderer: iced_texture_cache::testing::headless_tiny_skia(),
            };
            let widget = sidebar.widget();
            let tree = Tree::new(&widget as &dyn Widget<SidebarMsg, crate::ui::Theme, Renderer>);
            drop(widget);
            sidebar.tree = tree;
            sidebar.lay_out();
            sidebar
        }

        fn widget(
            &self,
        ) -> segmented_button::VerticalSegmentedButton<'_, segmented_button::SingleSelect, SidebarMsg>
        {
            sidebar_widget(&self.model, &self.ids)
        }

        /// Lays the sidebar out, returning its height.
        fn lay_out(&mut self) -> f32 {
            let mut widget = sidebar_widget(&self.model, &self.ids);
            widget
                .layout(
                    &mut self.tree,
                    &self.renderer,
                    &layout::Limits::new(Size::ZERO, SIDEBAR.size()),
                )
                .size()
                .height
        }

        fn state(&mut self) -> &mut LocalState {
            self.tree.state.downcast_mut::<LocalState>()
        }

        /// Polls the drag `drag`, carrying `local`, as a redraw would.
        fn poll(
            &mut self,
            drag: crate::ui::dnd::Drag,
            local: Option<LocalPayload>,
        ) -> Vec<SidebarMsg> {
            crate::ui::dnd::fake::set(drag, local);
            let mut messages = Vec::new();
            let widget = sidebar_widget(&self.model, &self.ids);
            let state = self.tree.state.downcast_mut::<LocalState>();
            let mut shell = Shell::new(&mut messages);
            if state.dragging_tab.is_some() {
                widget.poll_nav_drag(state, SIDEBAR, &mut shell);
            }
            widget.poll_file_drop(state, SIDEBAR, &mut shell);
            drop(widget);
            messages
        }
    }

    /// The sidebar tests' widget; see [`Sidebar`].
    fn sidebar_widget<'a>(
        model: &'a segmented_button::SingleSelectModel,
        ids: &[segmented_button::Entity],
    ) -> segmented_button::VerticalSegmentedButton<'a, segmented_button::SingleSelect, SidebarMsg>
    {
        let run = ids[..3].to_vec();
        segmented_button::vertical(model)
            .reorderable(move |entity| run.contains(&entity))
            .can_pin(|path| path == Path::new("/pin"))
            .on_nav_drop(SidebarMsg::Nav)
            .on_file_drop(|entity| Some(SidebarMsg::Into(entity)))
    }

    fn at(x: f32, y: f32) -> crate::ui::dnd::Drag {
        crate::ui::dnd::Drag {
            position: Some((x, y)),
            ..crate::ui::dnd::Drag::default()
        }
    }

    fn dropped_at(x: f32, y: f32) -> crate::ui::dnd::Drag {
        crate::ui::dnd::Drag {
            dropped: true,
            ended: true,
            ..at(x, y)
        }
    }

    fn files(drag: crate::ui::dnd::Drag) -> crate::ui::dnd::Drag {
        crate::ui::dnd::Drag {
            files: true,
            ..drag
        }
    }

    #[test]
    fn a_dragged_entry_opens_the_gap_and_moves_there() {
        let mut sidebar = Sidebar::new();
        assert_eq!(sidebar.lay_out(), 128.0);
        let music = sidebar.ids[2];
        sidebar.state().dragging_tab = Some(music);

        // Over the top of Recents: the gap opens above it.
        assert_eq!(
            sidebar.poll(at(10.0, 5.0), Some(LocalPayload::NavEntry)),
            []
        );
        assert_eq!(sidebar.state().nav.gap(), Some(0));
        assert_eq!(sidebar.lay_out(), 128.0 + 16.0);

        assert_eq!(
            sidebar.poll(dropped_at(10.0, 5.0), Some(LocalPayload::NavEntry)),
            [SidebarMsg::Nav(NavDrop::Move {
                dragged: music,
                before: Some(sidebar.ids[0]),
            })]
        );
        assert_eq!(sidebar.state().dragging_tab, None);
        assert_eq!(sidebar.state().nav.gap(), None);
    }

    #[test]
    fn the_gap_never_opens_below_a_fixed_entry() {
        let mut sidebar = Sidebar::new();
        sidebar.state().dragging_tab = Some(sidebar.ids[0]);
        // Over Trash and below it: the gap stays above Trash.
        let _ = sidebar.poll(at(10.0, 120.0), Some(LocalPayload::NavEntry));
        assert_eq!(sidebar.state().nav.gap(), Some(3));
        assert_eq!(
            sidebar.poll(dropped_at(10.0, 300.0), Some(LocalPayload::NavEntry)),
            [SidebarMsg::Nav(NavDrop::Move {
                dragged: sidebar.ids[0],
                before: None,
            })]
        );
    }

    #[test]
    fn a_drop_beside_itself_changes_nothing() {
        let mut sidebar = Sidebar::new();
        sidebar.state().dragging_tab = Some(sidebar.ids[1]);
        // The gap above Music is right below Home.
        assert_eq!(
            sidebar.poll(dropped_at(10.0, 66.0), Some(LocalPayload::NavEntry)),
            []
        );
    }

    #[test]
    fn a_drop_off_the_sidebar_unpins_and_a_cancel_does_not() {
        let mut sidebar = Sidebar::new();
        let home = sidebar.ids[1];
        let off = crate::ui::dnd::Drag {
            dropped: true,
            ended: true,
            ..at(300.0, 50.0)
        };
        sidebar.state().dragging_tab = Some(home);
        assert_eq!(
            sidebar.poll(off, Some(LocalPayload::NavEntry)),
            [SidebarMsg::Nav(NavDrop::Unpin(home))]
        );

        // Esc: over, dropped nowhere, and not performed.
        let cancelled = crate::ui::dnd::Drag {
            ended: true,
            ..crate::ui::dnd::Drag::default()
        };
        sidebar.state().dragging_tab = Some(home);
        assert_eq!(sidebar.poll(cancelled, Some(LocalPayload::NavEntry)), []);

        // Let go outside the window, where the compositor says so.
        sidebar.state().dragging_tab = Some(home);
        assert_eq!(
            sidebar.poll(
                crate::ui::dnd::Drag {
                    performed: true,
                    ..cancelled
                },
                Some(LocalPayload::NavEntry)
            ),
            [SidebarMsg::Nav(NavDrop::Unpin(home))]
        );
    }

    #[test]
    fn a_folder_near_an_edge_is_pinned_in_the_gap() {
        let mut sidebar = Sidebar::new();
        let pin = Some(LocalPayload::Paths(vec![PathBuf::from("/pin")]));
        // The top quarter of Home, which spans 32..64.
        assert_eq!(sidebar.poll(files(at(10.0, 34.0)), pin.clone()), []);
        assert_eq!(sidebar.state().nav.gap(), Some(1));
        assert_eq!(sidebar.state().file_drop_target, None);
        assert_eq!(
            sidebar.poll(files(dropped_at(10.0, 34.0)), pin),
            [SidebarMsg::Nav(NavDrop::Pin {
                path: PathBuf::from("/pin"),
                before: Some(sidebar.ids[1]),
            })]
        );
        assert_eq!(sidebar.state().nav.gap(), None);
    }

    /// A finished drag stays readable until the next one starts, and every
    /// redraw polls it again: that must not open its gap once more.
    #[test]
    fn a_finished_pin_leaves_no_gap_behind() {
        let mut sidebar = Sidebar::new();
        let pin = Some(LocalPayload::Paths(vec![PathBuf::from("/pin")]));
        let dropped = files(dropped_at(10.0, 34.0));
        assert_eq!(sidebar.poll(dropped, pin.clone()).len(), 1);

        assert_eq!(sidebar.poll(dropped, pin), []);
        assert_eq!(sidebar.state().nav.gap(), None);
        assert_eq!(sidebar.lay_out(), 128.0);
    }

    #[test]
    fn a_finished_drop_into_an_entry_leaves_no_highlight() {
        let mut sidebar = Sidebar::new();
        let dropped = files(dropped_at(10.0, 48.0));
        assert_eq!(
            sidebar.poll(dropped, None),
            [SidebarMsg::Into(sidebar.ids[1])]
        );

        assert_eq!(sidebar.poll(dropped, None), []);
        assert_eq!(sidebar.state().file_drop_target, None);
    }

    #[test]
    fn the_middle_of_an_entry_still_takes_the_files() {
        let mut sidebar = Sidebar::new();
        let home = sidebar.ids[1];
        let pin = Some(LocalPayload::Paths(vec![PathBuf::from("/pin")]));
        assert_eq!(
            sidebar.poll(files(dropped_at(10.0, 48.0)), pin),
            [SidebarMsg::Into(home)]
        );
    }

    #[test]
    fn only_one_folder_that_can_be_pinned_opens_the_gap() {
        for local in [
            // Not one the app lets pin: already pinned, or not a folder.
            Some(LocalPayload::Paths(vec![PathBuf::from("/other")])),
            // More than one.
            Some(LocalPayload::Paths(vec![
                PathBuf::from("/pin"),
                PathBuf::from("/pin"),
            ])),
            // From another app.
            None,
        ] {
            let mut sidebar = Sidebar::new();
            let home = sidebar.ids[1];
            assert_eq!(
                sidebar.poll(files(dropped_at(10.0, 34.0)), local),
                [SidebarMsg::Into(home)]
            );
        }
    }

    /// A tab bar of three tabs sharing 300 px, as the app's does.
    struct Tabs {
        model: segmented_button::SingleSelectModel,
        ids: Vec<segmented_button::Entity>,
        tree: Tree,
        renderer: crate::ui::Renderer,
    }

    const TAB_BAR: Rectangle = Rectangle {
        x: 0.0,
        y: 0.0,
        width: 300.0,
        height: 44.0,
    };

    fn tabs_widget(
        model: &segmented_button::SingleSelectModel,
    ) -> segmented_button::HorizontalSegmentedButton<'_, segmented_button::SingleSelect, ReorderEvent>
    {
        segmented_button::horizontal(model)
            .enable_tab_drag(String::from("x-earth-files/tab-drag"))
            .on_reorder(|event| event)
    }

    impl Tabs {
        fn new() -> Self {
            let (model, ids) = sample_model();
            let widget = tabs_widget(&model);
            let tree = Tree::new(&widget as &dyn Widget<ReorderEvent, crate::ui::Theme, Renderer>);
            drop(widget);
            let mut tabs = Self {
                model,
                ids,
                tree,
                renderer: iced_texture_cache::testing::headless_tiny_skia(),
            };
            tabs.lay_out();
            tabs
        }

        fn lay_out(&mut self) {
            let _ = tabs_widget(&self.model).layout(
                &mut self.tree,
                &self.renderer,
                &layout::Limits::new(Size::ZERO, TAB_BAR.size()),
            );
        }

        fn state(&mut self) -> &mut LocalState {
            self.tree.state.downcast_mut::<LocalState>()
        }

        /// The tabs drawn now, each with where and how wide.
        fn placed(&mut self) -> Vec<(segmented_button::Entity, f32, f32)> {
            let order = self.model.order.iter().copied().collect::<Vec<_>>();
            self.state().tabs.layout(&order, TAB_BAR.width, 0.0)
        }

        /// Starts dragging `tab`, as the threshold being met does.
        fn start(&mut self, tab: segmented_button::Entity) {
            let order = self.model.order.iter().copied().collect::<Vec<_>>();
            let state = self.state();
            state.dragging_tab = Some(tab);
            state.tabs.hide(tab, &order, TAB_BAR.width, 0.0);
        }

        /// Polls the tab drag `drag`, as a redraw would.
        fn poll(&mut self, drag: crate::ui::dnd::Drag) -> Vec<ReorderEvent> {
            crate::ui::dnd::fake::set(drag, Some(LocalPayload::Tab));
            let mut messages = Vec::new();
            let widget = tabs_widget(&self.model);
            let state = self.tree.state.downcast_mut::<LocalState>();
            let mut shell = Shell::new(&mut messages);
            if state.dragging_tab.is_some() {
                widget.poll_tab_drag(state, TAB_BAR, &mut shell);
            }
            drop(widget);
            messages
        }
    }

    #[test]
    fn a_dragged_tab_opens_a_gap_and_lands_in_it() {
        let mut tabs = Tabs::new();
        let first = tabs.ids[0];
        tabs.start(first);
        // It is out of the row.
        assert!(tabs.placed().iter().all(|(tab, ..)| *tab != first));

        // Past the middle of the last of the other two: the gap after it.
        assert_eq!(tabs.poll(at(290.0, 20.0)), []);
        assert_eq!(tabs.state().tabs.open(), Some(2));
        assert!(tabs.state().tabs.is_active());

        let dropped = tabs.poll(dropped_at(290.0, 20.0));
        assert_eq!(
            dropped,
            [ReorderEvent {
                dragged: first,
                target: tabs.ids[2],
                position: InsertPosition::After,
            }]
        );
        assert_eq!(tabs.state().dragging_tab, None);

        // Until the app reorders, it stays out of the row, and the gap open.
        assert_eq!(tabs.state().tabs.hidden(), Some(first));
        assert_eq!(tabs.state().tabs.open(), Some(2));

        // Once it has, the tab grows out of the gap: the gap had not opened
        // yet, so from nothing at the end of the row.
        tabs.model
            .reorder(first, tabs.ids[2], InsertPosition::After);
        tabs.lay_out();
        assert_eq!(tabs.state().tabs.hidden(), None);
        let placed = tabs.placed();
        let (_, x, width) = placed[2];
        assert_eq!(placed[2].0, first);
        assert!((x - 300.0).abs() < 0.01 && width.abs() < 0.01, "{placed:?}");
    }

    /// Tabs on springs overshoot their places, so while they move they are
    /// drawn clipped to their row: clear of the paging buttons, and of
    /// whatever is beside the bar.
    #[test]
    fn moving_tabs_are_drawn_inside_their_row() {
        let mut tabs = Tabs::new();
        let widget = tabs_widget(&tabs.model);
        let state = tabs.tree.state.downcast_mut::<LocalState>();
        assert_eq!(widget.tab_clip(state, TAB_BAR), None, "at rest");

        state.tabs.set_open(Some(1), 3, 50.0);
        assert_eq!(widget.tab_clip(state, TAB_BAR), Some(TAB_BAR));

        state.collapsed = true;
        let paging = f32::from(widget.button_height);
        assert_eq!(
            widget.tab_clip(state, TAB_BAR),
            Some(Rectangle {
                x: paging,
                width: TAB_BAR.width - 2.0 * paging,
                ..TAB_BAR
            })
        );
    }

    #[test]
    fn a_tab_dropped_at_its_place_or_off_the_bar_moves_nothing() {
        for drop in [
            // The slot it left, between the other two (150 each).
            dropped_at(100.0, 20.0),
            dropped_at(200.0, 20.0),
            // Below the bar.
            dropped_at(150.0, 100.0),
        ] {
            let mut tabs = Tabs::new();
            let second = tabs.ids[1];
            tabs.start(second);
            assert_eq!(tabs.poll(drop), []);
            // It is back in the row, the gap gone.
            assert_eq!(tabs.state().tabs.open(), None);
            assert_eq!(tabs.state().tabs.hidden(), None);
            assert!(tabs.placed().iter().any(|(tab, ..)| *tab == second));
        }
    }

    #[test]
    fn a_release_over_the_close_button_without_its_press_is_left_alone() {
        let mut ids = Vec::new();
        let model = segmented_button::Model::builder()
            .insert(|b| b.text("One").closable().with_id(|id| ids.push(id)))
            .insert(|b| b.text("Two").closable().with_id(|id| ids.push(id)))
            .insert(|b| b.text("Three").closable().with_id(|id| ids.push(id)))
            .build();
        let over_close = first_close_button_centre(&model);

        // A gesture begun elsewhere (a rubber band in the file list) ending
        // over the close button: neither closes the tab nor captures the
        // release that gesture's owner is waiting for.
        let (published, statuses) = send_to_closable_button(
            &model,
            over_close,
            &[Event::Mouse(mouse::Event::ButtonReleased(
                mouse::Button::Left,
            ))],
        );
        assert_eq!(published, []);
        assert_eq!(statuses, [iced_core::event::Status::Ignored]);

        // A click on the close button still closes.
        let (published, _) = send_to_closable_button(
            &model,
            over_close,
            &[
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
        );
        assert_eq!(published, [ids[0]]);
    }
}

impl operation::Focusable for LocalState {
    fn is_focused(&self) -> bool {
        self.focused
            .is_some_and(|f| f.updated_at == LAST_FOCUS_UPDATE.with(|f| f.get()))
    }

    fn focus(&mut self) {
        self.set_focused();
        self.focused_visible = true;
        self.focused_item = Item::Set;
    }

    fn unfocus(&mut self) {
        self.focused = None;
        self.focused_item = Item::None;
        self.focused_visible = false;
        self.show_context = None;
    }
}

/// The iced identifier of a segmented button.
#[derive(Debug, Clone, PartialEq)]
pub struct Id(widget::Id);

impl Id {
    /// Creates a custom [`Id`].
    ///
    /// `widget::Id::new` is a `const fn` taking `&'static str`. This signature
    /// takes a `Cow` because `crate::tab` passes a `format!`; the owned half
    /// goes through `From<String>`.
    pub fn new(id: impl Into<std::borrow::Cow<'static, str>>) -> Self {
        Self(match id.into() {
            std::borrow::Cow::Borrowed(id) => widget::Id::new(id),
            std::borrow::Cow::Owned(id) => widget::Id::from(id),
        })
    }

    /// Creates a unique [`Id`].
    ///
    /// This function produces a different [`Id`] every time it is called.
    #[must_use]
    #[inline]
    pub fn unique() -> Self {
        Self(widget::Id::unique())
    }
}

impl From<Id> for widget::Id {
    fn from(id: Id) -> Self {
        id.0
    }
}

/// Calculates the bounds of the close button within the area of an item.
fn close_bounds(area: Rectangle<f32>, icon_size: f32) -> Rectangle<f32> {
    Rectangle {
        x: area.x + area.width - icon_size - 8.0,
        y: area.center_y() - (icon_size / 2.0),
        width: icon_size,
        height: icon_size,
    }
}

/// Calculate the bounds of the `next_tab` button.
fn next_tab_bounds(bounds: &Rectangle, button_height: f32) -> Rectangle {
    Rectangle {
        x: bounds.x + bounds.width - button_height,
        y: bounds.y,
        width: button_height,
        height: button_height,
    }
}

/// Calculate the bounds of the `prev_tab` button.
fn prev_tab_bounds(bounds: &Rectangle, button_height: f32) -> Rectangle {
    Rectangle {
        x: bounds.x,
        y: bounds.y,
        width: button_height,
        height: button_height,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_icon<Message: 'static>(
    renderer: &mut Renderer,
    theme: &crate::ui::Theme,
    style: &renderer::Style,
    cursor: mouse::Cursor,
    viewport: &Rectangle,
    color: Color,
    bounds: Rectangle,
    icon: Icon,
) {
    let layout_node = layout::Node::new(Size {
        width: bounds.width,
        height: bounds.width,
    })
    .move_to(Point {
        x: bounds.x,
        y: bounds.y,
    });

    Widget::<Message, crate::ui::Theme, Renderer>::draw(
        Element::<Message>::from(icon).as_widget(),
        &Tree::empty(),
        renderer,
        theme,
        // `renderer::Style` carries only `text_color`; the icon reads its
        // colour out of it (see `crate::ui::theme::icon_color`).
        &renderer::Style { text_color: color },
        Layout::new(&layout_node),
        cursor,
        viewport,
    );
}

/// The drop hint for the gap in `slot` among the tabs `visible`: before the
/// tab in that slot, or after the last.
fn hint_for_slot(visible: &[Entity], slot: usize) -> Option<DropHint> {
    match visible.get(slot) {
        Some(&entity) => Some(DropHint {
            entity,
            side: DropSide::Before,
        }),
        None => visible.last().map(|&entity| DropHint {
            entity,
            side: DropSide::After,
        }),
    }
}

fn draw_drop_indicator(
    renderer: &mut Renderer,
    bounds: Rectangle,
    side: DropSide,
    vertical: bool,
    color: Color,
) {
    let thickness = 4.0;
    let quad_bounds = if vertical {
        let y = match side {
            DropSide::Before => bounds.y - thickness / 2.0,
            DropSide::After => bounds.y + bounds.height - thickness / 2.0,
        };

        Rectangle {
            x: bounds.x,
            y,
            width: bounds.width,
            height: thickness,
        }
    } else {
        let x = match side {
            DropSide::Before => bounds.x - thickness / 2.0,
            DropSide::After => bounds.x + bounds.width - thickness / 2.0,
        };

        Rectangle {
            x,
            y: bounds.y,
            width: thickness,
            height: bounds.height,
        }
    };

    renderer.fill_quad(
        renderer::Quad {
            bounds: quad_bounds,
            border: Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            shadow: Shadow::default(),
            snap: true,
        },
        Background::Color(color),
    );
}

fn left_button_released(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left,))
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
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right,))
    )
}

fn is_pressed(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. })
    )
}

fn is_lifted(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left,))
            | Event::Touch(touch::Event::FingerLifted { .. })
    )
}

fn touch_lifted(event: &Event) -> bool {
    matches!(event, Event::Touch(touch::Event::FingerLifted { .. }))
}
