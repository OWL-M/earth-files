// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0 AND MIT

// From iced_aw, license MIT

//! Vendored from pop-os/libcosmic, src/widget/menu/menu_tree.rs
//!
//! A tree structure for constructing a hierarchical menu

use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

use iced_core::{Element, renderer};

use crate::ui::convert::ToColor;
use crate::ui::convert::ToLength;
use crate::ui::theme;
use crate::ui::widget;
use crate::ui::widget::RcElementWrapper;
use crate::ui::widget::button::Button;
use crate::ui::widget::icon;
use crate::ui::widget::menu::action::MenuAction;
use crate::ui::widget::menu::aim::{ChildOpen, REASON_TIME};
use crate::ui::widget::menu::key_bind::KeyBind;
use crate::ui::widget::menu::unfold::{CHIP_RIGHT, CHIP_SLOT, Group};
use iced_core::{Alignment, Length};

/// Nested menu is essentially a tree of items, a menu is a collection of items
/// a menu itself can also be an item of another menu.
///
/// A `MenuTree` represents a node in the tree, it holds a widget as a menu item
/// for its parent, and a list of menu tree as child nodes.
/// Conceptually a node is either a menu(inner node) or an item(leaf node),
/// but there's no need to explicitly distinguish them here, if a menu tree
/// has children, it's a menu, otherwise it's an item
#[allow(missing_debug_implementations)]
#[derive(Clone)]
pub struct MenuTree<Message> {
    /// The menu tree will be flatten into a vector to build a linear widget tree,
    /// the `index` field is the index of the item in that vector
    pub(crate) index: usize,

    /// The item of the menu tree
    pub(crate) item: RcElementWrapper<Message>,
    /// The children of the menu tree
    pub(crate) children: Vec<MenuTree<Message>>,
    /// The width of the menu tree
    pub(crate) width: Option<u16>,
    /// The height of the menu tree
    pub(crate) height: Option<u16>,
    /// When this row's child popup opens.
    pub(crate) child_open: ChildOpen,
    /// Whether the arrow keys stop on this row.
    pub(crate) selectable: bool,
    /// Whether the row, or a split row's left part, runs something when
    /// pressed; Enter opens the child of a row that does not.
    pub(crate) pressable: bool,
    /// A row that only shows something, a dimmed row's reason: pressing it
    /// neither chooses anything nor closes the menu.
    pub(crate) passive: bool,
    /// The row's part in a group that unfolds in place.
    pub(crate) group: Group,
}

impl<Message: Clone + 'static> MenuTree<Message> {
    /// Create a new menu tree from a widget
    pub fn new(item: impl Into<RcElementWrapper<Message>>) -> Self {
        Self {
            index: 0,
            item: item.into(),
            children: Vec::new(),
            width: None,
            height: None,
            child_open: ChildOpen::Row,
            selectable: false,
            pressable: false,
            passive: false,
            group: Group::None,
        }
    }

    /// Create a menu tree from a widget and a vector of sub trees
    pub fn with_children(
        item: impl Into<RcElementWrapper<Message>>,
        children: Vec<impl Into<MenuTree<Message>>>,
    ) -> Self {
        Self {
            index: 0,
            item: item.into(),
            children: children.into_iter().map(Into::into).collect(),
            width: None,
            height: None,
            child_open: ChildOpen::Row,
            selectable: false,
            pressable: false,
            passive: false,
            group: Group::None,
        }
    }

    /// Sets when this row's child popup opens.
    #[must_use]
    pub fn child_open(mut self, child_open: ChildOpen) -> Self {
        self.child_open = child_open;
        self
    }

    /// Makes the arrow keys stop on this row; Enter presses its left part
    /// when `pressable`, else opens its child.
    #[must_use]
    pub fn selectable(mut self, pressable: bool) -> Self {
        self.selectable = true;
        self.pressable = pressable;
        self
    }

    /// Marks a row that only shows something; see [`MenuTree::passive`].
    #[must_use]
    pub fn passive(mut self) -> Self {
        self.passive = true;
        self
    }

    /// Sets the width of the menu tree.
    /// See [`ItemWidth`]
    ///
    /// [`ItemWidth`]:`super::ItemWidth`
    #[must_use]
    pub fn width(mut self, width: u16) -> Self {
        self.width = Some(width);
        self
    }

    /// Sets the height of the menu tree.
    /// See [`ItemHeight`]
    ///
    /// [`ItemHeight`]: `super::ItemHeight`
    #[must_use]
    pub fn height(mut self, height: u16) -> Self {
        self.height = Some(height);
        self
    }

    /* Keep `set_index()` and `flatten()` recurse in the same order */

    /// Set the index of each item
    pub(crate) fn set_index(&mut self) {
        /// inner counting function.
        fn rec<Message: Clone + 'static>(mt: &mut MenuTree<Message>, count: &mut usize) {
            // keep items under the same menu line up
            mt.children.iter_mut().for_each(|c| {
                c.index = *count;
                *count += 1;
            });

            mt.children.iter_mut().for_each(|c| rec(c, count));
        }

        let mut count = 0;
        self.index = count;
        count += 1;
        rec(self, &mut count);
    }

    /// Flatten the menu tree
    pub(crate) fn flatten(&self) -> Vec<&Self> {
        /// Inner flattening function
        fn rec<'a, Message: Clone + 'static>(
            mt: &'a MenuTree<Message>,
            flat: &mut Vec<&'a MenuTree<Message>>,
        ) {
            mt.children.iter().for_each(|c| {
                flat.push(c);
            });

            mt.children.iter().for_each(|c| {
                rec(c, flat);
            });
        }

        let mut flat = Vec::new();
        flat.push(self);
        rec(self, &mut flat);

        flat
    }
}

impl<Message: Clone + 'static> From<crate::ui::Element<'static, Message>> for MenuTree<Message> {
    fn from(value: crate::ui::Element<'static, Message>) -> Self {
        Self::new(RcElementWrapper::new(value))
    }
}

pub fn menu_button<'a, Message>(
    children: Vec<crate::ui::Element<'a, Message>>,
) -> crate::ui::widget::button::Button<'a, Message>
where
    Message: std::clone::Clone + 'a,
{
    widget::button::custom(
        widget::Row::from_vec(children)
            .align_y(Alignment::Center)
            .height(Length::Fill)
            .width(Length::Fill),
    )
    .height(Length::Fixed(36.0))
    .padding([4, 16])
    .width(Length::Fill)
    .class(theme::Button::MenuItem)
}

#[derive(Clone)]
/// Represents a menu item that performs an action when selected or a separator between menu items.
///
/// - `Action` - Represents a menu item that performs an action when selected.
///     - `L` - The label of the menu item.
///     - `A` - The action to perform when the menu item is selected, the action must implement the `MenuAction` trait.
/// - `CheckBox` - Represents a checkbox menu item.
///     - `L` - The label of the menu item.
///     - `bool` - The state of the checkbox.
///     - `A` - The action to perform when the menu item is selected, the action must implement the `MenuAction` trait.
/// - `Folder` - Represents a folder menu item.
///     - `L` - The label of the menu item.
///     - `Vec<MenuItem<A, L>>` - A vector of menu items.
/// - `Divider` - Represents a divider between menu items.
pub enum MenuItem<A: MenuAction, L: Into<Cow<'static, str>>> {
    /// Represents a button menu item.
    Button(L, Option<icon::Handle>, A),
    /// Represents a button menu item that is disabled.
    ButtonDisabled(L, Option<icon::Handle>, A),
    /// Represents a checkbox menu item.
    CheckBox(L, Option<icon::Handle>, bool, A),
    /// Represents a folder menu item.
    Folder(L, Vec<MenuItem<A, L>>),
    /// Represents a divider between menu items.
    Divider,
    /// A menu entry with every option available; see [`Entry`].
    Entry(Entry<A, L>),
    /// A split row: the entry's action, with its variants folded under it
    /// behind a count.
    Split(Entry<A, L>, Vec<MenuItem<A, L>>),
    /// Another program's row, with an icon: its actions folded under it
    /// behind a count.
    Submenu(L, Option<icon::Handle>, Vec<MenuItem<A, L>>),
}

impl<A: MenuAction, L: Into<Cow<'static, str>>> MenuItem<A, L> {
    /// Create an [`Entry`] menu item, configure it with the builder methods on [`Entry`].
    pub fn entry(label: L, action: A) -> Self {
        MenuItem::Entry(Entry::new(label, action))
    }
}

/// The leading icon column of a menu entry.
#[derive(Clone, Debug, Default)]
pub enum IconSlot {
    /// No icon and no space reserved for one.
    #[default]
    None,
    /// No icon, but the space for an icon is reserved (indented entry)
    Reserved,
    /// An icon.
    Icon(icon::Handle),
}

impl From<Option<icon::Handle>> for IconSlot {
    fn from(icon: Option<icon::Handle>) -> Self {
        icon.map_or(IconSlot::None, IconSlot::Icon)
    }
}

/// A menu entry: label, optional leading icon, optional check column, enabled state and action.
#[derive(Clone)]
pub struct Entry<A, L> {
    label: L,
    icon: IconSlot,
    /// `Some` draws the check column
    checked: Option<bool>,
    enabled: bool,
    action: A,
    /// Why a disabled entry cannot run, shown beside it when the pointer
    /// rests on it.
    reason: Option<L>,
    /// A pin after the label: whether it is pinned, and what pressing it
    /// sends.
    pin: Option<(bool, A)>,
}

impl<A, L> Entry<A, L> {
    pub fn new(label: L, action: A) -> Self {
        Self {
            label,
            icon: IconSlot::None,
            checked: None,
            enabled: true,
            action,
            reason: None,
            pin: None,
        }
    }

    /// Why the entry cannot run; it is shown only when it is disabled.
    #[must_use]
    pub fn reason(mut self, reason: L) -> Self {
        self.reason = Some(reason);
        self
    }

    /// A pin after the label, filled when `pinned`, sending `action` when
    /// pressed.
    #[must_use]
    pub fn pin(mut self, pinned: bool, action: A) -> Self {
        self.pin = Some((pinned, action));
        self
    }

    /// Whether the entry can run.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The entry's label.
    #[must_use]
    pub fn label(&self) -> &L {
        &self.label
    }

    /// Draw a leading icon
    #[must_use]
    pub fn icon(mut self, icon: icon::Handle) -> Self {
        self.icon = IconSlot::Icon(icon);
        self
    }

    /// Draw no icon, but resever the space
    #[must_use]
    pub fn reserve_icon(mut self) -> Self {
        self.icon = IconSlot::Reserved;
        self
    }

    /// Show a check column, ticked when `checked` is true, empty space when false
    #[must_use]
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// Disabled entries are drawn dimmed and do not react to presses
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// Create a root menu item.
///
/// # Arguments
/// - `label` - The label of the menu item.
///
/// # Returns
/// - A button for the root menu item.
pub fn menu_root<'a, Message, Renderer: renderer::Renderer>(
    label: impl Into<Cow<'a, str>> + 'a,
) -> Button<'a, Message>
where
    Element<'a, Message, crate::ui::Theme, Renderer>: From<widget::Button<'a, Message>>,
    Message: std::clone::Clone + 'a,
{
    // The menu bar handles the press itself; the root is still an enabled
    // button so it draws its hover state and requests a pointer cursor
    widget::button::custom(widget::text(label))
        .padding([4, 12])
        .class(theme::Button::MenuRoot)
        .force_enabled(true)
}

/// Room every row of a menu with counts keeps at its right end, where a
/// group row's count is drawn, so all shortcuts line up: a gap, the count
/// and the space after it.
const TRAIL: f32 = 8.0 + CHIP_SLOT + CHIP_RIGHT;

/// Height of the rows of an unfolded group.
pub const MEMBER_HEIGHT: f32 = 32.0;

/// Left padding of the rows of an unfolded group: past the guide line.
pub const MEMBER_INDENT: f32 = 38.0;

/// Where an entry's row sits, which decides its size and padding.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// A row of the menu; `trail` keeps the count's room at its end.
    Top { trail: bool },
    /// A row of a group, under its group row.
    Member,
}

/// Padding around a reason's text, in its popup.
const REASON_PADDING: u16 = 10;

/// The parts of an entry's row: check column, icon, label and shortcut.
fn entry_content<Message: Clone + 'static>(
    label: Cow<'static, str>,
    icon: IconSlot,
    checked: Option<bool>,
    key: String,
    key_class: theme::Text,
) -> Vec<crate::ui::Element<'static, Message>> {
    let spacing = crate::ui::theme::spacing();
    let mut items: Vec<crate::ui::Element<'static, Message>> = Vec::with_capacity(7);

    if let Some(checked) = checked {
        items.push(if checked {
            widget::icon::from_name("object-select-symbolic")
                .size(16)
                .icon()
                .class(theme::Svg::Custom(Rc::new(|theme| {
                    iced::widget::svg::Style {
                        color: Some(theme.cosmic().accent_text_color().to_color()),
                    }
                })))
                .width(Length::Fixed(16.0))
                .into()
        } else {
            widget::space::horizontal()
                .width(Length::Fixed(16.0))
                .into()
        });
        items.push(
            widget::space::horizontal()
                .width(spacing.space_xxs.to_length())
                .into(),
        );
    }

    match icon {
        IconSlot::Icon(icon) => {
            items.push(widget::icon::icon(icon).size(14).into());
            items.push(
                widget::space::horizontal()
                    .width(spacing.space_xxs.to_length())
                    .into(),
            );
        }
        IconSlot::Reserved => {
            items.push(
                widget::space::horizontal()
                    .width(Length::Fixed(14.0))
                    .into(),
            );
            items.push(
                widget::space::horizontal()
                    .width(spacing.space_xxs.to_length())
                    .into(),
            );
        }
        IconSlot::None => {}
    }

    let ellipsize = widget::EllipsizeMode::Middle(1);
    items.push(widget::ellipsize::text(label, ellipsize).into());
    items.push(widget::space::horizontal().into());
    items.push(
        widget::ellipsize::text(key, ellipsize)
            .class(key_class)
            .into(),
    );
    items
}

/// A bundled symbolic icon.
fn bundled_icon(bytes: &'static [u8]) -> icon::Handle {
    let mut handle = icon::from_svg_bytes(bytes);
    handle.symbolic = true;
    handle
}

/// A variant's pin: filled when it is the row's usual action.
fn pin_button<Message: Clone + 'static>(
    pinned: bool,
    message: Message,
) -> crate::ui::Element<'static, Message> {
    static PIN: &[u8] = include_bytes!("../../../../res/icons/symbolic/pin-symbolic.svg");
    static PINNED: &[u8] = include_bytes!("../../../../res/icons/symbolic/pin-filled-symbolic.svg");
    let icon = widget::icon::icon(bundled_icon(if pinned { PINNED } else { PIN })).size(16);
    let icon = if pinned {
        icon.class(theme::Svg::Custom(Rc::new(|theme| {
            iced::widget::svg::Style {
                color: Some(theme.cosmic().accent_text_color().to_color()),
            }
        })))
    } else {
        // A pin not pinned is faint, so it does not compete with the labels
        icon.class(theme::Svg::Custom(Rc::new(|theme| {
            let mut color = theme.cosmic().background(theme.transparent).component.on;
            color.alpha *= 0.4;
            iced::widget::svg::Style {
                color: Some(color.to_color()),
            }
        })))
    };
    widget::button::custom(icon)
        .padding(4)
        .class(theme::Button::MenuItem)
        .on_press(message)
        .into()
}

/// The popup beside a dimmed row saying why it cannot run.
fn reason_tree<Message: Clone + 'static>(reason: Cow<'static, str>) -> (MenuTree<Message>, u16) {
    let width = crate::ui::font::text_width(&reason, 14.0, crate::ui::font::default())
        + f32::from(2 * REASON_PADDING)
        + 2.0;
    let text = widget::container(widget::text::body(reason)).padding([6, REASON_PADDING]);
    (
        MenuTree::from(crate::ui::Element::from(text)).passive(),
        width.ceil() as u16,
    )
}

fn key_for<A: MenuAction>(key_binds: &HashMap<KeyBind, A>, action: &A) -> String {
    key_binds
        .iter()
        .find(|(_, a)| *a == action)
        .map_or_else(String::new, |(k, _)| k.to_string())
}

fn entry_tree<
    A: MenuAction<Message = Message>,
    L: Into<Cow<'static, str>>,
    Message: Clone + 'static,
>(
    entry: Entry<A, L>,
    key_binds: &HashMap<KeyBind, A>,
    key_class: theme::Text,
    place: Place,
) -> MenuTree<Message> {
    let Entry {
        label,
        icon,
        checked,
        enabled,
        action,
        reason,
        pin,
    } = entry;
    let key = key_for(key_binds, &action);
    let mut items = entry_content(label.into(), icon, checked, key, key_class);
    if let Some((pinned, pin_action)) = pin {
        items.push(pin_button(pinned, pin_action.message()));
    }
    let trail = matches!(place, Place::Top { trail: true } | Place::Member);
    if trail {
        items.push(
            widget::space::horizontal()
                .width(Length::Fixed(TRAIL))
                .into(),
        );
    }

    let mut button = menu_button(items);
    if trail {
        button = button.padding(iced::Padding {
            top: 4.0,
            right: 0.0,
            bottom: 4.0,
            left: 16.0,
        });
    }
    if place == Place::Member {
        // Its shortcut and pin end where the other rows' do: before the
        // room the counts take
        button = button
            .height(Length::Fixed(MEMBER_HEIGHT))
            .padding(iced::Padding {
                top: 4.0,
                right: 0.0,
                bottom: 4.0,
                left: MEMBER_INDENT,
            });
    }
    if enabled {
        button = button.on_press(action.message());
        return MenuTree::from(Element::from(button)).selectable(true);
    }
    match reason {
        Some(reason) => {
            let (child, width) = reason_tree(reason.into());
            MenuTree::with_children(Element::from(button), vec![child])
                .child_open(ChildOpen::Rest(REASON_TIME))
                .width(width)
        }
        None => MenuTree::from(Element::from(button)),
    }
}

/// A group row followed by its rows, folded under it: the count is how
/// many of them are actions.
fn group_trees<
    A: MenuAction<Message = Message>,
    L: Into<Cow<'static, str>> + 'static,
    Message: Clone + 'static,
>(
    mut head: MenuTree<Message>,
    rows: Vec<MenuItem<A, L>>,
    key_binds: &HashMap<KeyBind, A>,
) -> Vec<MenuTree<Message>> {
    let count = rows
        .iter()
        .filter(|row| !matches!(row, MenuItem::Divider))
        .count();
    let mut members = items(key_binds, rows, true);
    for member in &mut members {
        member.group = Group::Member;
    }
    head.group = Group::Head {
        rows: members.len(),
        count,
    };
    // The keys stop on it to unfold it, whether or not it runs anything
    head.selectable = true;
    std::iter::once(head).chain(members).collect()
}

/// A split row: the entry's action, its variants folded under it. With
/// nothing in it able to run it is a dimmed row, with its reason.
fn split_trees<
    A: MenuAction<Message = Message>,
    L: Into<Cow<'static, str>> + 'static,
    Message: Clone + 'static,
>(
    entry: Entry<A, L>,
    variants: Vec<MenuItem<A, L>>,
    key_binds: &HashMap<KeyBind, A>,
    key_class: theme::Text,
) -> Vec<MenuTree<Message>> {
    let any_variant = variants.iter().any(MenuItem::runs);
    let head = entry_tree(entry, key_binds, key_class, Place::Top { trail: true });
    // With no variant, or none that can run, it is a plain row
    if variants.is_empty() || !any_variant && !head.pressable {
        return vec![head];
    }
    group_trees(head, variants, key_binds)
}

/// Another program's row, with an icon: its actions folded under it. It
/// runs nothing itself, but looks live while something in it can run.
fn submenu_trees<
    A: MenuAction<Message = Message>,
    L: Into<Cow<'static, str>> + 'static,
    Message: Clone + 'static,
>(
    label: L,
    icon: Option<icon::Handle>,
    children: Vec<MenuItem<A, L>>,
    key_binds: &HashMap<KeyBind, A>,
    key_class: theme::Text,
) -> Vec<MenuTree<Message>> {
    let live = children.iter().any(MenuItem::runs);
    let mut items = entry_content(
        label.into(),
        IconSlot::from(icon),
        None,
        String::new(),
        key_class,
    );
    items.push(
        widget::space::horizontal()
            .width(Length::Fixed(TRAIL))
            .into(),
    );
    let button = menu_button(items)
        .padding(iced::Padding {
            top: 4.0,
            right: 0.0,
            bottom: 4.0,
            left: 16.0,
        })
        .force_enabled(live);
    let head = MenuTree::from(crate::ui::Element::from(button));
    if live {
        group_trees(head, children, key_binds)
    } else {
        vec![head]
    }
}

impl<A: MenuAction, L: Into<Cow<'static, str>>> MenuItem<A, L> {
    /// Whether choosing the item, or something under it, can run.
    fn runs(&self) -> bool {
        match self {
            Self::Button(..) | Self::CheckBox(..) => true,
            Self::ButtonDisabled(..) | Self::Divider => false,
            Self::Entry(entry) => entry.enabled,
            Self::Split(entry, variants) => entry.enabled || variants.iter().any(Self::runs),
            Self::Submenu(_, _, children) | Self::Folder(_, children) => {
                children.iter().any(Self::runs)
            }
        }
    }

    /// Whether the item can be a group row, with a count at its end.
    fn has_count(&self) -> bool {
        matches!(self, Self::Split(..) | Self::Submenu(..))
    }
}

/// Create a list of menu items from a vector of `MenuItem`.
///
/// The `MenuItem` can be either an action or a separator.
///
/// # Arguments
/// - `key_binds` - A reference to a `HashMap` that maps `KeyBind` to `A`.
/// - `children` - A vector of `MenuItem`.
///
/// # Returns
/// - A vector of `MenuTree`.
#[must_use]
pub fn menu_items<
    A: MenuAction<Message = Message>,
    L: Into<Cow<'static, str>> + 'static,
    Message: 'static + std::clone::Clone,
>(
    key_binds: &HashMap<KeyBind, A>,
    children: Vec<MenuItem<A, L>>,
) -> Vec<MenuTree<Message>> {
    items(key_binds, children, false)
}

/// The rows of `children`: a menu's own rows, or, as `members`, the rows
/// of a group under its group row.
fn items<
    A: MenuAction<Message = Message>,
    L: Into<Cow<'static, str>> + 'static,
    Message: 'static + std::clone::Clone,
>(
    key_binds: &HashMap<KeyBind, A>,
    children: Vec<MenuItem<A, L>>,
    members: bool,
) -> Vec<MenuTree<Message>> {
    // `theme::Text::Custom` holds a `fn(&Theme) -> crate::ui::widget::text::Style`:
    // this crate's text `Style` (colour plus the two selection colours), not
    // `iced_core`'s one-field `text::Style` this file imports as `TextStyle`.
    fn key_style(theme: &crate::ui::Theme) -> crate::ui::widget::text::Style {
        let mut color = theme.cosmic().background(theme.transparent).component.on;
        color.alpha *= 0.75;
        crate::ui::widget::text::Style {
            color: Some(color.to_color()),
            ..Default::default()
        }
    }
    let key_class = theme::Text::Custom(key_style);

    let size = children.len();
    // In a menu with counts every row keeps the count's room, so labels
    // and shortcuts line up
    let place = if members {
        Place::Member
    } else {
        Place::Top {
            trail: children.iter().any(MenuItem::has_count),
        }
    };

    children
        .into_iter()
        .enumerate()
        .flat_map(|(i, item)| {
            let mut trees = vec![];

            match item {
                MenuItem::Button(label, icon, action) => {
                    let mut entry = Entry::new(label, action);
                    entry.icon = icon.into();
                    trees.push(entry_tree(entry, key_binds, key_class, place));
                }
                MenuItem::ButtonDisabled(label, icon, action) => {
                    let mut entry = Entry::new(label, action).enabled(false);
                    entry.icon = icon.into();
                    trees.push(entry_tree(entry, key_binds, key_class, place));
                }
                MenuItem::CheckBox(label, icon, value, action) => {
                    let mut entry = Entry::new(label, action).checked(value);
                    entry.icon = icon.into();
                    trees.push(entry_tree(entry, key_binds, key_class, place));
                }
                MenuItem::Entry(entry) => {
                    trees.push(entry_tree(entry, key_binds, key_class, place));
                }
                // A group's rows do not nest groups: a variant is one row
                MenuItem::Split(entry, _) if members => {
                    trees.push(entry_tree(entry, key_binds, key_class, place));
                }
                MenuItem::Split(entry, variants) => {
                    trees.extend(split_trees(entry, variants, key_binds, key_class));
                }
                MenuItem::Submenu(label, icon, children) => {
                    trees.extend(submenu_trees(label, icon, children, key_binds, key_class));
                }
                MenuItem::Folder(label, children) => {
                    let l: Cow<'static, str> = label.into();
                    let live = !children.is_empty();
                    let tree = MenuTree::<Message>::with_children(
                        RcElementWrapper::new(crate::ui::Element::from(
                            menu_button::<'static, _>(vec![
                                widget::ellipsize::text(
                                    l.clone(),
                                    widget::EllipsizeMode::Middle(1),
                                )
                                .into(),
                                widget::space::horizontal().into(),
                                widget::icon::from_name("pan-end-symbolic")
                                    .size(16)
                                    .icon()
                                    .into(),
                            ])
                            .class(
                                // Menu folders have no on_press so they take on the disabled style by default
                                if children.is_empty() {
                                    // This will make the folder use the disabled style if it has no children
                                    theme::Button::MenuItem
                                } else {
                                    // This will make the folder use the enabled style if it has children
                                    theme::Button::MenuFolder
                                },
                            ),
                        )),
                        menu_items(key_binds, children),
                    );
                    trees.push(if live { tree.selectable(false) } else { tree });
                }
                MenuItem::Divider => {
                    if i != size - 1 {
                        trees.push(MenuTree::<Message>::from(Element::from(
                            widget::divider::horizontal::light(),
                        )));
                    }
                }
            }
            trees
        })
        .collect()
}

/// Create a menu tree from a widget and a vector of sub trees
pub fn nav_context<
    A: MenuAction<Message = Message>,
    L: Into<Cow<'static, str>> + From<&'static str> + 'static,
    Message: 'static + std::clone::Clone,
>(
    key_binds: &HashMap<KeyBind, A>,
    children: Vec<Vec<MenuItem<A, L>>>,
) -> Vec<MenuTree<Message>> {
    let menus = children
        .into_iter()
        .map(|m| MenuItem::<A, L>::Folder(L::from(""), m));
    let root = vec![MenuItem::<A, L>::Folder(L::from(""), menus.collect())];
    menu_items(key_binds, root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Act {
        Open,
        OpenTab,
        Paste,
        Pin,
        Peazip,
    }

    impl MenuAction for Act {
        type Message = Act;
        fn message(&self) -> Act {
            *self
        }
    }

    fn build(items: Vec<MenuItem<Act, &'static str>>) -> Vec<MenuTree<Act>> {
        menu_items(&HashMap::new(), items)
    }

    fn groups(trees: &[MenuTree<Act>]) -> Vec<Group> {
        trees.iter().map(|tree| tree.group).collect()
    }

    fn height(tree: &MenuTree<Act>) -> Length {
        tree.item
            .element
            .with_data_mut(|w| w.as_widget_mut().size().height)
    }

    /// A split row is its default action, followed by its other ways as
    /// rows of the same menu, folded under it: no popup of its own.
    #[test]
    fn a_split_row_is_followed_by_its_variants_folded_under_it() {
        let trees = build(vec![MenuItem::Split(
            Entry::new("Open", Act::Open),
            vec![
                MenuItem::Entry(Entry::new("Open in new tab", Act::OpenTab).pin(false, Act::Pin)),
                MenuItem::Divider,
                MenuItem::Button("Open with", None, Act::Paste),
            ],
        )]);
        assert_eq!(
            groups(&trees),
            [
                Group::Head { rows: 3, count: 2 },
                Group::Member,
                Group::Member,
                Group::Member
            ]
        );
        assert!(trees[0].selectable && trees[0].pressable);
        assert!(trees[0].children.is_empty(), "no popup of its own");
        assert!(trees[1].selectable && trees[1].pressable);
        assert!(!trees[2].selectable, "the divider");
        assert_eq!(height(&trees[0]), Length::Fixed(36.0));
        assert_eq!(height(&trees[1]), Length::Fixed(MEMBER_HEIGHT));
    }

    /// A dimmed row is not stopped on by the keys and shows its reason in a
    /// popup of its own once the pointer rests on it.
    #[test]
    fn a_dimmed_row_rests_before_showing_why() {
        let trees = build(vec![MenuItem::Entry(
            Entry::new("Paste", Act::Paste)
                .enabled(false)
                .reason("Nothing to paste"),
        )]);
        let row = &trees[0];
        assert!(!row.selectable);
        assert_eq!(row.child_open, ChildOpen::Rest(REASON_TIME));
        assert_eq!(row.children.len(), 1);
        assert!(row.children[0].passive);
        assert!(!row.children[0].selectable);
        assert!(row.width.is_some_and(|w| w > 2 * REASON_PADDING));
    }

    #[test]
    fn a_dimmed_row_without_a_reason_has_no_popup() {
        let trees = build(vec![MenuItem::Entry(
            Entry::new("Paste", Act::Paste).enabled(false),
        )]);
        assert!(trees[0].children.is_empty());
    }

    /// A split row whose action cannot run but a variant can is dimmed
    /// with its reason and still unfolds; one where nothing can run is a
    /// plain dimmed row.
    #[test]
    fn a_split_row_is_dimmed_only_when_nothing_in_it_runs() {
        let half = build(vec![MenuItem::Split(
            Entry::new("Open", Act::Open)
                .enabled(false)
                .reason("Files and folders do not open together"),
            vec![MenuItem::Entry(Entry::new("Open in new tab", Act::OpenTab))],
        )]);
        assert_eq!(half[0].group, Group::Head { rows: 1, count: 1 });
        assert!(half[0].selectable, "the keys stop on it to unfold it");
        assert!(!half[0].pressable);
        assert_eq!(half[0].child_open, ChildOpen::Rest(REASON_TIME));

        let none = build(vec![MenuItem::Split(
            Entry::new("Open", Act::Open)
                .enabled(false)
                .reason("Files and folders do not open together"),
            vec![MenuItem::Entry(
                Entry::new("Open in new tab", Act::OpenTab).enabled(false),
            )],
        )]);
        assert_eq!(groups(&none), [Group::None]);
        assert_eq!(none[0].child_open, ChildOpen::Rest(REASON_TIME));
        assert!(!none[0].selectable);
    }

    /// Another program's actions unfold under its row as well; its row runs
    /// nothing itself.
    #[test]
    fn a_program_row_unfolds_its_actions() {
        let trees = build(vec![MenuItem::Submenu(
            "PeaZip",
            None,
            vec![
                MenuItem::Button("Extract here", None, Act::Peazip),
                MenuItem::Divider,
                MenuItem::Button("Add to archive", None, Act::Peazip),
            ],
        )]);
        assert_eq!(trees[0].group, Group::Head { rows: 3, count: 2 });
        assert!(trees[0].selectable && !trees[0].pressable);
        assert!(trees[0].children.is_empty());

        let dead = build(vec![MenuItem::Submenu(
            "PeaZip",
            None,
            vec![MenuItem::ButtonDisabled("Extract here", None, Act::Peazip)],
        )]);
        assert_eq!(groups(&dead), [Group::None]);
        assert!(!dead[0].selectable);
    }

    #[test]
    fn dividers_are_not_stopped_on() {
        let trees = build(vec![
            MenuItem::Button("Open", None, Act::Open),
            MenuItem::Divider,
            MenuItem::Button("Paste", None, Act::Paste),
        ]);
        let stops: Vec<bool> = trees.iter().map(|t| t.selectable).collect();
        assert_eq!(stops, [true, false, true]);
    }
}
