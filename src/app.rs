// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::ui::app::{self, Task};
use crate::ui::clipboard;
use crate::ui::iced::futures::{self, SinkExt};
use crate::ui::iced::keyboard::key::Physical;
use crate::ui::iced::keyboard::{Event as KeyEvent, Key, Modifiers};
use crate::ui::iced::window::{self, Event as WindowEvent, Id as WindowId};
use crate::ui::iced::{self, Alignment, Event, Length, Size, Subscription, event, stream};
use crate::ui::iced_core::SmolStr;
use crate::ui::iced_core::widget::operation::focusable::unfocus;
use crate::ui::iced_runtime::task;
use crate::ui::shell::Application;
use crate::ui::shell::Core;
use crate::ui::shell::context_drawer;
use crate::ui::widget::about::About;
use crate::ui::widget::button::focus;
use crate::ui::widget::menu::action::MenuAction;
use crate::ui::widget::menu::key_bind::KeyBind;
use crate::ui::widget::scrollable;
use crate::ui::widget::scrollable::AbsoluteOffset;
use crate::ui::widget::segmented_button::{self, Entity, ReorderEvent};
use crate::ui::widget::{self, icon, settings};
use crate::ui::{Element, surface};
use mime_guess::Mime;
use notify_debouncer_full::notify::{self, RecommendedWatcher};
use notify_debouncer_full::{DebouncedEvent, Debouncer, RecommendedCache, new_debouncer};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use slotmap::Key as SlotMapKey;
use std::any::TypeId;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{self, Duration, Instant};
use std::{env, fmt, io, process};
use tokio::sync::mpsc;
use trash::TrashItem;

use crate::clipboard::{
    ClipboardCache, ClipboardCopy, ClipboardKind, ClipboardPaste, ClipboardPasteImage,
    ClipboardPasteText, ClipboardPasteVideo,
};
use crate::config::{AppTheme, Config, Favorite, IconSizes, State, Store, TabConfig, TypeToSearch};
use crate::dialog::{Dialog, DialogKind, DialogMessage, DialogResult, DialogSettings};
use crate::key_bind::key_binds;
use crate::localize::LANGUAGE_SORTER;
use crate::mime_app::{self, MimeApp, MimeAppCache, MimeAppMatch};
use crate::mounter::{
    MOUNTERS, MounterAuth, MounterItem, MounterItems, MounterKey, MounterMessage,
};
use crate::operation::{
    Controller, Operation, OperationError, OperationErrorType, OperationSelection, ReplaceResult,
};
use crate::spawn_detached::spawn_detached;
use crate::tab::{
    self, HeadingOptions, ItemMetadata, Location, SORT_OPTION_FALLBACK, SearchLocation, Tab,
};
use crate::trash::{Trash, TrashExt};
use crate::ui::convert::{ToColor, ToLength, ToPadding, ToPixels};
use crate::ui::theme::{Button, Container, Layer, Spacing, spacing};
use crate::zoom::{zoom_in_view, zoom_out_view, zoom_to_default};
use crate::{batch_rename, context_action, fl, home_dir, menu, mime_icon};

static PERMANENT_DELETE_BUTTON_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("permanent-delete-button"));

static DELETE_TRASH_BUTTON_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("delete-trash-button"));

static CONFIRM_OPEN_WITH_BUTTON_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("confirm-open-with-button"));

static CONFIRM_CONTEXT_ACTION_BUTTON_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("confirm-context-action-button"));

static EMPTY_TRASH_BUTTON_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("empty-trash-button"));

static SET_EXECUTABLE_AND_LAUNCH_CONFIRM_BUTTON_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("set-executable-and-launch-confirm-button"));

static LAUNCH_DESKTOP_ENTRY_CONFIRM_BUTTON_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("launch-desktop-entry-confirm-button"));

static FAVORITE_PATH_ERROR_REMOVE_BUTTON_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("favorite-path-error-remove-button"));

static MOUNT_ERROR_TRY_AGAIN_BUTTON_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("mount-error-try-again-button"));

pub(crate) static REPLACE_BUTTON_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("replace-button"));

#[derive(Clone, Debug)]
pub struct Flags {
    pub config_handler: Store,
    pub config: Config,
    pub state_handler: Store,
    pub state: State,
    pub locations: Vec<Location>,
    pub uris: Vec<url::Url>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Action {
    About,
    AddToSidebar,
    Compress,
    Copy,
    CopyPath,
    CopyTo,
    Cut,
    CosmicSettingsDesktop,
    CosmicSettingsDisplays,
    CosmicSettingsWallpaper,
    Delete,
    EditHistory,
    EditLocation,
    Eject,
    EmptyTrash,
    #[cfg(feature = "desktop")]
    ExecEntryAction(usize),
    ExtractAsFolder,
    ExtractHere,
    ExtractTo,
    Gallery,
    HistoryNext,
    HistoryPrevious,
    ItemDown,
    ItemLeft,
    ItemPageDown,
    ItemPageUp,
    ItemRight,
    ItemUp,
    LocationUp,
    MoveTo,
    NewFile,
    NewFolder,
    Open,
    OpenInNewTab,
    OpenInNewWindow,
    OpenItemLocation,
    OpenTerminal,
    OpenWith,
    RunContextAction(usize),
    Paste,
    PermanentlyDelete,
    Preview,
    Reload,
    RemoveFromRecents,
    Rename,
    RestoreFromTrash,
    SearchActivate,
    SelectFirst,
    SelectLast,
    SelectAll,
    SetSort(HeadingOptions, bool),
    Settings,
    TabClose,
    TabNew,
    TabNext,
    TabPrev,
    TabViewGrid,
    TabViewList,
    Undo,
    ToggleFoldersFirst,
    ToggleShowHidden,
    ToggleShowTypeColumn,
    ToggleSort(HeadingOptions),
    WindowClose,
    WindowNew,
    ZoomDefault,
    ZoomIn,
    ZoomOut,
    Recents,
}

impl Action {
    const fn message(&self, entity_opt: Option<Entity>) -> Message {
        match self {
            Self::About => Message::ToggleContextPage(ContextPage::About),
            Self::AddToSidebar => Message::AddToSidebar(entity_opt),
            Self::Compress => Message::Compress(entity_opt),
            Self::Copy => Message::Copy(entity_opt),
            Self::CopyPath => Message::CopyPath(entity_opt),
            Self::CopyTo => Message::CopyTo(entity_opt),
            Self::Cut => Message::Cut(entity_opt),
            Self::CosmicSettingsDesktop => Message::CosmicSettings("desktop"),
            Self::CosmicSettingsDisplays => Message::CosmicSettings("displays"),
            Self::CosmicSettingsWallpaper => Message::CosmicSettings("wallpaper"),
            Self::Delete => Message::Delete(entity_opt),
            Self::EditHistory => Message::ToggleContextPage(ContextPage::EditHistory),
            Self::EditLocation => Message::TabMessage(entity_opt, tab::Message::EditLocationEnable),
            Self::Eject => Message::Eject,
            Self::EmptyTrash => Message::TabMessage(None, tab::Message::EmptyTrash),
            Self::ExtractAsFolder => Message::ExtractAsFolder(entity_opt),
            Self::ExtractHere => Message::ExtractHere(entity_opt),
            Self::ExtractTo => Message::ExtractTo(entity_opt),
            #[cfg(feature = "desktop")]
            Self::ExecEntryAction(action) => {
                Message::TabMessage(entity_opt, tab::Message::ExecEntryAction(None, *action))
            }
            Self::Gallery => Message::TabMessage(entity_opt, tab::Message::GalleryToggle),
            Self::HistoryNext => Message::TabMessage(entity_opt, tab::Message::GoNext),
            Self::HistoryPrevious => Message::TabMessage(entity_opt, tab::Message::GoPrevious),
            Self::ItemDown => Message::TabMessage(entity_opt, tab::Message::ItemDown),
            Self::ItemLeft => Message::TabMessage(entity_opt, tab::Message::ItemLeft),
            Self::ItemPageDown => Message::TabMessage(entity_opt, tab::Message::ItemPageDown),
            Self::ItemPageUp => Message::TabMessage(entity_opt, tab::Message::ItemPageUp),
            Self::ItemRight => Message::TabMessage(entity_opt, tab::Message::ItemRight),
            Self::ItemUp => Message::TabMessage(entity_opt, tab::Message::ItemUp),
            Self::LocationUp => Message::TabMessage(entity_opt, tab::Message::LocationUp),
            Self::MoveTo => Message::MoveTo(entity_opt),
            Self::NewFile => Message::NewItem(entity_opt, false),
            Self::NewFolder => Message::NewItem(entity_opt, true),
            Self::Open => Message::TabMessage(entity_opt, tab::Message::Open(None)),
            Self::OpenInNewTab => Message::OpenInNewTab(entity_opt),
            Self::OpenInNewWindow => Message::OpenInNewWindow(entity_opt),
            Self::OpenItemLocation => Message::OpenItemLocation(entity_opt),
            Self::OpenTerminal => Message::OpenTerminal(entity_opt),
            Self::OpenWith => Message::OpenWithDialog(entity_opt),
            Self::RunContextAction(action) => {
                Message::TabMessage(entity_opt, tab::Message::RunContextAction(*action))
            }
            Self::Paste => Message::Paste(entity_opt),
            Self::PermanentlyDelete => Message::PermanentlyDelete(entity_opt),
            Self::Preview => Message::Preview,
            Self::Reload => Message::TabMessage(entity_opt, tab::Message::Reload),
            Self::RemoveFromRecents => Message::RemoveFromRecents(entity_opt),
            Self::Rename => Message::Rename(entity_opt),
            Self::RestoreFromTrash => Message::RestoreFromTrash(entity_opt),
            Self::SearchActivate => Message::SearchActivate,
            Self::SelectAll => Message::TabMessage(entity_opt, tab::Message::SelectAll),
            Self::SelectFirst => Message::TabMessage(entity_opt, tab::Message::SelectFirst),
            Self::SelectLast => Message::TabMessage(entity_opt, tab::Message::SelectLast),
            Self::SetSort(sort, dir) => {
                Message::TabMessage(entity_opt, tab::Message::SetSort(*sort, *dir))
            }
            Self::Settings => Message::ToggleContextPage(ContextPage::Settings),
            Self::TabClose => Message::TabClose(entity_opt),
            Self::TabNew => Message::TabNew,
            Self::TabNext => Message::TabNext,
            Self::TabPrev => Message::TabPrev,
            Self::TabViewGrid => Message::TabView(entity_opt, tab::View::Grid),
            Self::TabViewList => Message::TabView(entity_opt, tab::View::List),
            Self::ToggleFoldersFirst => Message::ToggleFoldersFirst,
            Self::ToggleShowHidden => Message::ToggleShowHidden,
            Self::ToggleShowTypeColumn => Message::ToggleShowTypeColumn,
            Self::Undo => Message::Undo,
            Self::ToggleSort(sort) => {
                Message::TabMessage(entity_opt, tab::Message::ToggleSort(*sort))
            }
            Self::WindowClose => Message::WindowClose,
            Self::WindowNew => Message::WindowNew,
            Self::ZoomDefault => Message::ZoomDefault(entity_opt),
            Self::ZoomIn => Message::ZoomIn(entity_opt),
            Self::ZoomOut => Message::ZoomOut(entity_opt),
            Self::Recents => Message::Recents,
        }
    }
}

impl MenuAction for Action {
    type Message = Message;

    fn message(&self) -> Message {
        self.message(None)
    }
}

#[derive(Clone, Debug)]
pub struct PreviewItem(pub Box<tab::Item>);

impl PartialEq for PreviewItem {
    fn eq(&self, other: &Self) -> bool {
        self.0.location_opt == other.0.location_opt
    }
}

impl Eq for PreviewItem {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreviewKind {
    Custom(PreviewItem),
    Location(Location),
    Selected,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NavMenuAction {
    ClearRecents,
    EmptyTrash,
    Open(segmented_button::Entity),
    OpenWith(segmented_button::Entity),
    OpenInNewTab(segmented_button::Entity),
    OpenInNewWindow(segmented_button::Entity),
    Preview(segmented_button::Entity),
    RunContextAction(segmented_button::Entity, usize),
    RemoveFromSidebar(segmented_button::Entity),
    ChangeSidebarLabel(segmented_button::Entity),
}

impl MenuAction for NavMenuAction {
    type Message = crate::ui::Action<Message>;

    fn message(&self) -> Self::Message {
        crate::ui::Action::App(Message::NavMenuAction(*self))
    }
}

/// Messages that are used specifically by our [`App`].
#[derive(Clone, Debug)]
pub enum Message {
    AddToSidebar(Option<Entity>),
    AppTheme(AppTheme),
    CloseToast(widget::ToastId),
    Compress(Option<Entity>),
    Config(Config),
    Copy(Option<Entity>),
    CopyPath(Option<Entity>),
    CopyTo(Option<Entity>),
    CopyToResult(DialogResult),
    CosmicSettings(&'static str),
    Cut(Option<Entity>),
    Delete(Option<Entity>),
    DesktopDialogs(bool),
    DialogCancel,
    DialogComplete,
    Eject,
    FileDialogMessage(DialogMessage),
    DialogPush(DialogPage, Option<widget::Id>),
    DialogUpdate(DialogPage),
    DialogUpdateComplete(DialogPage),
    ExtractAsFolder(Option<Entity>),
    ExtractHere(Option<Entity>),
    ExtractTo(Option<Entity>),
    ExtractToResult(DialogResult),
    Key(window::Id, Modifiers, Key, Physical, Option<SmolStr>),
    LaunchUrl(String),
    MaybeExit,
    ModifiersChanged(window::Id, Modifiers),
    MounterItems(MounterKey, MounterItems),
    MountResult(MounterKey, MounterItem, Result<bool, String>),
    MoveTo(Option<Entity>),
    MoveToResult(DialogResult),
    NavBarClose(Entity),
    NavBarContext(Entity),
    /// Files were dropped on a nav-bar bookmark.
    NavBarDrop(Entity),
    /// Files were dropped on a tab in the tab bar.
    TabDrop(Entity),
    NavMenuAction(NavMenuAction),
    NetworkAuth(MounterKey, String, MounterAuth, mpsc::Sender<MounterAuth>),
    NetworkDriveInput(String),
    NetworkDriveOpenEntityAfterMount {
        entity: Entity,
    },
    NetworkDriveOpenTabAfterMount {
        location: Location,
    },
    NetworkDriveSubmit,
    NetworkResult(MounterKey, String, Result<bool, String>),
    NewItem(Option<Entity>, bool),
    #[cfg(feature = "notify")]
    Notification(Arc<Mutex<notify_rust::NotificationHandle>>),
    NotifyEvents(Vec<DebouncedEvent>),
    NotifyWatcher(WatcherWrapper),
    OpenTerminal(Option<Entity>),
    OpenInNewTab(Option<Entity>),
    OpenInNewWindow(Option<Entity>),
    OpenItemLocation(Option<Entity>),
    OpenWithBrowse,
    OpenWithDialog(Option<Entity>),
    OpenWithSelection(usize),
    OpenWithSearchClear,
    Paste(Option<Entity>),
    PasteContents(PathBuf, ClipboardPaste),
    PasteImage(PathBuf),
    PasteImageContents(PathBuf, ClipboardPasteImage),
    PasteText(PathBuf),
    PasteTextContents(PathBuf, ClipboardPasteText),
    PasteVideo(PathBuf),
    PasteVideoContents(PathBuf, ClipboardPasteVideo),
    CheckClipboard,
    CheckClipboardImage,
    CheckClipboardVideo,
    CheckClipboardText,
    RetryCheckClipboard(ClipboardCache),
    ClipboardCached(ClipboardCache),
    PendingCancel(u64),
    PendingCancelAll,
    PendingComplete(u64, OperationSelection),
    PendingDismiss,
    PendingError(u64, OperationError),
    PendingResults(Vec<(u64, OperationSelection)>, Vec<(u64, OperationError)>),
    PendingPause(u64, bool),
    PendingPauseAll(bool),
    PermanentlyDelete(Option<Entity>),
    Preview,
    /// Items re-read off the event loop after the filesystem changed beneath
    /// them, tagged with the location and listing they were read from and the
    /// number of the batch that asked for them.
    RefreshedItems(Entity, Location, u64, u64, Vec<(PathBuf, Box<tab::Item>)>),
    /// A batch rename preview with its destination conflicts filled in, and
    /// the revision of the names it describes.
    BatchRenamePreview(u64, batch_rename::Preview),
    /// The default terminal, worked out on a worker at startup.
    DefaultTerminal(Option<String>),
    /// Open these paths with the applications the mime cache names. Exists so
    /// that opening can wait for that cache to be built.
    OpenFiles(Vec<PathBuf>),
    /// The types of files the user asked to open, worked out on a worker.
    OpenFilesResolved(u64, Vec<(Mime, PathBuf)>),
    /// Opening has taken long enough to be acknowledged.
    OpeningStillRunning(u64),
    /// Forget an open request: its files are not launched when their types
    /// arrive. The read itself is not interrupted.
    CancelOpening(u64),
    /// Offer applications for this file, which was chosen when the user asked
    /// rather than when the cache was ready.
    OpenWithFor(PathBuf, Mime),
    /// What a worker found a sidebar entry's type to be, for its loading page.
    OpenWithResolved(PathBuf, Result<Mime, String>),
    /// The same, for a terminal the user has already asked to open: take the
    /// answer and then open it.
    DefaultTerminalThenOpen(Option<String>, Box<[PathBuf]>),
    /// Open a terminal in each of these folders, resolved from the selection
    /// when the user asked rather than when the cache was ready.
    OpenTerminalIn(Box<[PathBuf]>),
    /// The result of looking at a path the user is typing towards.
    NameChecked(NameCheck),
    ReloadMimeAppCache,
    /// A freshly built mime app cache, ready to replace the one in use, and
    /// the number of the rebuild that produced it.
    MimeAppCacheReloaded(u64, MimeAppCacheWrapper),
    ReorderTab(ReorderEvent),
    RescanRecents,
    RescanTrash,
    RemoveFromRecents(Option<Entity>),
    Rename(Option<Entity>),
    ReplaceResult(ReplaceResult),
    RestoreFromTrash(Option<Entity>),
    SaveSortNames,
    ScrollTab(i16),
    SearchActivate,
    SearchClear,
    SearchInput(String),
    SetShowDetails(bool),
    SetShowRecents(bool),
    SetTypeToSearch(TypeToSearch),
    SystemThemeModeChange,
    Size(window::Id, Size),
    TabActivate(Entity),
    TabNext,
    TabPrev,
    TabClose(Option<Entity>),
    TabConfig(TabConfig),
    TabMessage(Option<Entity>, tab::Message),
    TabNew,
    TabRescan(
        Entity,
        Location,
        Option<Box<tab::Item>>,
        Vec<tab::Item>,
        Option<Vec<PathBuf>>,
    ),
    TabView(Option<Entity>, tab::View),
    ToggleContextPage(ContextPage),
    ToggleFoldersFirst,
    ToggleShowHidden,
    ToggleShowTypeColumn,
    ToggleAuthPasswordVisible,
    Undo,
    UndoTrash(widget::ToastId, Arc<[PathBuf]>),
    UndoTrashStart(Option<widget::ToastId>, Vec<TrashItem>),
    WindowClose,
    WindowCloseRequested(window::Id),
    WindowMaximize(window::Id, bool),
    WindowNew,
    ZoomDefault(Option<Entity>),
    ZoomIn(Option<Entity>),
    ZoomOut(Option<Entity>),
    Recents,
    Cosmic(app::Action),
    None,
    Surface(surface::Action<Message>),
    CutPaths(Vec<PathBuf>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextPage {
    About,
    EditHistory,
    NetworkDrive,
    Preview(Option<Entity>, PreviewKind),
    Settings,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ArchiveType {
    Tgz,
    #[default]
    Zip,
}

impl ArchiveType {
    pub const fn all() -> &'static [Self] {
        &[Self::Tgz, Self::Zip]
    }

    pub const fn extension(&self) -> &str {
        match self {
            Self::Tgz => ".tgz",
            Self::Zip => ".zip",
        }
    }
}

impl AsRef<str> for ArchiveType {
    fn as_ref(&self) -> &str {
        self.extension()
    }
}

#[derive(Clone, Debug)]
pub enum DialogPage {
    Compress {
        paths: Box<[PathBuf]>,
        to: PathBuf,
        name: String,
        archive_type: ArchiveType,
        password: Option<String>,
    },
    EmptyTrash,
    FailedOperation(u64),
    FailedOperations(Vec<u64>),
    ExtractPassword {
        id: u64,
        password: String,
    },
    MountError {
        mounter_key: MounterKey,
        item: MounterItem,
        error: String,
    },
    NetworkAuth {
        mounter_key: MounterKey,
        uri: String,
        auth: MounterAuth,
        auth_tx: mpsc::Sender<MounterAuth>,
    },
    NetworkError {
        mounter_key: MounterKey,
        uri: String,
        error: String,
    },
    NewItem {
        parent: PathBuf,
        name: String,
        dir: bool,
    },
    RunContextAction {
        action: usize,
        paths: Box<[PathBuf]>,
    },
    OpenWith {
        path: PathBuf,
        mime: mime_guess::Mime,
        selected: usize,
        store_opt: Option<Arc<MimeApp>>,
        search_app_name: String,
    },
    /// "Open with" for a path whose type is still being worked out on a
    /// worker. Shown at once, so the click is seen to have landed and can be
    /// cancelled; replaced by [`Self::OpenWith`] when the answer arrives.
    OpenWithLoading {
        path: PathBuf,
    },
    PermanentlyDelete {
        paths: Box<[PathBuf]>,
    },
    DeleteTrash {
        items: Vec<TrashItem>,
    },
    ChangeSidebarLabel {
        entity: Entity,
        label: String,
    },
    RenameItem {
        from: PathBuf,
        parent: PathBuf,
        name: String,
        dir: bool,
    },
    BatchRename {
        parent: PathBuf,
        /// Names of the items in `parent`, in display order
        names: Vec<String>,
        settings: batch_rename::Settings,
    },
    Replace {
        from: Box<tab::Item>,
        to: Box<tab::Item>,
        multiple: bool,
        apply_to_all: bool,
        conflict_count: usize,
        tx: mpsc::Sender<ReplaceResult>,
    },
    SetExecutableAndLaunch {
        path: PathBuf,
    },
    /// Confirm running a desktop entry that is not installed in one of the
    /// system or user application directories
    LaunchDesktopEntry {
        path: PathBuf,
        command: String,
    },
    FavoritePathError {
        path: PathBuf,
        entity: Entity,
    },
}

pub struct DialogPages {
    pages: VecDeque<DialogPage>,
}

impl Default for DialogPages {
    fn default() -> Self {
        Self::new()
    }
}

impl DialogPages {
    pub const fn new() -> Self {
        Self {
            pages: VecDeque::new(),
        }
    }

    pub fn front(&self) -> Option<&DialogPage> {
        self.pages.front()
    }

    pub fn front_mut(&mut self) -> Option<&mut DialogPage> {
        self.pages.front_mut()
    }

    pub fn push_back(&mut self, page: DialogPage) -> Task<Message> {
        let task = if self.pages.is_empty() {
            Task::done(crate::ui::Action::App(Message::DesktopDialogs(true)))
        } else {
            Task::none()
        };
        self.pages.push_back(page);
        task
    }

    pub fn push_front(&mut self, page: DialogPage) -> Task<Message> {
        let task = if self.pages.is_empty() {
            Task::done(crate::ui::Action::App(Message::DesktopDialogs(true)))
        } else {
            Task::none()
        };
        self.pages.push_front(page);
        task
    }

    #[must_use]
    pub fn pop_front(&mut self) -> Option<(DialogPage, Task<Message>)> {
        let page = self.pages.pop_front()?;
        let task = if self.pages.is_empty() {
            Task::done(crate::ui::Action::App(Message::DesktopDialogs(false)))
        } else {
            Task::none()
        };
        Some((page, task))
    }

    pub fn update_front(&mut self, page: DialogPage) {
        if !self.pages.is_empty() {
            self.pages[0] = page;
        }
    }
}

pub struct FavoriteIndex(usize);

pub struct MounterData(MounterKey, MounterItem);

#[derive(Clone, Debug)]
pub enum WindowKind {
    Dialogs(widget::Id),
    FileDialog(Option<Box<[PathBuf]>>),
    Preview(Option<Entity>, PreviewKind),
}

/// A rebuilt [`MimeAppCache`] on its way to the application.
///
/// A cache cannot be cloned or printed, and a message must be both. It is only
/// ever delivered once, so a clone gives up the cache rather than duplicating
/// it: the copy that carries it is the one that arrives.
pub struct MimeAppCacheWrapper {
    cache_opt: Option<MimeAppCache>,
}

impl MimeAppCacheWrapper {
    fn new(cache: MimeAppCache) -> Self {
        Self {
            cache_opt: Some(cache),
        }
    }

    fn take(&mut self) -> Option<MimeAppCache> {
        self.cache_opt.take()
    }
}

impl Clone for MimeAppCacheWrapper {
    fn clone(&self) -> Self {
        Self { cache_opt: None }
    }
}

impl fmt::Debug for MimeAppCacheWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MimeAppCacheWrapper").finish()
    }
}

impl PartialEq for MimeAppCacheWrapper {
    fn eq(&self, _other: &Self) -> bool {
        false
    }
}

pub struct WatcherWrapper {
    watcher_opt: Option<Debouncer<RecommendedWatcher, RecommendedCache>>,
}

impl Clone for WatcherWrapper {
    fn clone(&self) -> Self {
        Self { watcher_opt: None }
    }
}

impl fmt::Debug for WatcherWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WatcherWrapper").finish()
    }
}

impl PartialEq for WatcherWrapper {
    fn eq(&self, _other: &Self) -> bool {
        false
    }
}

struct Window {
    kind: WindowKind,
    modifiers: Modifiers,
}

impl Window {
    fn new(kind: WindowKind) -> Self {
        Self {
            kind,
            modifiers: Modifiers::empty(),
        }
    }
}

// The [`App`] stores application-specific state.
/// How many completed operations can be undone, newest first
const UNDO_DEPTH: usize = 20;

pub struct App {
    core: Core,
    about: About,
    nav_bar_context_id: segmented_button::Entity,
    nav_model: segmented_button::SingleSelectModel,
    tab_model: segmented_button::Model<segmented_button::SingleSelect>,
    config_handler: Store,
    state_handler: Store,
    config: Config,
    state: State,
    app_themes: Vec<String>,
    compio_tx: mpsc::Sender<Pin<Box<dyn Future<Output = ()> + Send>>>,
    context_page: ContextPage,
    dialog_pages: DialogPages,
    dialog_text_input: widget::Id,
    /// Whether the network auth dialog shows the password in clear text
    auth_password_visible: bool,
    /// The batch rename preview, recomputed when the dialog's settings change.
    /// It stats the filesystem, so it must not be built while rendering.
    batch_rename_preview: batch_rename::Preview,
    /// Bumped every time the batch rename preview is rebuilt, so a background
    /// conflict check that finishes after the next keystroke is discarded
    /// rather than shown against names that have since changed.
    batch_rename_revision: u64,
    key_binds: HashMap<KeyBind, Action>,
    mime_app_cache: MimeAppCache,
    modifiers: Modifiers,
    mounter_items: FxHashMap<MounterKey, MounterItems>,
    must_save_sort_names: bool,
    network_drive_connecting: Option<(MounterKey, String)>,
    network_drive_input: String,
    #[cfg(feature = "notify")]
    notification_opt: Option<Arc<Mutex<notify_rust::NotificationHandle>>>,
    pending_operation_id: u64,
    pending_operations: BTreeMap<u64, (Operation, Controller)>,
    progress_operations: BTreeSet<u64>,
    complete_operations: BTreeMap<u64, Operation>,
    failed_operations: BTreeMap<u64, (Operation, Controller, String)>,
    /// What undoes each of the last completed operations, newest last
    undo_stack: Vec<Vec<Operation>>,
    /// Operations started by an undo; their completion is not undoable again
    undo_ids: BTreeSet<u64>,
    scrollable_name: std::borrow::Cow<'static, str>,
    search_id: widget::Id,
    size: Option<Size>,
    toasts: widget::toaster::Toasts<Message>,
    watcher_opt: Option<(
        Debouncer<RecommendedWatcher, RecommendedCache>,
        FxHashSet<PathBuf>,
    )>,
    windows: FxHashMap<window::Id, Window>,
    type_select_prefix: String,
    type_select_last_key: Option<Instant>,
    auto_scroll_speed: Option<i16>,
    file_dialog_opt: Option<Dialog<Message>>,
    clipboard_cache: ClipboardCache,
    /// What the last background check said about the name typed into the new
    /// item or rename dialog.
    name_check: Option<NameCheck>,
    /// The number given to the next mime app cache rebuild, and the number of
    /// the newest one already installed.
    mime_app_rebuild: u64,
    mime_app_rebuild_applied: u64,
    /// Files being opened whose types are still being worked out on a
    /// worker, by request number, with the toast shown for any that has taken
    /// long enough to need one. A request that is cancelled leaves the map,
    /// and its answer is thrown away when it arrives.
    opening: FxHashMap<u64, Option<widget::toaster::ToastId>>,
    next_open_request: u64,
    /// Messages that arrived before the mime app cache had been built, to be
    /// handled again once it has. Bounded: past a handful the user is holding
    /// a key down, and replaying hundreds of them at once would be worse than
    /// dropping the extras.
    deferred_mime_messages: Vec<Message>,
    /// Bumped on every keystroke in that dialog. A check that finds the
    /// counter has moved on since it was scheduled drops out before touching
    /// the disk, so holding a key down costs one stat rather than one per
    /// repeat.
    name_check_revision: Arc<AtomicU64>,
}

/// How long opening files may take before the user is told it is still
/// happening. Working out a file's type is milliseconds locally, so in the
/// ordinary case this never shows; it is for a slow or absent mount, where a
/// click with no visible effect is its own kind of bug.
const OPENING_ACK_DELAY: Duration = Duration::from_millis(400);
/// A backstop for that toast. It is removed when the request ends, so this
/// only matters if something goes wrong on the way there.
const OPENING_TOAST_BACKSTOP: Duration = Duration::from_secs(60);

/// How long a typed name must stand still before it is checked against the
/// disk. Long enough that typing a name straight through checks it once,
/// short enough that the warning still feels like a reaction to typing.
const NAME_CHECK_DELAY: Duration = Duration::from_millis(150);

/// What is already at a path the user is typing towards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NameTaken {
    Folder,
    File,
}

/// The answer to one name check, and the question it answers.
#[derive(Clone, Debug)]
pub struct NameCheck {
    /// The path that was looked at. The typed name decides it, so this is
    /// also the revision: an answer about any other path is an answer to a
    /// question the user has since changed, and says nothing about this one.
    path: PathBuf,
    taken: Option<NameTaken>,
}

impl App {
    /// Returns true if the clipboard cache contains pasteable content
    fn clipboard_has_content(&self) -> bool {
        !matches!(self.clipboard_cache, ClipboardCache::Empty)
    }

    /// The new-item and rename dialogs: one name field with the shared checks.
    ///
    /// `from_opt` is the item being renamed, which may keep its own name;
    /// `select_delimiter` sets what a double click in the field selects up to.
    #[allow(clippy::too_many_arguments)]
    fn name_dialog<'a>(
        &'a self,
        title: String,
        confirm: String,
        dir: bool,
        parent: &'a Path,
        name: &'a str,
        from_opt: Option<&'a Path>,
        select_delimiter: Option<char>,
        on_input: impl Fn(String) -> Message + 'a,
    ) -> widget::Dialog<'a, Message> {
        let Spacing { space_xxs, .. } = spacing();
        let mut dialog = widget::dialog().title(title);

        let complete_maybe = if name.is_empty() {
            None
        } else if name == "." || name == ".." {
            dialog =
                dialog.tertiary_action(widget::text::body(fl!("name-invalid", filename = name)));
            None
        } else if name.contains('/') {
            dialog = dialog.tertiary_action(widget::text::body(fl!("name-no-slashes")));
            None
        } else {
            let path = parent.join(name);
            // Read, not looked up: this runs on every frame the dialog is
            // drawn, and asking the filesystem that often makes typing a name
            // as slow as the folder being typed into. `check_name` answers in
            // the background, and an answer about a different path is an
            // answer to what was typed before this keystroke.
            let taken = self
                .name_check
                .as_ref()
                .filter(|check| check.path == path)
                .and_then(|check| check.taken);
            if from_opt != Some(path.as_path())
                && let Some(taken) = taken
            {
                dialog = dialog.tertiary_action(widget::text::body(match taken {
                    NameTaken::Folder => fl!("folder-already-exists"),
                    NameTaken::File => fl!("file-already-exists"),
                }));
                None
            } else {
                if name.starts_with('.') {
                    dialog = dialog.tertiary_action(widget::text::body(fl!("name-hidden")));
                }
                // Offered even while the check is still out. The warning is
                // only ever a snapshot of a folder other programs also write
                // to, so it is not what keeps anything safe: creating or
                // renaming onto a name that is taken is refused by the
                // operation itself, where the file is actually touched.
                Some(Message::DialogComplete)
            }
        };

        let mut input = widget::text_input("", name)
            .id(self.dialog_text_input.clone())
            .on_input(on_input)
            .on_submit_maybe(complete_maybe.clone().map(|maybe| move |_| maybe.clone()));
        if let Some(delimiter) = select_delimiter {
            input = input.double_click_select_delimiter(delimiter);
        }

        dialog
            .primary_action(widget::button::suggested(confirm).on_press_maybe(complete_maybe))
            .secondary_action(
                widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
            )
            .control(
                widget::Column::with_children([
                    widget::text::body(if dir {
                        fl!("folder-name")
                    } else {
                        fl!("file-name")
                    })
                    .into(),
                    input.into(),
                ])
                .spacing(space_xxs.to_pixels()),
            )
    }

    fn push_dialog(&mut self, page: DialogPage, focus_id: Option<widget::Id>) -> Task<Message> {
        let t = self.dialog_pages.push_back(page);
        if let Some(focus_id) = focus_id {
            Task::batch([t, focus(focus_id)])
        } else {
            t
        }
    }

    fn open_file(&mut self, paths: &[impl AsRef<Path>]) -> Task<Message> {
        // Nothing useful can be decided from an empty cache: every mime lookup
        // would come back with nothing and every file would fall through to
        // `xdg-open`, quietly ignoring whatever application the user chose.
        // Better to open a moment later with the right one.
        if !self.mime_app_cache.is_loaded() {
            let paths: Vec<PathBuf> = paths
                .iter()
                .map(|path| path.as_ref().to_path_buf())
                .collect();
            self.defer_until_mime_apps(Message::OpenFiles(paths));
            return Task::none();
        }

        // The types are worked out on a worker. `mime_for_path` reads each
        // file -- its metadata and, for the content sniff, its first bytes --
        // and for a large selection, or one file on a mount that has gone
        // away, that is not work for the event loop. The inputs are exactly
        // what they were: what opens a file is decided the same way, only
        // somewhere else.
        let paths: Vec<PathBuf> = paths
            .iter()
            .map(|path| path.as_ref().to_path_buf())
            .collect();
        let request = self.next_open_request;
        self.next_open_request = self.next_open_request.wrapping_add(1);
        self.opening.insert(request, None);
        let resolve = Task::future(async move {
            let resolved = tokio::task::spawn_blocking(move || {
                paths
                    .into_iter()
                    .map(|path| (mime_icon::mime_for_path(&path, None, false), path))
                    .collect::<Vec<_>>()
            })
            .await;
            match resolved {
                Ok(resolved) => {
                    crate::ui::action::app(Message::OpenFilesResolved(request, resolved))
                }
                Err(err) => {
                    log::warn!("failed to work out what to open files with: {err}");
                    crate::ui::action::app(Message::CancelOpening(request))
                }
            }
        });
        // Acknowledged only if it takes long enough to be noticed
        let acknowledge = Task::future(async move {
            tokio::time::sleep(OPENING_ACK_DELAY).await;
            crate::ui::action::app(Message::OpeningStillRunning(request))
        });
        Task::batch([resolve, acknowledge])
    }

    /// Open files whose types are now known: the second half of
    /// [`Self::open_file`], reached once a worker has read them.
    fn open_resolved(&mut self, resolved: Vec<(Mime, PathBuf)>) -> Task<Message> {
        let mut tasks = Vec::new();

        // Associate all paths to its MIME type
        // This allows handling paths as groups if possible, such as launching a single video
        // player that is passed every path.
        let mut groups: FxHashMap<Mime, Vec<PathBuf>> = FxHashMap::default();
        let mut all_paths = Vec::with_capacity(resolved.len());
        let mut all_archives = true;
        let supported_archive_types = crate::archive::SUPPORTED_ARCHIVE_TYPES;
        for (mime, path) in resolved {
            if all_archives && !supported_archive_types.iter().copied().any(|t| mime == t) {
                all_archives = false;
            }
            all_paths.push(path.clone());
            groups.entry(mime).or_default().push(path);
        }

        if all_archives {
            // Use extract to dialog if all selected paths are supported archives
            return self.extract_to(&all_paths);
        }

        'outer: for (mime, paths) in groups {
            log::debug!("Attempting to launch app\n\tfor: {mime}\n\twith: {paths:?}");

            // First launch apps that can be launched directly
            if mime == "application/x-desktop" {
                #[cfg(feature = "desktop")]
                {
                    // A desktop entry runs an arbitrary command line, so only
                    // the ones installed as applications launch unasked. Any
                    // other one, such as a file just downloaded or unpacked,
                    // has to be confirmed first.
                    let (installed, unknown): (Vec<_>, Vec<_>) = paths
                        .into_iter()
                        .partition(|path| Self::desktop_entry_is_installed(path));
                    Self::launch_desktop_entries(&installed);
                    for path in unknown {
                        match Self::desktop_entry_command(&path) {
                            Some(command) => {
                                tasks.push(self.push_dialog(
                                    DialogPage::LaunchDesktopEntry { path, command },
                                    Some(LAUNCH_DESKTOP_ENTRY_CONFIRM_BUTTON_ID.clone()),
                                ));
                            }
                            None => {
                                log::warn!("failed to read desktop entry {}", path.display());
                            }
                        }
                    }
                    continue;
                }
            } else if mime == "application/x-executable" || mime == "application/vnd.appimage" {
                // Try opening executable
                for path in paths {
                    let mut command = std::process::Command::new(&path);
                    match spawn_detached(&mut command) {
                        Ok(()) => {}
                        Err(err) => match err.kind() {
                            io::ErrorKind::PermissionDenied => {
                                // If permission is denied, try marking as executable, then running
                                tasks.push(self.push_dialog(
                                    DialogPage::SetExecutableAndLaunch { path },
                                    Some(SET_EXECUTABLE_AND_LAUNCH_CONFIRM_BUTTON_ID.clone()),
                                ));
                            }
                            _ => {
                                log::warn!("failed to execute {}: {}", path.display(), err);
                            }
                        },
                    }
                }
                continue;
            }

            // Try mime apps, which should be faster than xdg-open. Each step
            // only sees the paths no earlier step managed to open.
            let mut paths = self.launch_from_mime_cache(&mime, &paths);
            if paths.is_empty() {
                continue;
            }

            // loop through subclasses if available
            if let Some(mime_sub_classes) = mime_icon::parent_mime_types(&mime) {
                for sub_class in mime_sub_classes {
                    paths = self.launch_from_mime_cache(&sub_class, &paths);
                    if paths.is_empty() {
                        continue 'outer;
                    }
                }
            }

            // Fall back to using open crate
            for path in paths {
                match open::that_detached(&path) {
                    Ok(()) => {
                        if self.config.show_recents {
                            crate::recents::record(
                                path.clone(),
                                Self::APP_ID.to_string(),
                                "earth-files".to_string(),
                            );
                        }
                    }
                    Err(err) => {
                        log::warn!("failed to open {}: {}", path.display(), err);
                    }
                }
            }
        }

        Task::batch(tasks)
    }

    #[cfg(feature = "desktop")]
    /// Whether a desktop entry lives in one of the directories applications
    /// are installed into, which is what makes it trusted to run unasked
    fn desktop_entry_is_installed(path: &Path) -> bool {
        use freedesktop_desktop_entry as fde;
        let Ok(path) = path.canonicalize() else {
            return false;
        };
        fde::default_paths().any(|dir| {
            dir.canonicalize()
                .is_ok_and(|dir| path.starts_with(&dir) && path != dir)
        })
    }

    /// The command line a desktop entry would run, for the confirmation dialog
    fn desktop_entry_command(path: &Path) -> Option<String> {
        use freedesktop_desktop_entry::DesktopEntry;
        DesktopEntry::from_path::<&str>(path, None)
            .ok()?
            .exec()
            .map(str::to_string)
    }

    fn launch_desktop_entries(paths: &[impl AsRef<Path>]) {
        use freedesktop_desktop_entry::DesktopEntry;
        let locales = freedesktop_desktop_entry::get_languages_from_env();

        for path in paths.iter().map(AsRef::as_ref) {
            match DesktopEntry::from_path::<&str>(path, None) {
                Ok(entry) => match entry.exec() {
                    Some(exec) => {
                        match mime_app::exec_to_command(
                            exec,
                            entry.name(&locales).as_deref().unwrap_or_default(),
                            Some(path),
                            &[] as &[&str; 0],
                        ) {
                            Some(commands) => {
                                let cwd_opt = entry.desktop_entry("Path");

                                for mut command in commands {
                                    if let Some(cwd) = cwd_opt {
                                        command.current_dir(cwd);
                                    }

                                    if let Err(err) = spawn_detached(&mut command) {
                                        log::warn!("failed to execute {}: {}", path.display(), err);
                                    }
                                }
                            }
                            None => {
                                log::warn!(
                                    "failed to parse {}: invalid Desktop Entry/Exec",
                                    path.display()
                                );
                            }
                        }
                    }
                    None => {
                        log::warn!(
                            "failed to parse {}: missing Desktop Entry/Exec",
                            path.display()
                        );
                    }
                },
                Err(err) => {
                    log::warn!("failed to parse {}: {}", path.display(), err);
                }
            }
        }
    }

    /// Launch `paths` with the first application registered for `mime` that
    /// accepts them, and return the paths that were not opened.
    ///
    /// An application whose `Exec` line takes one file at a time yields one
    /// command per path, so a single successful spawn does not mean the whole
    /// selection was handled. The unopened remainder is handed back, both so
    /// the caller can try a fallback for exactly those and so the ones that
    /// did open are not launched a second time.
    fn launch_from_mime_cache(&self, mime: &Mime, paths: &[PathBuf]) -> Vec<PathBuf> {
        let mut remaining: Vec<PathBuf> = paths.to_vec();
        for app in self.mime_app_cache.get(mime) {
            if remaining.is_empty() {
                break;
            }
            let Some(commands) = app.command(&remaining) else {
                continue;
            };
            // One command covers every path, or there is one command per path
            let per_path = commands.len() > 1;
            let mut opened = Vec::new();

            for (i, mut command) in commands.into_iter().enumerate() {
                let covered: Vec<PathBuf> = if per_path {
                    remaining.get(i).cloned().into_iter().collect()
                } else {
                    remaining.clone()
                };
                match spawn_detached(&mut command) {
                    Ok(()) => {
                        if self.config.show_recents {
                            for path in &covered {
                                crate::recents::record(
                                    path.into(),
                                    Self::APP_ID.to_string(),
                                    "earth-files".to_string(),
                                );
                            }
                        }
                        opened.extend(covered);
                    }
                    Err(err) => {
                        log::warn!("failed to open {covered:?} with {:?}: {}", app.id, err);
                    }
                }
            }

            remaining.retain(|path| !opened.contains(path));
        }

        remaining
    }

    #[cfg(feature = "desktop")]
    fn exec_entry_action(entry: &crate::desktop_entry::DesktopEntryData, action: usize) {
        if let Some(action) = entry.desktop_actions.get(action) {
            let mut exec = shlex::Shlex::new(&action.exec);
            match exec.next() {
                Some(cmd) if !cmd.contains('=') => {
                    let mut proc = tokio::process::Command::new(cmd);
                    proc.args(exec.filter(|arg| !arg.starts_with('%')));
                    let _ = proc.spawn();
                }
                _ => (),
            }
        } else {
            log::warn!(
                "Invalid actions index `{action}` for desktop entry {}",
                entry.name
            );
        }
    }

    fn destination_selection_dialog(
        &mut self,
        paths: &[impl AsRef<Path>],
        on_result: impl Fn(DialogResult) -> Message + 'static,
        title: impl Into<String>,
        accept_label: impl AsRef<str>,
    ) -> Task<Message> {
        if let Some(destination) = paths
            .first()
            .and_then(|first| first.as_ref().parent())
            .map(Path::to_path_buf)
        {
            let mut tasks = Vec::new();
            if let Some(old_dialog) = self.file_dialog_opt.take() {
                let old_id = old_dialog.window_id();
                self.windows.remove(&old_id);
                tasks.push(window::close(old_id));
            }
            let (mut dialog, dialog_task) = Dialog::new(
                DialogSettings::new()
                    .kind(DialogKind::OpenFolder)
                    .path(destination),
                Message::FileDialogMessage,
                on_result,
            );
            let set_title_task = dialog.set_title(title);
            dialog.set_accept_label(accept_label);
            self.windows.insert(
                dialog.window_id(),
                Window::new(WindowKind::FileDialog(Some(
                    paths.iter().map(|x| x.as_ref().to_path_buf()).collect(),
                ))),
            );
            self.file_dialog_opt = Some(dialog);
            tasks.push(set_title_task);
            tasks.push(dialog_task);
            Task::batch(tasks)
        } else {
            Task::none()
        }
    }

    /// Extract the selected archives next to themselves, either directly or
    /// each into a new folder named after it.
    fn extract_here(&mut self, entity_opt: Option<Entity>, as_folder: bool) -> Task<Message> {
        let paths: Box<[_]> = self.selected_paths(entity_opt).collect();
        if let Some(destination) = paths
            .first()
            .and_then(|first| first.parent())
            .map(Path::to_path_buf)
        {
            return self.operation(Operation::Extract {
                paths,
                to: destination,
                password: None,
                as_folder,
            });
        }
        Task::none()
    }

    fn extract_to(&mut self, paths: &[impl AsRef<Path>]) -> Task<Message> {
        self.destination_selection_dialog(
            paths,
            Message::ExtractToResult,
            fl!("extract-to-title"),
            fl!("extract-here"),
        )
    }

    fn move_to(&mut self, paths: &[impl AsRef<Path>]) -> Task<Message> {
        self.destination_selection_dialog(
            paths,
            Message::MoveToResult,
            fl!("move-to-title"),
            fl!("move-to-button-label"),
        )
    }

    fn copy_to(&mut self, paths: &[impl AsRef<Path>]) -> Task<Message> {
        self.destination_selection_dialog(
            paths,
            Message::CopyToResult,
            fl!("copy-to-title"),
            fl!("copy-to-button-label"),
        )
    }

    fn open_tab_entity(
        &mut self,
        location: Location,
        activate: bool,
        selection_paths: Option<Vec<PathBuf>>,
        scrollable_name: std::borrow::Cow<'static, str>,
        window_id: Option<window::Id>,
    ) -> (Entity, Task<Message>) {
        let tab = Tab::new(
            location.clone(),
            self.config.tab,
            self.config.thumb_cfg,
            Some(&self.state.sort_names),
            scrollable_name,
            window_id,
        );
        let entity = self
            .tab_model
            .insert()
            .text(tab.title())
            .data(tab)
            .closable();

        let entity = if activate {
            entity.activate().id()
        } else {
            entity.id()
        };

        let mut tasks = Vec::with_capacity(4);
        if activate {
            tasks.push(task::widget(unfocus()));
        }
        tasks.push(self.update_title());
        tasks.push(self.update_watcher());
        tasks.push(self.update_tab(entity, location, selection_paths));
        (entity, Task::batch(tasks))
    }

    fn open_tab(
        &mut self,
        location: Location,
        activate: bool,
        selection_paths: Option<Vec<PathBuf>>,
    ) -> Task<Message> {
        self.open_tab_entity(
            location,
            activate,
            selection_paths,
            self.scrollable_name.clone(),
            None,
        )
        .1
    }

    // This wrapper ensures that local folders use trash and remote folders permanently delete with a dialog
    /// Move the dropped files into `to`, or copy them if Ctrl was held.
    ///
    /// All drop targets use this handler: the file list, breadcrumb, nav bar and tab
    /// bar. Drops use the same `wl_data_device` transfer, MIME types and file operation
    /// as paste. This handler chooses whether to move or copy, independently of the
    /// drag source.
    fn drop_files(to: PathBuf, copy: bool) -> Task<Message> {
        clipboard::read_drop_data::<ClipboardPaste>().map(move |contents_opt| match contents_opt {
            Some(mut contents) => {
                contents.kind = if copy {
                    ClipboardKind::Copy
                } else {
                    ClipboardKind::Cut
                };
                crate::ui::action::app(Message::PasteContents(to.clone(), contents))
            }
            None => {
                log::warn!("a file drop carried nothing readable");
                crate::ui::action::app(Message::None)
            }
        })
    }

    fn delete(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Task<Message> {
        let mut dialog_paths = Vec::new();
        let mut trash_paths = Vec::new();

        for path in paths {
            // The link itself decides, not its target: a broken link, or one
            // pointing at a remote filesystem, still lives here and belongs in
            // the trash rather than in the permanent-delete dialog
            let can_trash = match path.symlink_metadata() {
                Ok(metadata) => matches!(tab::fs_kind(&metadata), tab::FsKind::Local),
                Err(err) => {
                    log::warn!("failed to get metadata for {}: {}", path.display(), err);
                    false
                }
            };
            if can_trash {
                trash_paths.push(path);
            } else {
                dialog_paths.push(path);
            }
        }

        let mut tasks = Vec::new();
        if !dialog_paths.is_empty() {
            tasks.push(self.update(Message::DialogPush(
                DialogPage::PermanentlyDelete {
                    paths: dialog_paths.into_boxed_slice(),
                },
                Some(PERMANENT_DELETE_BUTTON_ID.clone()),
            )));
        }
        if !trash_paths.is_empty() {
            tasks.push(self.operation(Operation::Delete { paths: trash_paths }));
        }
        Task::batch(tasks)
    }

    /// The mime app cache, for view code, or `None` while it is still being
    /// built.
    ///
    /// The preview panes already take this as an optional: without it they
    /// leave out the "opens with" row rather than showing a wrong one. That is
    /// exactly the right thing to show before the cache exists, so the pending
    /// state costs nothing here.
    fn mime_apps_for_view(&self) -> Option<&MimeAppCache> {
        self.mime_app_cache
            .is_loaded()
            .then_some(&self.mime_app_cache)
    }

    /// How many messages may wait on the mime app cache at once.
    ///
    /// The cache takes a few tens of milliseconds to build, so in practice at
    /// most one or two actions can land inside that window. A larger number
    /// means a key is being held down, and replaying all of them when the
    /// cache lands would be worse than dropping the extras.
    const MAX_DEFERRED_MIME_MESSAGES: usize = 8;

    /// Hold `message` until the mime app cache has been built, if it has not.
    ///
    /// Returns whether it was held. A handler that needs to know which
    /// application opens a file cannot answer with an empty cache -- it would
    /// silently find nothing rather than the right application -- so it waits
    /// instead, and is handled again once the answer exists.
    fn defer_until_mime_apps(&mut self, message: Message) -> bool {
        if self.mime_app_cache.is_loaded() {
            return false;
        }
        if self.deferred_mime_messages.len() >= Self::MAX_DEFERRED_MIME_MESSAGES {
            log::warn!("dropping {message:?}: too many actions waiting on the mime app cache");
            return true;
        }
        log::debug!("holding {message:?} until the mime app cache is built");
        self.deferred_mime_messages.push(message);
        true
    }

    /// Look at `path` in the background, and report what is there.
    ///
    /// The answer drives a warning under a name field, so it is worth being
    /// right about but not worth waiting for: a name typed towards a slow or
    /// remote folder would otherwise stall on every keystroke. What the
    /// dialog does with a stale or missing answer is deliberately harmless,
    /// because the operations themselves refuse to overwrite anything.
    fn check_name(&mut self, path: PathBuf) -> Task<Message> {
        let revision = self.name_check_revision.fetch_add(1, Ordering::Relaxed) + 1;
        let shared = Arc::clone(&self.name_check_revision);
        Task::future(async move {
            tokio::time::sleep(NAME_CHECK_DELAY).await;
            // Typing has moved on, and a later check is already scheduled for
            // whatever is in the field now.
            if shared.load(Ordering::Relaxed) != revision {
                return crate::ui::action::none();
            }

            let checked = tokio::task::spawn_blocking(move || {
                let taken = match std::fs::metadata(&path) {
                    Ok(metadata) if metadata.is_dir() => Some(NameTaken::Folder),
                    Ok(_) => Some(NameTaken::File),
                    Err(_) => None,
                };
                NameCheck { path, taken }
            })
            .await;

            match checked {
                Ok(check) => crate::ui::action::app(Message::NameChecked(check)),
                Err(err) => {
                    log::warn!("failed to check a typed name: {err}");
                    crate::ui::action::none()
                }
            }
        })
    }

    /// Rebuild the batch rename preview if that dialog is the one in front.
    ///
    /// The names themselves are worked out here, because they are pure string
    /// work and the list is redrawn with every keystroke. Whether each new
    /// name is already taken on disk is not: that is one `stat` per changed
    /// name, and a large selection makes typing into the dialog as slow as the
    /// folder it is renaming in. It is answered on a worker and merged in when
    /// it arrives, so the list never waits for the disk to show what it will
    /// rename things to.
    fn refresh_batch_rename_preview(&mut self) -> Task<Message> {
        let Some(DialogPage::BatchRename {
            parent,
            names,
            settings,
        }) = self.dialog_pages.front()
        else {
            return Task::none();
        };

        let tags = batch_rename::Tags::localized();
        self.batch_rename_preview = batch_rename::preview(parent, names, settings, &tags, false);

        // Checking every destination is only affordable on a local filesystem
        let parent = parent.clone();
        let names = names.clone();
        let settings = settings.clone();
        let revision = self.batch_rename_revision.wrapping_add(1);
        self.batch_rename_revision = revision;

        Task::future(async move {
            let checked = tokio::task::spawn_blocking(move || {
                let local = parent
                    .symlink_metadata()
                    .is_ok_and(|metadata| matches!(tab::fs_kind(&metadata), tab::FsKind::Local));
                if !local {
                    return None;
                }
                Some(batch_rename::preview(
                    &parent, &names, &settings, &tags, true,
                ))
            })
            .await;

            match checked {
                Ok(Some(preview)) => {
                    crate::ui::action::app(Message::BatchRenamePreview(revision, preview))
                }
                Ok(None) => crate::ui::action::none(),
                Err(err) => {
                    log::warn!("failed to check batch rename destinations: {err}");
                    crate::ui::action::none()
                }
            }
        })
    }

    fn operation(&mut self, operation: Operation) -> Task<Message> {
        let id = self.pending_operation_id;
        let controller = Controller::default();
        let compio_tx = self.compio_tx.clone();

        self.pending_operation_id += 1;
        if operation.show_progress_notification() {
            self.progress_operations.insert(id);
        }
        self.pending_operations
            .insert(id, (operation.clone(), controller.clone()));

        // Use a task to send operations to the compio runtime thread.
        crate::ui::Task::stream(crate::ui::iced::stream::channel(
            4,
            move |msg_tx| async move {
                let (tx, rx) = tokio::sync::oneshot::channel();

                let msg_tx = Arc::new(tokio::sync::Mutex::new(msg_tx));

                let msg_tx_clone = msg_tx.clone();

                // Every path out of here has to answer with one message.
                // Without that the operation stays pending for good: the
                // progress bar never finishes, the sleep inhibitor is never
                // released, and the app can no longer quit.
                let failed = |reason: &str| {
                    log::error!("operation {id} was never run: {reason}");
                    Message::PendingError(
                        id,
                        OperationError::from_msg(fl!("operation-failed-to-start")),
                    )
                };
                let msg = if compio_tx
                    .send(Box::pin(async move {
                        let msg = match operation.perform(&msg_tx_clone, controller).await {
                            Ok(result_paths) => Message::PendingComplete(id, result_paths),
                            Err(err) => Message::PendingError(id, err),
                        };

                        _ = tx.send(msg);
                    }))
                    .await
                    .is_err()
                {
                    failed("the file operation runtime is gone")
                } else {
                    match rx.await {
                        Ok(msg) => msg,
                        Err(_) => failed("the file operation runtime dropped it"),
                    }
                };
                let _ = msg_tx.lock().await.send(msg).await;
            },
        ))
        .map(crate::ui::Action::App)
    }

    /// Will join operations together into a single task that will return a single
    /// Message::PendingResults message when all operations are complete.
    fn join_operations(&mut self, operations: Vec<Operation>) -> Task<Message> {
        Task::batch(
            operations
                .into_iter()
                .map(|operation| self.operation(operation)),
        )
        .collect()
        .map(|messages| {
            let results = messages.into_iter().fold(
                Message::PendingResults(Vec::new(), Vec::new()),
                |mut acc, message| {
                    if let Message::PendingResults(completed, errors) = &mut acc {
                        match message {
                            crate::ui::Action::App(Message::PendingComplete(id, selection)) => {
                                completed.push((id, selection));
                            }
                            crate::ui::Action::App(Message::PendingError(id, err)) => {
                                errors.push((id, err));
                            }
                            _ => {}
                        }
                    }
                    acc
                },
            );
            crate::ui::Action::App(results)
        })
    }

    fn handle_completed_operations(
        &mut self,
        completed: Vec<(u64, OperationSelection)>,
    ) -> Task<Message> {
        let mut commands = Vec::with_capacity(4 * completed.len());
        let mut op_sel = OperationSelection::default();
        for (id, op_sel_pending) in completed {
            let trash_items = op_sel_pending.trash_items.clone();
            if let Some((op, _)) = self.pending_operations.get(&id)
                && !self.undo_ids.remove(&id)
            {
                let undo = op.undo(&op_sel_pending);
                if !undo.is_empty() {
                    self.undo_stack.push(undo);
                    if self.undo_stack.len() > UNDO_DEPTH {
                        self.undo_stack.remove(0);
                    }
                }
            }
            op_sel.ignored.extend(op_sel_pending.ignored);
            op_sel.selected.extend(op_sel_pending.selected);
            if let Some((op, _)) = self.pending_operations.remove(&id) {
                // Show toast for some operations
                if let Some(description) = op.toast() {
                    if let Operation::Delete { ref paths } = op {
                        // Restore the recorded entries; fall back to matching the
                        // trash by path when none were recorded
                        let action = if trash_items.is_empty() {
                            let paths: Arc<[PathBuf]> = Arc::from(paths.as_slice());
                            Box::new(move |tid| Message::UndoTrash(tid, paths.clone()))
                                as Box<dyn Fn(widget::ToastId) -> Message>
                        } else {
                            Box::new(move |tid| {
                                Message::UndoTrashStart(Some(tid), trash_items.clone())
                            })
                        };
                        commands.push(
                            self.toasts
                                .push(
                                    widget::toaster::Toast::new(description)
                                        .action(fl!("undo"), action),
                                )
                                .map(crate::ui::Action::App),
                        );
                    } else {
                        commands.push(
                            self.toasts
                                .push(widget::toaster::Toast::new(description))
                                .map(crate::ui::Action::App),
                        );
                    }
                }

                // If a favorite for a path has been renamed or moved, update it.
                if let Operation::Rename { ref from, ref to } = op {
                    if self.update_favorites([(from, to)].as_slice()) {
                        commands.push(self.update_config());
                    }
                } else if let Operation::BatchRename { ref renames } = op {
                    let path_changes: Box<[_]> =
                        renames.iter().map(|(from, to)| (from, to)).collect();
                    if self.update_favorites(&path_changes) {
                        commands.push(self.update_config());
                    }
                } else if let Operation::Move {
                    ref paths, ref to, ..
                } = op
                {
                    let path_changes: Box<[_]> = paths
                        .iter()
                        .filter_map(|from| from.file_name().map(|name| (from, to.join(name))))
                        .collect();
                    if self.update_favorites(&path_changes) {
                        commands.push(self.update_config());
                    }
                }

                if matches!(op, Operation::RemoveFromRecents { .. }) {
                    commands.push(self.rescan_recents());
                }

                let mut op = op;
                op.release_payload();
                self.complete_operations.insert(id, op);
            }
        }
        // Close progress notification if all relevant operations are finished
        if !self
            .pending_operations
            .values()
            .any(|(op, _)| op.show_progress_notification())
        {
            self.progress_operations.clear();
        }
        // Potentially show a notification
        commands.push(self.update_notification());
        // Rescan and select based on operation
        commands.push(self.rescan_operation_selection(op_sel));
        // Manually rescan any trash tabs after any operation is completed
        commands.push(self.rescan_trash());

        Task::batch(commands)
    }

    fn handle_operation_errors(&mut self, errors: Vec<(u64, OperationError)>) -> Task<Message> {
        let mut tasks = Vec::new();
        let mut failed = Vec::new();
        for (id, err) in errors.into_iter() {
            if let Some((op, controller)) = self.pending_operations.remove(&id) {
                // Only show dialog if not cancelled
                if !controller.is_cancelled() {
                    match err.kind {
                        OperationErrorType::Generic(_) => failed.push(id),
                        OperationErrorType::PasswordRequired => {
                            tasks.push(self.dialog_pages.push_back(DialogPage::ExtractPassword {
                                id,
                                password: String::new(),
                            }));
                        }
                    }
                }

                // Remove from progress
                self.progress_operations.remove(&id);
                // The payload stays: a failed operation can be retried from
                // the dialog, and retrying a paste whose bytes were dropped
                // would write an empty file and call it a success. It is
                // released when the retry succeeds, or dropped with the entry
                self.failed_operations
                    .insert(id, (op, controller, err.to_string()));
            }
        }
        if !failed.is_empty() {
            tasks.push(
                self.dialog_pages
                    .push_back(DialogPage::FailedOperations(failed)),
            );
            tasks.push(widget::text_input::focus(self.dialog_text_input.clone()));
        }

        // Close progress notification if all relevant operations are finished
        if !self
            .pending_operations
            .values()
            .any(|(op, _)| op.show_progress_notification())
        {
            self.progress_operations.clear();
        }
        // Manually rescan any trash tabs after any operation is completed
        tasks.push(self.rescan_trash());
        Task::batch(tasks)
    }

    fn remove_window(&mut self, id: &window::Id) {
        self.windows.remove(id);
    }

    fn rescan_operation_selection(&mut self, op_sel: OperationSelection) -> Task<Message> {
        log::info!("rescan_operation_selection {op_sel:?}");
        let entity = self.tab_model.active();
        let Some(tab) = self.tab_model.data::<Tab>(entity) else {
            return Task::none();
        };
        let Some(items) = tab.items_opt() else {
            return Task::none();
        };
        for item in items {
            if item.selected {
                if let Some(path) = item.path_opt()
                    && (op_sel.selected.contains(path) || op_sel.ignored.contains(path))
                {
                    // Ignore if path in selected or ignored paths
                    continue;
                }

                // Return if there is a previous selection not matching
                return Task::none();
            }
        }
        self.update_tab(entity, tab.location.clone(), Some(op_sel.selected))
    }

    fn update_tab(
        &mut self,
        entity: Entity,
        location: Location,
        selection_paths: Option<Vec<PathBuf>>,
    ) -> Task<Message> {
        if let Location::Search(_, term, ..) = location {
            self.search_set(entity, Some(term), selection_paths)
        } else {
            self.rescan_tab(entity, location, selection_paths)
        }
    }

    fn rescan_tab(
        &mut self,
        entity: Entity,
        location: Location,
        selection_paths: Option<Vec<PathBuf>>,
    ) -> Task<Message> {
        log::info!("rescan_tab {entity:?} {location:?} {selection_paths:?}");
        let icon_sizes = self.config.tab.icon_sizes;
        let mounter_items = self.mounter_items.clone();

        Task::future(async move {
            let location2 = location.clone();
            match tokio::task::spawn_blocking(move || location2.scan(icon_sizes)).await {
                Ok((parent_item_opt, mut items)) => {
                    #[cfg(feature = "gvfs")]
                    {
                        let mounter_paths: Box<[_]> = mounter_items
                            .values()
                            .flatten()
                            .filter_map(MounterItem::path)
                            .collect();
                        if !mounter_paths.is_empty() {
                            for item in &mut items {
                                item.is_mount_point =
                                    item.path_opt().is_some_and(|p| mounter_paths.contains(p));
                            }
                        }
                    }

                    crate::ui::action::app(Message::TabRescan(
                        entity,
                        location,
                        parent_item_opt,
                        items,
                        selection_paths,
                    ))
                }
                Err(err) => {
                    log::warn!("failed to rescan: {err}");
                    crate::ui::action::none()
                }
            }
        })
    }

    fn rescan_trash(&mut self) -> Task<Message> {
        let needs_reload: Box<[_]> = self
            .tab_model
            .iter()
            .filter_map(|entity| {
                let tab = self.tab_model.data::<Tab>(entity)?;
                tab.location
                    .is_trash()
                    .then_some((entity, tab.location.clone()))
            })
            .collect();

        let commands = needs_reload
            .into_iter()
            .map(|(entity, location)| self.update_tab(entity, location, None));

        Task::batch(commands)
    }

    fn rescan_recents(&mut self) -> Task<Message> {
        let needs_reload: Box<[_]> = self
            .tab_model
            .iter()
            .filter_map(|entity| {
                let tab = self.tab_model.data::<Tab>(entity)?;
                tab.location
                    .is_recents()
                    .then_some((entity, tab.location.clone()))
            })
            .collect();

        let commands = needs_reload
            .into_iter()
            .map(|(entity, location)| self.update_tab(entity, location, None));

        Task::batch(commands)
    }

    fn search_get(&self) -> Option<&str> {
        let entity = self.tab_model.active();
        let tab = self.tab_model.data::<Tab>(entity)?;
        match &tab.location {
            Location::Search(_, term, ..) => Some(term),
            _ => None,
        }
    }

    fn search_set_active(&mut self, term_opt: Option<String>) -> Task<Message> {
        let entity = self.tab_model.active();
        self.search_set(entity, term_opt, None)
    }

    fn search_set(
        &mut self,
        tab: Entity,
        term_opt: Option<String>,
        selection_paths: Option<Vec<PathBuf>>,
    ) -> Task<Message> {
        let mut title_location_opt = None;
        if let Some(tab) = self.tab_model.data_mut::<Tab>(tab) {
            let location_opt = match term_opt {
                Some(term) => {
                    let search_location = if let Some(path) = tab.location.path_opt() {
                        Some(SearchLocation::Path(path.clone()))
                    } else if tab.location.is_recents() {
                        Some(SearchLocation::Recents)
                    } else if tab.location.is_trash() {
                        Some(SearchLocation::Trash)
                    } else {
                        None
                    };

                    search_location.map(|search_location| {
                        (
                            Location::Search(
                                search_location,
                                term,
                                tab.config.show_hidden,
                                Instant::now(),
                            ),
                            true,
                        )
                    })
                }
                None => match &tab.location {
                    Location::Search(search_location, ..) => match search_location {
                        SearchLocation::Path(path) => Some((Location::Path(path.clone()), false)),
                        SearchLocation::Recents => Some((Location::Recents, false)),
                        SearchLocation::Trash => Some((Location::Trash, false)),
                    },
                    _ => None,
                },
            };
            if let Some((location, focus_search)) = location_opt {
                tab.change_location(&location, None);
                title_location_opt = Some((tab.title(), tab.location.clone(), focus_search));
            }
        }
        if let Some((title, location, focus_search)) = title_location_opt {
            self.tab_model.text_set(tab, title);
            return Task::batch([
                self.update_title(),
                self.update_watcher(),
                self.rescan_tab(tab, location, selection_paths),
                if focus_search {
                    widget::text_input::focus(self.search_id.clone())
                } else {
                    Task::none()
                },
            ]);
        }
        Task::none()
    }

    fn selected_paths(
        &self,
        entity_opt: Option<Entity>,
    ) -> impl Iterator<Item = PathBuf> + use<'_> {
        let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
        self.tab_model
            .data::<Tab>(entity)
            .into_iter()
            .flat_map(|tab| {
                tab.selected_locations()
                    .into_iter()
                    .filter_map(Location::into_path_opt)
            })
    }

    fn set_cut(&mut self, entity_opt: Option<Entity>) {
        let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
        if let Some(tab) = self.tab_model.data_mut::<Tab>(entity) {
            tab.cut_selected();
        }
    }

    fn update_config(&mut self) -> Task<Message> {
        self.key_binds = key_binds(&tab::Mode::App, &self.config.key_binds);
        self.core.set_nav_bar_width(self.config.nav_bar_width);
        crate::ui::theme::set_density(self.config.density);
        crate::ui::theme::set_header_size(self.config.header_size);
        self.update_nav_model();
        // Tabs are collected first to placate the borrowck
        let tabs: Box<[_]> = self.tab_model.iter().collect();
        // Update main conf and each tab with the new config
        let commands = std::iter::once(crate::ui::command::set_theme(
            self.config.app_theme.theme(),
        ))
        .chain(tabs.into_iter().map(|entity| {
            self.update(Message::TabMessage(
                Some(entity),
                tab::Message::Config(self.config.tab),
            ))
        }));
        Task::batch(commands)
    }

    fn activate_nav_model_location(&mut self, location: &Location) {
        let nav_bar_id = self.nav_model.iter().find(|&id| {
            self.nav_model
                .data::<Location>(id)
                .is_some_and(|l| l == location)
        });

        if let Some(id) = nav_bar_id {
            self.nav_model.activate(id);
        } else {
            let active = self.nav_model.active();
            segmented_button::Selectable::deactivate(&mut self.nav_model, active);
        }
    }

    fn update_nav_model(&mut self) {
        // Entities are renumbered by the rebuild, so an open sidebar context
        // menu no longer refers to anything
        self.nav_bar_context_id = segmented_button::Entity::null();
        let mut nav_model = segmented_button::ModelBuilder::default();

        if self.config.show_recents {
            nav_model = nav_model.insert(|b| {
                b.text(fl!("recents"))
                    .icon(icon::from_name("document-open-recent-symbolic"))
                    .data(Location::Recents)
            });
        }

        for (favorite_i, favorite) in self.config.favorites.iter().enumerate() {
            if let Some(path) = favorite.path_opt() {
                let name = favorite.display_name().unwrap_or_else(|| fl!("filesystem"));
                nav_model = nav_model.insert(move |b| {
                    b.text(name.clone())
                        .icon(
                            icon::icon(if path.is_dir() {
                                tab::folder_icon_symbolic(&path, 16)
                            } else {
                                icon::from_name("text-x-generic-symbolic").size(16).handle()
                            })
                            .size(16),
                        )
                        .data(match favorite {
                            Favorite::Network { uri, name, path } => {
                                Location::Network(uri.clone(), name.clone(), Some(path.to_owned()))
                            }
                            _ => Location::Path(path.clone()),
                        })
                        .data(FavoriteIndex(favorite_i))
                });
            }
        }

        nav_model = nav_model.insert(|b| {
            b.text(fl!("trash"))
                .icon(icon::icon(Trash::icon_symbolic(16)))
                .data(Location::Trash)
                .divider_above()
        });

        if !MOUNTERS.is_empty() {
            nav_model = nav_model.insert(|b| {
                b.text(fl!("networks"))
                    .icon(icon::icon(
                        icon::from_name("network-workgroup-symbolic")
                            .size(16)
                            .handle(),
                    ))
                    .data(Location::Network(
                        "network:///".to_string(),
                        fl!("networks"),
                        None,
                    ))
                    .divider_above()
            });
        }

        // Collect all mounter items
        let mut nav_items = Vec::new();
        for (key, items) in &self.mounter_items {
            nav_items.extend(items.iter().map(|item| (*key, item)));
        }
        // Sort by name lexically
        nav_items.sort_by(|a, b| LANGUAGE_SORTER.compare(&a.1.name(), &b.1.name()));
        // Add items to nav model
        for (i, (key, item)) in nav_items.into_iter().enumerate() {
            nav_model = nav_model.insert(|mut b| {
                b = b.text(item.name()).data(MounterData(key, item.clone()));
                let uri = item.uri();
                if let Some(path) = item.path() {
                    if item.is_remote() {
                        b = b.data(Location::Network(uri, item.name(), Some(path)));
                    } else {
                        b = b.data(Location::Path(path));
                    }
                } else if !uri.is_empty() {
                    b = b.data(Location::Network(uri, item.name(), None));
                }
                if let Some(icon) = item.icon(true) {
                    b = b.icon(icon::icon(icon).size(16));
                }
                if item.is_mounted() {
                    b = b.closable();
                }
                if i == 0 {
                    b = b.divider_above();
                }
                b
            });
        }

        self.nav_model = nav_model.build();

        let tab_entity = self.tab_model.active();
        if let Some(tab) = self.tab_model.data::<Tab>(tab_entity) {
            self.activate_nav_model_location(&tab.location.clone());
        }
    }

    fn update_notification(&mut self) -> Task<Message> {
        // Handle closing notification if there are no operations
        if self.pending_operations.is_empty() {
            #[cfg(feature = "notify")]
            if let Some(notification_arc) = self.notification_opt.take() {
                return Task::future(async move {
                    let closed = tokio::task::spawn_blocking(move || {
                        // Closing consumes the handle, so it must be the last reference
                        match Arc::try_unwrap(notification_arc).map(Mutex::into_inner) {
                            Ok(Ok(notification)) => {
                                notification.close();
                                true
                            }
                            _ => false,
                        }
                    })
                    .await;
                    if !matches!(closed, Ok(true)) {
                        log::warn!("progress notification could not be closed");
                    }
                    crate::ui::action::app(Message::MaybeExit)
                });
            }
        }

        Task::none()
    }

    fn update_title(&mut self) -> Task<Message> {
        let window_title = match self.tab_model.text(self.tab_model.active()) {
            Some(tab_title) => format!("{tab_title} — {}", fl!("earth-files")),
            None => fl!("earth-files"),
        };
        if let Some(window_id) = self.core.main_window_id() {
            self.set_window_title(window_title, window_id)
        } else {
            Task::none()
        }
    }

    fn update_watcher(&mut self) -> Task<Message> {
        if let Some((mut watcher, old_paths)) = self.watcher_opt.take() {
            let new_paths: FxHashSet<_> = self
                .tab_model
                .iter()
                .filter_map(|entity| {
                    let tab = self.tab_model.data::<Tab>(entity)?;
                    tab.location.path_opt().cloned()
                })
                .collect();

            // Unwatch paths no longer used
            for path in &old_paths {
                if !new_paths.contains(path) {
                    match watcher.unwatch(path) {
                        Ok(()) => {
                            log::debug!("unwatching {}", path.display());
                        }
                        Err(err) => {
                            log::debug!("failed to unwatch {}: {}", path.display(), err);
                        }
                    }
                }
            }

            // Watch new paths
            for path in &new_paths {
                if !old_paths.contains(path) {
                    match watcher.watch(path, notify::RecursiveMode::NonRecursive) {
                        Ok(()) => {
                            log::debug!("watching {}", path.display());
                        }
                        Err(err) => {
                            log::debug!("failed to watch {}: {}", path.display(), err);
                        }
                    }
                }
            }

            self.watcher_opt = Some((watcher, new_paths));
        }

        Task::none()
    }

    fn network_drive(&self) -> Element<'_, Message> {
        let Spacing {
            space_xxs, space_m, ..
        } = spacing();
        let mut table = widget::Column::with_capacity(8);
        for (i, line) in fl!("network-drive-schemes").lines().enumerate() {
            let mut row = widget::Row::with_capacity(2);
            for part in line.split(',') {
                row = row.push(
                    widget::container(if i == 0 {
                        widget::text::heading(part.to_string())
                    } else {
                        widget::text::body(part.to_string())
                    })
                    .width(Length::Fill)
                    .padding(space_xxs),
                );
            }
            table = table.push(row);
            if i == 0 {
                table = table.push(widget::divider::horizontal::light());
            }
        }
        widget::Column::with_children([
            widget::text::body(fl!("network-drive-description")).into(),
            table.into(),
        ])
        .spacing(space_m.to_pixels())
        .into()
    }

    fn edit_history(&self) -> Element<'_, Message> {
        let Spacing { space_m, .. } = spacing();

        let mut children = Vec::new();

        let progress_bar_height = Length::Fixed(4.0);

        if !self.pending_operations.is_empty() {
            let mut section = widget::settings::section().title(fl!("pending"));
            for (id, (op, controller)) in self.pending_operations.iter().rev() {
                let progress = controller.progress();
                section = section.add(widget::Column::with_children([
                    widget::Row::with_children([
                        widget::determinate_linear(progress)
                            .width(Length::Fill)
                            .girth(progress_bar_height)
                            .into(),
                        if controller.is_paused() {
                            widget::tooltip(
                                widget::button::icon(icon::from_name(
                                    "media-playback-start-symbolic",
                                ))
                                .on_press(Message::PendingPause(*id, false))
                                .padding(8),
                                widget::text::body(fl!("resume")),
                                widget::tooltip::Position::Top,
                            )
                            .into()
                        } else {
                            widget::tooltip(
                                widget::button::icon(icon::from_name(
                                    "media-playback-pause-symbolic",
                                ))
                                .on_press(Message::PendingPause(*id, true))
                                .padding(8),
                                widget::text::body(fl!("pause")),
                                widget::tooltip::Position::Top,
                            )
                            .into()
                        },
                        widget::tooltip(
                            widget::button::icon(icon::from_name("window-close-symbolic"))
                                .on_press(Message::PendingCancel(*id))
                                .padding(8),
                            widget::text::body(fl!("cancel")),
                            widget::tooltip::Position::Top,
                        )
                        .into(),
                    ])
                    .align_y(Alignment::Center)
                    .into(),
                    widget::text::body(op.pending_text(progress, controller.state())).into(),
                ]));
            }
            children.push(section.into());
        }

        if !self.failed_operations.is_empty() {
            let mut section = widget::settings::section().title(fl!("failed"));
            for (op, controller, error) in self.failed_operations.values().rev() {
                let progress = controller.progress();
                section = section.add(widget::Column::with_children([
                    widget::text::body(op.pending_text(progress, controller.state())).into(),
                    widget::text::body(error).into(),
                ]));
            }
            children.push(section.into());
        }

        if !self.complete_operations.is_empty() {
            let mut section = widget::settings::section().title(fl!("complete"));
            for op in self.complete_operations.values().rev() {
                section = section.add(widget::text::body(op.completed_text()));
            }
            children.push(section.into());
        }

        if children.is_empty() {
            children.push(widget::text::body(fl!("no-history")).into());
        }

        widget::Column::with_children(children)
            .spacing(space_m.to_pixels())
            .into()
    }

    fn preview<'a>(
        &'a self,
        entity_opt: &Option<Entity>,
        kind: &'a PreviewKind,
        context_drawer: bool,
    ) -> Element<'a, tab::Message> {
        let Spacing { space_l, .. } = spacing();

        let mut children = Vec::with_capacity(1);
        let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
        match kind {
            PreviewKind::Custom(PreviewItem(item)) => {
                children.push(item.preview_view(self.mime_apps_for_view()));
            }
            PreviewKind::Location(location) => {
                if let Some(tab) = self.tab_model.data::<Tab>(entity)
                    && let Some(items) = tab.items_opt()
                {
                    for item in items {
                        if item.location_opt.as_ref() == Some(location) {
                            children.push(item.preview_view(self.mime_apps_for_view()));
                            // Only show one property view to avoid issues like hangs when generating
                            // preview images on thousands of files
                            break;
                        }
                    }
                }
            }
            PreviewKind::Selected => {
                if let Some(tab) = self.tab_model.data::<Tab>(entity)
                    && let Some(items) = tab.items_opt()
                {
                    let preview_opt = {
                        let mut selected = items.iter().filter(|item| item.selected);

                        match (selected.next(), selected.next()) {
                            // At least two selected items
                            (Some(_), Some(_)) => {
                                Some(tab.multi_preview_view(self.mime_apps_for_view()))
                            }
                            // Exactly one selected item
                            (Some(item), None) => {
                                Some(item.preview_view(self.mime_apps_for_view()))
                            }
                            // No selected items
                            _ => None,
                        }
                    };

                    if let Some(preview) = preview_opt {
                        children.push(preview);
                    }

                    if children.is_empty()
                        && let Some(item) = &tab.parent_item_opt
                    {
                        children.push(item.preview_view(self.mime_apps_for_view()));
                    }
                }
            }
        }
        widget::Column::with_children(children)
            .padding(
                (if context_drawer {
                    [0, 0, 0, 0]
                } else {
                    [0, space_l, space_l, space_l]
                })
                .to_padding(),
            )
            .into()
    }

    fn settings(&self) -> Element<'_, Message> {
        let tab_config = self.config.tab;

        settings::view_column(vec![
            settings::section()
                .title(fl!("appearance"))
                .add({
                    let app_theme_selected = match self.config.app_theme {
                        AppTheme::Dark => 1,
                        AppTheme::Light => 2,
                        AppTheme::System => 0,
                    };
                    settings::item::builder(fl!("theme")).control(widget::dropdown(
                        &self.app_themes,
                        Some(app_theme_selected),
                        move |index| {
                            Message::AppTheme(match index {
                                1 => AppTheme::Dark,
                                2 => AppTheme::Light,
                                _ => AppTheme::System,
                            })
                        },
                    ))
                })
                .into(),
            settings::section()
                .title(fl!("type-to-search"))
                .add(
                    settings::item::builder(fl!("type-to-search-recursive")).radio(
                        TypeToSearch::Recursive,
                        Some(self.config.type_to_search),
                        Message::SetTypeToSearch,
                    ),
                )
                .add(
                    settings::item::builder(fl!("type-to-search-enter-path")).radio(
                        TypeToSearch::EnterPath,
                        Some(self.config.type_to_search),
                        Message::SetTypeToSearch,
                    ),
                )
                .add(settings::item::builder(fl!("type-to-search-select")).radio(
                    TypeToSearch::SelectByPrefix,
                    Some(self.config.type_to_search),
                    Message::SetTypeToSearch,
                ))
                .into(),
            settings::section()
                .title(fl!("other"))
                .add({
                    settings::item::builder(fl!("single-click")).toggler(
                        tab_config.single_click,
                        move |single_click| {
                            Message::TabConfig(TabConfig {
                                single_click,
                                ..tab_config
                            })
                        },
                    )
                })
                .add({
                    settings::item::builder(fl!("show-recents"))
                        .toggler(self.config.show_recents, Message::SetShowRecents)
                })
                .into(),
        ])
        .into()
    }

    // Update favorites based on renaming or moving dirs.
    fn update_favorites(&mut self, path_changes: &[(impl AsRef<Path>, impl AsRef<Path>)]) -> bool {
        let mut favorites_changed = false;
        let favorites = self
            .config
            .favorites
            .iter()
            .map(|favorite| {
                match favorite {
                    Favorite::Path(path) => {
                        for (from, to) in path_changes.iter().map(|(f, t)| (f.as_ref(), t.as_ref()))
                        {
                            if path.starts_with(from)
                                && let Ok(relative) = path.strip_prefix(from)
                            {
                                favorites_changed = true;
                                return Favorite::from_path(to.join(relative));
                            }
                        }
                    }
                    Favorite::Named { path, name } => {
                        for (from, to) in path_changes.iter().map(|(f, t)| (f.as_ref(), t.as_ref()))
                        {
                            if path.starts_with(from)
                                && let Ok(relative) = path.strip_prefix(from)
                            {
                                favorites_changed = true;
                                return Favorite::Named {
                                    path: to.join(relative),
                                    name: name.clone(),
                                };
                            }
                        }
                    }
                    _ => {}
                }
                favorite.clone()
            })
            .collect();

        if favorites_changed {
            self.config.favorites = favorites;
            if let Err(err) = self.config_handler.save_in_background(&self.config) {
                log::warn!("failed to update favorites after moving directories: {err:?}",);
            }
            return true;
        }

        false
    }
}

/// Implement [`Application`] to integrate with this app's shell.
impl Application for App {
    /// Argument received
    type Flags = Flags;

    /// Message type specific to our [`App`].
    type Message = Message;

    /// The unique application ID to supply to the window manager.
    const APP_ID: &'static str = "com.owlm.EarthFiles";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    /// Creates the application, and optionally emits command on initialize.
    fn init(mut core: Core, flags: Self::Flags) -> (Self, Task<Self::Message>) {
        core.window.context_is_overlay = false;
        core.window.show_context = flags.config.show_details;
        core.set_nav_bar_width(flags.config.nav_bar_width);

        let app_themes = vec![fl!("match-desktop"), fl!("dark"), fl!("light")];

        let key_binds = key_binds(&tab::Mode::App, &flags.config.key_binds);

        // Create a dedicated thread for the compio runtime to handle operations on.
        // Supports io_uring on Linux, IOPC on Windows, and polling everywhere else.
        let (compio_tx, mut compio_rx) = mpsc::channel(1);
        let tokio_handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            let _tokio = tokio_handle.enter();
            match compio::runtime::RuntimeBuilder::new().build() {
                Ok(runtime) => runtime.block_on(async move {
                    while let Some(task) = compio_rx.recv().await {
                        compio::runtime::spawn(task).detach();
                    }
                }),
                Err(err) => {
                    // Dropping the receiver makes every later operation fail
                    // fast with an error the user sees, rather than queue up
                    // behind a runtime that will never run them
                    log::error!("failed to start the file operation runtime: {err}");
                }
            }
        });

        let about = About::default()
            .name(fl!("earth-files"))
            .icon(icon::from_name(Self::APP_ID))
            .version(env!("CARGO_PKG_VERSION"))
            .author("owl-m (fork), System76 (upstream)")
            .comments(fl!("comment"))
            .license("GPL-3.0-only")
            .license_url("https://spdx.org/licenses/GPL-3.0-only")
            .developers([("Jeremy Soller", "jeremy@system76.com")])
            .links([
                (fl!("repository"), "https://github.com/owl-m/earth-files"),
                (
                    fl!("support"),
                    "https://github.com/owl-m/earth-files/issues",
                ),
            ]);

        let mut app = Self {
            core,
            about,
            nav_bar_context_id: segmented_button::Entity::null(),
            nav_model: segmented_button::ModelBuilder::default().build(),
            tab_model: segmented_button::ModelBuilder::default().build(),
            config_handler: flags.config_handler,
            state_handler: flags.state_handler,
            config: flags.config,
            state: flags.state,
            app_themes,
            compio_tx,
            context_page: ContextPage::Preview(None, PreviewKind::Selected),
            dialog_pages: DialogPages::new(),
            dialog_text_input: widget::Id::new("Dialog Text Input"),
            auth_password_visible: false,
            batch_rename_preview: batch_rename::Preview::default(),
            batch_rename_revision: 0,
            key_binds,
            // Built on a worker below rather than here: walking every desktop
            // entry on the system is not work to do before the first frame.
            // Anything that needs it waits through `defer_until_mime_apps`.
            mime_app_cache: MimeAppCache::empty(),
            modifiers: Modifiers::empty(),
            mounter_items: FxHashMap::default(),
            must_save_sort_names: false,
            network_drive_connecting: None,
            network_drive_input: String::new(),
            #[cfg(feature = "notify")]
            notification_opt: None,
            pending_operation_id: 0,
            pending_operations: BTreeMap::new(),
            progress_operations: BTreeSet::new(),
            complete_operations: BTreeMap::new(),
            failed_operations: BTreeMap::new(),
            undo_stack: Vec::new(),
            undo_ids: BTreeSet::new(),
            scrollable_name: std::borrow::Cow::Borrowed("File Scrollable"),
            search_id: widget::Id::new("File Search"),
            size: None,
            toasts: widget::toaster::Toasts::new(Message::CloseToast),
            watcher_opt: None,
            windows: FxHashMap::default(),
            type_select_prefix: String::new(),
            type_select_last_key: None,
            auto_scroll_speed: None,
            file_dialog_opt: None,
            clipboard_cache: ClipboardCache::Empty,
            name_check: None,
            mime_app_rebuild: 0,
            mime_app_rebuild_applied: 0,
            deferred_mime_messages: Vec::new(),
            opening: FxHashMap::default(),
            next_open_request: 0,
            name_check_revision: Arc::new(AtomicU64::new(0)),
        };

        let mut commands = vec![
            app.update_config(),
            app.update(Message::CheckClipboard),
            // The cache starts empty; this builds it, and primes the default
            // terminal along with it, on a worker.
            app.update(Message::ReloadMimeAppCache),
        ];

        for location in flags.locations {
            if let Some(path) = location.path_opt()
                && path.is_file()
                && let Some(parent) = path.parent()
            {
                commands.push(app.open_tab(
                    Location::Path(parent.to_path_buf()),
                    true,
                    Some(vec![path.clone()]),
                ));
                continue;
            }
            commands.push(app.open_tab(location, true, None));
        }
        for location in flags.uris {
            if let Some(e) = app.nav_model.iter().find(|e| {
                app.nav_model.data::<Location>(*e).is_some_and(
                    |l| matches!(l, Location::Network(uri, ..) if *uri == *location.as_str()),
                )
            }) {
                commands.push(crate::ui::task::message(crate::ui::Action::App(
                    Message::NetworkDriveOpenEntityAfterMount { entity: e },
                )));
            } else {
                // Not in the sidebar: open it directly, the mounter mounts it as needed
                let uri = location.to_string();
                commands.push(app.open_tab(Location::Network(uri.clone(), uri, None), true, None));
            }
        }

        if app.tab_model.entity_at(0).is_none() {
            if let Ok(current_dir) = env::current_dir() {
                commands.push(app.open_tab(Location::Path(current_dir), true, None));
            } else {
                commands.push(app.open_tab(Location::Path(home_dir()), true, None));
            }
        }

        (app, Task::batch(commands))
    }

    fn nav_bar(&self) -> Option<Element<'_, crate::ui::Action<Self::Message>>> {
        if !self.core.nav_bar_active() {
            return None;
        }

        let nav_model = self.nav_model()?;

        let mut nav = crate::ui::widget::nav_bar(nav_model, |entity| {
            crate::ui::Action::Cosmic(crate::ui::app::Action::NavBar(entity))
        })
        .on_context(|entity| crate::ui::Action::App(Message::NavBarContext(entity)))
        .on_close(|entity| crate::ui::Action::App(Message::NavBarClose(entity)))
        .on_middle_press(|entity| {
            crate::ui::Action::App(Message::NavMenuAction(NavMenuAction::OpenInNewTab(entity)))
        })
        .context_menu(self.nav_context_menu())
        .close_icon(icon::from_name("media-eject-symbolic").size(16).icon());

        {
            // A bookmark accepts drops only if it is a directory this app can paste
            // into and differs from the visible tab's directory. Resolve it here
            // because the callback outlives this borrow of the model.
            let showing = self
                .tab_model
                .data::<Tab>(self.tab_model.active())
                .map(|tab| tab.location.clone());
            let droppable: Vec<Entity> = nav_model
                .iter()
                .filter(|entity| {
                    nav_model.data::<Location>(*entity).is_some_and(|location| {
                        location.supports_paste()
                            && location.path_opt().is_some()
                            && showing.as_ref() != Some(location)
                    })
                })
                .collect();
            nav = nav.on_file_drop(move |entity| {
                droppable
                    .contains(&entity)
                    .then(|| crate::ui::Action::App(Message::NavBarDrop(entity)))
            });
        }

        {
            nav = nav
                .window_id_maybe(self.core().main_window_id())
                .on_surface_action(|action| crate::ui::Action::Surface(action.flatten()))
        }

        let mut nav = nav.into_container();

        if !self.core.is_condensed() {
            nav = nav.max_width(widget::nav_bar::MAX_WIDTH);
        }

        Some(Element::from(
            nav.width(Length::Shrink).height(Length::Fill),
        ))
    }

    fn nav_context_menu(
        &self,
    ) -> Option<Vec<widget::menu::Tree<crate::ui::Action<Self::Message>>>> {
        let items = self.nav_model.iter().map(|entity| {
            let favorite_index_opt = self.nav_model.data::<FavoriteIndex>(entity);
            let location_opt = self.nav_model.data::<Location>(entity);

            let mut items: Vec<widget::menu::Item<NavMenuAction, String>> = Vec::with_capacity(7);

            if location_opt
                .and_then(Location::path_opt)
                .is_some_and(|x| x.is_file())
            {
                items.push(widget::menu::Item::Button(
                    fl!("open"),
                    None,
                    NavMenuAction::Open(entity),
                ));
                items.push(widget::menu::Item::Button(
                    fl!("menu-open-with"),
                    None,
                    NavMenuAction::OpenWith(entity),
                ));
            } else {
                items.push(widget::menu::Item::Button(
                    fl!("open-in-new-tab"),
                    None,
                    NavMenuAction::OpenInNewTab(entity),
                ));
                items.push(widget::menu::Item::Button(
                    fl!("open-in-new-window"),
                    None,
                    NavMenuAction::OpenInNewWindow(entity),
                ));
            }
            if let Some(path) = location_opt.and_then(Location::path_opt) {
                let selected_dir = usize::from(path.is_dir());
                let action_items: Vec<_> = self
                    .config
                    .context_actions
                    .iter()
                    .enumerate()
                    .filter(|(_, action)| action.matches_selection(1, selected_dir))
                    .map(|(i, action)| {
                        widget::menu::Item::Button(
                            action.name.clone(),
                            None,
                            NavMenuAction::RunContextAction(entity, i),
                        )
                    })
                    .collect();

                if !action_items.is_empty() {
                    items.push(widget::menu::Item::Divider);
                    items.extend(action_items);
                }
            }
            items.push(widget::menu::Item::Divider);
            if matches!(location_opt, Some(Location::Path(..))) {
                items.push(widget::menu::Item::Button(
                    fl!("show-details"),
                    None,
                    NavMenuAction::Preview(entity),
                ));
            }
            items.push(widget::menu::Item::Divider);
            if favorite_index_opt.is_some() {
                items.push(widget::menu::Item::Button(
                    fl!("change-sidebar-label"),
                    None,
                    NavMenuAction::ChangeSidebarLabel(entity),
                ));
                items.push(widget::menu::Item::Button(
                    fl!("remove-from-sidebar"),
                    None,
                    NavMenuAction::RemoveFromSidebar(entity),
                ));
            }

            if matches!(location_opt, Some(Location::Recents)) && tab::has_recents() {
                items.push(widget::menu::Item::Button(
                    fl!("clear-recents-history"),
                    None,
                    NavMenuAction::ClearRecents,
                ));
            }

            if matches!(location_opt, Some(Location::Trash)) && !Trash::is_empty() {
                items.push(widget::menu::Item::Button(
                    fl!("empty-trash"),
                    None,
                    NavMenuAction::EmptyTrash,
                ));
            }
            items
        });

        Some(widget::menu::nav_context(&HashMap::new(), items.collect()))
    }

    fn nav_model(&self) -> Option<&segmented_button::SingleSelectModel> {
        Some(&self.nav_model)
    }

    fn on_nav_bar_resized(&mut self, width: u16) -> Task<Self::Message> {
        self.config.nav_bar_width = Some(width);
        if let Err(err) = self.config_handler.save_in_background(&self.config) {
            log::warn!("failed to save config \"nav_bar_width\": {err}");
        }
        Task::none()
    }

    fn on_nav_select(&mut self, entity: Entity) -> Task<Self::Message> {
        self.nav_model.activate(entity);
        if let Some(location) = self.nav_model.data::<Location>(entity) {
            let should_open = match location {
                #[cfg(feature = "gvfs")]
                Location::Network(uri, name, Some(path))
                    if !path.try_exists().unwrap_or_default() =>
                {
                    let mut found = false;

                    if let Some(key) = self
                        .mounter_items
                        .iter()
                        .find_map(|(k, items)| {
                            items.iter().find_map(|item| {
                                found |= item.path().is_some_and(|p| path.starts_with(&p))
                                    || item.name() == *name
                                    || item.uri() == *uri;
                                (!item.is_mounted() && found).then_some(*k)
                            })
                        })
                        .or(if found {
                            None
                        } else {
                            self.mounter_items.keys().copied().next()
                        })
                        && let Some(mounter) = MOUNTERS.get(&key)
                    {
                        return mounter.network_drive(uri.clone()).map(move |mounted| {
                            if mounted {
                                crate::ui::Action::App(Message::NetworkDriveOpenEntityAfterMount {
                                    entity,
                                })
                            } else {
                                crate::ui::action::none()
                            }
                        });
                    }

                    log::warn!(
                        "failed to open favorite, path does not exist: {}",
                        path.display()
                    );
                    return self.push_dialog(
                        DialogPage::FavoritePathError {
                            path: path.clone(),
                            entity,
                        },
                        Some(FAVORITE_PATH_ERROR_REMOVE_BUTTON_ID.clone()),
                    );
                }
                Location::Path(path) | Location::Network(_, _, Some(path)) => {
                    match path.try_exists() {
                        Ok(true) => true,
                        Ok(false) => {
                            log::warn!(
                                "failed to open favorite, path does not exist: {}",
                                path.display()
                            );
                            return self.push_dialog(
                                DialogPage::FavoritePathError {
                                    path: path.clone(),
                                    entity,
                                },
                                Some(FAVORITE_PATH_ERROR_REMOVE_BUTTON_ID.clone()),
                            );
                        }
                        Err(err) => {
                            log::warn!(
                                "failed to open favorite for path: {}, {}",
                                path.display(),
                                err
                            );
                            return self.push_dialog(
                                DialogPage::FavoritePathError {
                                    path: path.clone(),
                                    entity,
                                },
                                Some(FAVORITE_PATH_ERROR_REMOVE_BUTTON_ID.clone()),
                            );
                        }
                    }
                }

                _ => true,
            };

            if should_open {
                let message = Message::TabMessage(None, tab::Message::Location(location.clone()));
                return self.update(message);
            }
        }
        if let Some(data) = self.nav_model.data::<MounterData>(entity)
            && let Some(mounter) = MOUNTERS.get(&data.0)
        {
            return mounter
                .mount(data.1.clone())
                .map(|()| crate::ui::action::none());
        }
        Task::none()
    }

    fn on_app_exit(&mut self) -> Option<Message> {
        Some(Message::WindowClose)
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Self::Message> {
        Some(Message::WindowCloseRequested(id))
    }

    fn on_context_drawer(&mut self) -> Task<Self::Message> {
        if let ContextPage::Preview(..) = self.context_page {
            // Persist state of preview page
            if self.core.window.show_context != self.config.show_details {
                return self.update(Message::Preview);
            }
        }
        Task::none()
    }

    fn on_escape(&mut self) -> Task<Self::Message> {
        let entity = self.tab_model.active();

        // Close dialog if open
        if let Some((_page, task)) = self.dialog_pages.pop_front() {
            return task;
        }

        // Close gallery mode if open
        if let Some(tab) = self.tab_model.data_mut::<Tab>(entity)
            && tab.gallery
        {
            tab.gallery = false;
            return Task::none();
        }

        // Close menus and context panes in order per message
        // Why: It'd be weird to close everything all at once
        // Usually, the Escape key (for example) closes menus and panes one by one instead
        // of closing everything on one press
        if self.core.window.show_context {
            self.set_show_context(false);
            return crate::ui::task::message(crate::ui::action::app(Message::SetShowDetails(
                false,
            )));
        }
        if let Some(tab) = self.tab_model.data_mut::<Tab>(entity) {
            if tab.edit_location.is_some() {
                tab.dismiss_edit_location();
                return Task::none();
            }

            let had_focused_button = tab.select_focus_id().is_some();
            if tab.select_none() {
                if had_focused_button {
                    // Unfocus if there was a focused button
                    return widget::button::focus(widget::Id::unique());
                }
                return Task::none();
            }
        }

        if self.search_get().is_some() {
            // Close search if open
            return self.search_set_active(None);
        }

        Task::none()
    }

    /// Handle application events here.
    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        // Helper for updating config values efficiently
        macro_rules! config_set {
            ($name: ident, $value: expr) => {
                self.config.$name = $value;
                if let Err(err) = self.config_handler.save_in_background(&self.config) {
                    log::warn!("failed to save config {:?}: {}", stringify!($name), err);
                }
            };
        }

        match message {
            Message::AddToSidebar(entity_opt) => {
                let mut favorites = self.config.favorites.clone();
                // check if the selected entity is in the current tab
                // else just use the selected entity and check its location
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());

                for path in self.selected_paths(entity_opt) {
                    let is_network = self.tab_model.data::<Tab>(entity).and_then(|tab| {
                        let in_current_tab = tab
                            .location
                            .path_opt()
                            .zip(path.parent())
                            .is_some_and(|(t_path, parent)| parent == t_path);
                        let tab = if in_current_tab {
                            self.tab_model
                                .data::<Tab>(self.tab_model.active())
                                .unwrap_or(tab)
                        } else {
                            tab
                        };

                        let name = Location::Path(path.clone()).title();
                        if let Location::Network(uri, _, _) = tab
                            .items_opt
                            .as_ref()
                            .and_then(|items| items.iter().find(|&i| i.path_opt() == Some(&path)))
                            .unwrap()
                            .location_opt
                            .as_ref()
                            .unwrap()
                        {
                            Some((uri.clone(), name, path.clone()))
                        } else {
                            None
                        }
                    });
                    let name = Location::Path(path.clone()).title();
                    let favorite = if let Some((uri, _, _)) = is_network.clone() {
                        Favorite::Network { uri, name, path }
                    } else {
                        Favorite::from_path(path)
                    };
                    let favorite_path = favorite.path_opt();
                    if !favorites.iter().any(|f| f.path_opt() == favorite_path) {
                        favorites.push(favorite);
                    }
                }
                config_set!(favorites, favorites);
                return self.update_config();
            }
            Message::AppTheme(app_theme) => {
                config_set!(app_theme, app_theme);
                return self.update_config();
            }
            Message::Compress(entity_opt) => {
                let paths: Box<[_]> = self.selected_paths(entity_opt).collect();
                if let Some(current_path) = paths.first()
                    && let Some(destination) = current_path.parent().zip(current_path.file_stem())
                {
                    let to = destination.0.to_path_buf();
                    let name = destination.1.to_str().unwrap_or_default().to_string();
                    let archive_type = ArchiveType::default();
                    return self.push_dialog(
                        DialogPage::Compress {
                            paths,
                            to,
                            name,
                            archive_type,
                            password: None,
                        },
                        Some(self.dialog_text_input.clone()),
                    );
                }
            }
            Message::Config(config) => {
                if config != self.config {
                    log::info!("update config");
                    // Show details is preserved for existing instances
                    let show_details = self.config.show_details;
                    self.config = config;
                    self.config.show_details = show_details;
                    return self.update_config();
                }
            }
            Message::Copy(entity_opt) => {
                if let Some(entity) = entity_opt
                    && let Some(tab) = self.tab_model.data_mut::<Tab>(entity)
                {
                    tab.refresh_cut(&[]);
                }
                let paths = self.selected_paths(entity_opt);
                self.clipboard_cache = ClipboardCache::Files(ClipboardPaste {
                    paths: paths.map(|p| p.to_path_buf()).collect(),
                    kind: ClipboardKind::Copy,
                });
                let contents =
                    ClipboardCopy::new(ClipboardKind::Copy, self.selected_paths(entity_opt));
                return clipboard::write_data(contents);
            }
            Message::CopyPath(entity_opt) => {
                let paths = self.selected_paths(entity_opt);
                let path_strings: Vec<String> =
                    paths.into_iter().map(|p| p.display().to_string()).collect();
                let text = path_strings.join("\n");
                return clipboard::write(text);
            }
            Message::CopyTo(entity_opt) => {
                let selected_paths: Box<[_]> = self.selected_paths(entity_opt).collect();
                return self.copy_to(&selected_paths);
            }
            Message::CopyToResult(result) => {
                match result {
                    DialogResult::Cancel => {}
                    DialogResult::Open(selected_paths) => {
                        let mut file_paths = None;
                        if let Some(file_dialog) = &self.file_dialog_opt
                            && let Some(window) = self.windows.remove(&file_dialog.window_id())
                            && let WindowKind::FileDialog(paths) = window.kind
                        {
                            file_paths = paths;
                        }
                        if let Some(file_paths) = file_paths
                            && !selected_paths.is_empty()
                        {
                            self.file_dialog_opt = None;
                            return self.operation(Operation::Copy {
                                paths: file_paths.to_vec(),
                                to: selected_paths[0].clone(),
                            });
                        }
                    }
                }
                self.file_dialog_opt = None;
            }
            Message::Cut(entity_opt) => {
                self.set_cut(entity_opt);
                let paths = self.selected_paths(entity_opt);
                self.clipboard_cache = ClipboardCache::Files(ClipboardPaste {
                    paths: paths.map(|p| p.to_path_buf()).collect(),
                    kind: ClipboardKind::Cut,
                });
                let contents =
                    ClipboardCopy::new(ClipboardKind::Cut, self.selected_paths(entity_opt));

                return clipboard::write_data(contents);
            }
            Message::CloseToast(id) => {
                self.toasts.remove(id);
            }
            Message::CosmicSettings(arg) => {
                let mut command = process::Command::new("cosmic-settings");
                command.arg(arg);
                match spawn_detached(&mut command) {
                    Ok(()) => {}
                    Err(err) => {
                        log::warn!("failed to run cosmic-settings {arg}: {err}");
                    }
                }
            }
            Message::Delete(entity_opt) => {
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                if let Some(tab) = self.tab_model.data::<Tab>(entity) {
                    if tab.location.is_trash() {
                        if let Some(items) = tab.items_opt() {
                            let mut trash_items = Vec::new();
                            for item in items {
                                if item.selected
                                    && let ItemMetadata::Trash { entry, .. } = &item.metadata
                                {
                                    trash_items.push(entry.clone());
                                }
                            }
                            if !trash_items.is_empty() {
                                return self.update(Message::DialogPush(
                                    DialogPage::DeleteTrash { items: trash_items },
                                    Some(DELETE_TRASH_BUTTON_ID.clone()),
                                ));
                            }
                        }
                    } else {
                        let paths: Box<[_]> = self.selected_paths(entity_opt).collect();
                        if !paths.is_empty() {
                            return self.delete(paths);
                        }
                    }
                }
            }
            // Intentionally a no-op: this handler was desktop-mode-only and desktop mode
            // is gone, but DialogPages still emits this message.
            Message::DesktopDialogs(_show) => {}
            Message::DialogCancel => {
                if let Some((_page, task)) = self.dialog_pages.pop_front() {
                    return task;
                }
            }
            Message::DialogComplete => {
                if let Some((dialog_page, task)) = self.dialog_pages.pop_front() {
                    let mut tasks = vec![task];
                    match dialog_page {
                        // Nothing to complete yet: the type is still being
                        // read, and the page offers only Cancel.
                        DialogPage::OpenWithLoading { .. } => {}
                        DialogPage::Compress {
                            paths,
                            to,
                            name,
                            archive_type,
                            password,
                        } => {
                            let extension = archive_type.extension();
                            let name = format!("{name}{extension}");
                            let to = to.join(name);
                            tasks.push(self.operation(Operation::Compress {
                                paths: paths.into_vec(),
                                to,
                                archive_type,
                                password,
                            }));
                        }
                        DialogPage::EmptyTrash => {
                            tasks.push(self.operation(Operation::EmptyTrash));
                        }
                        DialogPage::FailedOperation(id) => {
                            if let Some((operation, _, _)) = self.failed_operations.remove(&id) {
                                tasks.push(self.operation(operation));
                            }
                        }
                        DialogPage::FailedOperations(ids) => {
                            for id in ids {
                                if let Some((operation, _, _)) = self.failed_operations.remove(&id)
                                {
                                    tasks.push(self.operation(operation));
                                }
                            }
                        }
                        DialogPage::ExtractPassword { id, password } => {
                            let (operation, _, _err) = self.failed_operations.get(&id).unwrap();
                            let new_op = match &operation {
                                Operation::Extract {
                                    to,
                                    paths,
                                    as_folder,
                                    ..
                                } => Operation::Extract {
                                    to: to.clone(),
                                    paths: paths.clone(),
                                    password: Some(password),
                                    as_folder: *as_folder,
                                },
                                _ => unreachable!(),
                            };
                            tasks.push(self.operation(new_op));
                        }
                        DialogPage::MountError {
                            mounter_key,
                            item,
                            error: _,
                        } => {
                            if let Some(mounter) = MOUNTERS.get(&mounter_key) {
                                tasks.push(mounter.mount(item).map(|()| crate::ui::action::none()));
                            }
                        }
                        DialogPage::NetworkAuth {
                            mounter_key: _,
                            uri: _,
                            auth,
                            auth_tx,
                        } => {
                            tasks.push(Task::future(async move {
                                // The mount can be torn down while the dialog
                                // is open, which closes the receiver
                                if let Err(err) = auth_tx.send(auth).await {
                                    log::warn!("failed to send mount credentials: {err}");
                                }
                                crate::ui::action::none()
                            }));
                        }
                        DialogPage::NetworkError {
                            mounter_key: _,
                            uri,
                            error: _,
                        } => {
                            tasks.push(self.update(Message::NetworkDriveInput(uri)));
                            tasks.push(self.update(Message::NetworkDriveSubmit));
                        }
                        DialogPage::NewItem { parent, name, dir } => {
                            let path = parent.join(name);
                            tasks.push(self.operation(if dir {
                                Operation::NewFolder { path }
                            } else {
                                Operation::NewFile { path }
                            }));
                        }
                        DialogPage::RunContextAction { action, paths } => {
                            context_action::run(&self.config.context_actions, action, &paths);
                        }
                        DialogPage::OpenWith {
                            path,
                            mime,
                            selected,
                            search_app_name,
                            ..
                        } => {
                            let mut available_apps =
                                self.mime_app_cache.get_apps_for_mime(&mime, true);
                            available_apps.retain(|(app, _)| {
                                app.name
                                    .to_lowercase()
                                    .trim()
                                    .contains(search_app_name.to_lowercase().as_str().trim())
                            });

                            if let Some((app, _)) = available_apps.get(selected) {
                                if let Some(mut command) =
                                    app.command(&[&path]).and_then(|v| v.into_iter().next())
                                {
                                    match spawn_detached(&mut command) {
                                        Ok(()) => {
                                            if self.config.show_recents {
                                                crate::recents::record(
                                                    path.clone(),
                                                    Self::APP_ID.to_string(),
                                                    "earth-files".to_string(),
                                                );
                                            }
                                        }
                                        Err(err) => {
                                            log::warn!(
                                                "failed to open {} with {:?}: {}",
                                                path.display(),
                                                app.id,
                                                err
                                            );
                                        }
                                    }
                                } else {
                                    log::warn!(
                                        "failed to open {} with {:?}: failed to get command",
                                        path.display(),
                                        app.id
                                    );
                                }
                            }
                        }
                        DialogPage::PermanentlyDelete { paths } => {
                            tasks.push(self.operation(Operation::PermanentlyDelete { paths }));
                        }
                        DialogPage::DeleteTrash { items } => {
                            tasks.push(self.operation(Operation::DeleteTrash { items }));
                        }
                        DialogPage::ChangeSidebarLabel { entity, label } => {
                            if let Some(FavoriteIndex(favorite_i)) =
                                self.nav_model.data::<FavoriteIndex>(entity)
                            {
                                let mut favorites = self.config.favorites.clone();
                                if let Some(favorite) = favorites.get_mut(*favorite_i) {
                                    *favorite = favorite.with_label(label.trim());
                                    config_set!(favorites, favorites);
                                    tasks.push(self.update_config());
                                }
                            }
                        }
                        DialogPage::RenameItem {
                            from, parent, name, ..
                        } => {
                            let to = parent.join(name);
                            tasks.push(self.operation(Operation::Rename { from, to }));
                        }
                        DialogPage::BatchRename { parent, .. } => {
                            let preview = std::mem::take(&mut self.batch_rename_preview);
                            if preview.ready() {
                                let renames = preview
                                    .rows
                                    .iter()
                                    .filter(|row| row.old != row.new)
                                    .map(|row| (parent.join(&row.old), parent.join(&row.new)))
                                    .collect();
                                tasks.push(self.operation(Operation::BatchRename { renames }));
                            }
                        }
                        DialogPage::Replace { .. } => {
                            log::warn!("replace dialog should be completed with replace result");
                        }
                        DialogPage::SetExecutableAndLaunch { path } => {
                            tasks.push(self.operation(Operation::SetExecutableAndLaunch { path }));
                        }
                        DialogPage::LaunchDesktopEntry { path, .. } => {
                            #[cfg(feature = "desktop")]
                            Self::launch_desktop_entries(&[path]);
                            #[cfg(not(feature = "desktop"))]
                            let _ = path;
                        }
                        DialogPage::FavoritePathError { entity, .. } => {
                            if let Some(FavoriteIndex(favorite_i)) =
                                self.nav_model.data::<FavoriteIndex>(entity)
                            {
                                let mut favorites = self.config.favorites.clone();
                                favorites.remove(*favorite_i);
                                config_set!(favorites, favorites);
                                tasks.push(self.update_config());
                            }
                        }
                    }
                    return Task::batch(tasks);
                }
            }
            Message::DialogPush(dialog_page, focused_id) => {
                return self.push_dialog(dialog_page, focused_id);
            }
            Message::DialogUpdate(dialog_page) => {
                let batch_rename = matches!(dialog_page, DialogPage::BatchRename { .. });
                let name_target = match &dialog_page {
                    DialogPage::NewItem { parent, name, .. } if !name.is_empty() => {
                        Some(parent.join(name))
                    }
                    DialogPage::RenameItem { parent, name, .. } if !name.is_empty() => {
                        Some(parent.join(name))
                    }
                    _ => None,
                };
                self.dialog_pages.update_front(dialog_page);
                if batch_rename {
                    return self.refresh_batch_rename_preview();
                }
                if let Some(path) = name_target {
                    return self.check_name(path);
                }
            }
            Message::BatchRenamePreview(revision, preview) => {
                // Anything older describes names the user has moved on from.
                if revision == self.batch_rename_revision {
                    self.batch_rename_preview = preview;
                }
            }
            Message::DefaultTerminal(id) => {
                self.mime_app_cache.adopt_terminal(id);
            }
            Message::DefaultTerminalThenOpen(id, paths) => {
                self.mime_app_cache.adopt_terminal(id);
                return self.update(Message::OpenTerminalIn(paths));
            }
            Message::NameChecked(check) => {
                self.name_check = Some(check);
            }
            Message::DialogUpdateComplete(dialog_page) => {
                return Task::batch([
                    self.update(Message::DialogUpdate(dialog_page)),
                    self.update(Message::DialogComplete),
                ]);
            }
            Message::ExtractAsFolder(entity_opt) => {
                return self.extract_here(entity_opt, true);
            }
            Message::ExtractHere(entity_opt) => {
                return self.extract_here(entity_opt, false);
            }
            Message::ExtractTo(entity_opt) => {
                let selected_paths: Box<[_]> = self.selected_paths(entity_opt).collect();
                return self.extract_to(&selected_paths);
            }
            Message::ExtractToResult(result) => {
                match result {
                    DialogResult::Cancel => {}
                    DialogResult::Open(selected_paths) => {
                        let mut archive_paths = None;
                        if let Some(file_dialog) = &self.file_dialog_opt
                            && let Some(window) = self.windows.remove(&file_dialog.window_id())
                            && let WindowKind::FileDialog(paths) = window.kind
                        {
                            archive_paths = paths;
                        }
                        if let Some(archive_paths) = archive_paths
                            && !selected_paths.is_empty()
                        {
                            self.file_dialog_opt = None;
                            return self.operation(Operation::Extract {
                                paths: archive_paths,
                                to: selected_paths[0].clone(),
                                password: None,
                                as_folder: false,
                            });
                        }
                    }
                }
                self.file_dialog_opt = None;
            }
            Message::FileDialogMessage(dialog_message) => {
                if let Some(dialog) = &mut self.file_dialog_opt {
                    return dialog.update(dialog_message);
                }
            }
            Message::Key(window_id, modifiers, key, physical_key, text) => {
                if self.core.main_window_id() == Some(window_id) {
                    let entity = self.tab_model.active();
                    for (key_bind, action) in &self.key_binds {
                        if key_bind.matches(modifiers, &key, Some(&physical_key)) {
                            return self.update(action.message(Some(entity)));
                        }
                    }

                    // Uncaptured keys with only shift modifiers go to the search or location box
                    if !modifiers.logo()
                        && !modifiers.control()
                        && !modifiers.alt()
                        && matches!(key, Key::Character(_))
                        && let Some(text) = text
                    {
                        match self.config.type_to_search {
                            TypeToSearch::Recursive => {
                                let mut term = self.search_get().unwrap_or_default().to_string();
                                term.push_str(&text);
                                return self.search_set_active(Some(term));
                            }
                            TypeToSearch::EnterPath => {
                                if let Some(tab) = self.tab_model.data_mut::<Tab>(entity) {
                                    let location = tab
                                        .edit_location
                                        .as_ref()
                                        .map_or_else(|| &tab.location, |x| &x.location);
                                    // Try to add text to end of location
                                    if let Location::Network(uri, ..) = location {
                                        let mut uri_string = uri.clone();
                                        uri_string.push_str(&text);
                                        tab.edit_location =
                                            Some(location.with_uri(uri_string).into());
                                    } else if let Some(path) = location.path_opt() {
                                        let mut path_string = path.to_string_lossy().into_owned();
                                        path_string.push_str(&text);
                                        tab.edit_location =
                                            Some(location.with_path(path_string.into()).into());
                                    }
                                }
                            }
                            TypeToSearch::SelectByPrefix => {
                                // Reset buffer if timeout elapsed
                                if let Some(last_key) = self.type_select_last_key
                                    && last_key.elapsed() >= tab::TYPE_SELECT_TIMEOUT
                                {
                                    self.type_select_prefix.clear();
                                }

                                // Accumulate character and select
                                self.type_select_prefix.push_str(&text.to_lowercase());
                                self.type_select_last_key = Some(Instant::now());

                                if let Some(tab) = self.tab_model.data_mut::<Tab>(entity) {
                                    tab.select_by_prefix(&self.type_select_prefix);
                                    if let Some(offset) = tab.select_focus_scroll() {
                                        return scrollable::scroll_to(
                                            tab.scrollable_id.clone(),
                                            AbsoluteOffset {
                                                x: Some(offset.x),
                                                y: Some(offset.y),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Message::MaybeExit => {
                if self.core.main_window_id().is_none() && self.pending_operations.is_empty() {
                    // Exit if window is closed and there are no pending operations.
                    // Settings and recents are written by worker threads, and
                    // `process::exit` runs nothing on the way out, so what is
                    // still queued has to be waited for here rather than after
                    // the loop returns -- which this never lets happen.
                    crate::shut_down();
                    process::exit(0);
                }
            }
            Message::LaunchUrl(url) => match open::that_detached(&url) {
                Ok(()) => {}
                Err(err) => {
                    log::warn!("failed to open {url:?}: {err}");
                }
            },
            Message::ModifiersChanged(window_id, modifiers) => {
                if self.core.main_window_id() == Some(window_id) {
                    self.modifiers = modifiers;
                }
                if let Some(window) = self.windows.get_mut(&window_id) {
                    window.modifiers = modifiers;
                }
            }
            Message::MounterItems(mounter_key, mounter_items) => {
                // Check for unmounted folders
                let mut unmounted = Vec::new();
                if let Some(old_items) = self.mounter_items.get(&mounter_key) {
                    for old_item in old_items {
                        if let Some(old_path) = old_item.path()
                            && old_item.is_mounted()
                        {
                            let mut still_mounted = false;
                            for item in &mounter_items {
                                if let Some(path) = item.path()
                                    && path == old_path
                                    && item.is_mounted()
                                {
                                    still_mounted = true;
                                    break;
                                }
                            }
                            if !still_mounted {
                                unmounted.push(old_path);
                            }
                        }
                    }
                }

                // Go back to home in any tabs that were unmounted
                let mut commands = Vec::new();
                {
                    let home_location = Location::Path(home_dir());
                    let entities: Box<[_]> = self.tab_model.iter().collect();
                    for entity in entities {
                        let title_opt = self.tab_model.data_mut::<Tab>(entity).and_then(|tab| {
                            unmounted
                                .iter()
                                .any(|unmounted| {
                                    tab.location
                                        .path_opt()
                                        .is_some_and(|location| location.starts_with(unmounted))
                                })
                                .then(|| {
                                    tab.change_location(&home_location, None);
                                    tab.title()
                                })
                        });
                        if let Some(title) = title_opt {
                            self.tab_model.text_set(entity, title);
                            commands.push(self.update_tab(entity, home_location.clone(), None));
                        }
                    }
                    if !commands.is_empty() {
                        commands.push(self.update_title());
                        commands.push(self.update_watcher());
                    }
                }

                // Insert new items
                self.mounter_items.insert(mounter_key, mounter_items);

                // Update nav bar
                self.update_nav_model();

                return Task::batch(commands);
            }
            Message::MountResult(mounter_key, item, res) => match res {
                Ok(true) => {
                    log::info!("connected to {item:?}");
                    // Automatically navigate to the mounted location
                    if let Some(path) = item.path() {
                        let location = if item.is_remote() {
                            Location::Network(item.uri(), item.name(), Some(path))
                        } else {
                            Location::Path(path)
                        };
                        let message = Message::TabMessage(None, tab::Message::Location(location));
                        return self.update(message);
                    }
                }
                Ok(false) => {
                    log::info!("cancelled connection to {item:?}");
                }
                Err(error) => {
                    log::warn!("failed to connect to {item:?}: {error}");
                    return self.push_dialog(
                        DialogPage::MountError {
                            mounter_key,
                            item,
                            error,
                        },
                        Some(MOUNT_ERROR_TRY_AGAIN_BUTTON_ID.clone()),
                    );
                }
            },
            Message::MoveTo(entity_opt) => {
                let selected_paths: Box<[_]> = self.selected_paths(entity_opt).collect();
                return self.move_to(&selected_paths);
            }
            Message::MoveToResult(result) => {
                match result {
                    DialogResult::Cancel => {}
                    DialogResult::Open(selected_paths) => {
                        let mut file_paths = None;
                        if let Some(file_dialog) = &self.file_dialog_opt
                            && let Some(window) = self.windows.remove(&file_dialog.window_id())
                            && let WindowKind::FileDialog(paths) = window.kind
                        {
                            file_paths = paths;
                        }
                        if let Some(file_paths) = file_paths
                            && !selected_paths.is_empty()
                        {
                            self.file_dialog_opt = None;
                            return self.operation(Operation::Move {
                                paths: file_paths.to_vec(),
                                to: selected_paths[0].clone(),
                                cross_device_copy: false,
                            });
                        }
                    }
                }
                self.file_dialog_opt = None;
            }
            Message::NetworkAuth(mounter_key, uri, auth, auth_tx) => {
                self.auth_password_visible = false;
                return self.push_dialog(
                    DialogPage::NetworkAuth {
                        mounter_key,
                        uri,
                        auth,
                        auth_tx,
                    },
                    Some(self.dialog_text_input.clone()),
                );
            }
            Message::NetworkDriveInput(input) => {
                self.network_drive_input = input;
            }
            Message::NetworkDriveSubmit => {
                if let Some((mounter_key, mounter)) = MOUNTERS.iter().next() {
                    self.network_drive_connecting =
                        Some((*mounter_key, self.network_drive_input.clone()));
                    return mounter
                        .network_drive(self.network_drive_input.clone())
                        .map(|_| crate::ui::action::none());
                }
                log::warn!(
                    "no mounter found for connecting to {:?}",
                    self.network_drive_input
                );
            }
            Message::NetworkResult(mounter_key, uri, res) => {
                if self
                    .network_drive_connecting
                    .as_ref()
                    .is_some_and(|(m, u)| *m == mounter_key && *u == uri)
                {
                    self.network_drive_connecting = None;
                }
                match res {
                    Ok(true) => {
                        log::info!("connected to {uri:?}");
                        if matches!(self.context_page, ContextPage::NetworkDrive) {
                            self.set_show_context(false);
                        }
                    }
                    Ok(false) => {
                        log::info!("cancelled connection to {uri:?}");
                    }
                    Err(error) => {
                        log::warn!("failed to connect to {uri:?}: {error}");
                        return self.dialog_pages.push_back(DialogPage::NetworkError {
                            mounter_key,
                            uri,
                            error,
                        });
                    }
                }
            }
            Message::NewItem(entity_opt, dir) => {
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                if let Some(tab) = self.tab_model.data_mut::<Tab>(entity)
                    && let Some(path) = tab.location.path_opt()
                {
                    return Task::batch([
                        self.dialog_pages.push_back(DialogPage::NewItem {
                            parent: path.clone(),
                            name: String::new(),
                            dir,
                        }),
                        widget::text_input::focus(self.dialog_text_input.clone()),
                    ]);
                }
            }
            #[cfg(feature = "notify")]
            Message::Notification(notification) => {
                self.notification_opt = Some(notification);
            }
            Message::NotifyEvents(events) => {
                log::debug!("{events:?}");

                let mut needs_reload = Vec::new();
                let mut warm: Vec<(Entity, tab::IconWarmup, crate::config::IconSizes)> = Vec::new();
                // Paths to re-read, per tab, deduplicated. A burst -- an
                // archive being unpacked, a program rewriting one file over
                // and over -- names the same path many times, and re-reading
                // it once per mention is work with nothing to show for it.
                let mut refresh: Vec<(Entity, Location, crate::config::IconSizes, Vec<PathBuf>)> =
                    Vec::new();
                let entities: Box<[_]> = self.tab_model.iter().collect();
                for entity in entities {
                    if let Some(tab) = self.tab_model.data_mut::<Tab>(entity)
                        && let Some(path) = tab.location.path_opt()
                    {
                        let mut contains_change = false;
                        let mut changed: Vec<PathBuf> = Vec::new();
                        for event in &events {
                            for event_path in &event.paths {
                                if event_path.starts_with(path) {
                                    if let notify::EventKind::Modify(
                                        notify::event::ModifyKind::Metadata(_)
                                        | notify::event::ModifyKind::Data(_),
                                    ) = event.kind
                                    {
                                        // Metadata or data changed, so the
                                        // matching item is out of date. Note
                                        // it; re-reading it is disk work and
                                        // happens on a worker.
                                        let known = tab.items_opt.as_ref().is_some_and(|items| {
                                            items.iter().any(|item| {
                                                item.path_opt() == Some(event_path)
                                                    && matches!(
                                                        item.metadata,
                                                        ItemMetadata::Path { .. }
                                                    )
                                            })
                                        });
                                        if known && !changed.contains(event_path) {
                                            changed.push(event_path.clone());
                                        }
                                    } else {
                                        // Any other events reload the whole tab
                                        contains_change = true;
                                        break;
                                    }
                                }
                            }
                        }
                        // Icons still standing in as placeholders from an
                        // earlier scan of this tab. Items being re-read below
                        // are warmed after they are adopted instead, because
                        // until then the tab still holds their old types.
                        let sizes = tab.config.icon_sizes;
                        let warmup = tab.refresh_icons(sizes);
                        if !warmup.is_empty() {
                            warm.push((entity, warmup, sizes));
                        }
                        if contains_change {
                            needs_reload.push((entity, tab.location.clone()));
                        } else if !changed.is_empty() {
                            refresh.push((entity, tab.location.clone(), sizes, changed));
                        }
                    }
                }

                let mut commands: Vec<Task<Message>> = needs_reload
                    .into_iter()
                    .map(|(entity, location)| self.update_tab(entity, location, None))
                    .collect();
                for (entity, location, sizes, paths) in refresh {
                    let Some((listing, batch)) = self
                        .tab_model
                        .data_mut::<Tab>(entity)
                        .map(|tab| (tab.listing(), tab.next_refresh_batch()))
                    else {
                        continue;
                    };
                    commands.push(Task::future(async move {
                        let rebuilt = tokio::task::spawn_blocking(move || {
                            paths
                                .into_iter()
                                .filter_map(|path| match tab::item_from_path(&path, sizes) {
                                    Ok(item) => Some((path, Box::new(item))),
                                    Err(err) => {
                                        log::warn!("failed to reload {}: {err}", path.display());
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                        })
                        .await;

                        // Answered even when it found nothing, so the count of
                        // batches still out stays honest.
                        let items = rebuilt.unwrap_or_else(|err| {
                            log::warn!("failed to reload changed items: {err}");
                            Vec::new()
                        });
                        crate::ui::action::app(Message::RefreshedItems(
                            entity, location, listing, batch, items,
                        ))
                    }));
                }
                for (entity, warmup, sizes) in warm {
                    commands.push(Task::future(async move {
                        if tokio::task::spawn_blocking(move || tab::warm_icons(&warmup, sizes))
                            .await
                            .is_err()
                        {
                            return crate::ui::action::none();
                        }
                        crate::ui::action::app(Message::TabMessage(
                            Some(entity),
                            tab::Message::IconsReady,
                        ))
                    }));
                }
                return Task::batch(commands);
            }
            Message::NotifyWatcher(mut watcher_wrapper) => match watcher_wrapper.watcher_opt.take()
            {
                Some(watcher) => {
                    self.watcher_opt = Some((watcher, FxHashSet::default()));
                    return self.update_watcher();
                }
                None => {
                    log::warn!("message did not contain notify watcher");
                }
            },
            Message::OpenTerminal(entity_opt) => {
                // Which folders to open is decided now, from the selection as
                // it is at the moment the user asks. Everything below may have
                // to wait for the mime app cache, and the selection can change
                // while it does -- opening whatever happens to be selected
                // when the wait ends is not what was asked for.
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                let mut paths: Box<[PathBuf]> = Box::from([]);
                if let Some(tab) = self.tab_model.data::<Tab>(entity)
                    && let Some(path) = tab.location.path_opt()
                {
                    if let Some(items) = tab.items_opt() {
                        paths = items
                            .iter()
                            .filter_map(|item| {
                                item.selected.then(|| item.path_opt().cloned()).flatten()
                            })
                            .collect();
                    }
                    if paths.is_empty() {
                        paths = Box::from([path.clone()]);
                    }
                }
                return self.update(Message::OpenTerminalIn(paths));
            }
            Message::OpenTerminalIn(paths) => {
                if self.defer_until_mime_apps(Message::OpenTerminalIn(paths.clone())) {
                    return Task::none();
                }
                if !self.mime_app_cache.terminal_known() {
                    // The lookup started at startup has not come back yet.
                    // Rather than run it here -- a whole process, with a two
                    // second deadline, on the event loop -- ask for it and
                    // come back to this once the answer is in.
                    return Task::future(async move {
                        let id = tokio::task::spawn_blocking(MimeAppCache::query_default_terminal)
                            .await
                            .unwrap_or_else(|err| {
                                log::warn!("failed to look up the default terminal: {err}");
                                None
                            });
                        crate::ui::action::app(Message::DefaultTerminalThenOpen(id, paths))
                    });
                }
                if let Some(terminal) = self.mime_app_cache.terminal() {
                    for path in &paths {
                        if let Some(mut command) = terminal
                            .command::<&str>(&[])
                            .and_then(|v| v.into_iter().next())
                        {
                            command.current_dir(path);
                            if let Err(err) = spawn_detached(&mut command) {
                                log::warn!(
                                    "failed to open {} with terminal {:?}: {}",
                                    path.display(),
                                    terminal.id,
                                    err
                                );
                            }
                        } else {
                            log::warn!("failed to get command for {:?}", terminal.id);
                        }
                    }
                }
            }
            Message::OpenInNewTab(entity_opt) => {
                let selected_paths: Box<[_]> = self
                    .selected_paths(entity_opt)
                    .filter(|p| p.is_dir())
                    .collect();
                return Task::batch(
                    selected_paths
                        .into_iter()
                        .map(|path| self.open_tab(Location::Path(path), false, None)),
                );
            }
            Message::OpenInNewWindow(entity_opt) => match env::current_exe() {
                Ok(exe) => self
                    .selected_paths(entity_opt)
                    .filter(|p| p.is_dir())
                    .for_each(|path| match process::Command::new(&exe).arg(path).spawn() {
                        Ok(_child) => {}
                        Err(err) => {
                            log::error!("failed to execute {}: {}", exe.display(), err);
                        }
                    }),
                Err(err) => {
                    log::error!("failed to get current executable path: {err}");
                }
            },
            Message::OpenItemLocation(entity_opt) => {
                let selected_paths: Box<[_]> = self.selected_paths(entity_opt).collect();
                return Task::batch(selected_paths.into_iter().filter_map(|path| {
                    path.parent()
                        .map(Path::to_path_buf)
                        .map(|parent| self.open_tab(Location::Path(parent), true, Some(vec![path])))
                }));
            }
            Message::OpenWithBrowse => match self.dialog_pages.pop_front() {
                Some((
                    DialogPage::OpenWith {
                        mime,
                        store_opt: Some(app),
                        ..
                    },
                    task,
                )) => {
                    let url = format!("mime:///{mime}");
                    if let Some(mut command) =
                        app.command(&[&url]).and_then(|v| v.into_iter().next())
                    {
                        if let Err(err) = spawn_detached(&mut command) {
                            log::warn!("failed to open {:?} with {:?}: {}", url, app.id, err);
                        }
                    } else {
                        log::warn!(
                            "failed to open {:?} with {:?}: failed to get command",
                            url,
                            app.id
                        );
                    }
                    return task;
                }
                Some((dialog_page, task)) => {
                    log::warn!("tried to open with browse from the wrong dialog");
                    return Task::batch([task, self.dialog_pages.push_front(dialog_page)]);
                }
                None => {}
            },
            Message::OpenFiles(paths) => {
                return self.open_file(&paths);
            }
            Message::OpenFilesResolved(request, resolved) => {
                // A cancelled request has left the map, and its files stay
                // closed however late its answer is.
                let Some(toast) = self.opening.remove(&request) else {
                    return Task::none();
                };
                if let Some(id) = toast {
                    self.toasts.remove(id);
                }
                return self.open_resolved(resolved);
            }
            Message::OpeningStillRunning(request) => {
                if let Some(slot) = self.opening.get_mut(&request)
                    && slot.is_none()
                {
                    let toast = widget::toaster::Toast::new(fl!("opening-files"))
                        .action(fl!("cancel"), move |_| Message::CancelOpening(request))
                        .duration(widget::toaster::Duration::Custom(OPENING_TOAST_BACKSTOP));
                    let (id, expiry) = self.toasts.push_with_id(toast);
                    *slot = Some(id);
                    return expiry.map(crate::ui::action::app);
                }
            }
            Message::CancelOpening(request) => {
                // Discard, not interrupt: the worker finishes reading the
                // types regardless, and what it finds is thrown away.
                if let Some(Some(id)) = self.opening.remove(&request) {
                    self.toasts.remove(id);
                }
            }
            Message::OpenWithDialog(entity_opt) => {
                // Which file the dialog is for is decided now, from the
                // selection as it is at the moment the user asks. Opening the
                // dialog may have to wait for the mime app cache, and the
                // selection can change while it does.
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                let chosen = self
                    .tab_model
                    .data::<Tab>(entity)
                    .and_then(Tab::items_opt)
                    .and_then(|items| {
                        items.iter().filter(|item| item.selected).find_map(|item| {
                            item.path_opt()
                                .map(|path| (path.clone(), item.mime.clone()))
                        })
                    });
                if let Some((path, mime)) = chosen {
                    return self.update(Message::OpenWithFor(path, mime));
                }
            }
            Message::OpenWithResolved(path, resolved) => {
                // Only for a loading page still up for this path. Cancelled,
                // or moved on from, and the answer is nobody's.
                let waiting = matches!(
                    self.dialog_pages.front(),
                    Some(DialogPage::OpenWithLoading { path: shown }) if *shown == path
                );
                if !waiting {
                    return Task::none();
                }
                match resolved {
                    Ok(mime) => return self.update(Message::OpenWithFor(path, mime)),
                    Err(err) => {
                        log::warn!("failed to get item for path {}: {}", path.display(), err);
                        if let Some((_, task)) = self.dialog_pages.pop_front() {
                            return task;
                        }
                    }
                }
            }
            Message::OpenWithFor(path, mime) => {
                if self.defer_until_mime_apps(Message::OpenWithFor(path.clone(), mime.clone())) {
                    return Task::none();
                }
                let page = DialogPage::OpenWith {
                    path: path.clone(),
                    mime,
                    selected: 0,
                    store_opt: "x-scheme-handler/mime"
                        .parse::<mime_guess::Mime>()
                        .ok()
                        .and_then(|mime| self.mime_app_cache.get(&mime).first().cloned()),
                    search_app_name: String::new(),
                };
                // A loading page for this path becomes the real one in place;
                // otherwise this is a fresh dialog.
                let shown = if matches!(
                    self.dialog_pages.front(),
                    Some(DialogPage::OpenWithLoading { path: shown }) if *shown == path
                ) {
                    self.dialog_pages.update_front(page);
                    Task::none()
                } else {
                    self.push_dialog(page, Some(CONFIRM_OPEN_WITH_BUTTON_ID.clone()))
                };
                return Task::batch([
                    shown,
                    widget::text_input::focus(self.dialog_text_input.clone()),
                ]);
            }
            Message::OpenWithSelection(index) => {
                if let Some(DialogPage::OpenWith { selected, .. }) = self.dialog_pages.front_mut() {
                    *selected = index;
                }
            }
            Message::OpenWithSearchClear => {
                if let Some(DialogPage::OpenWith {
                    search_app_name, ..
                }) = self.dialog_pages.front_mut()
                {
                    *search_app_name = String::new();
                }
            }
            Message::Paste(entity_opt) => {
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                if let Some(tab) = self.tab_model.data_mut::<Tab>(entity)
                    && let Some(path) = tab.location.path_opt()
                {
                    let to = path.clone();

                    // Use cached clipboard data if available (needed for Wayland popups)
                    match &self.clipboard_cache {
                        ClipboardCache::Files(contents) => {
                            if contents.paths.is_empty() {
                                return iced::Task::future(tokio::time::sleep(
                                    std::time::Duration::from_millis(300),
                                ))
                                .discard()
                                .chain(
                                    clipboard::read_data::<ClipboardPaste>().map(
                                        move |contents_opt| match contents_opt {
                                            Some(contents) => crate::ui::action::app(
                                                Message::PasteContents(to.clone(), contents),
                                            ),
                                            None => crate::ui::action::app(Message::PasteImage(
                                                to.clone(),
                                            )),
                                        },
                                    ),
                                );
                            }
                            return self
                                .update(Message::PasteContents(to.clone(), contents.clone()));
                        }
                        ClipboardCache::Image(contents) => {
                            return self
                                .update(Message::PasteImageContents(to.clone(), contents.clone()));
                        }
                        ClipboardCache::Video(contents) => {
                            return self
                                .update(Message::PasteVideoContents(to.clone(), contents.clone()));
                        }
                        ClipboardCache::Text(contents) => {
                            return self
                                .update(Message::PasteTextContents(to.clone(), contents.clone()));
                        }
                        ClipboardCache::Empty => {
                            // Cache is empty, try reading from clipboard directly
                            // (works when triggered from main window, e.g., Ctrl+V)
                            return clipboard::read_data::<ClipboardPaste>().map(
                                move |contents_opt| match contents_opt {
                                    Some(contents) => crate::ui::action::app(
                                        Message::PasteContents(to.clone(), contents),
                                    ),
                                    None => crate::ui::action::app(Message::PasteImage(to.clone())),
                                },
                            );
                        }
                    }
                }
            }
            Message::PasteContents(to, mut contents) => {
                contents.paths.retain(|p| *p != to);
                if !contents.paths.is_empty() {
                    return match contents.kind {
                        ClipboardKind::Copy => self.operation(Operation::Copy {
                            paths: contents.paths,
                            to,
                        }),
                        ClipboardKind::Cut => self.operation(Operation::Move {
                            paths: contents.paths,
                            to,
                            cross_device_copy: false,
                        }),
                    };
                }
            }
            Message::PasteImage(to) => {
                return clipboard::read_data::<ClipboardPasteImage>().map(move |contents_opt| {
                    match contents_opt {
                        Some(contents) => crate::ui::action::app(Message::PasteImageContents(
                            to.clone(),
                            contents,
                        )),
                        // No image data in clipboard, try video data
                        None => crate::ui::action::app(Message::PasteVideo(to.clone())),
                    }
                });
            }
            Message::PasteImageContents(to, contents) => {
                let Some(extension) = contents.extension() else {
                    log::warn!(
                        "Ignoring paste: unknown image MIME type {:?}",
                        contents.mime_type
                    );
                    return Task::none();
                };

                // Written by an operation like every other file this app
                // creates, so it reports failure, shows progress and can be
                // undone instead of blocking the interface and logging
                let path = to.join(format!("{}.{}", fl!("pasted-image"), extension));
                return self.operation(Operation::WriteFile {
                    path,
                    data: contents.data.into(),
                });
            }
            Message::PasteVideo(to) => {
                return clipboard::read_data::<ClipboardPasteVideo>().map(move |contents_opt| {
                    match contents_opt {
                        Some(contents) => crate::ui::action::app(Message::PasteVideoContents(
                            to.clone(),
                            contents,
                        )),
                        // No video data in clipboard, try text data
                        None => crate::ui::action::app(Message::PasteText(to.clone())),
                    }
                });
            }
            Message::PasteVideoContents(to, contents) => {
                let Some(extension) = contents.extension() else {
                    log::warn!(
                        "Ignoring paste: unknown video MIME type {:?}",
                        contents.mime_type
                    );
                    return Task::none();
                };

                // Written by an operation like every other file this app
                // creates, so it reports failure, shows progress and can be
                // undone instead of blocking the interface and logging
                let path = to.join(format!("{}.{}", fl!("pasted-video"), extension));
                return self.operation(Operation::WriteFile {
                    path,
                    data: contents.data.into(),
                });
            }
            Message::PasteText(to) => {
                return clipboard::read_data::<ClipboardPasteText>().map(move |contents_opt| {
                    match contents_opt {
                        Some(contents) => {
                            crate::ui::action::app(Message::PasteTextContents(to.clone(), contents))
                        }
                        None => crate::ui::action::none(),
                    }
                });
            }
            Message::PasteTextContents(to, contents) => {
                let path = to.join(format!("{}.txt", fl!("pasted-text")));
                return self.operation(Operation::WriteFile {
                    path,
                    data: contents.data.into_bytes().into(),
                });
            }
            Message::CheckClipboard => {
                // Check if clipboard has any paste-able content and cache it
                return clipboard::read_data::<ClipboardPaste>().map(|contents_opt| {
                    match contents_opt {
                        Some(contents) if contents.paths.is_empty() => crate::ui::action::app(
                            Message::RetryCheckClipboard(ClipboardCache::Files(contents)),
                        ),
                        Some(contents) => crate::ui::action::app(Message::ClipboardCached(
                            ClipboardCache::Files(contents),
                        )),
                        _ => crate::ui::action::app(Message::CheckClipboardImage),
                    }
                });
            }
            Message::CheckClipboardImage => {
                return clipboard::read_data::<ClipboardPasteImage>().map(|contents_opt| {
                    match contents_opt {
                        Some(contents) => crate::ui::action::app(Message::ClipboardCached(
                            ClipboardCache::Image(contents),
                        )),
                        None => crate::ui::action::app(Message::CheckClipboardVideo),
                    }
                });
            }
            Message::CheckClipboardVideo => {
                return clipboard::read_data::<ClipboardPasteVideo>().map(|contents_opt| {
                    match contents_opt {
                        Some(contents) => crate::ui::action::app(Message::ClipboardCached(
                            ClipboardCache::Video(contents),
                        )),
                        None => crate::ui::action::app(Message::CheckClipboardText),
                    }
                });
            }
            Message::CheckClipboardText => {
                return clipboard::read_data::<ClipboardPasteText>().map(|contents_opt| {
                    crate::ui::action::app(Message::ClipboardCached(match contents_opt {
                        Some(contents) => ClipboardCache::Text(contents),
                        None => ClipboardCache::Empty,
                    }))
                });
            }
            Message::RetryCheckClipboard(cache) => {
                let mut cmds = Vec::new();
                cmds.push(self.update(Message::ClipboardCached(cache)));

                cmds.push(
                    iced::Task::future(tokio::time::sleep(Duration::from_millis(300)))
                        .discard()
                        .chain(
                            clipboard::read_data::<ClipboardPaste>().map(|contents_opt| {
                                match contents_opt {
                                    Some(contents) if !contents.paths.is_empty() => {
                                        crate::ui::action::app(Message::ClipboardCached(
                                            ClipboardCache::Files(contents),
                                        ))
                                    }
                                    _ => crate::ui::action::app(Message::CheckClipboardImage),
                                }
                            }),
                        ),
                );
                return Task::batch(cmds);
            }
            Message::ClipboardCached(cache) => {
                self.clipboard_cache = cache;
            }
            Message::PendingCancel(id) => {
                if let Some((_, controller)) = self.pending_operations.get(&id) {
                    controller.cancel();
                    self.progress_operations.remove(&id);
                }
            }
            Message::PendingCancelAll => {
                for (id, (_, controller)) in &self.pending_operations {
                    controller.cancel();
                    self.progress_operations.remove(id);
                }
            }
            Message::PendingComplete(id, op_sel) => {
                return self.handle_completed_operations(vec![(id, op_sel)]);
            }
            Message::PendingDismiss => {
                self.progress_operations.clear();
            }
            Message::PendingError(id, err) => {
                return self.handle_operation_errors(vec![(id, err)]);
            }
            Message::PendingResults(completed, errors) => {
                return Task::batch(vec![
                    self.handle_completed_operations(completed),
                    self.handle_operation_errors(errors),
                ]);
            }
            Message::PendingPause(id, pause) => {
                if let Some((_, controller)) = self.pending_operations.get(&id) {
                    if pause {
                        controller.pause();
                    } else {
                        controller.unpause();
                    }
                }
            }
            Message::PendingPauseAll(pause) => {
                for (_, controller) in self.pending_operations.values() {
                    if pause {
                        controller.pause();
                    } else {
                        controller.unpause();
                    }
                }
            }
            Message::PermanentlyDelete(entity_opt) => {
                let paths: Box<[_]> = self.selected_paths(entity_opt).collect();
                if !paths.is_empty() {
                    return self.push_dialog(
                        DialogPage::PermanentlyDelete { paths },
                        Some(PERMANENT_DELETE_BUTTON_ID.clone()),
                    );
                }
            }
            Message::Preview => {
                let show_details = !self.config.show_details;
                self.context_page = ContextPage::Preview(None, PreviewKind::Selected);
                self.core.window.show_context = show_details;
                return crate::ui::task::message(Message::SetShowDetails(show_details));
            }
            Message::RemoveFromRecents(entity_opt) => {
                let paths: Box<[_]> = self.selected_paths(entity_opt).collect();
                return self.operation(Operation::RemoveFromRecents { paths });
            }
            Message::RefreshedItems(entity, location, listing, batch, rebuilt) => {
                let mut warm = None;
                if let Some(tab) = self.tab_model.data_mut::<Tab>(entity) {
                    // The tab may have been sent somewhere else, or rescanned
                    // from scratch, while these were being read; either way
                    // they describe a listing it is no longer showing.
                    if tab.location == location && tab.listing() == listing {
                        let mut adopted = Vec::new();
                        for (path, fresh) in rebuilt {
                            // Batches overlap: two bursts naming the same file
                            // start two workers, and the older one can finish
                            // last. Whatever was read more recently for a path
                            // is what the tab keeps.
                            if tab.accept_refresh(listing, batch, &path) {
                                adopted.push((path, fresh));
                            }
                        }
                        let mut any = false;
                        if let Some(items) = &mut tab.items_opt {
                            for (path, fresh) in adopted {
                                // One item per path in a listing, so the first
                                // match is the only one.
                                if let Some(item) =
                                    items.iter_mut().find(|item| item.path_opt() == Some(&path))
                                {
                                    item.adopt(*fresh);
                                    any = true;
                                }
                            }
                        }
                        if any {
                            // Asked for after adopting, not before: a refreshed
                            // item may now be a type whose icon has never been
                            // resolved, and until it is adopted the tab still
                            // holds the old type.
                            let sizes = tab.config.icon_sizes;
                            let warmup = tab.refresh_icons(sizes);
                            if !warmup.is_empty() {
                                warm = Some((warmup, sizes));
                            }
                        }
                    }
                }
                if let Some((warmup, sizes)) = warm {
                    return Task::future(async move {
                        if tokio::task::spawn_blocking(move || tab::warm_icons(&warmup, sizes))
                            .await
                            .is_err()
                        {
                            return crate::ui::action::none();
                        }
                        crate::ui::action::app(Message::TabMessage(
                            Some(entity),
                            tab::Message::IconsReady,
                        ))
                    });
                }
            }
            Message::ReloadMimeAppCache => {
                // Rebuilt on a worker and swapped in when it is ready. Building
                // it walks every desktop entry installed on the system, which
                // is far too much to do between two frames.
                let rebuild = self.mime_app_rebuild;
                self.mime_app_rebuild = self.mime_app_rebuild.wrapping_add(1);
                return Task::future(async move {
                    match tokio::task::spawn_blocking(|| {
                        let cache = MimeAppCache::new();
                        // While still on the worker: this shells out to
                        // `xdg-mime`, and doing it here keeps that off the
                        // handler that first opens a terminal.
                        cache.prime_terminal();
                        cache
                    })
                    .await
                    {
                        Ok(cache) => crate::ui::action::app(Message::MimeAppCacheReloaded(
                            rebuild,
                            MimeAppCacheWrapper::new(cache),
                        )),
                        Err(err) => {
                            log::warn!("failed to reload the mime app cache: {err}");
                            crate::ui::action::none()
                        }
                    }
                });
            }
            Message::MimeAppCacheReloaded(rebuild, mut wrapper) => {
                // Rebuilds overlap: two default-application changes, or a
                // change and the watcher noticing it, start two workers, and
                // the older one can finish last. Installing whichever arrives
                // would then put back associations that are already out of
                // date.
                if rebuild < self.mime_app_rebuild_applied {
                    log::debug!("discarding mime app cache rebuild {rebuild}: superseded");
                } else if let Some(cache) = wrapper.take() {
                    self.mime_app_rebuild_applied = rebuild;
                    self.mime_app_cache = cache;
                    // Whatever arrived while there was no cache to answer with
                    let waiting = std::mem::take(&mut self.deferred_mime_messages);
                    if !waiting.is_empty() {
                        return Task::batch(
                            waiting
                                .into_iter()
                                .map(|message| self.update(message))
                                .collect::<Vec<_>>(),
                        );
                    }
                }
            }
            Message::RescanRecents => {
                return self.rescan_recents();
            }
            Message::RescanTrash => {
                // Update trash icon if empty/full
                let maybe_entity = self.nav_model.iter().find(|&entity| {
                    self.nav_model
                        .data::<Location>(entity)
                        .is_some_and(|loc| matches!(loc, Location::Trash))
                });
                if let Some(entity) = maybe_entity {
                    self.nav_model
                        .icon_set(entity, icon::icon(Trash::icon_symbolic(16)));
                }

                return self.rescan_trash();
            }
            Message::Rename(entity_opt) => {
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                if let Some(tab) = self.tab_model.data_mut::<Tab>(entity)
                    && let Some(items) = tab.items_opt()
                {
                    let selected: Box<[_]> = items
                        .iter()
                        .filter_map(|item| {
                            if item.selected {
                                item.path_opt().cloned()
                            } else {
                                None
                            }
                        })
                        .collect();
                    // Several items in one folder are renamed together
                    let batch = match tab.location.path_opt() {
                        Some(parent)
                            if selected.len() > 1
                                && selected.iter().all(|path| path.parent() == Some(parent)) =>
                        {
                            Some((parent.clone(), tab.selected_names()))
                        }
                        _ => None,
                    };
                    if let Some((parent, names)) = batch {
                        let tags = batch_rename::Tags::localized();
                        let task = self.push_dialog(
                            DialogPage::BatchRename {
                                parent,
                                names,
                                settings: batch_rename::Settings::new(&tags),
                            },
                            Some(self.dialog_text_input.clone()),
                        );
                        let preview = self.refresh_batch_rename_preview();
                        return Task::batch([task, preview]);
                    }
                    if !selected.is_empty() {
                        let mut last_name = String::new();
                        let tasks: Vec<_> = selected
                            .into_iter()
                            .filter_map(|path| {
                                let parent = path.parent()?.to_path_buf();
                                let name = path.file_name()?.to_str()?.to_string();
                                let dir = path.is_dir();
                                last_name = name.clone();
                                Some(self.dialog_pages.push_back(DialogPage::RenameItem {
                                    from: path,
                                    parent,
                                    name,
                                    dir,
                                }))
                            })
                            .collect();
                        let tasks = tasks.into_iter().chain([
                            widget::text_input::focus(self.dialog_text_input.clone()),
                            widget::text_input::select_until_last(
                                self.dialog_text_input.clone(),
                                &last_name,
                                '.',
                            ),
                        ]);
                        return Task::batch(tasks);
                    }
                }
            }
            Message::ReplaceResult(replace_result) => {
                if let Some((dialog_page, task)) = self.dialog_pages.pop_front() {
                    match dialog_page {
                        DialogPage::Replace { tx, .. } => {
                            return Task::future(async move {
                                let _ = tx.send(replace_result).await;
                                crate::ui::action::none()
                            });
                        }
                        other => {
                            log::warn!("tried to send replace result to the wrong dialog");
                            return Task::batch([task, self.dialog_pages.push_front(other)]);
                        }
                    }
                }
            }
            Message::RestoreFromTrash(entity_opt) => {
                let mut trash_items = Vec::new();
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                if let Some(tab) = self.tab_model.data_mut::<Tab>(entity)
                    && let Some(items) = tab.items_opt()
                {
                    for item in items {
                        if item.selected
                            && let ItemMetadata::Trash { entry, .. } = &item.metadata
                        {
                            trash_items.push(entry.clone());
                        }
                    }
                }
                if !trash_items.is_empty() {
                    return self.operation(Operation::Restore { items: trash_items });
                }
            }
            Message::ScrollTab(scroll_speed) => {
                let entity = self.tab_model.active();
                return self.update(Message::TabMessage(
                    Some(entity),
                    tab::Message::ScrollTab(f32::from(scroll_speed) / 10.0),
                ));
            }
            Message::SearchActivate => {
                let mut tasks = vec![];

                if self.search_get().is_none() {
                    tasks.push(self.search_set_active(Some(String::new())));
                } else {
                    tasks.push(widget::text_input::focus(self.search_id.clone()));
                };

                return Task::batch(tasks);
            }
            Message::SearchClear => {
                return self.search_set_active(None);
            }
            Message::SearchInput(input) => {
                return self.search_set_active(Some(input));
            }
            Message::SetShowDetails(show_details) => {
                config_set!(show_details, show_details);
                return self.update_config();
            }
            Message::SetShowRecents(show_recents) => {
                config_set!(show_recents, show_recents);
                return self.update_config();
            }
            Message::SetTypeToSearch(type_to_search) => {
                config_set!(type_to_search, type_to_search);
                return self.update_config();
            }
            Message::SystemThemeModeChange => {
                return self.update_config();
            }
            Message::TabActivate(entity) => {
                let mut tasks = vec![];

                // Activate new tab
                self.tab_model.activate(entity);
                if let Some(tab) = self.tab_model.data::<Tab>(entity) {
                    {
                        //Restore scroll
                        let scroll = tab.scroll_opt.unwrap_or_default();
                        tasks.push(scrollable::scroll_to(
                            tab.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(scroll.x),
                                y: Some(scroll.y),
                            },
                        ));
                    }
                    self.activate_nav_model_location(&tab.location.clone());
                }
                tasks.push(self.update_title());
                return Task::batch(tasks);
            }
            Message::TabNext => {
                let len = self.tab_model.len();
                let pos = (self
                    .tab_model
                    .position(self.tab_model.active())
                    .expect("should always be at least one tab open")
                    + 1)
                    // Wraparound to 0 if i + 1 > num of tabs
                    % len as u16;

                let entity = self.tab_model.entity_at(pos);
                if let Some(entity) = entity {
                    return self.update(Message::TabActivate(entity));
                }
            }
            Message::TabPrev => {
                let pos = self
                    .tab_model
                    .position(self.tab_model.active())
                    .expect("should always be at least one tab open")
                    .checked_sub(1)
                    // Subtraction underflow => last tab; i.e. it wraps around
                    .unwrap_or_else(|| (self.tab_model.len() as u16).saturating_sub(1));

                let entity = self.tab_model.entity_at(pos);
                if let Some(entity) = entity {
                    return self.update(Message::TabActivate(entity));
                }
            }
            Message::TabClose(entity_opt) => {
                let mut tasks = Vec::with_capacity(2);

                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());

                // If the last tab is closed, close the window
                // Otherwise, activate closest item
                if self.tab_model.len() == 1 {
                    tasks.push(Task::future(async move {
                        crate::ui::action::app(Message::WindowClose)
                    }));
                } else if entity == self.tab_model.active()
                    && let Some(position) = self.tab_model.position(entity)
                {
                    let new_position = if position > 0 {
                        position - 1
                    } else {
                        position + 1
                    };

                    if let Some(new_entity) = self.tab_model.entity_at(new_position) {
                        tasks.push(self.update(Message::TabActivate(new_entity)));
                    }
                }

                // Remove item
                self.tab_model.remove(entity);

                tasks.push(self.update_watcher());

                return Task::batch(tasks);
            }
            Message::TabConfig(config) => {
                if config != self.config.tab {
                    config_set!(tab, config);
                    return self.update_config();
                }
            }
            Message::ToggleFoldersFirst => {
                let mut config = self.config.tab;
                config.folders_first = !config.folders_first;
                return self.update(Message::TabConfig(config));
            }
            Message::ToggleShowHidden => {
                let mut config = self.config.tab;
                config.show_hidden = !config.show_hidden;
                return self.update(Message::TabConfig(config));
            }
            Message::ToggleAuthPasswordVisible => {
                self.auth_password_visible = !self.auth_password_visible;
            }
            Message::ToggleShowTypeColumn => {
                let mut config = self.config.tab;
                config.show_type_column = !config.show_type_column;
                return self.update(Message::TabConfig(config));
            }
            Message::TabMessage(entity_opt, tab_message) => {
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                // The context menu opens on right-button release, so refresh paste availability now
                let right_click = matches!(tab_message, tab::Message::RightClick(..));

                let tab_commands = match self.tab_model.data_mut::<Tab>(entity) {
                    Some(tab) => tab.update(tab_message, self.modifiers),
                    _ => Vec::new(),
                };

                let mut commands = Vec::new();
                if right_click {
                    commands.push(self.update(Message::CheckClipboard));
                }
                for tab_command in tab_commands {
                    match tab_command {
                        tab::Command::WarmIcons(warmup, sizes) => {
                            commands.push(Task::future(async move {
                                if tokio::task::spawn_blocking(move || {
                                    tab::warm_icons(&warmup, sizes);
                                })
                                .await
                                .is_err()
                                {
                                    return crate::ui::action::none();
                                }
                                crate::ui::action::app(Message::TabMessage(
                                    Some(entity),
                                    tab::Message::IconsReady,
                                ))
                            }));
                        }
                        tab::Command::Action(action) => {
                            commands.push(self.update(action.message(Some(entity))));
                        }
                        tab::Command::AddNetworkDrive => {
                            self.context_page = ContextPage::NetworkDrive;
                            self.set_show_context(true);
                        }
                        tab::Command::AddToSidebar(path) => {
                            let mut favorites = self.config.favorites.clone();
                            let favorite = Favorite::from_path(path);
                            let favorite_path = favorite.path_opt();
                            if !favorites.iter().any(|f| f.path_opt() == favorite_path) {
                                favorites.push(favorite);
                            }
                            config_set!(favorites, favorites);
                            commands.push(self.update_config());
                        }
                        tab::Command::AutoScroll(scroll_speed) => {
                            // converting an f32 to an i16 here by multiplying by 10 and casting to i16
                            // further resolution isn't necessary
                            if let Some(scroll_speed_float) = scroll_speed {
                                self.auto_scroll_speed = Some((scroll_speed_float * 10.0) as i16);
                            } else {
                                self.auto_scroll_speed = None;
                            }
                        }
                        tab::Command::ChangeLocation(tab_title, tab_path, selection_paths) => {
                            self.activate_nav_model_location(&tab_path);

                            self.tab_model.text_set(entity, tab_title);
                            // clear the prefix selection buffer when changing location
                            self.type_select_prefix.clear();
                            commands.push(Task::batch([
                                self.update_title(),
                                self.update_watcher(),
                                self.update_tab(entity, tab_path, selection_paths),
                            ]));
                        }
                        tab::Command::Surface(action) => {
                            // re-type the tab's surface action the way its messages are re-typed
                            let action = action
                                .map(move |message| Message::TabMessage(Some(entity), message));
                            commands.push(self.update(Message::Surface(action)));
                        }
                        tab::Command::Delete(paths) => commands.push(self.delete(paths)),
                        tab::Command::DropFiles(to, copy) => {
                            commands.push(Self::drop_files(to, copy));
                        }
                        tab::Command::ClearRecents => {
                            crate::recents::clear();
                        }
                        tab::Command::EmptyTrash => {
                            return self.push_dialog(
                                DialogPage::EmptyTrash,
                                Some(EMPTY_TRASH_BUTTON_ID.clone()),
                            );
                        }
                        #[cfg(feature = "desktop")]
                        tab::Command::ExecEntryAction(entry, action) => {
                            Self::exec_entry_action(&entry, action);
                        }
                        tab::Command::RunContextAction(action) => {
                            let paths: Box<[_]> = self.selected_paths(Some(entity)).collect();
                            if let Some(preset) = self.config.context_actions.get(action) {
                                if preset.confirm {
                                    commands.push(self.push_dialog(
                                        DialogPage::RunContextAction { action, paths },
                                        Some(CONFIRM_CONTEXT_ACTION_BUTTON_ID.clone()),
                                    ));
                                } else {
                                    context_action::run(
                                        &self.config.context_actions,
                                        action,
                                        &paths,
                                    );
                                }
                            } else {
                                log::warn!("invalid context action index `{action}`");
                            }
                        }
                        tab::Command::Iced(iced_command) => {
                            commands.push(iced_command.0.map(move |x| {
                                crate::ui::action::app(Message::TabMessage(Some(entity), x))
                            }));
                        }
                        tab::Command::OpenFile(paths) => commands.push(self.open_file(&paths)),
                        tab::Command::OpenInNewTab(path) => {
                            commands.push(self.open_tab(Location::Path(path), false, None));
                        }
                        tab::Command::OpenInNewWindow(path) => match env::current_exe() {
                            Ok(exe) => match process::Command::new(&exe).arg(path).spawn() {
                                Ok(_child) => {}
                                Err(err) => {
                                    log::error!("failed to execute {}: {}", exe.display(), err);
                                }
                            },
                            Err(err) => {
                                log::error!("failed to get current executable path: {err}");
                            }
                        },
                        tab::Command::ResolveNetwork(request, uri) => {
                            // Asked on a worker: the mounter answers over a
                            // channel the GVFS thread writes to when it has
                            // been round the network and back, and waiting for
                            // that here is waiting for a server.
                            commands.push(Task::future(async move {
                                let asked = {
                                    let uri = uri.clone();
                                    tokio::task::spawn_blocking(move || {
                                        MOUNTERS.values().find_map(|mounter| mounter.dir_info(&uri))
                                    })
                                    .await
                                };
                                let resolved = match asked {
                                    Ok(info) => info.map(|(uri, display_name, path_opt)| {
                                        Location::Network(uri, display_name, path_opt)
                                    }),
                                    Err(err) => {
                                        log::warn!("failed to resolve {uri}: {err}");
                                        None
                                    }
                                };
                                crate::ui::action::app(Message::TabMessage(
                                    Some(entity),
                                    tab::Message::NetworkResolved(request, uri, resolved),
                                ))
                            }));
                        }
                        tab::Command::Preview(kind) => {
                            self.context_page = ContextPage::Preview(Some(entity), kind);
                            self.set_show_context(true);
                        }
                        tab::Command::SetOpenWith(mime, id) => {
                            // Queued here, in the order the user made the
                            // choices. Handing the queueing itself to a worker
                            // would let two choices race to reach the writer,
                            // and the earlier one could land second and win.
                            // Only the waiting goes to a worker: the write is
                            // a read-modify-write of `mimeapps.list`, which is
                            // disk work however small the file is.
                            let Some(ack) = MimeAppCache::queue_default(mime, id) else {
                                continue;
                            };
                            commands.push(Task::future(async move {
                                match tokio::task::spawn_blocking(move || {
                                    MimeAppCache::await_association(ack)
                                })
                                .await
                                {
                                    Ok(true) => crate::ui::action::app(Message::ReloadMimeAppCache),
                                    Ok(false) => crate::ui::action::none(),
                                    Err(err) => {
                                        log::warn!("failed to set the default application: {err}");
                                        crate::ui::action::none()
                                    }
                                }
                            }));
                        }
                        tab::Command::SetPermissions(path, mode) => {
                            commands.push(self.operation(Operation::SetPermissions { path, mode }));
                        }
                        tab::Command::SetMultiplePermissions(permissions) => {
                            commands.push(
                                self.join_operations(
                                    permissions
                                        .into_iter()
                                        .map(|(path, mode)| Operation::SetPermissions {
                                            path,
                                            mode,
                                        })
                                        .collect(),
                                ),
                            );
                        }
                        tab::Command::WindowDrag => {
                            if let Some(window_id) = self.core.main_window_id() {
                                commands.push(window::drag(window_id));
                            }
                        }
                        tab::Command::WindowToggleMaximize => {
                            if let Some(window_id) = self.core.main_window_id() {
                                commands.push(window::toggle_maximize(window_id));
                            }
                        }
                        tab::Command::SetSort(location, heading_options, direction) => {
                            let default_sort = tab::SORT_OPTION_FALLBACK
                                .get(&location)
                                .copied()
                                .unwrap_or((HeadingOptions::Name, true));
                            let changed = if default_sort == (heading_options, direction) {
                                self.state.sort_names.remove(&location).is_some()
                            } else {
                                // force reordering of inserted values so new settings are not dropped in the truncation step
                                _ = self.state.sort_names.remove(&location);
                                _ = self
                                    .state
                                    .sort_names
                                    .insert(location, (heading_options, direction))
                                    .is_none_or(|old| old != (heading_options, direction));

                                const MAX_SORT_NAMES: usize = 999;
                                if self.state.sort_names.len() > MAX_SORT_NAMES {
                                    // truncate is not a good fit because it drops the items at the end, which are newest...
                                    self.state.sort_names = self
                                        .state
                                        .sort_names
                                        .split_off(self.state.sort_names.len() - MAX_SORT_NAMES);
                                }

                                true
                            };

                            if !self.must_save_sort_names & changed {
                                self.must_save_sort_names = true;
                                return crate::ui::Task::future(async move {
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                    crate::ui::action::app(Message::SaveSortNames)
                                });
                            }
                        }
                    }
                }
                return Task::batch(commands);
            }
            Message::TabNew => {
                let active = self.tab_model.active();
                let location = match self.tab_model.data::<Tab>(active) {
                    Some(tab) => tab.location.clone(),
                    None => Location::Path(home_dir()),
                };
                return self.open_tab(location, true, None);
            }
            Message::TabRescan(entity, mut location, parent_item_opt, items, selection_paths) => {
                location = location.normalize();
                if let Some(tab) = self.tab_model.data_mut::<Tab>(entity) {
                    tab.location = tab.location.normalize();
                    if location == tab.location {
                        tab.parent_item_opt = parent_item_opt;
                        tab.set_items(items);
                        let location_str = location.to_string();
                        let sort = self
                            .state
                            .sort_names
                            .get(&location_str)
                            .or_else(|| SORT_OPTION_FALLBACK.get(&location_str))
                            .unwrap_or(&(HeadingOptions::Name, true));

                        tab.sort_name = sort.0;
                        tab.sort_direction = sort.1;

                        let mut tasks = Vec::with_capacity(4);

                        // Resolve the icons this listing needs on a worker.
                        // The listing is already on screen with placeholders;
                        // this swaps in the real icons when they are ready.
                        let sizes = tab.config.icon_sizes;
                        let wanted = tab.refresh_icons(sizes);
                        if !wanted.is_empty() {
                            tasks.push(Task::future(async move {
                                let warmed = tokio::task::spawn_blocking(move || {
                                    tab::warm_icons(&wanted, sizes);
                                })
                                .await;
                                if warmed.is_err() {
                                    return crate::ui::action::none();
                                }
                                crate::ui::action::app(Message::TabMessage(
                                    Some(entity),
                                    tab::Message::IconsReady,
                                ))
                            }));
                        }

                        // Apply a scroll offset restored from history, now that the
                        // items exist
                        tasks.push(Task::done(crate::ui::action::app(Message::TabMessage(
                            Some(entity),
                            tab::Message::ScrollRestore,
                        ))));

                        if let Some(selection_paths) = selection_paths {
                            tab.select_paths(selection_paths);

                            // Ensure selected path is scrolled to after redraw
                            tasks.push(Task::done(crate::ui::action::app(Message::TabMessage(
                                Some(entity),
                                tab::Message::ScrollToFocused,
                            ))));
                        }

                        tasks.push(clipboard::read_data::<ClipboardPaste>().map(|p| {
                            crate::ui::action::app(Message::CutPaths(match p {
                                Some(s) => match s.kind {
                                    ClipboardKind::Copy => Vec::new(),
                                    ClipboardKind::Cut => s.paths,
                                },
                                None => Vec::new(),
                            }))
                        }));

                        return Task::batch(tasks);
                    }
                }
            }
            Message::TabView(entity_opt, view) => {
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                if let Some(tab) = self.tab_model.data_mut::<Tab>(entity) {
                    tab.config.view = view;
                }
                let mut config = self.config.tab;
                config.view = view;
                return self.update(Message::TabConfig(config));
            }
            Message::CutPaths(paths) => {
                if let Some(tab) = self.tab_model.active_data_mut::<Tab>() {
                    tab.refresh_cut(&paths);
                }
            }
            Message::ToggleContextPage(context_page) => {
                if self.context_page == context_page
                    || matches!(self.context_page, ContextPage::Preview(_, _))
                {
                    self.set_show_context(!self.core.window.show_context);
                } else {
                    self.set_show_context(true);
                }
                self.context_page = context_page;
                // Preview status is preserved across restarts
                if matches!(self.context_page, ContextPage::Preview(_, _)) {
                    return crate::ui::task::message(crate::ui::action::app(
                        Message::SetShowDetails(self.core.window.show_context),
                    ));
                }
            }
            Message::Undo => {
                // Skip entries whose files have changed since; they can no longer apply
                while let Some(undo) = self.undo_stack.pop() {
                    if undo.iter().all(Operation::is_applicable) {
                        let tasks: Vec<_> = undo
                            .into_iter()
                            .map(|op| {
                                self.undo_ids.insert(self.pending_operation_id);
                                self.operation(op)
                            })
                            .collect();
                        return Task::batch(tasks);
                    }
                    log::info!("skipping an undo that no longer applies");
                }
            }
            Message::UndoTrash(id, recently_trashed) => {
                self.toasts.remove(id);

                let mut paths = Vec::with_capacity(recently_trashed.len());
                let icon_sizes = self.config.tab.icon_sizes;

                return crate::ui::task::future(async move {
                    match tokio::task::spawn_blocking(move || Location::Trash.scan(icon_sizes))
                        .await
                    {
                        Ok((_parent_item_opt, items)) => {
                            for path in &*recently_trashed {
                                for item in &items {
                                    if let ItemMetadata::Trash { ref entry, .. } = item.metadata {
                                        let original_path = entry.original_path();
                                        if &original_path == path {
                                            paths.push(entry.clone());
                                        }
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            log::warn!("failed to rescan: {err}");
                        }
                    }

                    Message::UndoTrashStart(None, paths)
                });
            }
            Message::UndoTrashStart(toast_id, items) => {
                if let Some(toast_id) = toast_id {
                    self.toasts.remove(toast_id);
                }
                // The same restore is what Undo would do: drop that entry and do
                // not offer to undo the restore itself
                self.undo_stack.retain(
                    |undo| !matches!(undo.as_slice(), [Operation::Restore { items: i }] if *i == items),
                );
                self.undo_ids.insert(self.pending_operation_id);
                return self.operation(Operation::Restore { items });
            }
            Message::WindowClose => {
                if let Some(window_id) = self.core.main_window_id() {
                    self.core.set_main_window_id(None);
                    return Task::batch([
                        window::close(window_id),
                        Task::future(async move { crate::ui::action::app(Message::MaybeExit) }),
                    ]);
                }
            }
            Message::WindowCloseRequested(id) => {
                self.remove_window(&id);
            }
            Message::WindowMaximize(id, maximized) => {
                return window::maximize(id, maximized);
            }
            Message::WindowNew => match env::current_exe() {
                Ok(exe) => {
                    // initialize command to spawn another instance of this application
                    let mut command = process::Command::new(&exe);

                    // make the new window open at the same location as the currently active tab by
                    // passing respective command line arguments
                    let entity = self.tab_model.active();
                    let active_tab_location =
                        self.tab_model.data::<Tab>(entity).map(|tab| &tab.location);
                    match active_tab_location {
                        Some(
                            Location::Path(path) | Location::Search(SearchLocation::Path(path), ..),
                        ) => {
                            command.arg(path);
                        }
                        Some(Location::Network(uri, ..)) => {
                            command.arg(uri);
                        }
                        Some(Location::Recents | Location::Search(SearchLocation::Recents, ..)) => {
                            command.arg("--recents");
                        }
                        Some(Location::Trash | Location::Search(SearchLocation::Trash, ..)) => {
                            command.arg("--trash");
                        }
                        None => {}
                    };

                    // spawn the new window
                    match command.spawn() {
                        Ok(_child) => {}
                        Err(err) => {
                            log::error!("failed to execute {}: {}", exe.display(), err);
                        }
                    }
                }
                Err(err) => {
                    log::error!("failed to get current executable path: {err}");
                }
            },
            Message::ZoomDefault(entity_opt) => {
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                let mut config = self.config.tab;
                if let Some(tab) = self.tab_model.data::<Tab>(entity) {
                    zoom_to_default(tab.config.view, &mut config.icon_sizes);
                }
                return self.update(Message::TabConfig(config));
            }
            Message::ZoomIn(entity_opt) => {
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                let mut config = self.config.tab;
                if let Some(tab) = self.tab_model.data::<Tab>(entity) {
                    zoom_in_view(tab.config.view, &mut config.icon_sizes);
                }
                return self.update(Message::TabConfig(config));
            }
            Message::ZoomOut(entity_opt) => {
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                let mut config = self.config.tab;
                if let Some(tab) = self.tab_model.data::<Tab>(entity) {
                    zoom_out_view(tab.config.view, &mut config.icon_sizes);
                }
                return self.update(Message::TabConfig(config));
            }
            Message::NavBarClose(entity) => {
                if let Some(data) = self.nav_model.data::<MounterData>(entity)
                    && let Some(mounter) = MOUNTERS.get(&data.0)
                {
                    return mounter
                        .unmount(data.1.clone())
                        .map(|()| crate::ui::action::none());
                }
            }
            Message::NavBarDrop(entity) => {
                if let Some(to) = self
                    .nav_model
                    .data::<Location>(entity)
                    .and_then(|location| location.path_opt().cloned())
                {
                    return Self::drop_files(to, self.modifiers.control());
                }
            }
            Message::TabDrop(entity) => {
                if let Some(to) = self
                    .tab_model
                    .data::<Tab>(entity)
                    .and_then(|tab| tab.location.path_opt().cloned())
                {
                    return Self::drop_files(to, self.modifiers.control());
                }
            }
            Message::NavBarContext(entity) => {
                self.nav_bar_context_id = entity;

                let tab_entity = self.tab_model.active();
                if let Some(tab) = self.tab_model.data_mut::<Tab>(tab_entity) {
                    // Close location editing if enabled
                    tab.dismiss_edit_location();
                }
            }
            Message::NavMenuAction(action) => match action {
                NavMenuAction::ClearRecents => crate::recents::clear(),
                NavMenuAction::EmptyTrash => {
                    return self
                        .push_dialog(DialogPage::EmptyTrash, Some(EMPTY_TRASH_BUTTON_ID.clone()));
                }
                NavMenuAction::Open(entity) => {
                    if let Some(path) = self
                        .nav_model
                        .data::<Location>(entity)
                        .and_then(Location::path_opt)
                        .cloned()
                    {
                        return self.open_file(&[path]);
                    }
                }
                NavMenuAction::OpenWith(entity) => {
                    // The path is known now; its type is not, and finding out
                    // means reading the disk -- for a bookmarked mount that
                    // has gone away, possibly for a long time. So the dialog
                    // goes up at once, saying so, with a way out, and the
                    // reading happens on a worker. Cancel discards the answer;
                    // it cannot interrupt the read.
                    if let Some(path) = self
                        .nav_model
                        .data::<Location>(entity)
                        .and_then(Location::path_opt)
                        .cloned()
                    {
                        let shown = self
                            .push_dialog(DialogPage::OpenWithLoading { path: path.clone() }, None);
                        let resolve = Task::future(async move {
                            let resolved = tokio::task::spawn_blocking({
                                let path = path.clone();
                                move || {
                                    tab::item_from_path(&path, IconSizes::default())
                                        .map(|item| item.mime)
                                }
                            })
                            .await
                            .unwrap_or_else(|err| Err(err.to_string()));
                            crate::ui::action::app(Message::OpenWithResolved(path, resolved))
                        });
                        return Task::batch([shown, resolve]);
                    }
                }
                NavMenuAction::RunContextAction(entity, action) => {
                    if let Some(path) = self
                        .nav_model
                        .data::<Location>(entity)
                        .and_then(Location::path_opt)
                        .cloned()
                    {
                        let paths = vec![path];
                        if let Some(preset) = self.config.context_actions.get(action) {
                            if preset.confirm {
                                return self.push_dialog(
                                    DialogPage::RunContextAction {
                                        action,
                                        paths: paths.into_boxed_slice(),
                                    },
                                    Some(CONFIRM_CONTEXT_ACTION_BUTTON_ID.clone()),
                                );
                            }
                            context_action::run(&self.config.context_actions, action, &paths);
                        } else {
                            log::warn!("invalid context action index `{action}`");
                        }
                    }
                }
                NavMenuAction::OpenInNewTab(entity) => {
                    let open_task = match self.nav_model.data::<Location>(entity) {
                        Some(Location::Network(uri, display_name, path)) => self.open_tab(
                            Location::Network(uri.clone(), display_name.clone(), path.clone()),
                            false,
                            None,
                        ),
                        Some(Location::Path(path)) => {
                            self.open_tab(Location::Path(path.clone()), false, None)
                        }
                        Some(Location::Recents) => self.open_tab(Location::Recents, false, None),
                        Some(Location::Trash) => self.open_tab(Location::Trash, false, None),
                        _ => Task::none(),
                    };

                    return open_task;
                }

                // Open the selected path in a new earth-files window.
                NavMenuAction::OpenInNewWindow(entity) => 'open_in_new_window: {
                    if let Some(location) = self.nav_model.data::<Location>(entity) {
                        match env::current_exe() {
                            Ok(exe) => {
                                let mut command = process::Command::new(&exe);
                                match location {
                                    Location::Path(path) => {
                                        command.arg(path);
                                    }
                                    Location::Trash => {
                                        command.arg("--trash");
                                    }
                                    Location::Network(uri, _, Some(_)) => {
                                        command.arg(uri);
                                    }
                                    Location::Network(..) => {
                                        command.arg("--network");
                                    }
                                    Location::Recents => {
                                        command.arg("--recents");
                                    }
                                    _ => {
                                        log::error!(
                                            "unsupported location for open in new window: {location:?}"
                                        );
                                        break 'open_in_new_window;
                                    }
                                }
                                match command.spawn() {
                                    Ok(_child) => {}
                                    Err(err) => {
                                        log::error!("failed to execute {}: {}", exe.display(), err);
                                    }
                                }
                            }
                            Err(err) => {
                                log::error!("failed to get current executable path: {err}");
                            }
                        }
                    }
                }

                NavMenuAction::Preview(entity) => {
                    if let Some(path) = self
                        .nav_model
                        .data::<Location>(entity)
                        .and_then(Location::path_opt)
                    {
                        match tab::item_from_path(path, IconSizes::default()) {
                            Ok(item) => {
                                self.context_page = ContextPage::Preview(
                                    None,
                                    PreviewKind::Custom(PreviewItem(Box::new(item))),
                                );
                                self.set_show_context(true);
                            }
                            Err(err) => {
                                log::warn!(
                                    "failed to get item from path {}: {}",
                                    path.display(),
                                    err
                                );
                            }
                        }
                    }
                }

                NavMenuAction::RemoveFromSidebar(entity) => {
                    if let Some(FavoriteIndex(favorite_i)) =
                        self.nav_model.data::<FavoriteIndex>(entity)
                    {
                        let mut favorites = self.config.favorites.clone();
                        favorites.remove(*favorite_i);
                        config_set!(favorites, favorites);
                        return self.update_config();
                    }
                }

                NavMenuAction::ChangeSidebarLabel(entity) => {
                    if let Some(favorite) = self.nav_model.data::<FavoriteIndex>(entity).and_then(
                        |FavoriteIndex(favorite_i)| self.config.favorites.get(*favorite_i),
                    ) {
                        let label = favorite.display_name().unwrap_or_else(|| fl!("filesystem"));
                        return Task::batch([
                            self.dialog_pages
                                .push_back(DialogPage::ChangeSidebarLabel { entity, label }),
                            widget::text_input::focus(self.dialog_text_input.clone()),
                            widget::text_input::select_all(self.dialog_text_input.clone()),
                        ]);
                    }
                }
            },
            Message::Recents => {
                if self.config.show_recents {
                    return self.open_tab(Location::Recents, false, None);
                }
            }
            Message::Cosmic(cosmic) => {
                // Forward shell actions to the shell.
                return Task::perform(async move { cosmic }, crate::ui::action::cosmic);
            }
            Message::None => {}
            Message::Size(window_id, size) => {
                if self.core.main_window_id() == Some(window_id) {
                    self.size = Some(size);
                }
            }
            Message::Eject => {
                #[cfg(feature = "gvfs")]
                {
                    let mut paths = self.selected_paths(None);
                    if let Some(p) = paths.next() {
                        {
                            for (k, mounter_items) in &self.mounter_items {
                                if let Some(mounter) = MOUNTERS.get(k)
                                    && let Some(item) = mounter_items
                                        .iter()
                                        .find(|&item| item.path().is_some_and(|path| path == p))
                                {
                                    return mounter
                                        .unmount(item.clone())
                                        .map(|()| crate::ui::action::none());
                                }
                            }
                        }
                    }
                }
            }
            Message::Surface(action) => {
                return crate::ui::task::message(crate::ui::Action::Surface(action));
            }
            Message::SaveSortNames => {
                self.must_save_sort_names = false;
                if let Err(err) = self.state_handler.save_in_background(&self.state) {
                    log::warn!("Failed to save sort names: {err:?}");
                }
            }
            Message::NetworkDriveOpenEntityAfterMount { entity } => {
                return self.on_nav_select(entity);
            }
            Message::NetworkDriveOpenTabAfterMount { location } => {
                return self.open_tab(location, false, None);
            }
            Message::ReorderTab(ReorderEvent {
                dragged,
                target,
                position,
            }) => {
                _ = self.tab_model.reorder(dragged, target, position);
            }
        }

        Task::none()
    }

    fn context_drawer(&self) -> Option<context_drawer::ContextDrawer<'_, Message>> {
        if !self.core.window.show_context {
            return None;
        }

        Some(match &self.context_page {
            ContextPage::About => context_drawer::context_drawer(
                widget::about::about(&self.about, |url| Message::LaunchUrl(url.to_string())),
                Message::ToggleContextPage(ContextPage::About),
            ),
            ContextPage::EditHistory => context_drawer::context_drawer(
                self.edit_history(),
                Message::ToggleContextPage(ContextPage::EditHistory),
            )
            .title(fl!("edit-history")),
            ContextPage::NetworkDrive => {
                let mut text_input =
                    widget::text_input(fl!("enter-server-address"), &self.network_drive_input);
                let button = if self.network_drive_connecting.is_some() {
                    widget::button::standard(fl!("connecting"))
                } else {
                    text_input = text_input
                        .on_input(Message::NetworkDriveInput)
                        .on_submit(|_| Message::NetworkDriveSubmit);
                    widget::button::standard(fl!("connect")).on_press(Message::NetworkDriveSubmit)
                };
                context_drawer::context_drawer(
                    self.network_drive(),
                    Message::ToggleContextPage(ContextPage::NetworkDrive),
                )
                .title(fl!("add-network-drive"))
                .header(text_input)
                .footer(widget::Row::with_children([
                    widget::space::horizontal().into(),
                    button.into(),
                ]))
            }
            ContextPage::Preview(entity_opt, kind) => {
                let entity = entity_opt.unwrap_or_else(|| self.tab_model.active());
                let actions = self
                    .tab_model
                    .data::<Tab>(entity)
                    .and_then(|tab| {
                        let mut selected = tab.items_opt()?.iter().filter(|item| item.selected);

                        match (selected.next(), selected.next()) {
                            // Exactly one item
                            (Some(item), None) => Some(
                                item.preview_actions()
                                    .map(move |x| Message::TabMessage(Some(entity), x)),
                            ),
                            // Zero or more than one item
                            _ => None,
                        }
                    })
                    .unwrap_or_else(|| widget::space::horizontal().into());
                context_drawer::context_drawer(
                    self.preview(entity_opt, kind, true)
                        .map(move |x| Message::TabMessage(Some(entity), x)),
                    Message::ToggleContextPage(ContextPage::Preview(Some(entity), kind.clone())),
                )
                .actions(actions)
            }
            ContextPage::Settings => context_drawer::context_drawer(
                self.settings(),
                Message::ToggleContextPage(ContextPage::Settings),
            )
            .title(fl!("settings")),
        })
    }

    fn dialog(&self) -> Option<Element<'_, Message>> {
        let entity = self.tab_model.active();
        if let Some(tab) = self.tab_model.data::<Tab>(entity)
            && tab.gallery
        {
            return Some(
                tab.gallery_view()
                    .map(move |x| Message::TabMessage(Some(entity), x)),
            );
        }
        let dialog_page = self.dialog_pages.front()?;

        let Spacing {
            space_xxxs,
            space_xxs,
            space_s,
            space_m,
            ..
        } = spacing();

        let dialog = match dialog_page {
            DialogPage::Compress {
                paths,
                to,
                name,
                archive_type,
                password,
            } => {
                let mut dialog = widget::dialog().title(fl!("create-archive"));

                let complete_maybe = if name.is_empty() {
                    None
                } else if name == "." || name == ".." {
                    dialog = dialog.tertiary_action(widget::text::body(fl!(
                        "name-invalid",
                        filename = name.as_str()
                    )));
                    None
                } else if name.contains('/') {
                    dialog = dialog.tertiary_action(widget::text::body(fl!("name-no-slashes")));
                    None
                } else {
                    let extension = archive_type.extension();
                    let name = format!("{name}{extension}");
                    let path = to.join(&name);
                    if path.exists() {
                        dialog =
                            dialog.tertiary_action(widget::text::body(fl!("file-already-exists")));
                        None
                    } else {
                        if name.starts_with('.') {
                            dialog = dialog.tertiary_action(widget::text::body(fl!("name-hidden")));
                        }
                        Some(Message::DialogComplete)
                    }
                };

                let archive_types = ArchiveType::all();
                let selected = archive_types.iter().position(|&x| x == *archive_type);
                dialog = dialog
                    .primary_action(
                        widget::button::suggested(fl!("create"))
                            .on_press_maybe(complete_maybe.clone()),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    )
                    .control(
                        widget::Column::with_children([
                            widget::text::body(fl!("file-name")).into(),
                            widget::Row::with_children([
                                widget::text_input("", name.as_str())
                                    .id(self.dialog_text_input.clone())
                                    .on_input(move |name| {
                                        Message::DialogUpdate(DialogPage::Compress {
                                            paths: paths.clone(),
                                            to: to.clone(),
                                            name,
                                            archive_type: *archive_type,
                                            password: password.clone(),
                                        })
                                    })
                                    .on_submit_maybe(
                                        complete_maybe.clone().map(|maybe| move |_| maybe.clone()),
                                    )
                                    .into(),
                                Element::from(widget::dropdown(
                                    archive_types,
                                    selected,
                                    move |index| index,
                                ))
                                .map(|index| {
                                    Message::DialogUpdate(DialogPage::Compress {
                                        paths: paths.clone(),
                                        to: to.clone(),
                                        name: name.clone(),
                                        archive_type: archive_types[index],
                                        password: password.clone(),
                                    })
                                }),
                            ])
                            .align_y(Alignment::Center)
                            .spacing(space_xxs.to_pixels())
                            .into(),
                        ])
                        .spacing(space_xxs.to_pixels()),
                    );

                if *archive_type == ArchiveType::Zip {
                    let password_unwrapped = password.clone().unwrap_or_default();
                    dialog = dialog.control(widget::Column::with_children([
                        widget::text::body(fl!("password")).into(),
                        widget::text_input("", password_unwrapped)
                            .password()
                            .on_input(move |password_unwrapped| {
                                Message::DialogUpdate(DialogPage::Compress {
                                    paths: paths.clone(),
                                    to: to.clone(),
                                    name: name.clone(),
                                    archive_type: *archive_type,
                                    password: Some(password_unwrapped),
                                })
                            })
                            .on_submit_maybe(complete_maybe.map(|maybe| move |_| maybe.clone()))
                            .into(),
                    ]));
                }

                dialog
            }
            DialogPage::EmptyTrash => widget::dialog()
                .title(fl!("empty-trash-title"))
                .body(fl!("empty-trash-warning"))
                .primary_action(
                    widget::button::suggested(fl!("empty-trash"))
                        .on_press(Message::DialogComplete)
                        .id(EMPTY_TRASH_BUTTON_ID.clone()),
                )
                .secondary_action(
                    widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                ),
            DialogPage::FailedOperation(id) => {
                let (operation, _, err) = self.failed_operations.get(id)?;

                widget::dialog()
                    .title("Failed operation")
                    .body(format!("{operation:#?}\n{err}"))
                    .icon(icon::from_name("dialog-error").size(64))
                    .primary_action(
                        widget::button::suggested(fl!("try-again"))
                            .on_press(Message::DialogComplete),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    )
            }
            DialogPage::FailedOperations(ids) => {
                let errors: Vec<String> = ids
                    .iter()
                    .filter_map(|id| match self.failed_operations.get(id) {
                        Some((operation, _, err)) => Some(format!("{operation:#?}\n{err}")),
                        _ => None,
                    })
                    .collect();

                widget::dialog()
                    .title("Failed operations")
                    .body(errors.join("\n\n"))
                    .icon(icon::from_name("dialog-error").size(64))
                    .primary_action(
                        widget::button::suggested(fl!("try-again"))
                            .on_press(Message::DialogComplete),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    )
            }
            DialogPage::ExtractPassword { id, password } => widget::dialog()
                .title(fl!("extract-password-required"))
                .icon(icon::from_name("dialog-error").size(64))
                .control(
                    widget::text_input("", password)
                        .password()
                        .on_input(move |password| {
                            Message::DialogUpdate(DialogPage::ExtractPassword { id: *id, password })
                        })
                        .on_submit(|_| Message::DialogComplete)
                        .id(self.dialog_text_input.clone()),
                )
                .primary_action(
                    widget::button::suggested(fl!("extract-here"))
                        .on_press(Message::DialogComplete),
                )
                .secondary_action(
                    widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                ),
            DialogPage::MountError {
                mounter_key: _,
                item: _,
                error,
            } => widget::dialog()
                .title(fl!("mount-error"))
                .body(error)
                .icon(icon::from_name("dialog-error").size(64))
                .primary_action(
                    widget::button::standard(fl!("try-again"))
                        .on_press(Message::DialogComplete)
                        .id(MOUNT_ERROR_TRY_AGAIN_BUTTON_ID.clone()),
                )
                .secondary_action(
                    widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                ),
            DialogPage::NetworkAuth {
                mounter_key,
                uri,
                auth,
                auth_tx,
            } => {
                let mut controls = widget::Column::with_capacity(4);
                let mut id_assigned = false;

                if let Some(username) = &auth.username_opt {
                    let mut input = widget::text_input(fl!("username"), username)
                        .on_input(move |value| {
                            Message::DialogUpdate(DialogPage::NetworkAuth {
                                mounter_key: *mounter_key,
                                uri: uri.clone(),
                                auth: MounterAuth {
                                    username_opt: Some(value),
                                    ..auth.clone()
                                },
                                auth_tx: auth_tx.clone(),
                            })
                        })
                        .on_submit(|_| Message::DialogComplete);
                    if !id_assigned {
                        input = input.id(self.dialog_text_input.clone());
                        id_assigned = true;
                    }
                    controls = controls.push(input);
                }

                if let Some(domain) = &auth.domain_opt {
                    let mut input = widget::text_input(fl!("domain"), domain)
                        .on_input(move |value| {
                            Message::DialogUpdate(DialogPage::NetworkAuth {
                                mounter_key: *mounter_key,
                                uri: uri.clone(),
                                auth: MounterAuth {
                                    domain_opt: Some(value),
                                    ..auth.clone()
                                },
                                auth_tx: auth_tx.clone(),
                            })
                        })
                        .on_submit(|_| Message::DialogComplete);
                    if !id_assigned {
                        input = input.id(self.dialog_text_input.clone());
                        id_assigned = true;
                    }
                    controls = controls.push(input);
                }

                if let Some(password) = &auth.password_opt {
                    let mut input = widget::secure_input(
                        fl!("password"),
                        password,
                        Some(Message::ToggleAuthPasswordVisible),
                        !self.auth_password_visible,
                    )
                    .on_input(move |value| {
                        Message::DialogUpdate(DialogPage::NetworkAuth {
                            mounter_key: *mounter_key,
                            uri: uri.clone(),
                            auth: MounterAuth {
                                password_opt: Some(value),
                                ..auth.clone()
                            },
                            auth_tx: auth_tx.clone(),
                        })
                    })
                    .on_submit(|_| Message::DialogComplete);
                    if !id_assigned {
                        input = input.id(self.dialog_text_input.clone());
                    }
                    controls = controls.push(input);
                }

                if let Some(remember) = &auth.remember_opt {
                    controls = controls.push(
                        widget::checkbox(*remember)
                            .label(fl!("remember-password"))
                            .on_toggle(move |value| {
                                Message::DialogUpdate(DialogPage::NetworkAuth {
                                    mounter_key: *mounter_key,
                                    uri: uri.clone(),
                                    auth: MounterAuth {
                                        remember_opt: Some(value),
                                        ..auth.clone()
                                    },
                                    auth_tx: auth_tx.clone(),
                                })
                            }),
                    );
                }

                let mut parts = auth.message.splitn(2, '\n');
                let title = parts.next().unwrap_or_default();
                let body = match parts.next() {
                    Some(body) if !body.is_empty() => format!("{uri}\n{body}"),
                    _ => uri.clone(),
                };

                let mut widget = widget::dialog()
                    .title(title)
                    .body(body)
                    .control(controls.spacing(space_s.to_pixels()))
                    .primary_action(
                        widget::button::suggested(fl!("connect")).on_press(Message::DialogComplete),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    );

                if let Some(_anonymous) = &auth.anonymous_opt {
                    widget = widget.tertiary_action(
                        widget::button::text(fl!("connect-anonymously")).on_press(
                            Message::DialogUpdateComplete(DialogPage::NetworkAuth {
                                mounter_key: *mounter_key,
                                uri: uri.clone(),
                                auth: MounterAuth {
                                    anonymous_opt: Some(true),
                                    ..auth.clone()
                                },
                                auth_tx: auth_tx.clone(),
                            }),
                        ),
                    );
                }

                widget
            }
            DialogPage::NetworkError {
                mounter_key: _,
                uri: _,
                error,
            } => widget::dialog()
                .title(fl!("network-drive-error"))
                .body(error)
                .icon(icon::from_name("dialog-error").size(64))
                .primary_action(
                    widget::button::standard(fl!("try-again")).on_press(Message::DialogComplete),
                )
                .secondary_action(
                    widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                ),
            DialogPage::NewItem { parent, name, dir } => self.name_dialog(
                if *dir {
                    fl!("create-new-folder")
                } else {
                    fl!("create-new-file")
                },
                fl!("save"),
                *dir,
                parent,
                name,
                None,
                None,
                move |name| {
                    Message::DialogUpdate(DialogPage::NewItem {
                        parent: parent.clone(),
                        name,
                        dir: *dir,
                    })
                },
            ),
            DialogPage::RunContextAction { action, paths } => {
                let name = self
                    .config
                    .context_actions
                    .get(*action)
                    .map_or_else(|| fl!("context-action"), |preset| preset.name.clone());

                widget::dialog()
                    .title(fl!("context-action-confirm-title", name = name))
                    .body(fl!("context-action-confirm-warning", items = paths.len()))
                    .icon(icon::from_name("dialog-error").size(64))
                    .primary_action(
                        widget::button::suggested(fl!("run"))
                            .on_press(Message::DialogComplete)
                            .id(CONFIRM_CONTEXT_ACTION_BUTTON_ID.clone()),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    )
            }
            DialogPage::OpenWithLoading { path } => {
                let name = match path.file_name() {
                    Some(file_name) => file_name.to_str(),
                    None => path.as_os_str().to_str(),
                }
                .unwrap_or_default();
                widget::dialog()
                    .title(fl!("open-with-title", name = name))
                    .control(widget::text::body(fl!("calculating")))
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    )
            }
            DialogPage::OpenWith {
                path,
                mime,
                selected,
                store_opt,
                search_app_name,
                ..
            } => {
                let name = match path.file_name() {
                    Some(file_name) => file_name.to_str(),
                    None => path.as_os_str().to_str(),
                };

                let mut column = widget::list_column();
                let mut available_apps = self.mime_app_cache.get_apps_for_mime(mime, true);
                available_apps.retain(|(app, _)| {
                    app.name
                        .to_lowercase()
                        .trim()
                        .contains(search_app_name.to_lowercase().as_str().trim())
                });
                let item_height = 32.0;
                let mut displayed_default = false;
                let mut last_kind = MimeAppMatch::Exact;
                for (i, &(app, kind)) in available_apps.iter().enumerate() {
                    if kind != last_kind {
                        match kind {
                            MimeAppMatch::Related => {
                                column = column.add(widget::text::heading(fl!("related-apps")));
                            }
                            MimeAppMatch::Other => {
                                column = column.add(widget::text::heading(fl!("other-apps")));
                            }
                            _ => {}
                        }
                        last_kind = kind;
                    }
                    column = column.add(
                        // `iced`'s `MouseArea` has no `on_double_press`; this
                        // crate's vendored `mouse_area` has `on_double_click`.
                        crate::mouse_area::MouseArea::new(
                            widget::button::custom(
                                widget::Row::with_children([
                                    icon(app.icon()).size(32).into(),
                                    if app.is_default(mime) && !displayed_default {
                                        displayed_default = true;
                                        widget::text::body(fl!(
                                            "default-app",
                                            name = Some(app.name.as_str())
                                        ))
                                        .into()
                                    } else {
                                        widget::text::body(app.name.clone()).into()
                                    },
                                    widget::space::horizontal().into(),
                                    if *selected == i {
                                        icon::from_name("checkbox-checked-symbolic").size(16).into()
                                    } else {
                                        widget::space::horizontal()
                                            .width(Length::Fixed(16.0))
                                            .into()
                                    },
                                ])
                                .spacing(space_s.to_pixels())
                                .height(Length::Fixed(item_height))
                                .align_y(Alignment::Center),
                            )
                            .width(Length::Fill)
                            .class(Button::MenuItem)
                            .force_enabled(true),
                        )
                        .on_press(move |_| Message::OpenWithSelection(i))
                        .on_double_click(|_| Message::DialogComplete),
                    );
                }

                let mut dialog = widget::dialog()
                    .title(fl!("open-with-title", name = name))
                    .primary_action(
                        widget::button::suggested(fl!("open"))
                            .on_press(Message::DialogComplete)
                            .id(CONFIRM_OPEN_WITH_BUTTON_ID.clone()),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    )
                    .control(
                        widget::text_input::search_input(
                            fl!("search-application"),
                            search_app_name,
                        )
                        .id(self.dialog_text_input.clone())
                        .on_clear(Message::OpenWithSearchClear)
                        .on_input(move |search_app_name| {
                            Message::DialogUpdate(DialogPage::OpenWith {
                                path: path.clone(),
                                mime: mime.clone(),
                                selected: *selected,
                                store_opt: store_opt.clone(),
                                search_app_name,
                            })
                        })
                        .on_submit(|_| Message::DialogComplete),
                    )
                    .control(widget::scrollable(column).height({
                        let max_size = self
                            .size
                            .map_or(480.0, |size| (size.height - 256.0).min(480.0));
                        // (32 (item_height) + 5.0 (custom button padding)) + (space_xxs (list item spacing) * 2)
                        let scrollable_height = available_apps.len() as f32
                            * f32::from(space_xxs).mul_add(2.0, item_height + 5.0);

                        if scrollable_height > max_size {
                            Length::Fixed(max_size)
                        } else {
                            Length::Shrink
                        }
                    }));

                if let Some(app) = store_opt {
                    dialog = dialog.tertiary_action(
                        widget::button::text(fl!("browse-store", store = app.name.as_str()))
                            .on_press(Message::OpenWithBrowse),
                    );
                }

                dialog
            }
            DialogPage::PermanentlyDelete { paths } => {
                let target = if paths.len() == 1 {
                    format!(
                        "\"{}\"",
                        paths[0].file_name().map_or_else(
                            || paths[0].to_string_lossy(),
                            std::ffi::OsStr::to_string_lossy
                        )
                    )
                } else {
                    fl!("selected-items", items = paths.len())
                };

                widget::dialog()
                    .title(fl!("permanently-delete-question"))
                    .primary_action(
                        widget::button::destructive(fl!("delete"))
                            .on_press(Message::DialogComplete)
                            .id(PERMANENT_DELETE_BUTTON_ID.clone()),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    )
                    .control(widget::text(fl!(
                        "permanently-delete-warning",
                        target = target
                    )))
            }
            DialogPage::DeleteTrash { items } => {
                let target = if items.len() == 1 {
                    format!("\"{}\"", items[0].name.to_string_lossy())
                } else {
                    fl!("selected-items", items = items.len())
                };

                widget::dialog()
                    .title(fl!("permanently-delete-question"))
                    .primary_action(
                        widget::button::destructive(fl!("delete"))
                            .on_press(Message::DialogComplete)
                            .id(DELETE_TRASH_BUTTON_ID.clone()),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    )
                    .control(widget::text(fl!(
                        "permanently-delete-warning",
                        target = target
                    )))
            }
            DialogPage::ChangeSidebarLabel { entity, label } => {
                let entity = *entity;
                let complete_maybe = if label.trim().is_empty() {
                    None
                } else {
                    Some(Message::DialogComplete)
                };

                widget::dialog()
                    .title(fl!("change-sidebar-label"))
                    .primary_action(
                        widget::button::suggested(fl!("save"))
                            .on_press_maybe(complete_maybe.clone()),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    )
                    .control(
                        widget::Column::with_children([
                            widget::text::body(fl!("sidebar-label")).into(),
                            widget::text_input("", label.as_str())
                                .id(self.dialog_text_input.clone())
                                .on_input(move |label| {
                                    Message::DialogUpdate(DialogPage::ChangeSidebarLabel {
                                        entity,
                                        label,
                                    })
                                })
                                .on_submit_maybe(complete_maybe.map(|maybe| move |_| maybe.clone()))
                                .into(),
                        ])
                        .spacing(space_xxs.to_pixels()),
                    )
            }
            DialogPage::RenameItem {
                from,
                parent,
                name,
                dir,
            } => self.name_dialog(
                if *dir {
                    fl!("rename-folder")
                } else {
                    fl!("rename-file")
                },
                fl!("rename-confirm"),
                *dir,
                parent,
                name,
                Some(from),
                Some('.'),
                move |name| {
                    Message::DialogUpdate(DialogPage::RenameItem {
                        from: from.clone(),
                        parent: parent.clone(),
                        name,
                        dir: *dir,
                    })
                },
            ),
            DialogPage::BatchRename {
                parent,
                names,
                settings,
            } => {
                use batch_rename::{Mode, Settings};

                let tags = batch_rename::Tags::localized();
                let preview = &self.batch_rename_preview;
                let update = |settings: Settings| {
                    Message::DialogUpdate(DialogPage::BatchRename {
                        parent: parent.clone(),
                        names: names.clone(),
                        settings,
                    })
                };

                let modes = widget::Row::with_children([
                    widget::radio(
                        fl!("batch-rename-template"),
                        Mode::Template,
                        Some(settings.mode),
                        |mode| {
                            update(Settings {
                                mode,
                                ..settings.clone()
                            })
                        },
                    )
                    .into(),
                    widget::radio(
                        fl!("batch-rename-find-replace"),
                        Mode::Replace,
                        Some(settings.mode),
                        |mode| {
                            update(Settings {
                                mode,
                                ..settings.clone()
                            })
                        },
                    )
                    .into(),
                ])
                .spacing(space_m.to_pixels());

                let fields: Element<'_, Message> = match settings.mode {
                    Mode::Template => {
                        let add_tag = |tag: &str| {
                            update(Settings {
                                template: format!("{}{tag}", settings.template),
                                ..settings.clone()
                            })
                        };
                        widget::Column::with_children([
                            widget::text_input("", settings.template.as_str())
                                .label(fl!("batch-rename-new-name"))
                                .id(self.dialog_text_input.clone())
                                .on_input(move |template| {
                                    update(Settings {
                                        template,
                                        ..settings.clone()
                                    })
                                })
                                .into(),
                            widget::Row::with_children([
                                widget::button::standard(fl!("batch-rename-add-name"))
                                    .on_press(add_tag(&tags.name))
                                    .into(),
                                widget::button::standard(fl!("batch-rename-add-number"))
                                    .on_press(add_tag(&tags.number))
                                    .into(),
                            ])
                            .spacing(space_xxs.to_pixels())
                            .into(),
                        ])
                        .spacing(space_xxs.to_pixels())
                        .into()
                    }
                    Mode::Replace => widget::Column::with_children([
                        widget::text_input("", settings.find.as_str())
                            .label(fl!("find"))
                            .id(self.dialog_text_input.clone())
                            .on_input(move |find| {
                                update(Settings {
                                    find,
                                    ..settings.clone()
                                })
                            })
                            .into(),
                        widget::text_input("", settings.replace.as_str())
                            .label(fl!("replace-with"))
                            .on_input(move |replace| {
                                update(Settings {
                                    replace,
                                    ..settings.clone()
                                })
                            })
                            .into(),
                    ])
                    .spacing(space_xxs.to_pixels())
                    .into(),
                };

                let conflict_color = crate::ui::theme::active()
                    .cosmic()
                    .destructive_color()
                    .to_color();
                let mut rows = widget::Column::with_capacity(preview.rows.len())
                    .spacing(space_xxxs.to_pixels());
                for row in &preview.rows {
                    let mut new = widget::text::body(row.new.clone());
                    if row.conflict {
                        new = new.class(crate::ui::theme::Text::Color(conflict_color));
                    }
                    rows = rows.push(
                        widget::Row::with_children([
                            widget::text::body(row.old.clone())
                                .width(Length::FillPortion(1))
                                .into(),
                            new.width(Length::FillPortion(1)).into(),
                        ])
                        .spacing(space_s.to_pixels()),
                    );
                }

                let mut dialog = widget::dialog()
                    .title(fl!("batch-rename-title", count = names.len()))
                    .control(modes)
                    .control(fields)
                    .control(widget::scrollable(rows).height(Length::Fixed(240.0)))
                    .primary_action(
                        widget::button::suggested(fl!("rename-confirm"))
                            .on_press_maybe(preview.ready().then_some(Message::DialogComplete)),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    );
                if preview.conflicts > 0 {
                    dialog = dialog.tertiary_action(widget::text::body(fl!(
                        "batch-rename-conflicts",
                        count = preview.conflicts
                    )));
                }
                dialog
            }
            DialogPage::Replace {
                from,
                to,
                multiple,
                apply_to_all,
                conflict_count,
                tx,
            } => {
                let dialog = widget::dialog()
                    .title(fl!("replace-title", filename = to.name.as_str()))
                    .body(fl!("replace-warning-operation"))
                    .control(
                        to.replace_view(fl!("original-file"))
                            .map(|x| Message::TabMessage(None, x)),
                    )
                    .control(
                        from.replace_view(fl!("replace-with"))
                            .map(|x| Message::TabMessage(None, x)),
                    )
                    .primary_action(
                        widget::button::suggested(fl!("replace"))
                            .on_press(Message::ReplaceResult(ReplaceResult::Replace(
                                *apply_to_all,
                            )))
                            .id(REPLACE_BUTTON_ID.clone()),
                    );
                if *multiple {
                    dialog
                        .control(
                            widget::checkbox(*apply_to_all)
                                .label(format!("{} ({})", fl!("apply-to-all"), *conflict_count))
                                .on_toggle(|apply_to_all| {
                                    Message::DialogUpdate(DialogPage::Replace {
                                        from: from.clone(),
                                        to: to.clone(),
                                        multiple: *multiple,
                                        apply_to_all,
                                        conflict_count: *conflict_count,
                                        tx: tx.clone(),
                                    })
                                }),
                        )
                        .secondary_action(
                            widget::button::standard(fl!("skip")).on_press(Message::ReplaceResult(
                                ReplaceResult::Skip(*apply_to_all),
                            )),
                        )
                        .tertiary_action(
                            widget::button::text(fl!("cancel"))
                                .on_press(Message::ReplaceResult(ReplaceResult::Cancel)),
                        )
                } else {
                    dialog
                        .secondary_action(
                            widget::button::standard(fl!("cancel"))
                                .on_press(Message::ReplaceResult(ReplaceResult::Cancel)),
                        )
                        .tertiary_action(
                            widget::button::text(fl!("keep-both"))
                                .on_press(Message::ReplaceResult(ReplaceResult::KeepBoth)),
                        )
                }
            }
            DialogPage::SetExecutableAndLaunch { path } => {
                let name = match path.file_name() {
                    Some(file_name) => file_name.to_str(),
                    None => path.as_os_str().to_str(),
                };
                widget::dialog()
                    .title(fl!("set-executable-and-launch"))
                    .primary_action(
                        widget::button::text(fl!("set-and-launch"))
                            .class(Button::Suggested)
                            .on_press(Message::DialogComplete)
                            .id(SET_EXECUTABLE_AND_LAUNCH_CONFIRM_BUTTON_ID.clone()),
                    )
                    .secondary_action(
                        widget::button::text(fl!("cancel"))
                            .class(Button::Standard)
                            .on_press(Message::DialogCancel),
                    )
                    .control(widget::text::text(fl!(
                        "set-executable-and-launch-description",
                        name = name
                    )))
            }
            DialogPage::LaunchDesktopEntry { path, command } => {
                let name = path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .into_owned();
                widget::dialog()
                    .title(fl!("launch-desktop-entry"))
                    .icon(icon::from_name("dialog-warning").size(64))
                    .body(fl!("launch-desktop-entry-description", name = name))
                    .control(widget::text::monotext(command.clone()))
                    .primary_action(
                        widget::button::text(fl!("launch-anyway"))
                            .class(Button::Destructive)
                            .on_press(Message::DialogComplete)
                            .id(LAUNCH_DESKTOP_ENTRY_CONFIRM_BUTTON_ID.clone()),
                    )
                    .secondary_action(
                        widget::button::text(fl!("cancel"))
                            .class(Button::Standard)
                            .on_press(Message::DialogCancel),
                    )
            }
            DialogPage::FavoritePathError { path, .. } => widget::dialog()
                .title(fl!("favorite-path-error"))
                .body(fl!(
                    "favorite-path-error-description",
                    path = path.as_os_str().to_str()
                ))
                .icon(icon::from_name("dialog-error").size(64))
                .primary_action(
                    widget::button::destructive(fl!("remove"))
                        .on_press(Message::DialogComplete)
                        .id(FAVORITE_PATH_ERROR_REMOVE_BUTTON_ID.clone()),
                )
                .secondary_action(
                    widget::button::standard(fl!("keep")).on_press(Message::DialogCancel),
                ),
        };
        Some(dialog.into())
    }

    fn footer(&self) -> Option<Element<'_, Message>> {
        if self.progress_operations.is_empty() {
            return None;
        }

        let Spacing {
            space_xs, space_s, ..
        } = spacing();

        let mut title = String::new();
        let mut total_progress = 0.0;
        let mut count = 0;
        let mut all_paused = true;
        for (op, controller) in self.pending_operations.values() {
            if !controller.is_paused() {
                all_paused = false;
            }
            if op.show_progress_notification() {
                let progress = controller.progress();
                if title.is_empty() {
                    title = op.pending_text(progress, controller.state());
                }
                total_progress += progress;
                count += 1;
            }
        }
        let running = count;
        // Adjust the progress bar so it does not jump around when operations finish
        for id in &self.progress_operations {
            if self.complete_operations.contains_key(id) {
                total_progress += 1.0;
                count += 1;
            }
        }
        let finished = count - running;
        total_progress /= count as f32;
        if running >= 1 && (running > 1 || finished > 0) {
            if finished > 0 {
                title = fl!(
                    "operations-running-finished",
                    running = running,
                    finished = finished,
                    percent = ((total_progress * 100.0) as i32)
                );
            } else {
                title = fl!(
                    "operations-running",
                    running = running,
                    percent = ((total_progress * 100.0) as i32)
                );
            }
        }

        let progress_bar_height = Length::Fixed(4.0);
        let progress_bar = widget::determinate_linear(total_progress)
            .width(Length::Fill)
            .girth(progress_bar_height);

        let container = widget::layer_container(widget::Column::with_children([
            widget::Row::with_children([
                progress_bar.into(),
                if all_paused {
                    widget::tooltip(
                        widget::button::icon(icon::from_name("media-playback-start-symbolic"))
                            .on_press(Message::PendingPauseAll(false))
                            .padding(8),
                        widget::text::body(fl!("resume")),
                        widget::tooltip::Position::Top,
                    )
                    .into()
                } else {
                    widget::tooltip(
                        widget::button::icon(icon::from_name("media-playback-pause-symbolic"))
                            .on_press(Message::PendingPauseAll(true))
                            .padding(8),
                        widget::text::body(fl!("pause")),
                        widget::tooltip::Position::Top,
                    )
                    .into()
                },
                widget::tooltip(
                    widget::button::icon(icon::from_name("window-close-symbolic"))
                        .on_press(Message::PendingCancelAll)
                        .padding(8),
                    widget::text::body(fl!("cancel")),
                    widget::tooltip::Position::Top,
                )
                .into(),
            ])
            .align_y(Alignment::Center)
            .into(),
            widget::text::body(title).into(),
            widget::space::vertical().height(space_s.to_length()).into(),
            widget::Row::with_children([
                widget::button::link(fl!("details"))
                    .on_press(Message::ToggleContextPage(ContextPage::EditHistory))
                    .padding(0)
                    .trailing_icon(true)
                    .into(),
                widget::space::horizontal().into(),
                widget::button::standard(fl!("dismiss"))
                    .on_press(Message::PendingDismiss)
                    .into(),
            ])
            .align_y(Alignment::Center)
            .into(),
        ]))
        .padding([8, space_xs])
        .layer(Layer::Primary);

        Some(container.into())
    }

    fn header_start(&self) -> Vec<Element<'_, Self::Message>> {
        vec![menu::menu_bar(
            &self.core,
            self.tab_model.active_data::<Tab>(),
            &self.config,
            &self.modifiers,
            &self.key_binds,
            self.clipboard_has_content(),
            !self.undo_stack.is_empty(),
        )]
    }

    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        let mut elements = Vec::with_capacity(2);

        if let Some(term) = self.search_get() {
            if self.core.is_condensed() {
                elements.push(
                    widget::button::icon(icon::from_name("system-search-symbolic"))
                        .on_press(Message::SearchClear)
                        .padding(8)
                        .selected(true)
                        .into(),
                );
            } else {
                elements.push(
                    widget::text_input::search_input("", term)
                        .width(Length::Fixed(240.0))
                        .id(self.search_id.clone())
                        .on_clear(Message::SearchClear)
                        .on_input(Message::SearchInput)
                        .into(),
                );
            }
        } else {
            elements.push(
                widget::button::icon(icon::from_name("system-search-symbolic"))
                    .on_press(Message::SearchActivate)
                    .padding(8)
                    .into(),
            );
        }

        elements
    }

    /// Creates a view after each update.
    fn view(&self) -> Element<'_, Self::Message> {
        let Spacing {
            space_xxs, space_s, ..
        } = spacing();

        let mut tab_column = widget::Column::with_capacity(4);

        if self.core.is_condensed()
            && let Some(term) = self.search_get()
        {
            tab_column = tab_column.push(
                widget::container(
                    widget::text_input::search_input("", term)
                        .width(Length::Fill)
                        .id(self.search_id.clone())
                        .on_clear(Message::SearchClear)
                        .on_input(Message::SearchInput),
                )
                .padding(space_xxs),
            );
        }

        if self.tab_model.len() > 1 {
            tab_column = tab_column.push(
                widget::container(
                    widget::tab_bar::horizontal(&self.tab_model)
                        .button_height(32)
                        .button_spacing(space_xxs)
                        // Drag-to-reorder tabs: segmented_button starts a drag with
                        // this mime. A distinct feature from file drag-and-drop.
                        .enable_tab_drag(String::from("x-earth-files/tab-drag"))
                        .on_reorder(Message::ReorderTab)
                        .tab_drag_threshold(25.)
                        // Files dropped on a tab go to that tab's directory.
                        // `Drag::files` distinguishes file drags from tab drags, so
                        // this callback and `on_reorder` cannot both fire.
                        .on_file_drop({
                            let active = self.tab_model.active();
                            let droppable: Vec<Entity> = self
                                .tab_model
                                .iter()
                                .filter(|entity| {
                                    *entity != active
                                        && self.tab_model.data::<Tab>(*entity).is_some_and(|tab| {
                                            tab.location.supports_paste()
                                                && tab.location.path_opt().is_some()
                                        })
                                })
                                .collect();
                            move |entity| {
                                droppable
                                    .contains(&entity)
                                    .then(|| Message::TabDrop(entity))
                            }
                        })
                        .on_activate(Message::TabActivate)
                        .on_close(|entity| Message::TabClose(Some(entity))),
                )
                .width(Length::Fill)
                .padding([0, space_s]),
            );
        }

        let entity = self.tab_model.active();
        if let Some(tab) = self.tab_model.data::<Tab>(entity) {
            let tab_view = tab
                .view(
                    &self.key_binds,
                    &self.modifiers,
                    self.clipboard_has_content(),
                    &self.config.context_actions,
                )
                .map(move |message| Message::TabMessage(Some(entity), message));
            tab_column = tab_column.push(tab_view);
        }

        // The toaster is added on top of an empty element to ensure that it does not override context menus
        tab_column = tab_column.push(widget::toaster(&self.toasts, widget::space::horizontal()));

        let content: Element<_> = tab_column.into();

        // Uncomment to debug layout:
        //content.explain(crate::ui::iced::Color::WHITE)
        content
    }

    fn view_window(&self, id: WindowId) -> Element<'_, Self::Message> {
        let content = match self.windows.get(&id) {
            Some(window) => match &window.kind {
                WindowKind::Dialogs(id) => match self.dialog() {
                    Some(element) => return widget::autosize::autosize(element, id.clone()).into(),
                    None => widget::space::horizontal().into(),
                },
                WindowKind::Preview(entity_opt, kind) => self
                    .preview(entity_opt, kind, false)
                    .map(|x| Message::TabMessage(*entity_opt, x)),
                WindowKind::FileDialog(..) => match &self.file_dialog_opt {
                    Some(dialog) => return dialog.view(id),
                    None => widget::text("Unknown window ID").into(),
                },
            },
            None => {
                return self.view_main().map(|message| match message {
                    crate::ui::Action::App(app) => app,
                    crate::ui::Action::Cosmic(cosmic) => Message::Cosmic(cosmic),
                    crate::ui::Action::Surface(action) => Message::Surface(action),
                    // Intercepted by `TryInto` before the daemon ever calls
                    // `update`, exactly as in `ui::shell::runner::update`;
                    // this arm exists only because the match must be total.
                    crate::ui::Action::Exwl(_) | crate::ui::Action::None => Message::None,
                });
            }
        };

        widget::container(widget::id_container(
            widget::scrollable(content),
            widget::Id::new("main container for files"),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .class(Container::WindowBackground)
        .into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        struct WatcherSubscription;
        struct TrashWatcherSubscription;
        struct RecentsWatcherSubscription;

        let mut subscriptions = vec![
            // Focus, close and resize events carry no window id here.
            // Filter them by window id if a multi-window bug shows up.
            event::listen_with(|event, status, window_id| match event {
                Event::Keyboard(KeyEvent::KeyPressed {
                    key,
                    physical_key,
                    modifiers,
                    text,
                    ..
                }) => match status {
                    event::Status::Ignored => {
                        Some(Message::Key(window_id, modifiers, key, physical_key, text))
                    }
                    event::Status::Captured => None,
                },
                Event::Keyboard(KeyEvent::ModifiersChanged(modifiers)) => {
                    Some(Message::ModifiersChanged(window_id, modifiers))
                }
                Event::Window(WindowEvent::Focused) => Some(Message::CheckClipboard),
                Event::Window(WindowEvent::CloseRequested) => Some(Message::WindowClose),
                Event::Window(WindowEvent::Opened { position: _, size }) => {
                    Some(Message::Size(window_id, size))
                }
                Event::Window(WindowEvent::Resized(s)) => Some(Message::Size(window_id, s)),
                _ => None,
            }),
            self.config_handler
                .subscription::<Config>()
                .map(Message::Config),
            Subscription::run_with(TypeId::of::<WatcherSubscription>(), |_| {
                stream::channel(
                    100,
                    |mut output: futures::channel::mpsc::Sender<Message>| async move {
                        let watcher_res = {
                            let mut output = output.clone();
                            new_debouncer(
                                time::Duration::from_millis(250),
                                Some(time::Duration::from_millis(250)),
                                move |events_res: notify_debouncer_full::DebounceEventResult| {
                                    match events_res {
                                        Ok(mut events) => {
                                            log::debug!("{events:?}");

                                            events.retain(|event| {
                                            match &event.kind {
                                                notify::EventKind::Access(_) => {
                                                    // Data not mutated
                                                    false
                                                }
                                                notify::EventKind::Modify(
                                                    notify::event::ModifyKind::Metadata(e),
                                                ) if (*e != notify::event::MetadataKind::Any
                                                    && *e
                                                        != notify::event::MetadataKind::WriteTime) =>
                                                {
                                                    // Data not mutated nor modify time changed
                                                    false
                                                }
                                                _ => true
                                            }
                                        });

                                            if !events.is_empty() {
                                                match futures::executor::block_on(async {
                                                    output.send(Message::NotifyEvents(events)).await
                                                }) {
                                                    Ok(()) => {}
                                                    Err(err) => {
                                                        log::warn!(
                                                            "failed to send notify events: {err:?}"
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        Err(err) => {
                                            log::warn!("failed to watch files: {err:?}");
                                        }
                                    }
                                },
                            )
                        };

                        match watcher_res {
                            Ok(watcher) => {
                                match output
                                    .send(Message::NotifyWatcher(WatcherWrapper {
                                        watcher_opt: Some(watcher),
                                    }))
                                    .await
                                {
                                    Ok(()) => {}
                                    Err(err) => {
                                        log::warn!("failed to send notify watcher: {err:?}");
                                    }
                                }
                            }
                            Err(err) => {
                                log::warn!("failed to create file watcher: {err:?}");
                            }
                        }

                        std::future::pending().await
                    },
                )
            }),
            #[cfg(feature = "desktop")]
            Subscription::run(|| {
                stream::channel(
                    1,
                    |mut output: futures::channel::mpsc::Sender<Message>| async move {
                        mime_app::watch(move || {
                            futures::executor::block_on(async {
                                _ = output.send(Message::ReloadMimeAppCache).await;
                            })
                        })
                        .await;

                        std::future::pending().await
                    },
                )
            }),
            Subscription::run_with(TypeId::of::<TrashWatcherSubscription>(), |_| {
                stream::channel(
                    1,
                    |mut output: futures::channel::mpsc::Sender<Message>| async move {
                        let watcher_res = new_debouncer(
                            time::Duration::from_millis(250),
                            Some(time::Duration::from_millis(250)),
                            move |event_res: notify_debouncer_full::DebounceEventResult| {
                                match event_res {
                                    Ok(events) => {
                                        // Rescan on any event. We don't need to evaluate each event
                                        // because as long as the trash changed in any way we need to
                                        // rescan.
                                        let should_rescan =
                                            events.iter().any(|event| !event.kind.is_access());

                                        if should_rescan
                                            && let Err(e) = futures::executor::block_on(async {
                                                output.send(Message::RescanTrash).await
                                            })
                                        {
                                            log::warn!(
                                                "trash needs to be rescanned but sending message failed: {e:?}"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!("failed to watch trash bin for changes: {e:?}");
                                    }
                                }
                            },
                        );

                        match (watcher_res, Trash::folders()) {
                            (Ok(mut watcher), Ok(trash_bins)) => {
                                // Watch the "bins" themselves as well as the files folder where
                                // trashed items are placed. This allows us to avoid recursively
                                // watching the trash which is slow but also properly get events.
                                let trash_paths = trash_bins
                                    .into_iter()
                                    .flat_map(|path| [path.join("files"), path]);
                                for path in trash_paths {
                                    if let Err(e) =
                                        watcher.watch(&path, notify::RecursiveMode::NonRecursive)
                                    {
                                        log::warn!(
                                            "failed to add trash bin `{}` to watcher: {e:?}",
                                            path.display()
                                        );
                                    }
                                }

                                // Don't drop the watcher
                                std::future::pending().await
                            }
                            (Err(e), _) => {
                                log::warn!("failed to create new watcher for trash bin: {e:?}");
                            }
                            (_, Err(e)) => {
                                log::warn!("could not find any valid trash bins to watch: {e:?}");
                            }
                        }

                        std::future::pending().await
                    },
                )
            }),
            Subscription::run_with(TypeId::of::<RecentsWatcherSubscription>(), |_| {
                stream::channel(
                    1,
                    |mut output: futures::channel::mpsc::Sender<Message>| async move {
                        let Some(recents_path) = recently_used_xbel::dir() else {
                            log::warn!(
                                "failed to watch recents changes: .recently_used.xbel does not exist"
                            );
                            return std::future::pending().await;
                        };

                        let watcher_res = new_debouncer(
                            time::Duration::from_millis(250),
                            Some(time::Duration::from_millis(250)),
                            move |event_res: notify_debouncer_full::DebounceEventResult| {
                                match event_res {
                                    Ok(events) => {
                                        // Programs differ in how they modify the recents file so the
                                        // rescan is triggered on any event but access.
                                        if events.iter().any(|event| {
                                            let kind = event.kind;
                                            kind.is_create()
                                                || kind.is_modify()
                                                || kind.is_remove()
                                                || kind.is_other()
                                        }) && let Err(e) = futures::executor::block_on(async {
                                            output.send(Message::RescanRecents).await
                                        }) {
                                            log::warn!(
                                                "open recents tabs need to be updated but sending message failed: {e:?}"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "failed to watch recents file for changes: {e:?}"
                                        )
                                    }
                                }
                            },
                        );

                        match watcher_res {
                            Ok(mut watcher) => {
                                if let Err(e) = watcher
                                    .watch(&recents_path, notify::RecursiveMode::NonRecursive)
                                {
                                    log::warn!(
                                        "failed to add recents file `{}` to watcher: {}",
                                        recents_path.display(),
                                        e
                                    );
                                }

                                // Don't drop the watcher.
                                std::future::pending::<()>().await;
                            }
                            Err(e) => {
                                log::warn!("failed to create new watcher for recents file: {e:?}")
                            }
                        }

                        std::future::pending().await
                    },
                )
            }),
        ];

        if let Some(scroll_speed) = self.auto_scroll_speed {
            subscriptions.push(
                iced::time::every(time::Duration::from_millis(10))
                    .with(scroll_speed)
                    .map(|(scroll_speed, _)| Message::ScrollTab(scroll_speed)),
            );
        }

        subscriptions.extend(MOUNTERS.iter().map(|(key, mounter)| {
            mounter
                .subscription()
                .with(*key)
                .map(|(key, mounter_message)| match mounter_message {
                    MounterMessage::Items(items) => Message::MounterItems(key, items),
                    MounterMessage::MountResult(item, res) => Message::MountResult(key, item, res),
                    MounterMessage::NetworkAuth(uri, auth, auth_tx) => {
                        Message::NetworkAuth(key, uri, auth, auth_tx)
                    }
                    MounterMessage::NetworkResult(uri, res) => {
                        Message::NetworkResult(key, uri, res)
                    }
                })
        }));

        // The embedded file chooser runs its own shell, and nothing else
        // drives it: without this it gets no key handling, no Escape, and no
        // watcher or config updates
        if let Some(dialog) = &self.file_dialog_opt {
            subscriptions.push(dialog.subscription().map(Message::FileDialogMessage));
        }

        if !self.pending_operations.is_empty() {
            // Hold a logind lock for as long as operations are pending: the
            // subscription ends when the last one finishes, which drops the fd
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
            struct InhibitSubscription;
            subscriptions.push(Subscription::run_with(
                TypeId::of::<InhibitSubscription>(),
                |_| {
                    stream::channel(
                        1,
                        |_output: futures::channel::mpsc::Sender<Message>| async move {
                            let _lock = crate::inhibit::block_sleep_and_shutdown(&fl!(
                                "notification-in-progress"
                            ))
                            .await;
                            futures::future::pending::<()>().await;
                        },
                    )
                },
            ));

            if self.core.main_window_id().is_some() {
                // Force refresh the UI every 100ms while an operation is active.
                if self
                    .pending_operations
                    .values()
                    .any(|(_, controller)| !controller.is_paused())
                {
                    subscriptions.push(
                        crate::ui::iced::time::every(Duration::from_millis(100))
                            .map(|_| Message::None),
                    );
                }
            } else {
                // Handle notification when window is closed and operations are in progress
                #[cfg(feature = "notify")]
                {
                    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
                    struct NotificationSubscription;
                    subscriptions.push(Subscription::run_with(
                        TypeId::of::<NotificationSubscription>(),
                        |_| {
                            stream::channel(
                                1,
                                move |mut msg_tx: futures::channel::mpsc::Sender<_>| async move {
                                    tokio::task::spawn_blocking(move || {
                                        match notify_rust::Notification::new()
                                            .summary(&fl!("notification-in-progress"))
                                            .timeout(notify_rust::Timeout::Never)
                                            .show()
                                        {
                                            Ok(notification) => {
                                                let _ = futures::executor::block_on(async {
                                                    msg_tx
                                                        .send(Message::Notification(Arc::new(
                                                            Mutex::new(notification),
                                                        )))
                                                        .await
                                                });
                                            }
                                            Err(err) => {
                                                log::warn!("failed to create notification: {err}");
                                            }
                                        }
                                    })
                                    .await
                                    .unwrap();

                                    std::future::pending().await
                                },
                            )
                        },
                    ));
                }
            }
        }

        let mut selected_previews = Vec::new();
        if self.core.window.show_context
            && let ContextPage::Preview(entity_opt, PreviewKind::Selected) = self.context_page
        {
            selected_previews.push(Some(entity_opt.unwrap_or_else(|| self.tab_model.active())));
        }

        subscriptions.extend(self.tab_model.iter().filter_map(|entity| {
            let tab = self.tab_model.data::<Tab>(entity)?;
            Some(
                tab.subscription(
                    selected_previews
                        .iter()
                        .any(|preview| preview.as_ref() == Some(entity).as_ref()),
                )
                .with(entity)
                .map(|(entity, tab_msg)| Message::TabMessage(Some(entity), tab_msg)),
            )
        }));

        Subscription::batch(subscriptions)
    }
}

// Utilities to build a temporary file hierarchy for tests.
//
// Ideally, tests would use the cap-std crate which limits path traversal.
#[cfg(test)]
pub(crate) mod test_utils {
    use std::cmp::Ordering;
    use std::fs;
    use std::fs::File;
    use std::io::{self, Write};
    use std::iter;
    use std::path::Path;

    use log::{debug, trace};
    use tempfile::{TempDir, tempdir};

    use crate::config::{IconSizes, TabConfig, ThumbCfg};
    use crate::tab::Item;

    use super::*;

    // Default number of files, directories, and nested directories for test file system
    pub const NUM_FILES: usize = 2;
    pub const NUM_HIDDEN: usize = 1;
    pub const NUM_DIRS: usize = 2;
    pub const NUM_NESTED: usize = 1;
    pub const NAME_LEN: usize = 5;

    /// Add `n` temporary files in `dir`
    ///
    /// Each file is assigned a numeric name from [0, n) with a prefix.
    pub fn file_flat_hier<D: AsRef<Path>>(dir: D, n: usize, prefix: &str) -> io::Result<Vec<File>> {
        let dir = dir.as_ref();
        (0..n)
            .map(|i| -> io::Result<File> {
                let name = format!("{prefix}{i}");
                let path = dir.join(&name);

                let mut file = File::create(path)?;
                file.write_all(name.as_bytes())?;

                Ok(file)
            })
            .collect()
    }

    // Random alphanumeric String of length `len`
    fn rand_string(len: usize) -> String {
        let mut rng = fastrand::Rng::new();
        iter::repeat_with(|| rng.alphanumeric()).take(len).collect()
    }

    /// Create a small, temporary file hierarchy.
    ///
    /// # Arguments
    ///
    /// * `files` - Number of files to create in temp directories
    /// * `hidden` - Number of hidden files to create
    /// * `dirs` - Number of directories to create
    /// * `nested` - Number of nested directories to create in new dirs
    /// * `name_len` - Length of randomized directory names
    pub fn simple_fs(
        files: usize,
        hidden: usize,
        dirs: usize,
        nested: usize,
        name_len: usize,
    ) -> io::Result<TempDir> {
        // Files created inside of a TempDir are deleted with the directory
        // TempDir won't leak resources as long as the destructor runs
        let root = tempdir()?;
        debug!("Root temp directory: {}", root.as_ref().display());
        trace!(
            "Creating {files} files and {hidden} hidden files in {dirs} temp dirs with {nested} nested temp dirs"
        );

        // All paths for directories and nested directories
        let paths = iter::repeat_with(|| {
            let root = root.as_ref();
            let current = rand_string(name_len);

            iter::once(root.join(&current)).chain(
                iter::repeat_with(move || {
                    let mut path = root.join(&current);
                    path.push(rand_string(name_len));
                    path
                })
                .take(nested),
            )
        })
        .take(dirs)
        .flatten();

        // Create directories from `paths` and add a few files
        for path in paths {
            fs::create_dir_all(&path)?;

            // Normal files
            file_flat_hier(&path, files, "")?;
            // Hidden files
            file_flat_hier(&path, hidden, ".")?;

            for entry in path.read_dir()? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    trace!("Created file: {}", entry.path().display());
                }
            }
        }

        Ok(root)
    }

    /// Empty file hierarchy
    pub fn empty_fs() -> io::Result<TempDir> {
        tempdir()
    }

    /// Sort files.
    ///
    /// Directories are placed before files.
    /// Files are lexically sorted.
    /// This is more or less copied right from the [Tab] code
    pub fn sort_files(a: &Path, b: &Path) -> Ordering {
        match (a.is_dir(), b.is_dir()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => LANGUAGE_SORTER.compare(
                a.file_name()
                    .expect("temp entries should have names")
                    .to_str()
                    .expect("temp entries should be valid UTF-8"),
                b.file_name()
                    .expect("temp entries should have names")
                    .to_str()
                    .expect("temp entries should be valid UTF-8"),
            ),
        }
    }

    /// Read directory entries from `path` and sort.
    pub fn read_dir_sorted(path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries: Vec<_> = path
            .read_dir()?
            .map(|maybe_entry| maybe_entry.map(|entry| entry.path()))
            .collect::<io::Result<_>>()?;
        entries.sort_by(|a, b| sort_files(a, b));

        Ok(entries)
    }

    /// Filter `path` for directories
    pub fn filter_dirs(path: &Path) -> io::Result<impl Iterator<Item = PathBuf> + use<>> {
        Ok(path.read_dir()?.filter_map(|entry| {
            entry.ok().and_then(|entry| {
                let path = entry.path();
                path.is_dir().then_some(path)
            })
        }))
    }

    // Filter `path` for files
    pub fn filter_files(path: &Path) -> io::Result<impl Iterator<Item = PathBuf> + use<>> {
        Ok(path.read_dir()?.filter_map(|entry| {
            entry.ok().and_then(|entry| {
                let path = entry.path();
                path.is_file().then_some(path)
            })
        }))
    }

    /// Boiler plate for Tab tests
    pub fn tab_click_new(
        files: usize,
        hidden: usize,
        dirs: usize,
        nested: usize,
        name_len: usize,
    ) -> io::Result<(TempDir, Tab)> {
        let fs = simple_fs(files, hidden, dirs, nested, name_len)?;
        let path = fs.path();

        // New tab with items
        let location = Location::Path(path.to_owned());
        let (parent_item_opt, items) = location.scan(IconSizes::default());
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.parent_item_opt = parent_item_opt;
        tab.set_items(items);

        // Ensure correct number of directories as a sanity check
        let items = tab.items_opt().expect("tab should be populated with Items");
        assert_eq!(NUM_DIRS, items.len());

        Ok((fs, tab))
    }

    /// Equality for [Path] and [Item].
    pub fn eq_path_item(path: &Path, item: &Item) -> bool {
        let name = path
            .file_name()
            .expect("temp entries should have names")
            .to_str()
            .expect("temp entries should be valid UTF-8");
        let is_dir = path.is_dir();

        // NOTE: I don't want to change `tab::hidden_attribute` to `pub(crate)` for
        // tests without asking
        let is_hidden = name.starts_with('.');

        name == item.name
            && is_dir == item.metadata.is_dir()
            && path == item.path_opt().expect("item should have path")
            && is_hidden == item.hidden
    }

    /// Asserts `tab`'s location changed to `path`
    pub fn assert_eq_tab_path(tab: &Tab, path: &Path) {
        // Paths should be the same
        let Some(tab_path) = tab.location.path_opt() else {
            panic!("Expected tab's location to be a path");
        };

        assert_eq!(
            path,
            tab_path,
            "Tab's path is {} instead of being updated to {}",
            tab_path.display(),
            path.display()
        );
    }
}
