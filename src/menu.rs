// SPDX-License-Identifier: GPL-3.0-only

use crate::mime_app::Mime;
use crate::ui::Element;
use crate::ui::iced::keyboard::Modifiers;
use crate::ui::shell::Core;
use crate::ui::widget::menu::action::MenuAction;
use crate::ui::widget::menu::key_bind::KeyBind;
use crate::ui::widget::menu::{self, ItemHeight, ItemWidth, MenuBar};
use crate::ui::widget::{self, responsive_menu_bar};
use i18n_embed::LanguageLoader;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::app::{Action, Message};
use crate::config::{Config, ContextActionPreset};
use crate::fl;
use crate::service_menu::{ServiceAction, ServiceEntry, ServiceItem};
use crate::tab::{
    self, HeadingOptions, ItemMetadata, Location, LocationMenuAction, SearchLocation, Tab,
};
use crate::trash::{Trash, TrashExt};
use crate::ui::theme::spacing;

/// The name of the responsive menu bar's widget id.
///
/// `widget::Id`s compare by their input string, so storing the name is
/// sufficient. `responsive_menu_bar` also needs the name because `Id` has no
/// `Display`.
const MENU_ID: &str = "responsive-menu";

const fn menu_button_optional(
    label: String,
    action: Action,
    enabled: bool,
) -> menu::Item<Action, String> {
    if enabled {
        menu::Item::Button(label, None, action)
    } else {
        menu::Item::ButtonDisabled(label, None, action)
    }
}

/// A menu action dispatched to the tab the menu was opened on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabAction(pub Action);

impl MenuAction for TabAction {
    type Message = tab::Message;

    fn message(&self) -> tab::Message {
        tab::Message::ContextAction(self.0)
    }
}

/// A split row whose usual action can be pinned to one of its variants.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum SplitRow {
    OpenFolder,
    Cut,
    Copy,
    Extract,
    Trash,
    NewFolder,
}

/// A built-in variant of a split row, which can be pinned as its usual
/// action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum MenuVariant {
    OpenInNewTab,
    OpenInNewWindow,
    OpenTerminal,
    MoveTo,
    CopyPath,
    CopyTo,
    ExtractAsFolder,
    ExtractTo,
    PermanentlyDelete,
    NewFile,
}

/// The variant pinned as each split row's usual action.
pub type MenuPins = BTreeMap<SplitRow, MenuVariant>;

/// No pins, for the file chooser.
pub static NO_PINS: MenuPins = BTreeMap::new();

/// An application the selection can be opened with, for the Open row.
#[derive(Clone, Debug)]
pub struct MenuApp {
    pub name: String,
    pub icon: widget::icon::Handle,
    /// Whether it is the default application for the selection's type.
    pub is_default: bool,
}

/// What the context menu shows besides the tab's own state.
#[derive(Clone)]
pub struct Sources<'a> {
    pub clipboard_paste_available: bool,
    pub context_actions: &'a [ContextActionPreset],
    /// Applications opening every selected item.
    pub apps: Rc<[MenuApp]>,
    /// Whether pinning an application makes it the default: the selection
    /// is all of one type.
    pub apps_pinnable: bool,
    /// Other programs' actions for the selection.
    pub services: Rc<[ServiceEntry]>,
    pub pins: &'a MenuPins,
    /// What `apps` and `services` were made from (see [`choice_fingerprint`]),
    /// carried by the actions that name them by number.
    pub fingerprint: u64,
    /// The same with the selection's type too, for the pins that make an
    /// application its default.
    pub default_fingerprint: u64,
}

impl Sources<'_> {
    /// Sources with no applications, other programs or pins.
    pub fn plain(
        clipboard_paste_available: bool,
        context_actions: &[ContextActionPreset],
    ) -> Sources<'_> {
        Sources {
            clipboard_paste_available,
            context_actions,
            apps: Rc::from([]),
            apps_pinnable: false,
            services: Rc::from([]),
            pins: &NO_PINS,
            fingerprint: 0,
            default_fingerprint: 0,
        }
    }
}

/// What the numbered choices of a context menu were drawn from: the
/// applications by id, the selection's type when it is one, and other
/// programs' actions. An open menu is a copy made when it opened, so a
/// choice is run only when this is still the same: a type found later by
/// content, or a reload, can change the lists under the numbers.
pub fn choice_fingerprint<'a>(
    app_ids: impl IntoIterator<Item = &'a str>,
    mime: Option<&Mime>,
    services: &[ServiceEntry],
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    for id in app_ids {
        id.hash(&mut hasher);
    }
    mime.map(Mime::as_str).hash(&mut hasher);
    services.hash(&mut hasher);
    hasher.finish()
}

/// One of the menu's own icons, bundled so every icon theme shows them.
fn menu_icon(name: &str) -> widget::icon::Handle {
    macro_rules! icons {
        ($($name:literal),* $(,)?) => {
            match name {
                $($name => include_bytes!(concat!(
                    "../res/icons/symbolic/menu-", $name, "-symbolic.svg"
                )).as_slice(),)*
                _ => include_bytes!("../res/icons/symbolic/menu-open-symbolic.svg").as_slice(),
            }
        };
    }
    let bytes: &'static [u8] = icons!(
        "open",
        "apps",
        "tab",
        "window",
        "terminal",
        "cut",
        "copy",
        "paste",
        "rename",
        "path",
        "move",
        "copy-to",
        "sidebar",
        "extract",
        "compress",
        "details",
        "trash",
        "delete",
        "new-folder",
        "new-file",
        "select-all",
        "sort",
        "hidden",
        "eject",
        "restore",
        "location",
        "recent",
    );
    let mut handle = widget::icon::from_svg_bytes(bytes);
    handle.symbolic = true;
    handle
}

/// A way of doing a split row's thing: the built-in one, or a variant.
#[derive(Clone)]
struct Choice {
    variant: Option<MenuVariant>,
    label: String,
    icon: widget::icon::Handle,
    action: Action,
}

impl MenuVariant {
    fn choice(self) -> Choice {
        let (label, icon, action) = match self {
            Self::OpenInNewTab => (fl!("open-in-new-tab"), "tab", Action::OpenInNewTab),
            Self::OpenInNewWindow => (fl!("open-in-new-window"), "window", Action::OpenInNewWindow),
            Self::OpenTerminal => (fl!("open-in-terminal"), "terminal", Action::OpenTerminal),
            Self::MoveTo => (fl!("move-to"), "move", Action::MoveTo),
            Self::CopyPath => (fl!("copy-path"), "path", Action::CopyPath),
            Self::CopyTo => (fl!("copy-to"), "copy-to", Action::CopyTo),
            Self::ExtractAsFolder => (fl!("extract-as-folder"), "extract", Action::ExtractAsFolder),
            Self::ExtractTo => (fl!("extract-to"), "extract", Action::ExtractTo),
            Self::PermanentlyDelete => (
                fl!("delete-permanently"),
                "delete",
                Action::PermanentlyDelete,
            ),
            Self::NewFile => (fl!("new-file"), "new-file", Action::NewFile),
        };
        Choice {
            variant: Some(self),
            label,
            icon: menu_icon(icon),
            action,
        }
    }
}

impl Choice {
    fn builtin(label: String, icon: &str, action: Action) -> Self {
        Self {
            variant: None,
            label,
            icon: menu_icon(icon),
            action,
        }
    }

    fn entry(&self) -> menu::Entry<TabAction, String> {
        menu::Entry::new(self.label.clone(), TabAction(self.action)).icon(self.icon.clone())
    }
}

/// A split row: `builtin` on its left part unless a variant is pinned, its
/// variants behind the arrow, each with a pin. With a variant pinned, that
/// variant is the left part and `builtin` comes first among the variants,
/// so it can be run or pinned back. `extra` items follow the variants
/// unpinned.
fn split_row(
    row: SplitRow,
    builtin: Choice,
    variants: Vec<Choice>,
    extra: Vec<menu::Item<TabAction, String>>,
    pins: &MenuPins,
) -> menu::Item<TabAction, String> {
    let pin = |choice: &Choice, pinned: bool| {
        let to = if pinned { None } else { choice.variant };
        choice
            .entry()
            .pin(pinned, TabAction(Action::PinVariant(row, to)))
    };
    let pinned = pins
        .get(&row)
        .and_then(|v| variants.iter().find(|c| c.variant == Some(*v)));
    let (left, mut list) = match pinned {
        Some(chosen) => (
            chosen
                .entry()
                .pin(true, TabAction(Action::PinVariant(row, None))),
            std::iter::once(menu::Item::Entry(pin(&builtin, false)))
                .chain(
                    variants
                        .iter()
                        .map(|c| menu::Item::Entry(pin(c, c.variant == chosen.variant))),
                )
                .collect::<Vec<_>>(),
        ),
        None => (
            builtin.entry(),
            variants
                .iter()
                .map(|c| menu::Item::Entry(pin(c, false)))
                .collect(),
        ),
    };
    if !extra.is_empty() {
        list.push(menu::Item::Divider);
        list.extend(extra);
    }
    menu::Item::Split(left, list)
}

/// A row that cannot run now, saying why when the pointer rests on it.
fn dimmed(
    label: String,
    icon: &str,
    action: Action,
    reason: String,
) -> menu::Item<TabAction, String> {
    menu::Item::Entry(
        menu::Entry::new(label, TabAction(action))
            .icon(menu_icon(icon))
            .enabled(false)
            .reason(reason),
    )
}

/// A plain row with one of the menu's icons.
fn plain(label: String, icon: &str, action: Action) -> menu::Item<TabAction, String> {
    menu::Item::Entry(menu::Entry::new(label, TabAction(action)).icon(menu_icon(icon)))
}

/// The sort row: its label is the order in words, its left part reverses
/// it, and its variants are the sort keys and Folders first.
fn sort_row(tab: &Tab, keys: [(String, HeadingOptions); 4]) -> menu::Item<TabAction, String> {
    let (sort_name, sort_direction, _) = tab.sort_options();
    let mut variants: Vec<menu::Item<TabAction, String>> = keys
        .into_iter()
        .map(|(label, key)| {
            menu::Item::Entry(
                menu::Entry::new(label, TabAction(Action::ToggleSort(key)))
                    .checked(sort_name == key),
            )
        })
        .collect();
    variants.push(menu::Item::Divider);
    variants.push(menu::Item::Entry(
        menu::Entry::new(
            fl!("list-directories-first"),
            TabAction(Action::ToggleFoldersFirst),
        )
        .checked(tab.config.folders_first),
    ));
    menu::Item::Split(
        menu::Entry::new(
            tab::sort_order_words(sort_name, sort_direction),
            TabAction(Action::SetSort(sort_name, !sort_direction)),
        )
        .icon(menu_icon("sort")),
        variants,
    )
}

fn sort_keys() -> [(String, HeadingOptions); 4] {
    [
        (fl!("name"), HeadingOptions::Name),
        (fl!("modified"), HeadingOptions::Modified),
        (fl!("size"), HeadingOptions::Size),
        (fl!("type-heading"), HeadingOptions::Type),
    ]
}

fn trash_sort_keys() -> [(String, HeadingOptions); 4] {
    [
        (fl!("name"), HeadingOptions::Name),
        (fl!("trashed-on"), HeadingOptions::TrashedOn),
        (fl!("size"), HeadingOptions::Size),
        (fl!("type-heading"), HeadingOptions::Type),
    ]
}

/// Other programs' rows: each program with several actions as a row
/// opening them, a lone action as a row of its own, then the user's own
/// context actions.
fn program_rows(
    sources: &Sources,
    selected: usize,
    selected_dir: usize,
) -> Vec<menu::Item<TabAction, String>> {
    let service_icon = |name: &Option<String>| {
        name.as_deref()
            .map(|name| widget::icon::from_name(name).size(16).handle())
    };
    let action_item = |entry: u16, item: u16, action: &ServiceAction| {
        menu::Item::Button(
            action.name.clone(),
            service_icon(&action.icon),
            TabAction(Action::RunServiceAction(entry, item, sources.fingerprint)),
        )
    };
    let mut rows: Vec<menu::Item<TabAction, String>> = sources
        .services
        .iter()
        .enumerate()
        .map(|(e, entry)| {
            let e = e as u16;
            match entry {
                ServiceEntry::Program { name, icon, items } => menu::Item::Submenu(
                    name.clone(),
                    service_icon(icon),
                    items
                        .iter()
                        .enumerate()
                        .map(|(i, item)| match item {
                            ServiceItem::Action(action) => action_item(e, i as u16, action),
                            ServiceItem::Separator => menu::Item::Divider,
                        })
                        .collect(),
                ),
                ServiceEntry::Action(action) => action_item(e, 0, action),
            }
        })
        .collect();
    rows.extend(
        sources
            .context_actions
            .iter()
            .enumerate()
            .filter(|(_, action)| action.matches_selection(selected, selected_dir))
            .map(|(i, action)| {
                menu::Item::Button(
                    action.name.clone(),
                    None,
                    TabAction(Action::RunContextAction(i)),
                )
            }),
    );
    rows
}

/// Appends `group` to `children`, after a divider when both have rows.
fn push_group(
    children: &mut Vec<menu::Item<TabAction, String>>,
    group: Vec<menu::Item<TabAction, String>>,
) {
    if group.is_empty() {
        return;
    }
    if !children.is_empty() {
        children.push(menu::Item::Divider);
    }
    children.extend(group);
}

pub fn context_menu(
    tab: &Tab,
    key_binds: &HashMap<KeyBind, Action>,
    sources: &Sources,
) -> Vec<menu::Tree<tab::Message>> {
    let children = context_menu_items(tab, sources);
    let key_binds: HashMap<KeyBind, TabAction> = key_binds
        .iter()
        .map(|(key_bind, action)| (key_bind.clone(), TabAction(*action)))
        .collect();
    menu::items(&key_binds, children)
}

/// The rows of `tab`'s context menu, for what is selected and where.
fn context_menu_items(tab: &Tab, sources: &Sources) -> Vec<menu::Item<TabAction, String>> {
    let menu_item =
        |label: String, action: Action| menu::Item::Button(label, None, TabAction(action));
    let pins = sources.pins;

    // Allow paste when clipboard has data and we're in a location that supports it
    let can_paste = sources.clipboard_paste_available && tab.location.supports_paste();
    let paste_row = || {
        if can_paste {
            plain(fl!("paste"), "paste", Action::Paste)
        } else {
            dimmed(
                fl!("paste"),
                "paste",
                Action::Paste,
                fl!("dimmed-nothing-to-paste"),
            )
        }
    };

    let mut selected_dir = 0;
    let mut selected = 0;
    let mut selected_trash_only = false;
    let mut selected_desktop_entry = None;
    let mut selected_types: Vec<Mime> = vec![];
    let mut selected_mount_point = 0;
    let mut any_trash_item = false;
    // Nothing with no record of where it came from can be put back
    let mut any_unknown_origin = false;
    if let Some(items) = tab.items_opt() {
        for item in items {
            if item.selected {
                selected += 1;
                if item.metadata.is_dir() {
                    selected_mount_point += i32::from(item.is_mount_point);
                    selected_dir += 1;
                }
                match &item.location_opt {
                    Some(Location::Trash) | Some(Location::Search(SearchLocation::Trash, ..)) => {
                        selected_trash_only = true
                    }
                    Some(Location::Path(path))
                        if selected == 1
                            && path.extension().and_then(|s| s.to_str()) == Some("desktop") =>
                    {
                        selected_desktop_entry = Some(&**path);
                    }
                    _ => (),
                }
                if let ItemMetadata::Trash { entry, .. } = &item.metadata {
                    any_trash_item = true;
                    any_unknown_origin |= crate::trashing::origin_unknown(entry);
                }
                selected_types.push(item.mime.clone());
            }
        }
    }
    selected_types.sort_unstable();
    selected_types.dedup();
    selected_trash_only = selected_trash_only && selected == 1;
    // Parse the desktop entry if it is the only selection
    let selected_desktop_entry = selected_desktop_entry.and_then(|path| {
        if selected == 1 {
            let lang_id = crate::localize::LANGUAGE_LOADER.current_language();
            let language = lang_id.language.as_str();
            // Cache?
            crate::desktop_entry::load_desktop_file(&[language.into()], path.into())
        } else {
            None
        }
    });
    let all_archives = !selected_types.is_empty()
        && selected_types.iter().all(|t| {
            crate::archive::SUPPORTED_ARCHIVE_TYPES
                .iter()
                .any(|m| t.as_str() == *m)
        });

    // The Open row for files: the default application on its left, the
    // applications for the selection behind the arrow
    let file_open_row = || {
        let apps = sources.apps.iter().enumerate().map(|(i, app)| {
            let entry = menu::Entry::new(
                app.name.clone(),
                TabAction(Action::OpenWithApp(i as u16, sources.fingerprint)),
            )
            .icon(app.icon.clone());
            menu::Item::Entry(if sources.apps_pinnable {
                entry.pin(
                    app.is_default,
                    TabAction(Action::SetDefaultApp(i as u16, sources.default_fingerprint)),
                )
            } else {
                entry
            })
        });
        let mut variants: Vec<_> = apps.collect();
        if selected == 1 {
            if !variants.is_empty() {
                variants.push(menu::Item::Divider);
            }
            variants.push(plain(fl!("menu-open-with"), "apps", Action::OpenWith));
        }
        menu::Item::Split(
            menu::Entry::new(fl!("open"), TabAction(Action::Open)).icon(menu_icon("open")),
            variants,
        )
    };
    let folder_open_row = || {
        let mut variants = vec![
            MenuVariant::OpenInNewTab.choice(),
            MenuVariant::OpenInNewWindow.choice(),
        ];
        let mut extra = Vec::new();
        if selected == 1 {
            variants.push(MenuVariant::OpenTerminal.choice());
            extra.push(plain(fl!("menu-open-with"), "apps", Action::OpenWith));
        }
        split_row(
            SplitRow::OpenFolder,
            Choice::builtin(fl!("open"), "open", Action::Open),
            variants,
            extra,
            pins,
        )
    };
    let cut_row = || {
        split_row(
            SplitRow::Cut,
            Choice::builtin(fl!("cut"), "cut", Action::Cut),
            vec![MenuVariant::MoveTo.choice()],
            Vec::new(),
            pins,
        )
    };
    let copy_row = || {
        split_row(
            SplitRow::Copy,
            Choice::builtin(fl!("copy"), "copy", Action::Copy),
            vec![MenuVariant::CopyPath.choice(), MenuVariant::CopyTo.choice()],
            Vec::new(),
            pins,
        )
    };
    let trash_row = || {
        split_row(
            SplitRow::Trash,
            Choice::builtin(fl!("move-to-trash"), "trash", Action::Delete),
            vec![MenuVariant::PermanentlyDelete.choice()],
            Vec::new(),
            pins,
        )
    };

    let mut children: Vec<menu::Item<TabAction, String>> = Vec::new();
    match (&tab.mode, &tab.location) {
        (
            tab::Mode::App,
            Location::Path(..)
            | Location::Search(SearchLocation::Path(..), ..)
            | Location::Search(SearchLocation::Recents, ..)
            | Location::Recents
            | Location::Network(_, _, Some(_)),
        ) => {
            let in_recents = tab.location.is_recents();
            let in_search = matches!(tab.location, Location::Search(..));
            if selected_trash_only {
                children.push(plain(fl!("open"), "open", Action::Open));
                children.push(if Trash::is_empty() {
                    dimmed(
                        fl!("empty-trash"),
                        "delete",
                        Action::EmptyTrash,
                        fl!("dimmed-trash-empty"),
                    )
                } else {
                    plain(fl!("empty-trash"), "delete", Action::EmptyTrash)
                });
            } else if selected > 0 {
                // Open
                let mut open = Vec::new();
                if selected_dir == selected {
                    open.push(folder_open_row());
                } else if selected_dir == 0 {
                    open.push(file_open_row());
                    if let Some(entry) = selected_desktop_entry {
                        open.extend(
                            entry
                                .desktop_actions
                                .into_iter()
                                .enumerate()
                                .map(|(i, action)| {
                                    menu_item(action.name, Action::ExecEntryAction(i))
                                }),
                        );
                    }
                } else {
                    open.push(dimmed(
                        fl!("open"),
                        "open",
                        Action::Open,
                        fl!("dimmed-files-and-folders"),
                    ));
                }
                if selected_dir == 0 {
                    open.push(dimmed(
                        fl!("open-in-terminal"),
                        "terminal",
                        Action::OpenTerminal,
                        fl!("dimmed-folder-terminal"),
                    ));
                }
                if in_recents || in_search {
                    open.push(plain(
                        fl!("open-item-location"),
                        "location",
                        Action::OpenItemLocation,
                    ));
                }
                push_group(&mut children, open);

                // Edit
                let mut edit = Vec::new();
                if selected_mount_point == 0 {
                    edit.push(cut_row());
                }
                edit.push(copy_row());
                if tab.location.supports_paste() {
                    edit.push(paste_row());
                }
                if selected_mount_point == 0 {
                    edit.push(plain(
                        if selected > 1 {
                            fl!("rename-items", count = selected)
                        } else {
                            fl!("rename")
                        },
                        "rename",
                        Action::Rename,
                    ));
                }
                push_group(&mut children, edit);

                // Archive
                let mut archive = Vec::new();
                if all_archives {
                    archive.push(split_row(
                        SplitRow::Extract,
                        Choice::builtin(fl!("extract-here"), "extract", Action::ExtractHere),
                        vec![
                            MenuVariant::ExtractAsFolder.choice(),
                            MenuVariant::ExtractTo.choice(),
                        ],
                        Vec::new(),
                        pins,
                    ));
                }
                archive.push(plain(fl!("compress"), "compress", Action::Compress));
                push_group(&mut children, archive);

                // Other programs
                push_group(&mut children, program_rows(sources, selected, selected_dir));

                // Place and details
                let mut place = Vec::new();
                if !any_trash_item {
                    place.push(if selected_dir == selected {
                        plain(fl!("add-to-sidebar"), "sidebar", Action::AddToSidebar)
                    } else {
                        dimmed(
                            fl!("add-to-sidebar"),
                            "sidebar",
                            Action::AddToSidebar,
                            fl!("dimmed-folder-sidebar"),
                        )
                    });
                }
                if in_recents {
                    place.push(plain(
                        fl!("remove-from-recents"),
                        "recent",
                        Action::RemoveFromRecents,
                    ));
                }
                push_group(&mut children, place);
                push_group(
                    &mut children,
                    vec![plain(fl!("show-details"), "details", Action::Preview)],
                );

                // Last: the trash row, or Eject for a drive
                let mut last = Vec::new();
                if any_trash_item {
                    last.push(if any_unknown_origin {
                        dimmed(
                            fl!("restore-from-trash"),
                            "restore",
                            Action::RestoreFromTrash,
                            fl!("dimmed-origin-unknown"),
                        )
                    } else {
                        plain(
                            fl!("restore-from-trash"),
                            "restore",
                            Action::RestoreFromTrash,
                        )
                    });
                    last.push(plain(fl!("delete-permanently"), "delete", Action::Delete));
                } else if selected_mount_point == 0 {
                    last.push(trash_row());
                } else if selected == 1 {
                    last.push(plain(fl!("eject"), "eject", Action::Eject));
                }
                push_group(&mut children, last);
            } else {
                let mut new = Vec::new();
                if tab.location != Location::Recents {
                    new.push(split_row(
                        SplitRow::NewFolder,
                        Choice::builtin(fl!("new-folder"), "new-folder", Action::NewFolder),
                        vec![MenuVariant::NewFile.choice()],
                        Vec::new(),
                        pins,
                    ));
                }
                if tab.location.supports_paste() {
                    new.push(paste_row());
                }
                if tab.mode.multiple() {
                    new.push(plain(fl!("select-all"), "select-all", Action::SelectAll));
                }
                push_group(&mut children, new);
                if tab.location != Location::Recents {
                    push_group(
                        &mut children,
                        vec![plain(
                            fl!("open-in-terminal"),
                            "terminal",
                            Action::OpenTerminal,
                        )],
                    );
                }
                push_group(
                    &mut children,
                    vec![
                        sort_row(tab, sort_keys()),
                        menu::Item::Entry(
                            menu::Entry::new(
                                fl!("show-hidden-files"),
                                TabAction(Action::ToggleShowHidden),
                            )
                            .icon(menu_icon("hidden"))
                            .checked(tab.config.show_hidden),
                        ),
                    ],
                );
                // With nothing selected the details pane shows the current folder
                push_group(
                    &mut children,
                    vec![plain(fl!("show-details"), "details", Action::Preview)],
                );
            }
        }
        (
            tab::Mode::Dialog(dialog_kind),
            Location::Path(..)
            | Location::Search(SearchLocation::Path(..), ..)
            | Location::Search(SearchLocation::Recents, ..)
            | Location::Recents
            | Location::Network(_, _, Some(_)),
        ) => {
            if selected > 0 {
                let mut open = Vec::new();
                if selected_dir == 1 && selected == 1 || selected_dir == 0 {
                    open.push(plain(fl!("open"), "open", Action::Open));
                }
                if matches!(tab.location, Location::Search(..)) || tab.location.is_recents() {
                    open.push(plain(
                        fl!("open-item-location"),
                        "location",
                        Action::OpenItemLocation,
                    ));
                }
                push_group(&mut children, open);
                push_group(
                    &mut children,
                    vec![plain(fl!("show-details"), "details", Action::Preview)],
                );
            } else {
                let mut new = Vec::new();
                if dialog_kind.save() {
                    new.push(plain(fl!("new-folder"), "new-folder", Action::NewFolder));
                }
                if tab.mode.multiple() {
                    new.push(plain(fl!("select-all"), "select-all", Action::SelectAll));
                }
                push_group(&mut children, new);
                push_group(
                    &mut children,
                    vec![
                        sort_row(tab, sort_keys()),
                        menu::Item::Entry(
                            menu::Entry::new(
                                fl!("show-hidden-files"),
                                TabAction(Action::ToggleShowHidden),
                            )
                            .icon(menu_icon("hidden"))
                            .checked(tab.config.show_hidden),
                        ),
                    ],
                );
            }
        }
        (_, Location::Network(..)) => {
            if selected > 0 {
                if selected_dir == 1 && selected == 1 || selected_dir == 0 {
                    children.push(menu_item(fl!("open"), Action::Open));
                }
            } else {
                if tab.mode.multiple() {
                    children.push(menu_item(fl!("select-all"), Action::SelectAll));
                }
                push_group(&mut children, vec![sort_row(tab, sort_keys())]);
            }
        }
        (_, Location::Trash | Location::Search(SearchLocation::Trash, ..)) => {
            if selected > 0 {
                children.push(if any_unknown_origin {
                    dimmed(
                        fl!("restore-from-trash"),
                        "restore",
                        Action::RestoreFromTrash,
                        fl!("dimmed-origin-unknown"),
                    )
                } else {
                    plain(
                        fl!("restore-from-trash"),
                        "restore",
                        Action::RestoreFromTrash,
                    )
                });
                children.push(plain(fl!("delete-permanently"), "delete", Action::Delete));
                push_group(&mut children, program_rows(sources, selected, selected_dir));
                push_group(
                    &mut children,
                    vec![plain(fl!("show-details"), "details", Action::Preview)],
                );
            } else {
                if tab.mode.multiple() {
                    children.push(plain(fl!("select-all"), "select-all", Action::SelectAll));
                }
                push_group(&mut children, vec![sort_row(tab, trash_sort_keys())]);
                push_group(
                    &mut children,
                    vec![if Trash::is_empty() {
                        dimmed(
                            fl!("empty-trash"),
                            "delete",
                            Action::EmptyTrash,
                            fl!("dimmed-trash-empty"),
                        )
                    } else {
                        plain(fl!("empty-trash"), "delete", Action::EmptyTrash)
                    }],
                );
            }
        }
    }

    children
}

pub fn dialog_menu(
    tab: &Tab,
    key_binds: &HashMap<KeyBind, Action>,
    show_details: bool,
) -> Element<'static, Message> {
    let (sort_name, sort_direction, _) = tab.sort_options();
    let sort_item = |label, sort, dir| {
        menu::Item::CheckBox(
            label,
            None,
            sort_name == sort && sort_direction == dir,
            Action::SetSort(sort, dir),
        )
    };
    let in_trash = tab.location.is_trash();

    let mut selected_gallery = 0;
    if let Some(items) = tab.items_opt() {
        for item in items {
            if item.selected && item.can_gallery() {
                selected_gallery += 1;
            }
        }
    }

    MenuBar::new(vec![
        menu::Tree::with_children(
            Element::from(
                widget::button::icon(widget::icon::from_name(match tab.config.view {
                    tab::View::Grid => "view-grid-symbolic",
                    tab::View::List => "view-list-symbolic",
                }))
                // This prevents the button from being shown as insensitive
                .on_press(Message::None)
                .padding(8),
            ),
            menu::items(
                key_binds,
                vec![
                    menu::Item::CheckBox(
                        fl!("grid-view"),
                        None,
                        matches!(tab.config.view, tab::View::Grid),
                        Action::TabViewGrid,
                    ),
                    menu::Item::CheckBox(
                        fl!("list-view"),
                        None,
                        matches!(tab.config.view, tab::View::List),
                        Action::TabViewList,
                    ),
                ],
            ),
        ),
        menu::Tree::with_children(
            Element::from(
                widget::button::icon(widget::icon::from_name(if sort_direction {
                    "view-sort-ascending-symbolic"
                } else {
                    "view-sort-descending-symbolic"
                }))
                // This prevents the button from being shown as insensitive
                .on_press(Message::None)
                .padding(8),
            ),
            menu::items(
                key_binds,
                vec![
                    sort_item(fl!("sort-a-z"), tab::HeadingOptions::Name, true),
                    sort_item(fl!("sort-z-a"), tab::HeadingOptions::Name, false),
                    sort_item(
                        fl!("sort-newest-first"),
                        if in_trash {
                            tab::HeadingOptions::TrashedOn
                        } else {
                            tab::HeadingOptions::Modified
                        },
                        false,
                    ),
                    sort_item(
                        fl!("sort-oldest-first"),
                        if in_trash {
                            tab::HeadingOptions::TrashedOn
                        } else {
                            tab::HeadingOptions::Modified
                        },
                        true,
                    ),
                    sort_item(
                        fl!("sort-smallest-to-largest"),
                        tab::HeadingOptions::Size,
                        true,
                    ),
                    sort_item(
                        fl!("sort-largest-to-smallest"),
                        tab::HeadingOptions::Size,
                        false,
                    ),
                    sort_item(fl!("sort-type-a-z"), tab::HeadingOptions::Type, true),
                    sort_item(fl!("sort-type-z-a"), tab::HeadingOptions::Type, false),
                ],
            ),
        ),
        menu::Tree::with_children(
            Element::from(
                widget::button::icon(widget::icon::from_name("view-more-symbolic"))
                    // This prevents the button from being shown as insensitive
                    .on_press(Message::None)
                    .padding(8),
            ),
            menu::items(
                key_binds,
                vec![
                    menu::Item::Button(fl!("zoom-in"), None, Action::ZoomIn),
                    menu::Item::Button(fl!("default-size"), None, Action::ZoomDefault),
                    menu::Item::Button(fl!("zoom-out"), None, Action::ZoomOut),
                    menu::Item::Divider,
                    menu::Item::CheckBox(
                        fl!("show-hidden-files"),
                        None,
                        tab.config.show_hidden,
                        Action::ToggleShowHidden,
                    ),
                    menu::Item::CheckBox(
                        fl!("list-directories-first"),
                        None,
                        tab.config.folders_first,
                        Action::ToggleFoldersFirst,
                    ),
                    menu::Item::CheckBox(fl!("show-details"), None, show_details, Action::Preview),
                    menu::Item::Divider,
                    menu_button_optional(
                        fl!("gallery-preview"),
                        Action::Gallery,
                        selected_gallery > 0,
                    ),
                ],
            ),
        ),
    ])
    .item_height(ItemHeight::Dynamic(40))
    .item_width(ItemWidth::Uniform(360))
    .spacing(spacing().space_xxxs.into())
    .into()
}

pub fn menu_bar<'a>(
    core: &Core,
    tab_opt: Option<&Tab>,
    config: &Config,
    modifiers: &Modifiers,
    key_binds: &HashMap<KeyBind, Action>,
    clipboard_paste_available: bool,
    can_undo: bool,
) -> Element<'a, Message> {
    let sort_options = tab_opt.map(Tab::sort_options);
    let sort_item = |label, sort, dir| {
        menu::Item::CheckBox(
            label,
            None,
            sort_options.is_some_and(|(sort_name, sort_direction, _)| {
                sort_name == sort && sort_direction == dir
            }),
            Action::SetSort(sort, dir),
        )
    };
    let in_trash = tab_opt.is_some_and(|tab| tab.location.is_trash());

    let mut selected_dir = 0;
    let mut selected = 0;
    let mut selected_gallery = 0;
    let mut any_unknown_origin = false;
    if let Some(items) = tab_opt.and_then(|tab| tab.items_opt()) {
        for item in items {
            if item.selected {
                selected += 1;
                if let ItemMetadata::Trash { entry, .. } = &item.metadata {
                    any_unknown_origin |= crate::trashing::origin_unknown(entry);
                }
                if item.metadata.is_dir() {
                    selected_dir += 1;
                }
                if item.can_gallery() {
                    selected_gallery += 1;
                }
            }
        }
    }

    // Allow paste when clipboard has data and we're in a location that supports it
    let can_paste =
        clipboard_paste_available && tab_opt.is_some_and(|tab| tab.location.supports_paste());

    // Outside the trash the label only changes when the action does: Delete
    // already deletes for good inside the trash, but anywhere else it trashes
    let (delete_item, delete_item_action) = if in_trash {
        (fl!("delete-permanently"), Action::Delete)
    } else if modifiers.shift() && !modifiers.control() {
        (fl!("delete-permanently"), Action::PermanentlyDelete)
    } else {
        (fl!("move-to-trash"), Action::Delete)
    };

    let bar = responsive_menu_bar()
        .item_height(ItemHeight::Dynamic(40))
        .item_width(ItemWidth::Uniform(360))
        .spacing(2.0)
        .into_element(
            core,
            key_binds,
            MENU_ID,
            Message::Surface,
            vec![
                (
                    fl!("file"),
                    vec![
                        menu::Item::Button(fl!("new-tab"), None, Action::TabNew),
                        menu::Item::Button(fl!("new-window"), None, Action::WindowNew),
                        menu::Item::Button(fl!("new-folder"), None, Action::NewFolder),
                        menu::Item::Button(fl!("new-file"), None, Action::NewFile),
                        menu_button_optional(
                            fl!("open"),
                            Action::Open,
                            (selected > 0 && selected_dir == 0)
                                || (selected_dir == 1 && selected == 1),
                        ),
                        menu_button_optional(
                            fl!("menu-open-with"),
                            Action::OpenWith,
                            selected == 1,
                        ),
                        menu::Item::Divider,
                        menu_button_optional(fl!("rename"), Action::Rename, selected > 0),
                        menu::Item::Divider,
                        menu::Item::Button(fl!("reload-folder"), None, Action::Reload),
                        menu::Item::Divider,
                        menu_button_optional(
                            fl!("add-to-sidebar"),
                            Action::AddToSidebar,
                            selected > 0,
                        ),
                        menu::Item::Divider,
                        menu_button_optional(
                            fl!("restore-from-trash"),
                            Action::RestoreFromTrash,
                            selected > 0 && in_trash && !any_unknown_origin,
                        ),
                        menu_button_optional(delete_item, delete_item_action, selected > 0),
                        menu::Item::Divider,
                        menu::Item::Button(fl!("close-tab"), None, Action::TabClose),
                        menu::Item::Button(fl!("quit"), None, Action::WindowClose),
                    ],
                ),
                (
                    (fl!("edit")),
                    vec![
                        menu_button_optional(fl!("undo"), Action::Undo, can_undo),
                        menu::Item::Divider,
                        menu_button_optional(fl!("cut"), Action::Cut, selected > 0),
                        menu_button_optional(fl!("copy"), Action::Copy, selected > 0),
                        menu_button_optional(fl!("move-to"), Action::MoveTo, selected > 0),
                        menu_button_optional(fl!("copy-to"), Action::CopyTo, selected > 0),
                        menu_button_optional(fl!("paste"), Action::Paste, can_paste),
                        menu::Item::Button(fl!("select-all"), None, Action::SelectAll),
                        menu::Item::Divider,
                        menu::Item::Button(fl!("history"), None, Action::EditHistory),
                    ],
                ),
                (
                    (fl!("view")),
                    vec![
                        menu::Item::Button(fl!("zoom-in"), None, Action::ZoomIn),
                        menu::Item::Button(fl!("default-size"), None, Action::ZoomDefault),
                        menu::Item::Button(fl!("zoom-out"), None, Action::ZoomOut),
                        menu::Item::Divider,
                        menu::Item::CheckBox(
                            fl!("grid-view"),
                            None,
                            tab_opt.is_some_and(|tab| matches!(tab.config.view, tab::View::Grid)),
                            Action::TabViewGrid,
                        ),
                        menu::Item::CheckBox(
                            fl!("list-view"),
                            None,
                            tab_opt.is_some_and(|tab| matches!(tab.config.view, tab::View::List)),
                            Action::TabViewList,
                        ),
                        menu::Item::Divider,
                        menu::Item::CheckBox(
                            fl!("show-hidden-files"),
                            None,
                            tab_opt.is_some_and(|tab| tab.config.show_hidden),
                            Action::ToggleShowHidden,
                        ),
                        menu::Item::CheckBox(
                            fl!("list-directories-first"),
                            None,
                            tab_opt.is_some_and(|tab| tab.config.folders_first),
                            Action::ToggleFoldersFirst,
                        ),
                        menu::Item::CheckBox(
                            fl!("show-details"),
                            None,
                            config.show_details,
                            Action::Preview,
                        ),
                        menu::Item::Divider,
                        menu_button_optional(
                            fl!("gallery-preview"),
                            Action::Gallery,
                            selected_gallery > 0,
                        ),
                        menu::Item::Divider,
                        menu::Item::Button(fl!("menu-settings"), None, Action::Settings),
                        menu::Item::Divider,
                        menu::Item::Button(fl!("menu-about"), None, Action::About),
                    ],
                ),
                (
                    (fl!("sort")),
                    vec![
                        sort_item(fl!("sort-a-z"), tab::HeadingOptions::Name, true),
                        sort_item(fl!("sort-z-a"), tab::HeadingOptions::Name, false),
                        sort_item(
                            fl!("sort-newest-first"),
                            if in_trash {
                                tab::HeadingOptions::TrashedOn
                            } else {
                                tab::HeadingOptions::Modified
                            },
                            false,
                        ),
                        sort_item(
                            fl!("sort-oldest-first"),
                            if in_trash {
                                tab::HeadingOptions::TrashedOn
                            } else {
                                tab::HeadingOptions::Modified
                            },
                            true,
                        ),
                        sort_item(
                            fl!("sort-smallest-to-largest"),
                            tab::HeadingOptions::Size,
                            true,
                        ),
                        sort_item(
                            fl!("sort-largest-to-smallest"),
                            tab::HeadingOptions::Size,
                            false,
                        ),
                        sort_item(fl!("sort-type-a-z"), tab::HeadingOptions::Type, true),
                        sort_item(fl!("sort-type-z-a"), tab::HeadingOptions::Type, false),
                    ],
                ),
            ],
        );
    // In a pill, 2 px around the roots as between them
    widget::container(bar)
        .padding(2)
        .class(crate::ui::theme::Container::Pill)
        .into()
}

/// The breadcrumb context menu. Tabs, windows and the sidebar only exist in
/// the app, so the dialog gets the details entry alone.
pub fn location_context_menu(
    ancestor_index: usize,
    mode: &tab::Mode,
) -> Vec<menu::Tree<tab::Message>> {
    let mut items = Vec::new();
    if matches!(mode, tab::Mode::App) {
        items.push(menu::Item::Button(
            fl!("open-in-new-tab"),
            None,
            LocationMenuAction::OpenInNewTab(ancestor_index),
        ));
        items.push(menu::Item::Button(
            fl!("open-in-new-window"),
            None,
            LocationMenuAction::OpenInNewWindow(ancestor_index),
        ));
        items.push(menu::Item::Divider);
    }
    items.push(menu::Item::Button(
        fl!("show-details"),
        None,
        LocationMenuAction::Preview(ancestor_index),
    ));
    if matches!(mode, tab::Mode::App) {
        items.push(menu::Item::Divider);
        items.push(menu::Item::Button(
            fl!("add-to-sidebar"),
            None,
            LocationMenuAction::AddToSidebar(ancestor_index),
        ));
    }
    menu::items(&HashMap::new(), items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{IconSizes, TabConfig, ThumbCfg};
    use crate::service_menu::ServiceAction;
    use std::fs;
    use std::path::PathBuf;

    /// A tab listing a folder holding `files` and `folders`, with `selected`
    /// selected; `types` overrides the type found for a name.
    fn tab_with(
        files: &[&str],
        folders: &[&str],
        selected: &[&str],
        types: &[(&str, &'static str)],
    ) -> (tempfile::TempDir, Tab) {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in files {
            fs::write(dir.path().join(name), b"").expect("write");
        }
        for name in folders {
            fs::create_dir(dir.path().join(name)).expect("mkdir");
        }
        let mut tab = Tab::new(
            Location::Path(dir.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(tab::scan_path(
            &dir.path().to_path_buf(),
            IconSizes::default(),
        ));
        for item in tab.items_opt_mut().expect("items") {
            item.selected = selected.contains(&item.name.as_str());
            if let Some((_, mime)) = types.iter().find(|(name, _)| *name == item.name) {
                item.mime = Mime::from_static(mime);
            }
        }
        (dir, tab)
    }

    /// The rows as text: a dimmed row in brackets, a split row with its
    /// variants after `>`, a program's row with its actions after `>>`.
    fn rows(items: &[menu::Item<TabAction, String>]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                menu::Item::Button(label, ..) | menu::Item::CheckBox(label, ..) => label.clone(),
                menu::Item::ButtonDisabled(label, ..) => format!("({label})"),
                menu::Item::Entry(entry) if entry.is_enabled() => entry.label().clone(),
                menu::Item::Entry(entry) => format!("({})", entry.label()),
                menu::Item::Split(entry, variants) => {
                    format!("{} > {}", entry.label(), rows(variants).join(", "))
                }
                menu::Item::Submenu(label, _, children) | menu::Item::Folder(label, children) => {
                    format!("{label} >> {}", rows(children).join(", "))
                }
                menu::Item::Divider => "--".into(),
            })
            .collect()
    }

    fn sources() -> Sources<'static> {
        Sources::plain(false, &[])
    }

    #[test]
    fn a_choice_runs_only_while_its_lists_are_unchanged() {
        let services = [ServiceEntry::Action(ServiceAction {
            name: "Checksum".into(),
            icon: None,
            action: "sum".into(),
            file: PathBuf::from("/menus/a.desktop"),
        })];
        let zip = Mime::from_static("application/zip");
        let png = Mime::from_static("image/png");
        let base = choice_fingerprint(["a.desktop", "b.desktop"], Some(&zip), &services);
        assert_eq!(
            base,
            choice_fingerprint(["a.desktop", "b.desktop"], Some(&zip), &services)
        );
        assert_ne!(
            base,
            choice_fingerprint(["b.desktop", "a.desktop"], Some(&zip), &services)
        );
        assert_ne!(
            base,
            choice_fingerprint(["a.desktop", "b.desktop"], Some(&png), &services)
        );
        assert_ne!(
            base,
            choice_fingerprint(["a.desktop", "b.desktop"], Some(&zip), &[])
        );
    }

    #[test]
    fn rename_items_counts_with_the_item_count_string() {
        let text = crate::fl!("rename-items", count = 3);
        assert!(text.contains('3') && text.contains("items"), "{text:?}");
    }

    #[test]
    fn an_archive_offers_extracting_and_dims_what_is_for_folders() {
        let (_dir, tab) = tab_with(
            &["photos.zip"],
            &[],
            &["photos.zip"],
            &[("photos.zip", "application/zip")],
        );
        let rows = rows(&context_menu_items(&tab, &sources()));
        assert!(
            rows.contains(&format!("({})", fl!("open-in-terminal"))),
            "{rows:#?}"
        );
        assert!(
            rows.contains(&format!("({})", fl!("paste"))),
            "nothing to paste"
        );
        assert!(rows.contains(&format!(
            "{} > {}, {}",
            fl!("extract-here"),
            fl!("extract-as-folder"),
            fl!("extract-to")
        )));
        assert!(rows.contains(&format!("({})", fl!("add-to-sidebar"))));
        assert_eq!(
            rows.last(),
            Some(&format!(
                "{} > {}",
                fl!("move-to-trash"),
                fl!("delete-permanently")
            ))
        );
    }

    #[test]
    fn a_file_that_is_not_an_archive_has_no_extract_row() {
        let (_dir, tab) = tab_with(
            &["notes.md"],
            &[],
            &["notes.md"],
            &[("notes.md", "text/markdown")],
        );
        let rows = rows(&context_menu_items(&tab, &sources()));
        assert!(
            !rows.iter().any(|row| row.starts_with(&fl!("extract-here"))),
            "{rows:#?}"
        );
        assert!(rows.contains(&fl!("compress")));
    }

    #[test]
    fn a_folder_opens_in_more_ways_and_goes_in_the_sidebar() {
        let (_dir, tab) = tab_with(&[], &["Downloads"], &["Downloads"], &[]);
        let rows = rows(&context_menu_items(&tab, &sources()));
        assert_eq!(
            rows[0],
            format!(
                "{} > {}, {}, {}, --, {}",
                fl!("open"),
                fl!("open-in-new-tab"),
                fl!("open-in-new-window"),
                fl!("open-in-terminal"),
                fl!("menu-open-with")
            )
        );
        assert!(rows.contains(&fl!("add-to-sidebar")), "{rows:#?}");
    }

    #[test]
    fn files_and_folders_together_do_not_open() {
        let (_dir, tab) = tab_with(&["a.txt"], &["b"], &["a.txt", "b"], &[]);
        let rows = rows(&context_menu_items(&tab, &sources()));
        assert_eq!(rows[0], format!("({})", fl!("open")));
        assert!(rows.contains(&fl!("rename-items", count = 2)));
    }

    #[test]
    fn empty_space_offers_new_items_and_the_sort_row() {
        let (_dir, tab) = tab_with(&["a.txt"], &[], &[], &[]);
        let rows = rows(&context_menu_items(&tab, &sources()));
        assert_eq!(
            rows[0],
            format!("{} > {}", fl!("new-folder"), fl!("new-file"))
        );
        let (sort, ascending, _) = tab.sort_options();
        let sort_row = tab::sort_order_words(sort, ascending);
        assert!(
            rows.iter()
                .any(|row| row.starts_with(&format!("{sort_row} > "))),
            "{rows:#?}"
        );
        assert_eq!(rows.last(), Some(&fl!("show-details")));
    }

    #[test]
    fn a_pinned_variant_is_the_row_and_the_built_in_comes_first() {
        let (_dir, tab) = tab_with(&["p.zip"], &[], &["p.zip"], &[("p.zip", "application/zip")]);
        let pins: MenuPins = [(SplitRow::Extract, MenuVariant::ExtractTo)].into();
        let sources = Sources {
            pins: &pins,
            ..sources()
        };
        let rows = rows(&context_menu_items(&tab, &sources));
        assert!(
            rows.contains(&format!(
                "{} > {}, {}, {}",
                fl!("extract-to"),
                fl!("extract-here"),
                fl!("extract-as-folder"),
                fl!("extract-to")
            )),
            "{rows:#?}"
        );
    }

    #[test]
    fn other_programs_are_listed_with_their_actions() {
        let (_dir, tab) = tab_with(&["a.txt"], &[], &["a.txt"], &[]);
        let action = |name: &str| ServiceAction {
            name: name.into(),
            icon: None,
            action: "run".into(),
            file: PathBuf::from("/menus/a.desktop"),
        };
        let services: Rc<[ServiceEntry]> = Rc::from([
            ServiceEntry::Program {
                name: "PeaZip".into(),
                icon: None,
                items: vec![
                    ServiceItem::Action(action("Extract here")),
                    ServiceItem::Separator,
                    ServiceItem::Action(action("Add to ZIP")),
                ],
            },
            ServiceEntry::Action(action("Checksum")),
        ]);
        let sources = Sources {
            services,
            ..sources()
        };
        let rows = rows(&context_menu_items(&tab, &sources));
        assert!(
            rows.contains(&"PeaZip >> Extract here, --, Add to ZIP".to_owned()),
            "{rows:#?}"
        );
        assert!(rows.contains(&"Checksum".to_owned()));
    }
}
