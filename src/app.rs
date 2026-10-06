// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::ui::app::{self, Task};
use crate::ui::clipboard;
use crate::ui::iced::futures::{self, SinkExt};
use crate::ui::iced::keyboard::key::Physical;
use crate::ui::iced::keyboard::{Event as KeyEvent, Key, Modifiers};
use crate::ui::iced::window::{self, Event as WindowEvent, Id as WindowId};
use crate::ui::iced::{
    self, Alignment, Event, Length, Rectangle, Size, Subscription, event, stream,
};
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
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{self, Duration, Instant};
use std::{env, fmt, process};
use tokio::sync::mpsc;
use trash::TrashItem;

use crate::action_card::{ActionCard, Shown};
use crate::clipboard::{
    ClipboardCache, ClipboardCopy, ClipboardKind, ClipboardPaste, ClipboardPasteImage,
    ClipboardPasteText, ClipboardPasteVideo,
};
use crate::config::{AppTheme, Config, Favorite, IconSizes, State, Store, TabConfig, TypeToSearch};
use crate::dialog::{Dialog, DialogKind, DialogMessage, DialogResult, DialogSettings};
use crate::exit_gate::{ExitGate, Step};
use crate::key_bind::key_binds;
use crate::localize::LANGUAGE_SORTER;
use crate::mime_app::{self, MimeApp, MimeAppCache, MimeAppMatch};
use crate::mounter::{
    MOUNTERS, MounterAuth, MounterItem, MounterItems, MounterKey, MounterMessage, Outcome,
};
use crate::operation::{
    Ask, Blocked, BlockedAnswer, BlockedQuestion, Controller, ControllerState, Operation,
    OperationError, OperationErrorType, OperationSelection, ReplaceResult,
};
use crate::progress;
use crate::spawn_detached::spawn_detached;
use crate::tab::{
    self, HeadingOptions, ItemMetadata, Location, SORT_OPTION_FALLBACK, SearchLocation, Tab,
};
use crate::trash::{Trash, TrashExt};
use crate::ui::convert::{PushMaybe, ToColor, ToPadding, ToPixels};
use crate::ui::theme::{Button, Container, Spacing, spacing};
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
/// The default answer of the dialog asking about a path an operation may
/// not touch, focused so Enter gives it.
pub(crate) static BLOCKED_BUTTON_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("blocked-button"));

/// The progress notification shown while a closed window's operations run.
/// A `Mutex` because messages are `Clone` and the handle is not.
#[cfg(feature = "notify")]
type ProgressNotice = Arc<Mutex<notify_rust::NotificationHandle>>;
#[cfg(not(feature = "notify"))]
type ProgressNotice = ();

/// A question shown as a desktop notification: its id, and the notification
/// itself, which closing it by that id shows again first.
#[cfg(feature = "notify")]
type QuestionNotice = (u32, notify_rust::Notification);
#[cfg(not(feature = "notify"))]
type QuestionNotice = ();

/// Whether ejecting a drive looks at its trash first, to ask about
/// emptying it: the setting on, and a drive the app trashes on, not a
/// network one.
fn asks_before_ejecting(empty_unmount: bool, remote: bool) -> bool {
    empty_unmount && !remote
}

/// The trash folders there are now, sorted, for the trash watcher.
fn trash_bins() -> Vec<PathBuf> {
    let mut bins: Vec<PathBuf> = Trash::folders()
        .map(|bins| bins.into_iter().collect())
        .unwrap_or_default();
    bins.sort();
    bins
}

/// Whether operation `id`'s notification on record in `notices` is the
/// desktop's `notice`: a late answer or close of an older one is not.
fn notice_on_record(notices: &BTreeMap<u64, NoticeSlot>, id: u64, notice: u32) -> bool {
    matches!(
        notices.get(&id),
        Some(NoticeSlot { shown: Some(shown), .. }) if notice_id(shown) == notice
    )
}

/// An operation's question notification: which showing of it is the one
/// on record, and the notification once the desktop has shown it.
struct NoticeSlot {
    generation: u64,
    shown: Option<QuestionNotice>,
}

/// Takes the notification the desktop just showed for operation `id` as
/// its showing `generation`, if that showing is still the one wanted.
/// Returns it otherwise, to be closed: it came too late, for a question
/// that is gone or a showing since replaced.
fn accept_notice(
    notices: &mut BTreeMap<u64, NoticeSlot>,
    id: u64,
    generation: u64,
    notice: QuestionNotice,
) -> Option<QuestionNotice> {
    match notices.get_mut(&id) {
        Some(slot) if slot.generation == generation && slot.shown.is_none() => {
            slot.shown = Some(notice);
            None
        }
        _ => Some(notice),
    }
}

/// The desktop's id for a question's notification: which notification a
/// late answer or close is about.
#[cfg(feature = "notify")]
fn notice_id(notice: &QuestionNotice) -> u32 {
    notice.0
}
#[cfg(not(feature = "notify"))]
fn notice_id((): &QuestionNotice) -> u32 {
    0
}

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
    Cut(Option<Entity>),
    Delete(Option<Entity>),
    DesktopDialogs(bool),
    DialogCancel,
    DialogComplete,
    Eject,
    /// The trash of the drive about to be ejected was looked at: whether it
    /// holds anything.
    EjectChecked(MounterKey, MounterItem, PathBuf, bool),
    /// Eject the drive asked about without emptying its trash.
    EjectWithoutEmptying,
    /// The trash folders there are now, as found by a worker for the search
    /// with this number.
    TrashBins(u64, Vec<PathBuf>),
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
    /// A drag over the sidebar moved, pinned or unpinned an entry.
    NavDrop(segmented_button::NavDrop),
    /// Undo unpinning `Favorite`, which was at this place among the pinned
    /// entries and Recents; see [`crate::config::sidebar::entries`].
    NavUnpinUndo(widget::ToastId, Favorite, usize),
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
    /// The progress notification is up, or could not be shown.
    #[cfg(feature = "notify")]
    NotificationShown(Option<ProgressNotice>),
    /// The progress notification was taken down.
    #[cfg(feature = "notify")]
    NotificationClosed,
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
    /// Stop operation `id` where it is and keep what it did.
    PendingAbort(u64),
    /// Watches running operations for ones whose drive stopped responding.
    PendingTick,
    /// Keep waiting for operation `id`'s drive: a fresh time limit.
    StalledWait(u64),
    PendingComplete(u64, OperationSelection),
    PendingError(u64, OperationError),
    PendingResults(Vec<(u64, OperationSelection)>, Vec<(u64, OperationError)>),
    PendingPause(u64, bool),
    /// A running operation, known by its controller, cannot touch a path
    /// and waits for what to do on this sender.
    OperationBlocked(Controller, BlockedQuestion, mpsc::Sender<BlockedAnswer>),
    /// The user answered operation `id`'s question.
    BlockedAnswer(u64, BlockedAnswer),
    /// The "Same for the rest" box of operation `id`'s question.
    BlockedSameForRest(u64, bool),
    /// Operation `id`'s question went out as a desktop notification.
    QuestionNotified(u64, u64, QuestionNotice),
    /// Operation `id`'s question was answered on its desktop notification,
    /// which is gone with the answer.
    QuestionNoticeAnswered(u64, u32, BlockedAnswer),
    /// Operation `id`'s question notification, with the desktop's id for
    /// it, closed without an answer: dismissed, or taken down by the app.
    QuestionNoticeClosed(u64, u32),
    /// Bring the question notifications in step with who can see the
    /// window: after a focus change, or a while after one was dismissed.
    QuestionNoticesSync,
    /// A window of ours gained or lost the keyboard.
    WindowFocusChanged(bool),
    /// The pointer came over the progress card's row with this id, or left
    /// it: an ended row is held while it is over it.
    ProgressHover(u64, bool),
    /// Runs what is left of failed operation `id` again, from its row.
    RetryFailed(u64),
    /// A folder load, mount or unmount tracked in the progress card ended,
    /// and how.
    ProgressEnd(progress::Key, Outcome),
    /// Keeps the progress card's rows appearing and leaving on time.
    ProgressTick,
    /// The user closed the progress card's row with this id, which stayed
    /// for the paths its operation skipped.
    ProgressDismiss(u64),
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
    /// With a request number it fills in that loading page and is dropped if
    /// the page has gone; without one it is a fresh dialog.
    OpenWithFor(PathBuf, Mime, Option<u64>),
    /// What a worker found a sidebar entry's type to be, for the loading page
    /// with this request number.
    OpenWithResolved(u64, PathBuf, Result<Mime, String>),
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
    /// The search field has sprung shut after the search ended.
    SearchClosed,
    SearchInput(String),
    SearchSubmit,
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
    /// A listing read on a worker: the tab's number for the read (see
    /// [`Tab::start_scan`]), the normalized location it is of, its parent
    /// item, the items, the breadcrumb names and the title, the
    /// paths to select once shown, and whether the read worked (an
    /// unreadable folder arrives as an empty listing, and its progress row
    /// ends as failed rather than done).
    TabRescan(
        Entity,
        u64,
        Location,
        Option<Box<tab::Item>>,
        Vec<tab::Item>,
        Vec<(Location, String)>,
        String,
        Option<Vec<PathBuf>>,
        bool,
    ),
    TabView(Option<Entity>, tab::View),
    ToggleContextPage(ContextPage),
    ToggleFoldersFirst,
    /// Widen the search to subfolders, or narrow it back to this folder.
    SetSearchRecursive(bool),
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
    /// Ejecting a drive whose trash holds something: empty it first?
    EmptyBeforeEject {
        mounter_key: MounterKey,
        item: MounterItem,
        root: PathBuf,
    },
    FailedOperation(u64),
    /// Operation `id` asks about a path it may not touch; its question is in
    /// [`App::blocked`].
    Blocked(u64),
    /// Operation `id` has had no progress for [`STALL_LIMIT`].
    Stalled(u64),
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
    /// Numbered, so an answer -- or a wait queued behind the mime app cache
    /// -- is for this page and no other; and the worker is told when the
    /// page is dismissed, so it does not start a read nobody wants.
    OpenWithLoading {
        path: PathBuf,
        request: u64,
        cancelled: Arc<AtomicBool>,
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
        /// For two folders: the files each holds and their size, `(from,
        /// to)`, to compare them by
        totals: Option<((u64, u64), (u64, u64))>,
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
    /// Confirm running an executable file; `command` is what the dialog shows
    LaunchExecutable {
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
        // However it was dismissed -- Cancel, Escape, completion -- a page
        // still waiting on a worker tells that worker not to bother.
        if let DialogPage::OpenWithLoading { cancelled, .. } = &page {
            cancelled.store(true, Ordering::Relaxed);
        }
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

    /// Takes away operation `id`'s question about a path, wherever it waits
    /// in the queue: it was answered elsewhere, or its operation ended.
    /// Takes away the question about operation `id`'s unresponsive drive:
    /// it answered after all, or it ended.
    pub fn remove_stalled(&mut self, id: u64) -> Task<Message> {
        let Some(index) = self
            .pages
            .iter()
            .position(|page| matches!(page, DialogPage::Stalled(stalled) if *stalled == id))
        else {
            return Task::none();
        };
        if index == 0 {
            return self.pop_front().map_or_else(Task::none, |(_, task)| task);
        }
        self.pages.remove(index);
        Task::none()
    }

    pub fn remove_blocked(&mut self, id: u64) -> Task<Message> {
        let Some(index) = self
            .pages
            .iter()
            .position(|page| matches!(page, DialogPage::Blocked(blocked) if *blocked == id))
        else {
            return Task::none();
        };
        if index == 0 {
            return self.pop_front().map_or_else(Task::none, |(_, task)| task);
        }
        self.pages.remove(index);
        Task::none()
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

/// How long an operation goes without progress before its row says the
/// drive is not responding.
const STALL_SHOW: Duration = Duration::from_secs(10);

/// How long it goes without progress before the user is asked what to do.
const STALL_LIMIT: Duration = Duration::from_secs(60);

/// What the app last saw of a running operation's progress, and since when
/// it has not changed.
struct Stall {
    /// Progress (as bits), and what its checks walked: files and bytes.
    seen: (u32, u64, u64),
    since: Instant,
    /// Whether it was asked about, so it is asked once per wait.
    asked: bool,
}

/// What an operation asked about a path it may not touch.
struct Question {
    blocked: Blocked,
    /// Where the answer goes; `None` once answered. The question stays
    /// until its operation ends, so its row can roll it up.
    tx: Option<mpsc::Sender<BlockedAnswer>>,
    /// The "Same for the rest" box.
    same_for_rest: bool,
    /// What it offers besides Skip and Cancel: "Same for the rest" for how
    /// many, Retry, root.
    ask: Ask,
    /// The path as the dialog shows it, when it could be looked at.
    item: Option<Box<tab::Item>>,
}

impl Question {
    /// Sends `answer` to the waiting operation, once: whether this was the
    /// answer it took.
    fn answer(&mut self, answer: BlockedAnswer) -> bool {
        // The operation waits on this one answer, so there is room
        self.tx.take().is_some_and(|tx| tx.try_send(answer).is_ok())
    }
}

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
    /// The trash folders the trash watcher watches: the home trash and those
    /// of the drives mounted now. Looked at again as drives come and go and
    /// as operations end, since a delete can make a drive's trash.
    trash_bins: Vec<PathBuf>,
    /// The number of the latest search for the trash folders: an answer to
    /// an older one, overtaken while a drive came or went, is dropped.
    trash_bins_asked: u64,
    must_save_sort_names: bool,
    network_drive_connecting: Option<(MounterKey, String)>,
    network_drive_input: String,
    /// Where the progress notification is, which decides when a closed
    /// window's process may exit.
    exit_gate: ExitGate<ProgressNotice>,
    pending_operation_id: u64,
    pending_operations: BTreeMap<u64, (Operation, Controller)>,
    /// The last question each running operation asked about a path it may
    /// not touch. Dropping one still unanswered answers Cancel.
    blocked: BTreeMap<u64, Question>,
    /// Running operations whose progress has not moved, by operation.
    stalls: BTreeMap<u64, Stall>,
    /// The question asked in a desktop notification, by operation: `None`
    /// while the notification is still being shown.
    question_notices: BTreeMap<u64, NoticeSlot>,
    /// Counts the showings of question notifications, to tell a late one
    /// from the one wanted.
    notice_generation: u64,
    /// The long-running work the card in the bottom-right corner shows.
    progress: progress::Tasks,
    /// The active tab's button, on a card above the progress card.
    action_card: ActionCard,
    /// The action card's height and margin from the window's edge, as last
    /// laid out: the room the active tab keeps under its files.
    action_card_height: Cell<f32>,
    /// Where the file view (tabs, files and cards, without the sidebar) was
    /// last drawn: what dialogs are centred on.
    file_view_bounds: Cell<Rectangle>,
    complete_operations: BTreeMap<u64, Operation>,
    failed_operations: BTreeMap<u64, (Operation, Controller, String)>,
    /// What undoes each of the last completed operations, newest last
    undo_stack: Vec<Vec<Operation>>,
    /// Operations started by an undo; their completion is not undoable again
    undo_ids: BTreeSet<u64>,
    /// The parts of each running undo still to come, by the id of the part
    /// running now: each waits for the one before, as a restore needs its
    /// place free first. Undos started one after another keep their own.
    undo_queues: HashMap<u64, VecDeque<Operation>>,
    /// Drives to eject once the operation emptying their trash, by its id,
    /// has finished. Cancelled or failed, the drive stays.
    unmount_after: HashMap<u64, (MounterKey, MounterItem)>,
    scrollable_name: std::borrow::Cow<'static, str>,
    search_id: widget::Id,
    /// The tab whose search just ended, with its last term: its field stays
    /// in the header, springing shut, until `SearchClosed`.
    search_closing: Option<(Entity, String)>,
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
    opening: FxHashMap<u64, Opening>,
    next_open_request: u64,
    /// Numbers a sidebar "open with" request, so its answer -- and a wait it
    /// may have queued behind the mime app cache -- can be told apart from a
    /// later request for the same path, and dropped once it is cancelled.
    next_open_with_request: u64,
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

/// Reads that decide how to open something, allowed at once: the type of each
/// file the user asked to open, and the type of a sidebar entry for "open
/// with". Bounded for the same reason as every other read that can meet a
/// dead mount: a worker stuck on one keeps its permit, and without a bound
/// every retry against that mount would add another stuck worker.
static OPEN_SEMAPHORE: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

/// An open request in flight.
struct Opening {
    /// The toast shown once it took long enough to need one.
    toast: Option<widget::toaster::ToastId>,
    /// Set when the request is cancelled. The worker looks at it before it
    /// starts and between files, and stops; it cannot interrupt a read that
    /// is already in the kernel, only decline to start the next one.
    cancelled: Arc<AtomicBool>,
}

/// How long opening files may take before the user is told it is still
/// happening. Working out a file's type is milliseconds locally, so in the
/// ordinary case this never shows; it is for a slow or absent mount, where a
/// click with no visible effect is its own kind of bug.
const OPENING_ACK_DELAY: Duration = Duration::from_millis(400);
/// How long that toast stays. It is the request's only Cancel, so when it
/// goes -- by this expiring, or by being dismissed -- the request goes with
/// it: a file that finally opens a minute after a click, on a mount that came
/// back, is not what was asked for.
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
        let cancelled = Arc::new(AtomicBool::new(false));
        self.opening.insert(
            request,
            Opening {
                toast: None,
                cancelled: Arc::clone(&cancelled),
            },
        );
        let resolve = Task::future(async move {
            let resolved = tab::bounded_blocking(Arc::clone(&OPEN_SEMAPHORE), move || {
                let mut resolved = Vec::with_capacity(paths.len());
                for path in paths {
                    // Before each file, not only before the first: a
                    // selection of many files on a slow mount is cancelled
                    // somewhere in the middle, and the rest need not be read.
                    if cancelled.load(Ordering::Relaxed) {
                        return None;
                    }
                    resolved.push((mime_icon::mime_for_path(&path, None, false), path));
                }
                Some(resolved)
            })
            .await;
            match resolved {
                Some(Some(resolved)) => {
                    crate::ui::action::app(Message::OpenFilesResolved(request, resolved))
                }
                // Cancelled before it finished, or never run
                _ => crate::ui::action::app(Message::CancelOpening(request)),
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
                // An executable runs arbitrary code, so it is confirmed first
                for path in paths {
                    let page = Self::launch_executable_dialog(path);
                    let button_id = match page {
                        DialogPage::SetExecutableAndLaunch { .. } => {
                            SET_EXECUTABLE_AND_LAUNCH_CONFIRM_BUTTON_ID.clone()
                        }
                        _ => LAUNCH_DESKTOP_ENTRY_CONFIRM_BUTTON_ID.clone(),
                    };
                    tasks.push(self.push_dialog(page, Some(button_id)));
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

    /// The confirmation asked before an executable file is run: to mark it
    /// executable first when it is not, or just to launch it when it is
    fn launch_executable_dialog(path: PathBuf) -> DialogPage {
        use std::os::unix::fs::PermissionsExt;
        let executable =
            std::fs::metadata(&path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
        if executable {
            let command = path.to_string_lossy().into_owned();
            DialogPage::LaunchExecutable { path, command }
        } else {
            DialogPage::SetExecutableAndLaunch { path }
        }
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
        tasks.push(self.update_tab(entity, location, selection_paths, true));
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
    /// Move the dropped files into `to`, or copy them if Shift was held (see
    /// [`crate::ui::dnd::drop_copies`]).
    ///
    /// All drop targets use this handler: the file list, breadcrumb, nav bar and tab
    /// bar. Drops use the same `wl_data_device` transfer, MIME types and file operation
    /// as paste. This handler chooses whether to move or copy, independently of the
    /// drag source.
    fn drop_files(to: PathBuf, copy: bool) -> Task<Message> {
        // Or it would be finished unread before the read gets to it.
        crate::ui::dnd::claim_drop();
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
        // A drop back where the drag started, or a cut pasted into the folder
        // it came from, moves nothing: it is not started at all
        let Some(operation) = operation.without_no_op_moves() else {
            return Task::none();
        };
        let id = self.pending_operation_id;
        let controller = Controller::default();
        let compio_tx = self.compio_tx.clone();
        // Refused when it meets one still running on the same items
        let in_use = self.in_use(&operation);

        self.pending_operation_id += 1;
        if operation.show_progress_notification() {
            self.progress.start(
                progress::Key::Operation(id),
                operation.pending_text(0.0, ControllerState::Running),
                Duration::ZERO,
                Instant::now(),
            );
        }
        self.pending_operations
            .insert(id, (operation.clone(), controller.clone()));
        if let Some(reason) = in_use {
            return Task::done(crate::ui::Action::App(Message::PendingError(
                id,
                OperationError::from_msg(reason),
            )));
        }

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

    /// Why `operation` may not start beside those running, if it may not: a
    /// path of it is in use by one that it would get in the way of (§5.1 of
    /// the scenarios). Copies may share; nothing else may share with a move,
    /// and a delete may not share with a copy or move either way.
    fn in_use(&self, operation: &Operation) -> Option<String> {
        for (running, controller) in self.pending_operations.values() {
            if let Some(path) = collision(running, operation) {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                let other = running.pending_text(controller.progress(), controller.state());
                return Some(fl!("in-use", name = name, operation = other));
            }
        }
        None
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

    /// Ejects or unmounts `item`, tracked as a task.
    fn unmount(&mut self, mounter_key: MounterKey, item: MounterItem) -> Task<Message> {
        let Some(mounter) = MOUNTERS.get(&mounter_key) else {
            return Task::none();
        };
        let key = self.progress_unmount(&item);
        mounter
            .unmount(item)
            .map(move |outcome| crate::ui::action::app(Message::ProgressEnd(key.clone(), outcome)))
    }

    /// Ejects `item`, first asking to empty its trash when the setting says
    /// so and the trash holds anything. The trash is looked at off the UI
    /// thread: a drive can be slow to answer.
    fn eject(&mut self, mounter_key: MounterKey, item: MounterItem) -> Task<Message> {
        let root = item
            .path()
            .filter(|_| asks_before_ejecting(self.config.empty_unmount, item.is_remote()));
        let Some(root) = root else {
            return self.unmount(mounter_key, item);
        };
        Task::future(async move {
            let at = root.clone();
            let has_items =
                tokio::task::spawn_blocking(move || crate::trashing::drive_has_items(&at))
                    .await
                    .unwrap_or(false);
            crate::ui::action::app(Message::EjectChecked(mounter_key, item, root, has_items))
        })
    }

    /// Looks for the trash folders again, on a worker: a drive slow to
    /// answer must not hold up the window. The answer comes as
    /// [`Message::TrashBins`].
    fn refresh_trash_bins(&mut self) -> Task<Message> {
        self.trash_bins_asked += 1;
        let asked = self.trash_bins_asked;
        Task::future(async move {
            match tokio::task::spawn_blocking(trash_bins).await {
                Ok(bins) => crate::ui::action::app(Message::TrashBins(asked, bins)),
                Err(err) => {
                    log::warn!("failed to look for the trash folders: {err}");
                    crate::ui::action::none()
                }
            }
        })
    }

    fn handle_completed_operations(
        &mut self,
        completed: Vec<(u64, OperationSelection)>,
    ) -> Task<Message> {
        let mut commands = Vec::with_capacity(4 * completed.len());
        commands.push(self.refresh_trash_bins());
        // Emptied, or emptied but for what was skipped: the drive goes
        for (id, _) in &completed {
            if let Some((mounter_key, item)) = self.unmount_after.remove(id) {
                commands.push(self.unmount(mounter_key, item));
            }
        }
        let mut op_sel = OperationSelection::default();
        for (id, op_sel_pending) in completed {
            commands.push(self.undo_next(id));
            // Done: its drive is not asked about any more
            self.stalls.remove(&id);
            commands.push(self.dialog_pages.remove_stalled(id));
            let trash_items = op_sel_pending.trash_items.clone();
            if let Some((op, _)) = self.pending_operations.get(&id) {
                let undo = op.undo(&op_sel_pending);
                self.record_undo(id, undo);
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
                    let path_changes = move_path_changes(&op_sel_pending.moved, paths, to);
                    if self.update_favorites(&path_changes) {
                        commands.push(self.update_config());
                    }
                }

                if matches!(op, Operation::RemoveFromRecents { .. }) {
                    commands.push(self.rescan_recents());
                }

                commands.push(self.drop_question(id));
                // Skipped paths or not, it reports success. Seen in the card
                // while the window has the keyboard; otherwise as a desktop
                // notification instead
                let key = progress::Key::Operation(id);
                if self.window_is_seen() {
                    self.progress
                        .finish(&key, true, Some(op.completed_text()), Instant::now());
                } else {
                    self.progress.remove(&key);
                    commands.push(notify_done(op.completed_text()));
                }

                let mut op = op;
                op.release_payload();
                self.complete_operations.insert(id, op);
            }
        }
        commands.push(self.sync_question_notices());
        // Potentially show a notification
        commands.push(self.maybe_exit());
        // Rescan and select based on operation
        commands.push(self.rescan_operation_selection(op_sel));
        // Manually rescan any trash tabs after any operation is completed
        commands.push(self.rescan_trash());

        Task::batch(commands)
    }

    /// Starts `part` of an undo, with `rest` to follow once it completes.
    fn undo_part(&mut self, part: Operation, rest: VecDeque<Operation>) -> Task<Message> {
        // A move that emptied a folder removed it, and undoing it has to put
        // the files back inside; a paste into a folder that is gone must fail
        // instead, so only an undo recreates it
        if let Operation::Move { to, .. } = &part
            && let Err(err) = std::fs::create_dir_all(to)
        {
            log::warn!("failed to create {}: {err}", to.display());
        }
        let id = self.pending_operation_id;
        self.undo_ids.insert(id);
        if !rest.is_empty() {
            self.undo_queues.insert(id, rest);
        }
        self.operation(part)
    }

    /// The next part of an undo, once the part before, `id`, completed.
    fn undo_next(&mut self, id: u64) -> Task<Message> {
        let Some(mut rest) = self.undo_queues.remove(&id) else {
            return Task::none();
        };
        match rest.pop_front() {
            Some(next) => self.undo_part(next, rest),
            None => Task::none(),
        }
    }

    /// Keep `undo` as the way back from operation `id`, unless that
    /// operation was itself an undo
    fn record_undo(&mut self, id: u64, undo: Vec<Operation>) {
        if self.undo_ids.remove(&id) || undo.is_empty() {
            return;
        }
        self.undo_stack.push(undo);
        if self.undo_stack.len() > UNDO_DEPTH {
            self.undo_stack.remove(0);
        }
    }

    fn handle_operation_errors(&mut self, errors: Vec<(u64, OperationError)>) -> Task<Message> {
        let mut tasks = vec![self.refresh_trash_bins()];
        for (id, err) in errors.into_iter() {
            // Cancelled, aborted or failed: the drive stays
            self.unmount_after.remove(&id);
            // An undo part that failed stops the parts after it
            self.undo_queues.remove(&id);
            tasks.push(self.drop_question(id));
            // Nor is its drive asked about any more
            self.stalls.remove(&id);
            tasks.push(self.dialog_pages.remove_stalled(id));
            if let Some((op, controller)) = self.pending_operations.remove(&id) {
                // What was done before the failure can be undone on its own,
                // and a retry has only the rest to do
                let undo = op.undo_after_failure(&err.partial);
                self.record_undo(id, undo);
                // The card's row names the operation as it was started, not
                // what is left of it; its caption already says "Failed".
                let failed_text = op.pending_text(controller.progress(), ControllerState::Running);
                let op = op.remaining(&err.partial);
                // A cancelled one goes at once: the user asked for that. One
                // waiting on a password ends as Failed too, while its dialog
                // asks; the retry starts a row of its own. Any other failure
                // stays on its row, with its reason and Try again, until the
                // user closes it: no dialog, nothing centred.
                let key = progress::Key::Operation(id);
                if controller.is_cancelled() {
                    self.progress.remove(&key);
                } else if let OperationErrorType::PasswordRequired = err.kind {
                    tasks.push(self.dialog_pages.push_back(DialogPage::ExtractPassword {
                        id,
                        password: String::new(),
                    }));
                    self.progress
                        .finish(&key, false, Some(failed_text), Instant::now());
                } else {
                    self.progress.fail_kept(&key, failed_text, Instant::now());
                }
                // The payload stays: a failed operation can be retried from
                // the dialog, and retrying a paste whose bytes were dropped
                // would write an empty file and call it a success. It is
                // released when the retry succeeds, or dropped with the entry
                self.failed_operations
                    .insert(id, (op, controller, err.to_string()));
            }
        }
        tasks.push(self.sync_question_notices());
        // A failure can be the last operation a closed window was waiting on
        tasks.push(self.maybe_exit());
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
        self.update_tab(entity, tab.location.clone(), Some(op_sel.selected), false)
    }

    /// Re-reads the tab's folder, or re-runs its search. `track` shows a slow
    /// folder read in the progress card: only for a read the user asked for,
    /// since background refreshes of a busy, slow folder would otherwise keep
    /// a "Loading" row flickering in and out.
    ///
    /// A search itself never gets a row: it runs incrementally, not through
    /// [`Location::scan`], which answers a search with an empty list at once,
    /// so its tracked read normally ends within [`progress::SLOW`]. Only an
    /// empty search of a folder, which reads the folder, or clearing a search
    /// back to its folder can show one.
    fn update_tab(
        &mut self,
        entity: Entity,
        location: Location,
        selection_paths: Option<Vec<PathBuf>>,
        track: bool,
    ) -> Task<Message> {
        if let Location::Search(_, term, ..) = location {
            self.search_set(entity, Some(term), selection_paths, track)
        } else {
            self.rescan_tab(entity, location, selection_paths, track)
        }
    }

    /// Reads the tab's folder off the event loop. `track` as for
    /// [`Self::update_tab`].
    fn rescan_tab(
        &mut self,
        entity: Entity,
        location: Location,
        selection_paths: Option<Vec<PathBuf>>,
        track: bool,
    ) -> Task<Message> {
        log::info!("rescan_tab {entity:?} {location:?} {selection_paths:?}");
        let icon_sizes = self.config.tab.icon_sizes;
        let mounter_items = self.mounter_items.clone();
        let scan = self
            .tab_model
            .data_mut::<Tab>(entity)
            .map_or(0, Tab::start_scan);
        // Shown only if the folder is slow to read, as on a waking drive.
        if track {
            self.progress.start(
                progress::Key::Load(entity),
                fl!("task-loading", name = location.title(false)),
                progress::SLOW,
                Instant::now(),
            );
        }

        Task::future(async move {
            // Normalizing and naming ask the filesystem once per folder on
            // the way up, which on a mount that has stopped answering is a
            // stall per folder: worker's work, like the listing itself.
            let read = tokio::task::spawn_blocking(move || {
                let location = location.normalize();
                // Names first: a slow drive's folder is shown before its files
                // are opened, and `Tab::subscription` reads them afterwards.
                let (parent_item_opt, items, read_ok) =
                    location.try_scan(icon_sizes, tab::Typing::NameFirst);
                let ancestors = location.ancestors(true);
                let title = location.title(true);
                (location, parent_item_opt, items, ancestors, title, read_ok)
            })
            .await;
            match read {
                Ok((location, parent_item_opt, mut items, ancestors, title, read_ok)) => {
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
                        scan,
                        location,
                        parent_item_opt,
                        items,
                        ancestors,
                        title,
                        selection_paths,
                        read_ok,
                    ))
                }
                Err(err) => {
                    log::warn!("failed to rescan: {err}");
                    // Keyed by tab, not by scan: a stale scan's failure (a
                    // panic in `scan`, so rare) can end the current scan's
                    // row as Failed. Accepted.
                    crate::ui::action::app(Message::ProgressEnd(
                        progress::Key::Load(entity),
                        Outcome::Failed,
                    ))
                }
            }
        })
    }

    /// Offer applications for `path` once its type has been read from the
    /// disk, which may take a while: a bookmarked mount may have gone away,
    /// and a file on a slow drive may so far be typed by its name only. The
    /// dialog goes up at once, saying so, with a way out, and the reading
    /// happens on a worker. Cancel discards the answer; it cannot interrupt
    /// the read.
    fn open_with_after_reading(&mut self, path: PathBuf) -> Task<Message> {
        let request = self.next_open_with_request;
        self.next_open_with_request = self.next_open_with_request.wrapping_add(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let shown = self.push_dialog(
            DialogPage::OpenWithLoading {
                path: path.clone(),
                request,
                cancelled: Arc::clone(&cancelled),
            },
            None,
        );
        let resolve = Task::future(async move {
            let resolved = tab::bounded_blocking(Arc::clone(&OPEN_SEMAPHORE), {
                let path = path.clone();
                move || {
                    // Dismissed while this waited for its turn:
                    // do not start a read nobody wants
                    if cancelled.load(Ordering::Relaxed) {
                        return None;
                    }
                    Some(tab::item_from_path(&path, IconSizes::default()).map(|item| item.mime))
                }
            })
            .await;
            match resolved {
                Some(Some(resolved)) => {
                    crate::ui::action::app(Message::OpenWithResolved(request, path, resolved))
                }
                _ => crate::ui::action::none(),
            }
        });
        Task::batch([shown, resolve])
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
            .map(|(entity, location)| self.update_tab(entity, location, None, false));

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
            .map(|(entity, location)| self.update_tab(entity, location, None, false));

        Task::batch(commands)
    }

    /// What sits at the right-hand end of the search field: the eye that says
    /// whether subfolders are searched, then the button that clears the
    /// search.
    ///
    /// Both live in one slot because the text input has one: `on_clear` is
    /// itself a `trailing_icon`, so anything beside the clear button has to
    /// share that slot rather than ask for another.
    ///
    /// The eye is shown only when searching a folder. Trash and recent files
    /// are flat lists, and a control that cannot do anything is worse than
    /// no control at all.
    /// The eye, when there is a search it can act on.
    ///
    /// Its state comes from the search that is running, not from the config:
    /// history restores a search with the options it was made with, and an
    /// eye showing the config would then be describing a different search
    /// from the one on screen.
    fn search_scope_button(&self) -> Option<Element<'_, Message>> {
        let recursive =
            self.tab_model
                .active_data::<Tab>()
                .and_then(|tab| match &tab.location {
                    Location::Search(tab::SearchLocation::Path(..), _, options, _) => {
                        Some(options.recursive)
                    }
                    _ => None,
                })?;
        Some(
            widget::button::custom(widget::icon::icon(tab::search_scope_icon(recursive)).size(16))
                .class(crate::ui::theme::Button::Icon)
                .selected(recursive)
                .on_press(Message::SetSearchRecursive(!recursive))
                .padding(8)
                .into(),
        )
    }

    fn search_trailing(&self) -> Element<'_, Message> {
        let mut row: Vec<Element<'_, Message>> = Vec::with_capacity(2);
        row.extend(self.search_scope_button());
        row.push(
            widget::button::custom(widget::icon::from_name("edit-clear-symbolic").size(16))
                .class(crate::ui::theme::Button::Icon)
                .on_press(Message::SearchClear)
                .padding(8)
                .into(),
        );
        widget::Row::with_children(row)
            .align_y(Alignment::Center)
            .into()
    }

    /// The header's search field, springing open to the left out of the
    /// search button it replaces, or, `closing`, back into it.
    fn search_field<'a>(&'a self, term: &'a str, closing: bool) -> Element<'a, Message> {
        // The search button: a 16 px icon with 8 px of padding around it.
        const SEARCH_BUTTON_WIDTH: f32 = 32.0;

        widget::spring_width(
            SEARCH_BUTTON_WIDTH,
            search_text_input(&self.search_id, term, closing)
                .width(Length::Fixed(240.0))
                .trailing_icon(self.search_trailing()),
        )
        .collapsed(closing)
        .on_collapsed(Message::SearchClosed)
        .into()
    }

    /// The last term of the active tab's search while its field springs
    /// shut, after the search ended.
    fn search_closing_term(&self) -> Option<&str> {
        self.search_closing
            .as_ref()
            .filter(|(entity, _)| *entity == self.tab_model.active())
            .map(|(_, term)| term.as_str())
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
        self.search_closing = match (&term_opt, self.search_get()) {
            (None, Some(term)) => Some((entity, term.to_owned())),
            _ => None,
        };
        self.search_set(entity, term_opt, None, true)
    }

    /// Starts, changes or ends the tab's search. `track` as for
    /// [`Self::update_tab`]: it can show a row only when the read is of a
    /// folder (an empty search, or the search ended), not for the search's
    /// results.
    fn search_set(
        &mut self,
        tab: Entity,
        term_opt: Option<String>,
        selection_paths: Option<Vec<PathBuf>>,
        track: bool,
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
                                tab.search_options_for_query(),
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
                // Opening an empty search of the folder on screen, or closing
                // it, lists the same folder: nothing to read again
                let rescan = if selection_paths.is_none() && tab.only_switches_search(&location) {
                    tab.switch_search(&location);
                    false
                } else {
                    tab.change_location_keeping_listing(&location);
                    true
                };
                title_location_opt =
                    Some((tab.title(), tab.location.clone(), focus_search, rescan));
            }
        }
        if let Some((title, location, focus_search, rescan)) = title_location_opt {
            self.tab_model.text_set(tab, title);
            return Task::batch([
                self.update_title(),
                self.update_watcher(),
                if rescan {
                    self.rescan_tab(tab, location, selection_paths, track)
                } else {
                    // The listing keeps its scroll offset: bring the top the
                    // search selected into view, as Enter opens it
                    Task::done(crate::ui::action::app(Message::TabMessage(
                        Some(tab),
                        tab::Message::ScrollToFocused,
                    )))
                },
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

        // Recents sits among the pinned entries, wherever it was dragged to.
        let recents_at = crate::config::sidebar::recents_index(
            self.config.recents_position,
            self.config.favorites.len(),
        );
        let recents = |b: segmented_button::BuilderEntity<segmented_button::SingleSelect>| {
            b.text(fl!("recents"))
                .icon(icon::from_name("document-open-recent-symbolic"))
                .data(Location::Recents)
        };

        for (favorite_i, favorite) in self.config.favorites.iter().enumerate() {
            if self.config.show_recents && favorite_i == recents_at {
                nav_model = nav_model.insert(recents);
            }
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
        if self.config.show_recents && recents_at == self.config.favorites.len() {
            nav_model = nav_model.insert(recents);
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

    /// With the window closed, moves towards exiting: shows the progress
    /// notification while operations run, takes it down once they are done,
    /// and exits once nothing is left in flight. Every exit goes through here;
    /// see [`ExitGate`].
    fn maybe_exit(&mut self) -> Task<Message> {
        if self.core.main_window_id().is_some() {
            return Task::none();
        }
        // With the window gone, a question left on its card goes out as a
        // notification
        let notices = self.sync_question_notices();
        let pending = !self.pending_operations.is_empty();
        let step = match self.exit_gate.step(pending, cfg!(feature = "notify")) {
            Step::Wait => Task::none(),
            #[cfg(feature = "notify")]
            Step::Show => Task::future(async move {
                let shown = tokio::task::spawn_blocking(|| {
                    notify_rust::Notification::new()
                        .summary(&fl!("notification-in-progress"))
                        .timeout(notify_rust::Timeout::Never)
                        .show()
                        .map_err(|err| err.to_string())
                })
                .await
                .map_err(|err| err.to_string())
                .and_then(|shown| shown);
                let notice = match shown {
                    Ok(notification) => Some(Arc::new(Mutex::new(notification))),
                    Err(err) => {
                        log::warn!("failed to create notification: {err}");
                        None
                    }
                };
                crate::ui::action::app(Message::NotificationShown(notice))
            }),
            #[cfg(feature = "notify")]
            Step::Close(notice) => Task::future(async move {
                let closed = tokio::task::spawn_blocking(move || {
                    // Closing consumes the handle, so it must be the last reference
                    match Arc::try_unwrap(notice).map(Mutex::into_inner) {
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
                crate::ui::action::app(Message::NotificationClosed)
            }),
            // `can_notify` is false, so the gate never asks for either
            #[cfg(not(feature = "notify"))]
            Step::Show | Step::Close(()) => Task::none(),
            Step::Exit => {
                // Settings and recents are written by worker threads, and
                // `process::exit` runs nothing on the way out, so what is
                // still queued has to be waited for here rather than after
                // the loop returns -- which this never lets happen.
                crate::shut_down();
                process::exit(0);
            }
        };
        Task::batch([notices, step])
    }

    /// Forgets operation `id`'s question, which answers Cancel if it was
    /// still waiting, and takes its dialog down.
    fn drop_question(&mut self, id: u64) -> Task<Message> {
        if self.blocked.remove(&id).is_some() {
            self.progress.unblock(&progress::Key::Operation(id));
        }
        Task::batch([
            self.dialog_pages.remove_blocked(id),
            self.sync_question_notices(),
        ])
    }

    /// Notes which running operations have had no progress, and asks about
    /// one once it has gone [`STALL_LIMIT`] without. Paused ones and ones
    /// waiting on a question are not stalled; one that moves again is no
    /// longer, and its question goes.
    fn watch_stalls(&mut self, now: Instant) -> Task<Message> {
        let mut tasks = Vec::new();
        self.stalls
            .retain(|id, _| self.pending_operations.contains_key(id));
        for (&id, (_, controller)) in &self.pending_operations {
            let waiting = controller.is_paused()
                || self
                    .blocked
                    .get(&id)
                    .is_some_and(|question| question.tx.is_some());
            let (files, bytes) = controller.checked();
            let seen = (controller.progress().to_bits(), files, bytes);
            let stall = self.stalls.entry(id).or_insert(Stall {
                seen,
                since: now,
                asked: false,
            });
            if waiting || stall.seen != seen {
                let was_asked = stall.asked;
                *stall = Stall {
                    seen,
                    since: now,
                    asked: false,
                };
                if was_asked {
                    tasks.push(self.dialog_pages.remove_stalled(id));
                }
            } else if !stall.asked && now.duration_since(stall.since) >= STALL_LIMIT {
                stall.asked = true;
                tasks.push(self.dialog_pages.push_back(DialogPage::Stalled(id)));
            }
        }
        Task::batch(tasks)
    }

    /// How long operation `id` has gone without progress, once its row
    /// should say so.
    fn stalled_for(&self, id: u64) -> Option<Duration> {
        let stall = self.stalls.get(&id)?;
        let waited = stall.since.elapsed();
        (waited >= STALL_SHOW).then_some(waited)
    }

    /// Whether anyone is looking at the window: it is open and one of ours
    /// has the keyboard. Unfocused or minimised, it is not.
    fn window_is_seen(&self) -> bool {
        self.core.main_window_id().is_some() && self.core.focused_window().is_some()
    }

    /// Whether operation `id`'s notification on record is the desktop's
    /// `notice`.
    fn notice_is(&self, id: u64, notice: u32) -> bool {
        notice_on_record(&self.question_notices, id, notice)
    }

    /// Keeps the desktop notification for questions in step with the card:
    /// the question being asked goes out as one while nobody can see its row
    /// (the window is closed, or none of ours has the keyboard), and any
    /// other is taken down.
    fn sync_question_notices(&mut self) -> Task<Message> {
        let asking = match self.progress.asking() {
            Some(progress::Key::Operation(id)) => Some(*id),
            _ => None,
        };
        let mut tasks = Vec::new();
        let seen = self.window_is_seen();
        // Seen again, the window asks; its notification goes
        let stale: Vec<u64> = self
            .question_notices
            .keys()
            .filter(|id| Some(**id) != asking || seen)
            .copied()
            .collect();
        for id in stale {
            // One still being shown is closed when it reports in
            if let Some(NoticeSlot {
                shown: Some(notice),
                ..
            }) = self.question_notices.remove(&id)
            {
                tasks.push(close_question_notice(notice));
            }
        }
        let generation = self.notice_generation + 1;
        if let Some(id) = asking
            && !seen
            && !self.question_notices.contains_key(&id)
            && let Some(task) = self.show_question_notice(id, generation)
        {
            self.notice_generation = generation;
            self.question_notices.insert(
                id,
                NoticeSlot {
                    generation,
                    shown: None,
                },
            );
            tasks.push(task);
        }
        Task::batch(tasks)
    }

    /// Shows operation `id`'s question as a desktop notification, whose
    /// buttons answer it. `None` when there is nothing to ask.
    #[cfg(feature = "notify")]
    fn show_question_notice(&self, id: u64, generation: u64) -> Option<Task<Message>> {
        let question = self
            .blocked
            .get(&id)
            .filter(|question| question.tx.is_some())?;
        let (op, controller) = self.pending_operations.get(&id)?;
        let (mut text, skip) = question_text(&question.blocked);
        if let Some(detail) = question_detail(&question.blocked) {
            text = format!("{text}\n{detail}");
        }
        let mut notification = notify_rust::Notification::new()
            .summary(&op.pending_text(controller.progress(), ControllerState::Paused))
            .body(&text)
            .timeout(notify_rust::Timeout::Never)
            .finalize();
        // As the dialog offers them: Skip, or Try again where nothing is
        // skipped
        if question.ask.skip {
            notification.action(NOTICE_SKIP, &skip);
        }
        if question.ask.retry {
            let label = if question.ask.skip {
                fl!("retry")
            } else {
                fl!("try-again")
            };
            notification.action(NOTICE_RETRY, &label);
        }
        if question.ask.abort {
            notification.action(NOTICE_ABORT, &fl!("abort"));
        }
        if question.ask.permanently {
            notification.action(NOTICE_PERMANENTLY, &fl!("delete-permanently"));
        }
        if let Some(again) = question.ask.root {
            notification.action(NOTICE_ROOT, &root_label(&question.blocked, again));
        }
        let notification = notification
            .action(NOTICE_CANCEL, &fl!("cancel"))
            .finalize();
        Some(
            crate::ui::Task::stream(crate::ui::iced::stream::channel(
                2,
                move |mut out: futures::channel::mpsc::Sender<Message>| async move {
                    let shown = {
                        let notification = notification.clone();
                        tokio::task::spawn_blocking(move || notification.show()).await
                    };
                    let handle = match shown {
                        Ok(Ok(handle)) => handle,
                        Ok(Err(err)) => {
                            log::warn!("failed to show the question as a notification: {err}");
                            return;
                        }
                        Err(err) => {
                            log::warn!("failed to show the question as a notification: {err}");
                            return;
                        }
                    };
                    let notice_id = handle.id();
                    let notice = (notice_id, notification);
                    let _ = out
                        .send(Message::QuestionNotified(id, generation, notice))
                        .await;
                    // Until a button is pressed or the notification closes,
                    // from either side
                    let picked = tokio::task::spawn_blocking(move || {
                        let mut picked = None;
                        handle.wait_for_action(|action| picked = notice_answer(action));
                        picked
                    })
                    .await
                    .ok()
                    .flatten();
                    let message = match picked {
                        Some(answer) => Message::QuestionNoticeAnswered(id, notice_id, answer),
                        None => Message::QuestionNoticeClosed(id, notice_id),
                    };
                    let _ = out.send(message).await;
                },
            ))
            .map(crate::ui::Action::App),
        )
    }

    #[cfg(not(feature = "notify"))]
    fn show_question_notice(&self, _id: u64, _generation: u64) -> Option<Task<Message>> {
        None
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

    /// Tracks mounting `item`, shown if it is slow: the key it ends with.
    fn progress_mount(&mut self, item: &MounterItem) -> progress::Key {
        let key = progress::Key::Mount(item.id());
        self.progress.start(
            key.clone(),
            fl!("task-mounting", name = item.name()),
            progress::SLOW,
            Instant::now(),
        );
        key
    }

    /// Tracks unmounting or ejecting `item`, shown if it is slow: the key it
    /// ends with.
    fn progress_unmount(&mut self, item: &MounterItem) -> progress::Key {
        let key = progress::Key::Unmount(item.id());
        self.progress.start(
            key.clone(),
            fl!("task-unmounting", name = item.name()),
            progress::SLOW,
            Instant::now(),
        );
        key
    }

    /// The card in the bottom-right corner: a row per long-running task,
    /// under the toasts.
    fn progress_card(&self) -> Option<Element<'_, Message>> {
        let Spacing {
            space_xxs,
            space_xs,
            space_s,
            ..
        } = spacing();

        let visible = self.progress.visible(Instant::now());
        if visible.rows.is_empty() {
            return None;
        }
        // The details show the same operations, questions included; the card
        // rolls up while they are open and comes back when they close
        let details_open =
            self.core.window.show_context && self.context_page == ContextPage::EditHistory;

        let header = widget::Row::with_children([
            widget::text::heading(fl!("tasks-count", count = visible.total())).into(),
            widget::space::horizontal().into(),
            widget::button::link(fl!("details"))
                .on_press(Message::ToggleContextPage(ContextPage::EditHistory))
                .padding(0)
                .trailing_icon(true)
                .into(),
        ])
        .align_y(Alignment::Center);

        // Keyed by the row's id, so a row keeps its own collapse and widget
        // state when one above it is removed, rather than inheriting that
        // row's by position. The gap above each row is inside its collapse,
        // so it shrinks with the row instead of snapping shut on removal.
        let gap = iced::padding::top(space_xxs.to_pixels());
        let mut rows = widget::keyed::Column::with_capacity(visible.rows.len());
        for row in &visible.rows {
            rows = rows.push(
                row.id,
                widget::spring_height(
                    iced::widget::mouse_area(
                        widget::container(self.progress_row(row)).padding(gap),
                    )
                    .on_enter(Message::ProgressHover(row.id, true))
                    .on_exit(Message::ProgressHover(row.id, false))
                    .on_right_press(Message::ProgressDismiss(row.id)),
                )
                .collapsed(row.leaving),
            );
        }
        let more = (visible.more > 0).then(|| {
            widget::container(widget::text::caption(fl!(
                "tasks-more",
                count = visible.more
            )))
            .padding(gap)
        });
        let column = widget::Column::with_capacity(3)
            .push(header)
            .push(rows)
            .push_maybe(more);

        let card = widget::container(column)
            .width(Length::Fixed(360.0))
            .padding([space_xs, space_s])
            .class(Container::Tooltip);
        // The card slides out with its last rows.
        Some(
            widget::spring_height(card)
                .collapsed(visible.leaving() || details_open)
                .into(),
        )
    }

    /// The card above the progress card holding the active tab's button.
    fn action_card(&self, above_progress: bool) -> Option<Element<'_, Message>> {
        let shown = self.action_card.shown()?;
        Some(Self::action_card_view(
            shown,
            above_progress,
            &self.action_card_height,
        ))
    }

    /// The action card for `shown`: the location's icon, name and what it
    /// holds, then the button. `above_progress` leaves a gap under it for
    /// the progress card; the gap is inside the slide, so it closes with the
    /// card. The card's height and its margin from the window's edge go
    /// into `height`, for the tab to keep that much room under its files;
    /// the progress card, which comes and goes, gets none.
    fn action_card_view<'a>(
        shown: Shown,
        above_progress: bool,
        height: &'a Cell<f32>,
    ) -> Element<'a, Message> {
        let Spacing {
            space_xxxs,
            space_xxs,
            space_xs,
            space_s,
            ..
        } = spacing();

        let action = shown.action;
        let (title, icon_name, label, message) = action.parts();
        let button = widget::button::standard(label).on_press_maybe(
            (!shown.leaving).then(|| Message::TabMessage(Some(shown.entity), message)),
        );
        // The detail drops under the title only when the two do not fit on
        // one line beside the button.
        let info = widget::Row::with_capacity(2)
            .push(
                widget::Row::with_children([
                    icon::from_name(icon_name).size(16).icon().into(),
                    widget::text::heading(title).into(),
                ])
                .spacing(space_xxs.to_pixels())
                .align_y(Alignment::Center),
            )
            .push_maybe(
                action
                    .detail()
                    .map(|detail| widget::text::body(format!("· {detail}"))),
            )
            .spacing(space_xxs.to_pixels())
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .wrap()
            .vertical_spacing(space_xxxs.to_pixels());
        let card = widget::container(
            widget::Row::with_children([info.into(), button.into()])
                .spacing(space_xs.to_pixels())
                .align_y(Alignment::Center),
        )
        .width(Length::Fixed(360.0))
        .padding([space_xs, space_s])
        .class(Container::Tooltip);
        let card = widget::measure_height(card, height).plus(widget::toaster::OFFSET);
        let gap = if above_progress { space_xxxs } else { 0 };
        widget::spring_height(
            widget::container(card).padding(iced::padding::bottom(gap.to_pixels())),
        )
        .collapsed(shown.leaving)
        .into()
    }

    /// One row of the progress card.
    fn progress_row<'a>(&'a self, row: &progress::Row<'a>) -> Element<'a, Message> {
        let bar_height = Length::Fixed(4.0);

        // A file operation still running reads its progress live, and can
        // be paused and cancelled.
        if let progress::Key::Operation(id) = row.key
            && matches!(
                row.state,
                progress::State::Running | progress::State::Asking | progress::State::Waiting
            )
            && let Some((op, controller)) = self.pending_operations.get(id)
        {
            // Its question, if it waits on one, is a dialog of its own
            let stopped = row.state != progress::State::Running;
            return Self::pending_operation_view(
                *id,
                op,
                controller,
                stopped,
                self.stalled_for(*id),
            );
        }

        if let (progress::Key::Operation(id), progress::State::Failed) = (row.key, row.state)
            && let Some((_, _, reason)) = self.failed_operations.get(id)
        {
            return Self::failed_row(*id, row, reason);
        }

        let bar: Option<Element<'a, Message>> = match row.state {
            progress::State::Running => Some(
                widget::indeterminate_linear()
                    .width(Length::Fill)
                    .girth(bar_height)
                    .into(),
            ),
            progress::State::Done => Some(
                widget::determinate_linear(1.0)
                    .width(Length::Fill)
                    .girth(bar_height)
                    .into(),
            ),
            progress::State::Asking | progress::State::Waiting | progress::State::Failed => None,
        };
        let status = match row.state {
            progress::State::Running => None,
            progress::State::Asking => Some(fl!("task-paused")),
            progress::State::Waiting => Some(fl!("task-paused-waiting")),
            progress::State::Done => Some(fl!("task-done")),
            progress::State::Failed => Some(fl!("task-failed")),
        };
        widget::Column::with_capacity(2)
            .push_maybe(bar)
            .push(
                widget::Row::with_capacity(3)
                    .push(widget::text::body(row.label))
                    .push(widget::space::horizontal())
                    .push_maybe(status.map(widget::text::caption))
                    .align_y(Alignment::Center),
            )
            .spacing(spacing().space_xxs.to_pixels())
            .into()
    }

    /// The dialog asking what operation `id` should do about a path it may
    /// not touch, built like the one for a file that already exists: the
    /// question, the path's own block, "Same for the rest" while more could
    /// be asked, and Cancel, Retry (when it could work) and Skip.
    fn blocked_dialog(id: u64, question: &Question) -> widget::Dialog<'_, Message> {
        let (text, skip) = question_text(&question.blocked);
        // Answered, it only waits to be taken down
        let answer = |answer| {
            question
                .tx
                .is_some()
                .then_some(Message::BlockedAnswer(id, answer))
        };
        let same_for_rest = question.same_for_rest && question.ask.count > 1;
        let mut dialog = widget::dialog().title(text);
        if let Some(detail) = question_detail(&question.blocked) {
            dialog = dialog.body(detail);
        }
        // Not an error: the question stands, and root can be tried again
        if question.ask.not_granted {
            dialog = dialog.control(widget::text::caption(fl!("root-not-granted")));
        }
        if let Some(item) = &question.item {
            dialog = dialog.control(
                item.replace_view(item.name.clone())
                    .map(|message| Message::TabMessage(None, message)),
            );
        }
        if question.ask.count > 1 {
            dialog = dialog.control(
                widget::checkbox(question.same_for_rest)
                    .label(fl!("same-for-rest-count", count = question.ask.count))
                    .on_toggle(move |on| Message::BlockedSameForRest(id, on)),
            );
        }
        // The default answer: Skip, or Try again where nothing is skipped
        let primary = if question.ask.skip {
            widget::button::suggested(skip)
                .on_press_maybe(answer(BlockedAnswer::Skip(same_for_rest)))
        } else {
            widget::button::suggested(fl!("try-again")).on_press_maybe(answer(BlockedAnswer::Retry))
        };
        dialog = dialog
            .primary_action(primary.id(BLOCKED_BUTTON_ID.clone()))
            .tertiary_action(
                widget::button::standard(fl!("cancel"))
                    .on_press_maybe(answer(BlockedAnswer::Cancel)),
            );
        // Beside it: root, Retry when Skip is the default, Abort
        let mut others: Vec<Element<'_, Message>> = Vec::new();
        if question.ask.permanently {
            others.push(
                widget::button::standard(fl!("delete-permanently"))
                    .on_press_maybe(answer(BlockedAnswer::Permanently(same_for_rest)))
                    .into(),
            );
        }
        if let Some(again) = question.ask.root {
            others.push(
                widget::button::standard(root_label(&question.blocked, again))
                    .on_press_maybe(answer(BlockedAnswer::RetryAsRoot(same_for_rest)))
                    .into(),
            );
        }
        if question.ask.retry && question.ask.skip {
            others.push(
                widget::button::standard(fl!("retry"))
                    .on_press_maybe(answer(BlockedAnswer::Retry))
                    .into(),
            );
        }
        if question.ask.abort {
            others.push(
                widget::button::standard(fl!("abort"))
                    .on_press_maybe(answer(BlockedAnswer::Abort))
                    .into(),
            );
        }
        if !others.is_empty() {
            dialog = dialog.secondary_action(
                widget::Row::with_children(others).spacing(spacing().space_xxs.to_pixels()),
            );
        }
        dialog
    }

    /// A row whose operation failed: what it was doing, why it stopped, and
    /// Try again. It stays until closed.
    fn failed_row<'a>(id: u64, row: &progress::Row<'a>, reason: &'a str) -> Element<'a, Message> {
        let space_xxs = spacing().space_xxs.to_pixels();
        widget::Column::with_capacity(2)
            .push(
                widget::Row::with_capacity(4)
                    .push(widget::text::body(row.label))
                    .push(widget::space::horizontal())
                    .push(
                        widget::button::link(fl!("try-again"))
                            .on_press(Message::RetryFailed(id))
                            .padding(0),
                    )
                    .push(widget::tooltip(
                        widget::button::icon(icon::from_name("window-close-symbolic"))
                            .on_press(Message::ProgressDismiss(row.id))
                            .padding(8),
                        widget::text::body(fl!("close")),
                        widget::tooltip::Position::Top,
                    ))
                    .spacing(space_xxs)
                    .align_y(Alignment::Center),
            )
            .push(widget::text::caption(reason))
            .spacing(space_xxs)
            .into()
    }

    /// A running file operation: its live bar, pause or resume, cancel, and
    /// what it is doing. Shared by the progress card and the edit history.
    /// `stopped`: it waits on the user about a path it may not touch, so the
    /// bar turns the warning colour and the text says it is paused.
    fn pending_operation_view<'a>(
        id: u64,
        op: &Operation,
        controller: &Controller,
        stopped: bool,
        stalled: Option<Duration>,
    ) -> Element<'a, Message> {
        let progress = controller.progress();
        // Waiting on the user, it says so without a percentage, and its
        // question has the Cancel: the pause and cancel buttons step aside
        if stopped {
            return widget::Column::with_children([
                widget::determinate_linear(1.0)
                    .style(widget::progress_bar::style::Bar::Warning)
                    .width(Length::Fill)
                    .girth(Length::Fixed(4.0))
                    .into(),
                widget::text::body(op.pending_text_asking()).into(),
            ])
            .spacing(spacing().space_xxs.to_pixels())
            .into();
        }
        let pause = if controller.is_paused() {
            widget::tooltip(
                widget::button::icon(icon::from_name("media-playback-start-symbolic"))
                    .on_press(Message::PendingPause(id, false))
                    .padding(8),
                widget::text::body(fl!("resume")),
                widget::tooltip::Position::Top,
            )
        } else {
            widget::tooltip(
                widget::button::icon(icon::from_name("media-playback-pause-symbolic"))
                    .on_press(Message::PendingPause(id, true))
                    .padding(8),
                widget::text::body(fl!("pause")),
                widget::tooltip::Position::Top,
            )
        };
        // A drive that stopped responding: the bar counts towards the time
        // when the user is asked, in the warning colour
        let bar = match stalled {
            Some(waited) => widget::determinate_linear(
                (waited.as_secs_f32() / STALL_LIMIT.as_secs_f32()).min(1.0),
            )
            .style(widget::progress_bar::style::Bar::Warning),
            None => widget::determinate_linear(progress),
        };
        widget::Column::with_children([
            widget::Row::with_children([
                bar.width(Length::Fill).girth(Length::Fixed(4.0)).into(),
                pause.into(),
                widget::tooltip(
                    widget::button::icon(icon::from_name("media-playback-stop-symbolic"))
                        .on_press(Message::PendingAbort(id))
                        .padding(8),
                    widget::text::body(fl!("abort-tooltip")),
                    widget::tooltip::Position::Top,
                )
                .into(),
                widget::tooltip(
                    widget::button::icon(icon::from_name("window-close-symbolic"))
                        .on_press(Message::PendingCancel(id))
                        .padding(8),
                    widget::text::body(fl!("cancel-tooltip")),
                    widget::tooltip::Position::Top,
                )
                .into(),
            ])
            .align_y(Alignment::Center)
            .into(),
            widget::text::body(if let Some(waited) = stalled {
                fl!("stalled-for", seconds = waited.as_secs())
            } else if controller.is_checking() {
                let (files, bytes) = controller.checked();
                fl!(
                    "checking",
                    files = files,
                    size = crate::tab::format_size(bytes)
                )
            } else {
                op.pending_text(progress, controller.state())
            })
            .into(),
        ])
        .into()
    }

    fn edit_history(&self) -> Element<'_, Message> {
        let Spacing { space_m, .. } = spacing();

        let mut children = Vec::new();

        if !self.pending_operations.is_empty() {
            let mut section = widget::settings::section().title(fl!("pending"));
            for (id, (op, controller)) in self.pending_operations.iter().rev() {
                let question = self
                    .blocked
                    .get(id)
                    .filter(|question| question.tx.is_some());
                section = section.add(Self::pending_operation_view(
                    *id,
                    op,
                    controller,
                    question.is_some(),
                    self.stalled_for(*id),
                ));
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

    fn title(&self, id: WindowId) -> &str {
        crate::dialog::window_title(
            self.file_dialog_opt
                .iter()
                .map(|dialog| (dialog.window_id(), dialog.title())),
            &self.core.title,
            id,
        )
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
            trash_bins: Vec::new(),
            trash_bins_asked: 0,
            must_save_sort_names: false,
            network_drive_connecting: None,
            network_drive_input: String::new(),
            exit_gate: ExitGate::default(),
            pending_operation_id: 0,
            pending_operations: BTreeMap::new(),
            progress: progress::Tasks::default(),
            action_card: ActionCard::default(),
            action_card_height: Cell::new(0.0),
            file_view_bounds: Cell::new(Rectangle::default()),
            complete_operations: BTreeMap::new(),
            failed_operations: BTreeMap::new(),
            blocked: BTreeMap::new(),
            question_notices: BTreeMap::new(),
            notice_generation: 0,
            stalls: BTreeMap::new(),
            undo_stack: Vec::new(),
            undo_ids: BTreeSet::new(),
            undo_queues: HashMap::new(),
            unmount_after: HashMap::new(),
            scrollable_name: std::borrow::Cow::Borrowed("File Scrollable"),
            search_id: widget::Id::new("File Search"),
            search_closing: None,
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
            next_open_with_request: 0,
            name_check_revision: Arc::new(AtomicU64::new(0)),
        };

        let mut commands = vec![
            app.update_config(),
            app.update(Message::CheckClipboard),
            // The cache starts empty; this builds it, and primes the default
            // terminal along with it, on a worker.
            app.update(Message::ReloadMimeAppCache),
            // The trash watcher starts once the trash folders are found
            app.refresh_trash_bins(),
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
            // A bookmark accepts drops if it is a directory this app can paste
            // into, the one on show included: a drag held over a place opens
            // it, and may then be let go there. Resolve it here because the
            // callback outlives this borrow of the model.
            let showing = self
                .tab_model
                .data::<Tab>(self.tab_model.active())
                .map(|tab| tab.location.clone());
            let droppable: Vec<Entity> = nav_model
                .iter()
                .filter(|entity| {
                    nav_model.data::<Location>(*entity).is_some_and(|location| {
                        location.supports_paste() && location.path_opt().is_some()
                    })
                })
                .collect();
            let shown: Vec<Entity> = nav_model
                .iter()
                .filter(|entity| {
                    showing.is_some() && nav_model.data::<Location>(*entity) == showing.as_ref()
                })
                .collect();
            nav = nav
                .on_file_drop(move |entity| {
                    droppable
                        .contains(&entity)
                        .then(|| crate::ui::Action::App(Message::NavBarDrop(entity)))
                })
                // Held over by a file drag, a place is shown, as on a click;
                // the one already on show has nothing to open
                .on_drag_hover_open(move |entity| {
                    (!shown.contains(&entity))
                        .then(|| crate::ui::Action::Cosmic(crate::ui::app::Action::NavBar(entity)))
                });
        }

        {
            // Pinned entries and Recents can be dragged to a new place, and a
            // folder not pinned yet can be dragged in to pin it.
            let reorderable: Vec<Entity> = nav_model
                .iter()
                .filter(|entity| {
                    nav_model.data::<FavoriteIndex>(*entity).is_some()
                        || matches!(nav_model.data::<Location>(*entity), Some(Location::Recents))
                })
                .collect();
            let pinned: Vec<PathBuf> = self
                .config
                .favorites
                .iter()
                .filter_map(Favorite::path_opt)
                .collect();
            nav = nav
                .reorderable(move |entity| reorderable.contains(&entity))
                .can_pin(move |path| path.is_dir() && !pinned.iter().any(|pinned| pinned == path))
                .on_nav_drop(|drop| crate::ui::Action::App(Message::NavDrop(drop)));
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
                        let key = progress::Key::Mount(uri.clone());
                        self.progress.start(
                            key.clone(),
                            fl!("task-mounting", name = uri_for_display(uri)),
                            progress::SLOW,
                            Instant::now(),
                        );
                        return mounter.network_drive(uri.clone()).then(move |outcome| {
                            let end = Task::done(crate::ui::action::app(Message::ProgressEnd(
                                key.clone(),
                                outcome,
                            )));
                            if outcome == Outcome::Done {
                                end.chain(Task::done(crate::ui::Action::App(
                                    Message::NetworkDriveOpenEntityAfterMount { entity },
                                )))
                            } else {
                                end
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
            let item = data.1.clone();
            let key = self.progress_mount(&item);
            return mounter.mount(item).map(move |outcome| {
                crate::ui::action::app(Message::ProgressEnd(key.clone(), outcome))
            });
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

    fn drawer_slide_fits_columns(&self, extent: f32, opening: bool) -> bool {
        self.tab_model
            .data::<Tab>(self.tab_model.active())
            .is_some_and(|tab| tab.column_slide_fits(extent, opening))
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

    /// The action card follows the active tab, which changes tab, location
    /// and listing from many places: re-read after each message.
    fn after_update(&mut self) {
        let entity = self.tab_model.active();
        // The network drives connected now, as the sidebar lists them
        let connected = self
            .mounter_items
            .values()
            .flatten()
            .filter(|item| item.is_remote() && item.is_mounted())
            .count();
        let current = self
            .tab_model
            .data::<Tab>(entity)
            .and_then(|tab| tab.action(connected))
            .map(|action| (entity, action));

        self.action_card.sync(current, Instant::now());
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

                        let name = Location::Path(path.clone()).title(true);
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
                    // A favourite is named once, on a click, and a network
                    // one wants the name its backend gives.
                    let name = Location::Path(path.clone()).title(true);
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
                    // The name is already filled in, so ask about it as if
                    // it had just been typed.
                    let check =
                        self.check_name(to.join(format!("{name}{}", archive_type.extension())));
                    return Task::batch([
                        self.push_dialog(
                            DialogPage::Compress {
                                paths,
                                to,
                                name,
                                archive_type,
                                password: None,
                            },
                            Some(self.dialog_text_input.clone()),
                        ),
                        check,
                    ]);
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
                // An opening toast is that request's only Cancel. Dismissed
                // or expired, the request goes with it rather than lingering
                // with no way to stop it.
                let cancelled_open = self
                    .opening
                    .iter()
                    .find(|(_, opening)| opening.toast == Some(id))
                    .map(|(request, _)| *request);
                if let Some(request) = cancelled_open {
                    return self.update(Message::CancelOpening(request));
                }
                self.toasts.remove(id);
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
                if let Some((page, task)) = self.dialog_pages.pop_front() {
                    // Escape on a question about a path cancels its operation
                    // Escape on a drive not responding cancels its operation
                    if let DialogPage::Stalled(id) = page {
                        return Task::batch([task, self.update(Message::PendingCancel(id))]);
                    }
                    if let DialogPage::Blocked(id) = page {
                        return Task::batch([
                            task,
                            self.update(Message::BlockedAnswer(id, BlockedAnswer::Cancel)),
                        ]);
                    }
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
                        DialogPage::EmptyBeforeEject {
                            mounter_key,
                            item,
                            root,
                        } => {
                            // The id the emptying is about to get
                            self.unmount_after
                                .insert(self.pending_operation_id, (mounter_key, item));
                            tasks.push(self.operation(Operation::EmptyDriveTrash { root }));
                        }
                        // Enter gives the default answer, Skip
                        // Enter keeps waiting
                        DialogPage::Stalled(id) => {
                            tasks.push(self.update(Message::StalledWait(id)));
                        }
                        DialogPage::Blocked(id) => {
                            if let Some(question) = self.blocked.get(&id) {
                                let answer = if question.ask.skip {
                                    BlockedAnswer::Skip(
                                        question.same_for_rest && question.ask.count > 1,
                                    )
                                } else {
                                    BlockedAnswer::Retry
                                };
                                tasks.push(self.update(Message::BlockedAnswer(id, answer)));
                            }
                        }
                        DialogPage::FailedOperation(id) => {
                            if let Some((operation, _, _)) = self.failed_operations.remove(&id) {
                                tasks.push(self.operation(operation));
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
                                let key = self.progress_mount(&item);
                                tasks.push(mounter.mount(item).map(move |outcome| {
                                    crate::ui::action::app(Message::ProgressEnd(
                                        key.clone(),
                                        outcome,
                                    ))
                                }));
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
                        DialogPage::LaunchExecutable { path, .. } => {
                            let mut command = std::process::Command::new(&path);
                            if let Err(err) = spawn_detached(&mut command) {
                                log::warn!("failed to execute {}: {}", path.display(), err);
                            }
                        }
                        DialogPage::FavoritePathError { entity, .. } => {
                            // Through `sidebar`, so Recents keeps its place.
                            if let Some(edited) = self
                                .nav_model
                                .data::<FavoriteIndex>(entity)
                                .and_then(|FavoriteIndex(favorite_i)| {
                                    crate::config::sidebar::unpin_favorite(
                                        &self.config.favorites,
                                        self.config.recents_position,
                                        *favorite_i,
                                    )
                                })
                            {
                                config_set!(favorites, edited.favorites);
                                config_set!(recents_position, edited.recents_position);
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
                    DialogPage::Compress {
                        to,
                        name,
                        archive_type,
                        ..
                    } if !name.is_empty() => {
                        Some(to.join(format!("{name}{}", archive_type.extension())))
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
                                        return scrollable::glide_to(
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
                return self.maybe_exit();
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
                            commands.push(self.update_tab(
                                entity,
                                home_location.clone(),
                                None,
                                true,
                            ));
                        }
                    }
                    if !commands.is_empty() {
                        commands.push(self.update_title());
                        commands.push(self.update_watcher());
                    }
                }

                // Insert new items
                self.mounter_items.insert(mounter_key, mounter_items);
                commands.push(self.refresh_trash_bins());

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
                    let uri = self.network_drive_input.clone();
                    let key = progress::Key::Mount(uri.clone());
                    self.progress.start(
                        key.clone(),
                        fl!("task-mounting", name = uri_for_display(&uri)),
                        progress::SLOW,
                        Instant::now(),
                    );
                    return mounter.network_drive(uri).map(move |outcome| {
                        crate::ui::action::app(Message::ProgressEnd(key.clone(), outcome))
                    });
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
            Message::NotificationShown(notice) => {
                self.exit_gate.shown(notice);
                // The operations may have finished while it was being shown
                return self.maybe_exit();
            }
            #[cfg(feature = "notify")]
            Message::NotificationClosed => {
                self.exit_gate.closed();
                return self.maybe_exit();
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
                    .map(|(entity, location)| self.update_tab(entity, location, None, false))
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
                let Some(opening) = self.opening.remove(&request) else {
                    return Task::none();
                };
                if let Some(id) = opening.toast {
                    self.toasts.remove(id);
                }
                return self.open_resolved(resolved);
            }
            Message::OpeningStillRunning(request) => {
                if let Some(opening) = self.opening.get_mut(&request)
                    && opening.toast.is_none()
                {
                    let toast = widget::toaster::Toast::new(fl!("opening-files"))
                        .action(fl!("cancel"), move |_| Message::CancelOpening(request))
                        .duration(widget::toaster::Duration::Custom(OPENING_TOAST_BACKSTOP));
                    let (id, expiry) = self.toasts.push_with_id(toast);
                    opening.toast = Some(id);
                    return expiry.map(crate::ui::action::app);
                }
            }
            Message::CancelOpening(request) => {
                // Discard, and stop starting: the worker declines the next
                // file once it sees this, and whatever it already found is
                // thrown away when it arrives. A read already in the kernel
                // is not interrupted; nothing can do that.
                if let Some(opening) = self.opening.remove(&request) {
                    opening.cancelled.store(true, Ordering::Relaxed);
                    if let Some(id) = opening.toast {
                        self.toasts.remove(id);
                    }
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
                                .map(|path| (path.clone(), item.mime.clone(), item.type_checked))
                        })
                    });
                if let Some((path, mime, type_checked)) = chosen {
                    // Typed by its name only so far: the applications offered
                    // have to be for what the file is
                    if !type_checked {
                        return self.open_with_after_reading(path);
                    }
                    return self.update(Message::OpenWithFor(path, mime, None));
                }
            }
            Message::OpenWithResolved(request, path, resolved) => {
                // Only for the loading page this was asked for, if it is
                // still up. Cancelled, or replaced by a later request for the
                // same path, and the answer is nobody's.
                let waiting = matches!(
                    self.dialog_pages.front(),
                    Some(DialogPage::OpenWithLoading { request: shown, .. }) if *shown == request
                );
                if !waiting {
                    return Task::none();
                }
                match resolved {
                    Ok(mime) => {
                        return self.update(Message::OpenWithFor(path, mime, Some(request)));
                    }
                    Err(err) => {
                        log::warn!("failed to get item for path {}: {}", path.display(), err);
                        if let Some((_, task)) = self.dialog_pages.pop_front() {
                            return task;
                        }
                    }
                }
            }
            Message::OpenWithFor(path, mime, origin) => {
                if self.defer_until_mime_apps(Message::OpenWithFor(
                    path.clone(),
                    mime.clone(),
                    origin,
                )) {
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
                // From a loading page: fill that page in if it is still up,
                // and otherwise do nothing at all -- it was cancelled while
                // this waited, and a dialog reappearing after Cancel is worse
                // than no dialog. Without one: a fresh dialog.
                let shown = match origin {
                    Some(request) => {
                        if matches!(
                            self.dialog_pages.front(),
                            Some(DialogPage::OpenWithLoading { request: shown, .. }) if *shown == request
                        ) {
                            self.dialog_pages.update_front(page);
                            Task::none()
                        } else {
                            return Task::none();
                        }
                    }
                    None => self.push_dialog(page, Some(CONFIRM_OPEN_WITH_BUTTON_ID.clone())),
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
            Message::PendingAbort(id) => {
                if let Some((_, controller)) = self.pending_operations.get(&id) {
                    controller.abort();
                    self.progress.remove(&progress::Key::Operation(id));
                }
                self.stalls.remove(&id);
                return Task::batch([self.dialog_pages.remove_stalled(id), self.drop_question(id)]);
            }
            Message::PendingTick => return self.watch_stalls(Instant::now()),
            Message::StalledWait(id) => {
                if let Some(stall) = self.stalls.get_mut(&id) {
                    stall.since = Instant::now();
                    stall.asked = false;
                }
                return self.dialog_pages.remove_stalled(id);
            }
            Message::PendingCancel(id) => {
                if let Some((_, controller)) = self.pending_operations.get(&id) {
                    controller.cancel();
                    self.progress.remove(&progress::Key::Operation(id));
                }
                // One waiting on a question hears the cancel through it
                self.stalls.remove(&id);
                return Task::batch([self.dialog_pages.remove_stalled(id), self.drop_question(id)]);
            }
            Message::OperationBlocked(controller, question, tx) => {
                // Unknown, the question is dropped, which answers Cancel
                let Some((&id, _)) = self
                    .pending_operations
                    .iter()
                    .find(|(_, (_, pending))| pending.is_same(&controller))
                else {
                    return Task::none();
                };
                self.blocked.insert(
                    id,
                    Question {
                        blocked: question.blocked,
                        tx: Some(tx),
                        same_for_rest: false,
                        ask: question.ask,
                        item: question.item,
                    },
                );
                self.progress.block(&progress::Key::Operation(id));
                return Task::batch([
                    self.push_dialog(DialogPage::Blocked(id), Some(BLOCKED_BUTTON_ID.clone())),
                    self.sync_question_notices(),
                ]);
            }
            Message::BlockedAnswer(id, answer) => {
                if let Some(question) = self.blocked.get_mut(&id) {
                    question.answer(answer);
                    self.progress.unblock(&progress::Key::Operation(id));
                }
                return Task::batch([
                    self.dialog_pages.remove_blocked(id),
                    self.sync_question_notices(),
                ]);
            }
            Message::QuestionNotified(id, generation, notice) => {
                // Answered, over, or shown again while it was being shown
                if let Some(late) =
                    accept_notice(&mut self.question_notices, id, generation, notice)
                {
                    return close_question_notice(late);
                }
            }
            Message::QuestionNoticeAnswered(id, notice, answer) => {
                // Only from the notification on record: an older one's late
                // answer was for a question that is gone
                if self.notice_is(id, notice) {
                    self.question_notices.remove(&id);
                    return self.update(Message::BlockedAnswer(id, answer));
                }
            }
            Message::QuestionNoticeClosed(id, notice) => {
                // Dismissed while still the one asking: it comes back after a
                // moment, as nothing else can answer it while nobody sees the
                // window. One the app took down is no longer listed, and a
                // late close of an older one leaves the newer alone.
                if self.notice_is(id, notice) {
                    self.question_notices.remove(&id);
                    return Task::future(async {
                        tokio::time::sleep(NOTICE_AGAIN).await;
                        crate::ui::action::app(Message::QuestionNoticesSync)
                    });
                }
            }
            Message::QuestionNoticesSync => return self.sync_question_notices(),
            Message::WindowFocusChanged(focused) => {
                // Looked at once the shell has taken the change in
                let sync = Task::future(async {
                    tokio::time::sleep(FOCUS_SETTLE).await;
                    crate::ui::action::app(Message::QuestionNoticesSync)
                });
                if focused {
                    return Task::batch([self.update(Message::CheckClipboard), sync]);
                }
                return sync;
            }
            Message::BlockedSameForRest(id, same_for_rest) => {
                if let Some(question) = self.blocked.get_mut(&id) {
                    question.same_for_rest = same_for_rest;
                }
            }
            Message::RetryFailed(id) => {
                if let Some((operation, _, _)) = self.failed_operations.remove(&id) {
                    // The retry starts a row of its own
                    self.progress.remove(&progress::Key::Operation(id));
                    return self.operation(operation);
                }
            }
            Message::PendingComplete(id, op_sel) => {
                return self.handle_completed_operations(vec![(id, op_sel)]);
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
            Message::ProgressEnd(key, outcome) => match outcome {
                Outcome::Done => self.progress.finish(&key, true, None, Instant::now()),
                Outcome::Failed => self.progress.finish(&key, false, None, Instant::now()),
                // The user called it off, so no "Failed".
                Outcome::Cancelled => self.progress.remove(&key),
            },
            Message::ProgressDismiss(row) => {
                self.progress.dismiss(row, Instant::now());
            }
            Message::ProgressHover(row, over) => {
                self.progress.hover(row, over, Instant::now());
            }
            Message::ProgressTick => {
                // A slid-shut action card is forgotten by `after_update`,
                // which runs after this
                self.progress.prune(Instant::now());
            }
            Message::PermanentlyDelete(entity_opt) => {
                let paths: Box<[_]> = self.selected_paths(entity_opt).collect();
                if !paths.is_empty() {
                    // Asked first, unless the user said never to
                    if self.config.delete_i_am_stupid {
                        return self.operation(Operation::PermanentlyDelete { paths });
                    }
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
                    // they describe a listing it is no longer showing. Opening
                    // or closing an empty search keeps the listing.
                    if tab.location.lists_same_as(&location) && tab.listing() == listing {
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
                        // With no record of where it came from it has
                        // nowhere to go back to
                        if item.selected
                            && let ItemMetadata::Trash { entry, .. } = &item.metadata
                            && !crate::trashing::origin_unknown(entry)
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
            Message::SearchClosed => {
                self.search_closing = None;
            }
            Message::SearchInput(input) => {
                return self.search_set_active(Some(input));
            }
            // The search keeps its top result selected until the user picks
            // another, so this opens that one or theirs.
            Message::SearchSubmit => {
                return self.update(Message::TabMessage(None, tab::Message::Open(None)));
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
                self.progress.remove(&progress::Key::Load(entity));

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
            Message::SetSearchRecursive(recursive) => {
                // The config remembers this for the next search; the search
                // that is running is told directly, because it may already
                // disagree with a config that is not changing here.
                let mut config = self.config.tab;
                config.search_recursive = recursive;
                config_set!(tab, config);
                return Task::batch([
                    self.update_config(),
                    self.update(Message::TabMessage(
                        None,
                        tab::Message::SetSearchRecursive(recursive),
                    )),
                ]);
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
                                self.update_tab(entity, tab_path, selection_paths, true),
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
            Message::TabRescan(
                entity,
                scan,
                location,
                parent_item_opt,
                items,
                ancestors,
                title,
                selection_paths,
                read_ok,
            ) => {
                // Both sides were normalized already: the tab's location by
                // `Tab::new` or `change_location`, this one on the worker.
                // Opening or closing an empty search meanwhile lists the same
                // folder, and reads nothing itself: the read is still wanted.
                // A read started after this one has the newer listing.
                if let Some(tab) = self.tab_model.data_mut::<Tab>(entity)
                    && tab.is_latest_scan(scan)
                    && tab.location.lists_same_as(&location)
                {
                    self.progress.finish(
                        &progress::Key::Load(entity),
                        read_ok,
                        None,
                        Instant::now(),
                    );
                    tab.parent_item_opt = parent_item_opt;
                    // Asked of the listing being replaced. A selection being
                    // restored is the user's, not the search's.
                    let search_selects = tab.search_selection_is_own() && selection_paths.is_none();
                    tab.set_items(items);
                    // Read for the other mode, folder or empty search, the
                    // names hold but the breadcrumbs and title are this one's
                    let (ancestors, title) = if location == tab.location {
                        (ancestors, title)
                    } else {
                        tab.named_folders(ancestors.into_iter().map(|(_, name)| name).collect())
                            .unwrap_or_else(|| {
                                (tab.location.ancestors(false), tab.location.title(false))
                            })
                    };
                    tab.location_ancestors = ancestors;
                    // An empty search of a folder is sorted as the folder is
                    let location_str = location
                        .empty_search_folder()
                        .map_or_else(|| location.to_string(), |path| path.display().to_string());
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
                    // An empty search lists the folder, and no result arrives
                    // to select the top of it
                    tab.follow_search_top(search_selects);

                    tasks.push(clipboard::read_data::<ClipboardPaste>().map(|p| {
                        crate::ui::action::app(Message::CutPaths(match p {
                            Some(s) => match s.kind {
                                ClipboardKind::Copy => Vec::new(),
                                ClipboardKind::Cut => s.paths,
                            },
                            None => Vec::new(),
                        }))
                    }));

                    // The tab has been titled from its path; the worker's
                    // title is the one the filesystem gives.
                    if tab.location_title != title {
                        tab.location_title = title.clone();
                        self.tab_model.text_set(entity, title);
                        tasks.push(self.update_title());
                    }

                    return Task::batch(tasks);
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
                        // One part at a time, in order
                        let mut parts: VecDeque<Operation> = undo.into();
                        return match parts.pop_front() {
                            Some(first) => self.undo_part(first, parts),
                            None => Task::none(),
                        };
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
                // Exits now, or once the operations still running finish or
                // fail (`maybe_exit`).
                if main_window_closed(&mut self.core, id) {
                    return Task::future(async move { crate::ui::action::app(Message::MaybeExit) });
                }
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
                if let Some(data) = self.nav_model.data::<MounterData>(entity) {
                    let (mounter_key, item) = (data.0, data.1.clone());
                    return self.eject(mounter_key, item);
                }
            }
            Message::NavBarDrop(entity) => {
                if let Some(to) = self
                    .nav_model
                    .data::<Location>(entity)
                    .and_then(|location| location.path_opt().cloned())
                {
                    return Self::drop_files(to, crate::ui::dnd::drop_copies(self.modifiers));
                }
            }
            Message::NavDrop(drop) => {
                use crate::config::sidebar::{self, Edit};
                use segmented_button::NavDrop;

                let len = self.config.favorites.len();
                let recents_position = self.config.recents_position;
                // Where an entry is among the pinned entries and Recents; see
                // `sidebar::entries`.
                let place = |entity: Entity| {
                    if matches!(
                        self.nav_model.data::<Location>(entity),
                        Some(Location::Recents)
                    ) {
                        Some(sidebar::recents_index(recents_position, len))
                    } else {
                        self.nav_model
                            .data::<FavoriteIndex>(entity)
                            .map(|FavoriteIndex(i)| {
                                sidebar::favorite_entry(*i, recents_position, len)
                            })
                    }
                };
                // Past the last entry: the pinned entries and Recents.
                let before = |before: Option<Entity>| before.map_or(Some(len + 1), place);

                let edit = match drop {
                    NavDrop::Move {
                        dragged,
                        before: to,
                    } => place(dragged)
                        .zip(before(to))
                        .map(|(from, before)| Edit::Move { from, before }),
                    NavDrop::Pin { path, before: to } => before(to).map(|before| Edit::Pin {
                        favorite: Favorite::from_path(path),
                        before,
                    }),
                    NavDrop::Unpin(entity) => place(entity).map(|at| Edit::Unpin { at }),
                };
                let Some(edited) = edit
                    .and_then(|edit| sidebar::edit(&self.config.favorites, recents_position, edit))
                else {
                    return Task::none();
                };

                let toast = edited.unpinned.map(|(favorite, at)| {
                    let name = favorite.display_name().unwrap_or_else(|| fl!("filesystem"));
                    self.toasts
                        .push(
                            widget::toaster::Toast::new(fl!("removed-from-sidebar", name = name))
                                .action(fl!("undo"), move |id| {
                                    Message::NavUnpinUndo(id, favorite.clone(), at)
                                }),
                        )
                        .map(crate::ui::Action::App)
                });
                config_set!(favorites, edited.favorites);
                config_set!(recents_position, edited.recents_position);
                return Task::batch([self.update_config(), toast.unwrap_or_else(Task::none)]);
            }
            Message::NavUnpinUndo(id, favorite, at) => {
                use crate::config::sidebar::{self, Edit};

                self.toasts.remove(id);
                if let Some(edited) = sidebar::edit(
                    &self.config.favorites,
                    self.config.recents_position,
                    Edit::Restore { favorite, at },
                ) {
                    config_set!(favorites, edited.favorites);
                    config_set!(recents_position, edited.recents_position);
                    return self.update_config();
                }
            }
            Message::TabDrop(entity) => {
                if let Some(to) = self
                    .tab_model
                    .data::<Tab>(entity)
                    .and_then(|tab| tab.location.path_opt().cloned())
                {
                    return Self::drop_files(to, crate::ui::dnd::drop_copies(self.modifiers));
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
                    if let Some(path) = self
                        .nav_model
                        .data::<Location>(entity)
                        .and_then(Location::path_opt)
                        .cloned()
                    {
                        return self.open_with_after_reading(path);
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
                    // Through `sidebar`, so Recents keeps its place.
                    if let Some(edited) = self.nav_model.data::<FavoriteIndex>(entity).and_then(
                        |FavoriteIndex(favorite_i)| {
                            crate::config::sidebar::unpin_favorite(
                                &self.config.favorites,
                                self.config.recents_position,
                                *favorite_i,
                            )
                        },
                    ) {
                        config_set!(favorites, edited.favorites);
                        config_set!(recents_position, edited.recents_position);
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
                    // Found before `self` is borrowed to track the row.
                    let found = self.selected_paths(None).next().and_then(|p| {
                        self.mounter_items.iter().find_map(|(k, mounter_items)| {
                            let item = mounter_items
                                .iter()
                                .find(|&item| item.path().is_some_and(|path| path == p))?;
                            Some((*k, item.clone()))
                        })
                    });
                    if let Some((mounter_key, item)) = found {
                        return self.eject(mounter_key, item);
                    }
                }
            }
            Message::EjectChecked(mounter_key, item, root, has_items) => {
                if !has_items {
                    return self.unmount(mounter_key, item);
                }
                return self.push_dialog(
                    DialogPage::EmptyBeforeEject {
                        mounter_key,
                        item,
                        root,
                    },
                    Some(EMPTY_TRASH_BUTTON_ID.clone()),
                );
            }
            Message::TrashBins(asked, bins) => {
                // Overtaken by a newer search, it may miss what changed since
                if asked != self.trash_bins_asked {
                    return Task::none();
                }
                crate::trash::set_folders(bins.iter().cloned().collect());
                // Changed, the watcher follows, and the trash icon and open
                // trash tabs are brought up to date
                if bins != self.trash_bins {
                    self.trash_bins = bins;
                    return self.update(Message::RescanTrash);
                }
            }
            Message::EjectWithoutEmptying => {
                if matches!(
                    self.dialog_pages.front(),
                    Some(DialogPage::EmptyBeforeEject { .. })
                ) && let Some((
                    DialogPage::EmptyBeforeEject {
                        mounter_key, item, ..
                    },
                    task,
                )) = self.dialog_pages.pop_front()
                {
                    return Task::batch([task, self.unmount(mounter_key, item)]);
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

    fn dialog_area(&self) -> Option<Rectangle> {
        let area = self.file_view_bounds.get();
        (area.width > 0.0 && area.height > 0.0).then_some(area)
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
                    // Read, not looked up, for the reason `name_dialog` gives:
                    // this runs on every frame the dialog is drawn.
                    let taken = self
                        .name_check
                        .as_ref()
                        .filter(|check| check.path == path)
                        .and_then(|check| check.taken);
                    if taken.is_some() {
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
            DialogPage::EmptyBeforeEject { item, .. } => widget::dialog()
                .title(fl!("empty-before-eject-title"))
                .body(fl!("empty-before-eject-body", name = item.name()))
                .primary_action(
                    widget::button::destructive(fl!("empty-trash"))
                        .on_press(Message::DialogComplete)
                        .id(EMPTY_TRASH_BUTTON_ID.clone()),
                )
                .secondary_action(
                    widget::button::standard(fl!("do-not-empty"))
                        .on_press(Message::EjectWithoutEmptying),
                )
                .tertiary_action(
                    widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                ),
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
            DialogPage::Blocked(id) => Self::blocked_dialog(*id, self.blocked.get(id)?),
            DialogPage::Stalled(id) => {
                let (op, controller) = self.pending_operations.get(id)?;
                widget::dialog()
                    .title(fl!("stalled-title"))
                    .body(op.pending_text(controller.progress(), controller.state()))
                    .primary_action(
                        widget::button::suggested(fl!("try-again"))
                            .on_press(Message::StalledWait(*id))
                            .id(BLOCKED_BUTTON_ID.clone()),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("abort")).on_press(Message::PendingAbort(*id)),
                    )
                    .tertiary_action(
                        widget::button::standard(fl!("cancel"))
                            .on_press(Message::PendingCancel(*id)),
                    )
            }
            DialogPage::FailedOperation(id) => {
                let (operation, controller, err) = self.failed_operations.get(id)?;

                widget::dialog()
                    .title(fl!("failed-operations-title", count = 1))
                    .body(failed_text(operation, controller, err))
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
            DialogPage::OpenWithLoading { path, .. } => {
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
                totals,
                multiple,
                apply_to_all,
                conflict_count,
                tx,
            } => {
                // A block per side, with a folder's totals under it
                fn side(
                    item: &tab::Item,
                    heading: String,
                    totals: Option<(u64, u64)>,
                ) -> Element<'_, Message> {
                    let mut column = widget::Column::with_capacity(2)
                        .push(
                            item.replace_view(heading)
                                .map(|x| Message::TabMessage(None, x)),
                        )
                        .spacing(spacing().space_xxxs.to_pixels());
                    if let Some((files, bytes)) = totals {
                        column = column.push(widget::text::caption(fl!(
                            "folder-totals",
                            files = files,
                            size = tab::format_size(bytes)
                        )));
                    }
                    column.into()
                }
                let folders = totals.is_some();
                let mut dialog = widget::dialog()
                    .title(fl!("replace-title", filename = to.name.as_str()))
                    .body(if folders {
                        fl!("replace-folder-warning")
                    } else {
                        fl!("replace-warning-operation")
                    })
                    .control(side(to, fl!("original-file"), totals.map(|(_, to)| to)))
                    .control(side(
                        from,
                        fl!("replace-with"),
                        totals.map(|(from, _)| from),
                    ))
                    .primary_action(
                        widget::button::suggested(fl!("replace"))
                            .on_press(Message::ReplaceResult(ReplaceResult::Replace(
                                *apply_to_all,
                            )))
                            .id(REPLACE_BUTTON_ID.clone()),
                    )
                    .tertiary_action(
                        widget::button::standard(fl!("cancel"))
                            .on_press(Message::ReplaceResult(ReplaceResult::Cancel)),
                    );
                // Beside it: Merge for two folders, Keep both, Skip
                let mut others: Vec<Element<'_, Message>> = Vec::new();
                if folders {
                    others.push(
                        widget::button::standard(fl!("merge"))
                            .on_press(Message::ReplaceResult(ReplaceResult::Merge(*apply_to_all)))
                            .into(),
                    );
                }
                others.push(
                    widget::button::standard(fl!("keep-both"))
                        .on_press(Message::ReplaceResult(ReplaceResult::KeepBoth))
                        .into(),
                );
                if *multiple {
                    others.push(
                        widget::button::standard(fl!("skip"))
                            .on_press(Message::ReplaceResult(ReplaceResult::Skip(*apply_to_all)))
                            .into(),
                    );
                }
                dialog = dialog.secondary_action(
                    widget::Row::with_children(others).spacing(spacing().space_xxs.to_pixels()),
                );
                if *multiple && *conflict_count > 1 {
                    dialog = dialog.control(
                        widget::checkbox(*apply_to_all)
                            .label(format!("{} ({})", fl!("apply-to-all"), *conflict_count))
                            .on_toggle(|apply_to_all| {
                                Message::DialogUpdate(DialogPage::Replace {
                                    from: from.clone(),
                                    to: to.clone(),
                                    totals: *totals,
                                    multiple: *multiple,
                                    apply_to_all,
                                    conflict_count: *conflict_count,
                                    tx: tx.clone(),
                                })
                            }),
                    );
                }
                dialog
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
            DialogPage::LaunchDesktopEntry { path, command }
            | DialogPage::LaunchExecutable { path, command } => {
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

    /// The active tab's history and crumb pills, or its path field.
    fn header_center(&self) -> Vec<Element<'_, Self::Message>> {
        let entity = self.tab_model.active();
        self.tab_model
            .data::<Tab>(entity)
            .map(|tab| {
                tab.header_view()
                    .map(move |message| Message::TabMessage(Some(entity), message))
            })
            .into_iter()
            .collect()
    }

    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        let mut elements = Vec::with_capacity(2);

        let closing = self.search_closing_term();
        if let Some(term) = self.search_get() {
            if self.core.is_condensed() {
                // The field itself is collapsed here, so the eye stands on
                // its own beside the button that clears the search. Without
                // it a narrow window could not widen a search at all.
                elements.extend(self.search_scope_button());
                elements.push(
                    widget::button::icon(icon::line::handle(icon::line::SEARCH))
                        .on_press(Message::SearchClear)
                        .padding(8)
                        .selected(true)
                        .into(),
                );
            } else {
                elements.push(self.search_field(term, false));
            }
        } else if let Some(term) = closing.filter(|_| !self.core.is_condensed()) {
            elements.push(self.search_field(term, true));
        } else {
            elements.push(
                widget::button::icon(icon::line::handle(icon::line::SEARCH))
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

        if self.core.is_condensed() {
            let open = self.search_get();
            if let Some(term) = open.or(self.search_closing_term()) {
                let closing = open.is_none();
                let mut input =
                    search_text_input(&self.search_id, term, closing).width(Length::Fill);
                if !closing {
                    input = input.on_clear(Message::SearchClear);
                }
                // Unrolls under the header, and rolls back up once the
                // search ends.
                tab_column = tab_column.push(
                    widget::spring_height(widget::container(input).padding(space_xxs))
                        .collapsed(closing)
                        .on_collapsed(Message::SearchClosed),
                );
            }
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
                        // The active tab included: a drag held over a tab
                        // switches to it, and may then be let go there.
                        .on_file_drop({
                            let droppable: Vec<Entity> = self
                                .tab_model
                                .iter()
                                .filter(|entity| {
                                    self.tab_model.data::<Tab>(*entity).is_some_and(|tab| {
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
                        // Held over by a file drag, a tab is switched to;
                        // the active one has nothing to switch
                        .on_drag_hover_open({
                            let active = self.tab_model.active();
                            move |entity| (entity != active).then_some(Message::TabActivate(entity))
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
            // Room under the files for the action card over them, while it
            // is this tab's and not sliding shut
            let bottom_reserve = self
                .action_card
                .shown()
                .filter(|shown| shown.entity == entity && !shown.leaving)
                .map(|_| &self.action_card_height);
            let tab_view = tab
                .view(
                    &self.key_binds,
                    &self.modifiers,
                    self.clipboard_has_content(),
                    &self.config.context_actions,
                    self.core.drawer_slide.column_slide(),
                    bottom_reserve,
                )
                .map(move |message| Message::TabMessage(Some(entity), message));
            tab_column = tab_column.push(tab_view);
        }

        // The toaster is added on top of an empty element to ensure that it does not override context menus
        // The action and progress cards are pinned under them, in the same corner.
        let progress_card = self.progress_card();
        tab_column = tab_column.push(widget::toaster(
            &self.toasts,
            [self.action_card(progress_card.is_some()), progress_card],
            widget::space::horizontal(),
        ));

        let content: Element<_> = widget::measure_bounds(tab_column, &self.file_view_bounds).into();

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
                Event::Window(WindowEvent::Focused) => Some(Message::WindowFocusChanged(true)),
                Event::Window(WindowEvent::Unfocused) => Some(Message::WindowFocusChanged(false)),
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
            // Keyed by the folders, so a changed set starts a new watcher
            Subscription::run_with(
                (
                    TypeId::of::<TrashWatcherSubscription>(),
                    self.trash_bins.clone(),
                ),
                |(_, trash_bins)| {
                    let trash_bins = trash_bins.clone();
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
                                            log::warn!(
                                                "failed to watch trash bin for changes: {e:?}"
                                            );
                                        }
                                    }
                                },
                            );

                            match watcher_res {
                                Ok(mut watcher) => {
                                    // Watch the "bins" themselves as well as the files folder where
                                    // trashed items are placed. This allows us to avoid recursively
                                    // watching the trash which is slow but also properly get events.
                                    let trash_paths = trash_bins
                                        .into_iter()
                                        .flat_map(|path| [path.join("files"), path]);
                                    for path in trash_paths {
                                        if let Err(e) = watcher
                                            .watch(&path, notify::RecursiveMode::NonRecursive)
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
                                Err(e) => {
                                    log::warn!("failed to create new watcher for trash bin: {e:?}");
                                }
                            }

                            std::future::pending().await
                        },
                    )
                },
            ),
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

        let now = Instant::now();
        if let Some(at) = [
            self.progress.next_deadline(now),
            self.action_card.next_deadline(now),
        ]
        .into_iter()
        .flatten()
        .min()
        {
            // Rows appear after their delay and leave after their linger,
            // and the action card is forgotten once it has slid shut,
            // without anything else happening to redraw them: one wake-up at
            // the next such moment, not a steady tick. Keyed on the moment,
            // so a new deadline starts a new wait.
            subscriptions.push(Subscription::run_with(
                ("progress_deadline", at),
                |&(_, at)| {
                    stream::channel(
                        1,
                        move |mut output: futures::channel::mpsc::Sender<Message>| async move {
                            tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
                            let _ = output.send(Message::ProgressTick).await;
                        },
                    )
                },
            ));
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
                            .map(|_| Message::PendingTick),
                    );
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
/// Where each path a move took went, for rewriting favorites. The pairs the
/// operation recorded come first: a Keep Both conflict gives the destination
/// another name, and guessing `to/name` would point the favorite at the file
/// that was already there. The guesses are kept after them for a folder merged
/// into one that already existed, which records only its children.
/// A failed operation as its dialog lists it: what it was doing, then why
/// it stopped.
fn failed_text(operation: &Operation, controller: &Controller, err: &str) -> String {
    let doing = operation.pending_text(controller.progress(), ControllerState::Failed);
    format!("{doing}\n{err}")
}

/// What a question says, and the label of its Skip answer, which for an
/// original a move could not remove keeps it.
fn question_text(blocked: &Blocked) -> (String, String) {
    let path = blocked.path();
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    match blocked {
        Blocked::Read(_) => (fl!("blocked-read", name = name), fl!("skip")),
        Blocked::List(_) => (fl!("blocked-list", name = name), fl!("skip")),
        Blocked::Remove(_) => (fl!("blocked-remove", name = name), fl!("keep-original")),
        Blocked::Link { fs: Some(fs), .. } => (
            fl!("blocked-link-fs", name = name, fs = fs.to_string()),
            fl!("skip"),
        ),
        Blocked::Link { fs: None, .. } => (fl!("blocked-link", name = name), fl!("skip")),
        Blocked::Move { .. } => (fl!("blocked-move", name = name), fl!("skip")),
        Blocked::Destination(_) => (fl!("destination-no-permission", folder = name), fl!("skip")),
        Blocked::Failed { .. } => (fl!("blocked-failed", name = name), fl!("try-again")),
        Blocked::TooBig { fs, .. } => (
            fl!("blocked-too-big", name = name, fs = fs.to_string()),
            fl!("skip"),
        ),
        Blocked::BadName { fs, .. } => (
            fl!("blocked-bad-name", name = name, fs = fs.to_string()),
            fl!("skip"),
        ),
        Blocked::Delete(_) => (fl!("blocked-delete", name = name), fl!("skip")),
        Blocked::NoTrash(_) => (fl!("blocked-no-trash", name = name), fl!("skip")),
    }
}

/// The first path of `later` that `running` holds in a way that keeps
/// `later` from starting.
fn collision(running: &Operation, later: &Operation) -> Option<PathBuf> {
    let (running_kind, running_items) = busy_items(running)?;
    let (kind, items) = busy_items(later)?;
    let collides = match (running_kind, kind) {
        (Busy::Move, _) | (Busy::Copy, Busy::Delete) | (Busy::Delete, Busy::Copy | Busy::Move) => {
            true
        }
        (Busy::Copy, Busy::Copy | Busy::Move) | (Busy::Delete, Busy::Delete) => false,
    };
    if !collides {
        return None;
    }
    items.into_iter().find(|item| {
        running_items
            .iter()
            .any(|other| item.starts_with(other) || other.starts_with(item))
    })
}

/// What an operation does to the paths it holds, for telling which may run
/// beside each other.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Busy {
    Copy,
    Move,
    Delete,
}

/// The paths `operation` holds while it runs: what it takes and, for a
/// copy or move, what it makes. `None` for operations that hold nothing
/// another could get in the way of.
fn busy_items(operation: &Operation) -> Option<(Busy, Vec<PathBuf>)> {
    let made = |paths: &[PathBuf], to: &Path| -> Vec<PathBuf> {
        paths
            .iter()
            .cloned()
            .chain(
                paths
                    .iter()
                    .filter_map(|path| path.file_name())
                    .map(|name| to.join(name)),
            )
            .collect()
    };
    match operation {
        Operation::Copy { paths, to } => Some((Busy::Copy, made(paths, to))),
        Operation::Move { paths, to, .. } => Some((Busy::Move, made(paths, to))),
        Operation::Delete { paths } => Some((Busy::Delete, paths.clone())),
        Operation::PermanentlyDelete { paths } => Some((Busy::Delete, paths.to_vec())),
        _ => None,
    }
}

/// Why a question is asked, when its title does not say: for a move, the
/// folder its originals could not be removed from, and the reason.
fn question_detail(blocked: &Blocked) -> Option<String> {
    if let Blocked::Failed { reason, .. } = blocked {
        return Some(reason.clone());
    }
    let Blocked::Move {
        folder, read_only, ..
    } = blocked
    else {
        return None;
    };
    let name = folder.file_name().map_or_else(
        || folder.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let reason = if *read_only {
        fl!("reason-read-only")
    } else {
        fl!("reason-no-permission")
    };
    Some(fl!("blocked-move-reason", folder = name, reason = reason))
}

/// What the root button says: asking for the password the first time,
/// using what was granted after. Deleting with root is always for good, and
/// says so.
fn root_label(blocked: &Blocked, again: bool) -> String {
    if matches!(blocked, Blocked::Delete(_)) {
        fl!("delete-permanently-as-root")
    } else if again {
        fl!("use-root-again")
    } else {
        fl!("retry-as-root")
    }
}

/// The actions of a question's desktop notification.
#[cfg(feature = "notify")]
const NOTICE_SKIP: &str = "skip";
#[cfg(feature = "notify")]
const NOTICE_RETRY: &str = "retry";
#[cfg(feature = "notify")]
const NOTICE_ABORT: &str = "abort";
#[cfg(feature = "notify")]
const NOTICE_ROOT: &str = "root";
#[cfg(feature = "notify")]
const NOTICE_CANCEL: &str = "cancel";
#[cfg(feature = "notify")]
const NOTICE_PERMANENTLY: &str = "permanently";

/// How long a dismissed question notification stays away before it comes
/// back.
const NOTICE_AGAIN: Duration = Duration::from_secs(1);

/// How long after a focus change the notifications are looked at again,
/// for the shell to have taken the change in.
const FOCUS_SETTLE: Duration = Duration::from_millis(50);

/// The answer a question's notification action stands for; none for a
/// click on the notification itself or its closing.
#[cfg(feature = "notify")]
fn notice_answer(action: &str) -> Option<BlockedAnswer> {
    match action {
        NOTICE_SKIP => Some(BlockedAnswer::Skip(false)),
        NOTICE_RETRY => Some(BlockedAnswer::Retry),
        NOTICE_ABORT => Some(BlockedAnswer::Abort),
        NOTICE_ROOT => Some(BlockedAnswer::RetryAsRoot(false)),
        NOTICE_CANCEL => Some(BlockedAnswer::Cancel),
        NOTICE_PERMANENTLY => Some(BlockedAnswer::Permanently(false)),
        _ => None,
    }
}

/// Takes a question's notification down. Closing goes by its id, which
/// needs a handle, and the one there was is waiting for its answer: showing
/// the same notification again under that id gives another to close with.
#[cfg(feature = "notify")]
fn close_question_notice((id, mut notification): QuestionNotice) -> Task<Message> {
    Task::future(async move {
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(handle) = notification.id(id).show() {
                handle.close();
            }
        })
        .await;
        crate::ui::action::none()
    })
}

#[cfg(not(feature = "notify"))]
fn close_question_notice((): QuestionNotice) -> Task<Message> {
    Task::none()
}

/// Tells the desktop an operation succeeded, for when the window is not
/// looked at: `text` says what it did.
#[cfg(feature = "notify")]
fn notify_done(text: String) -> Task<Message> {
    Task::future(async move {
        let shown = tokio::task::spawn_blocking(move || {
            notify_rust::Notification::new().summary(&text).show()
        })
        .await;
        if let Ok(Err(err)) = shown {
            log::warn!("failed to show the success notification: {err}");
        }
        crate::ui::action::none()
    })
}

#[cfg(not(feature = "notify"))]
fn notify_done(_text: String) -> Task<Message> {
    Task::none()
}

fn move_path_changes(
    moved: &[(PathBuf, PathBuf)],
    paths: &[PathBuf],
    to: &Path,
) -> Vec<(PathBuf, PathBuf)> {
    moved
        .iter()
        .cloned()
        .chain(
            paths
                .iter()
                .filter_map(|from| from.file_name().map(|name| (from.clone(), to.join(name)))),
        )
        .collect()
}

/// `uri` without any `user[:password]@` before its host, for showing on
/// screen: a network drive's address can carry the credentials typed into it.
fn uri_for_display(uri: &str) -> String {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return uri.to_owned();
    };
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    match authority.rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => uri.to_owned(),
    }
}

/// Lets go of the main window when the compositor closed `id` and it was the
/// main one, returning whether it was.
///
/// A close from the compositor — a keybind, a server-side titlebar — never
/// reaches `Message::WindowClose`: exwlshell tears the surface down itself and
/// reports only `window::Event::Closed`, and its event loop keeps running with
/// no windows left because the shell starts in `StartMode::Background`. So
/// this is where the process learns it may exit.
fn main_window_closed(core: &mut Core, id: window::Id) -> bool {
    if core.main_window_id() != Some(id) {
        return false;
    }
    core.set_main_window_id(None);
    true
}

/// The search field's input. `closing`, once its search has ended and it
/// springs shut, it takes no input: without `on_input` it lets go of the
/// focus, so keys go to type-to-search again instead of reopening the old
/// term.
fn search_text_input<'a>(
    id: &widget::Id,
    term: &'a str,
    closing: bool,
) -> widget::TextInput<'a, Message> {
    let input = widget::text_input::search_input("", term).id(id.clone());
    if closing {
        input
    } else {
        input
            .on_input(Message::SearchInput)
            .on_submit(|_| Message::SearchSubmit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ejecting asks about the trash only with the setting on, and never
    /// for a network drive, where nothing is trashed
    #[test]
    fn ejecting_asks_about_the_trash_only_when_set_and_local() {
        assert!(super::asks_before_ejecting(true, false));
        assert!(!super::asks_before_ejecting(false, false));
        assert!(!super::asks_before_ejecting(true, true));
    }

    /// Only the notification on record answers or closes: a late word from
    /// an older one, or from one still being shown, changes nothing
    #[cfg(feature = "notify")]
    #[test]
    fn only_the_notification_on_record_counts() {
        let mut notices: BTreeMap<u64, NoticeSlot> = BTreeMap::new();
        notices.insert(
            1,
            NoticeSlot {
                generation: 1,
                shown: Some((7, notify_rust::Notification::new())),
            },
        );
        notices.insert(
            2,
            NoticeSlot {
                generation: 2,
                shown: None,
            },
        );
        assert!(notice_on_record(&notices, 1, 7));
        assert!(!notice_on_record(&notices, 1, 6), "an older one");
        assert!(!notice_on_record(&notices, 2, 7), "still being shown");
        assert!(!notice_on_record(&notices, 3, 7), "none on record");
    }

    /// A notification shown too late, for a showing since replaced, is not
    /// taken in place of the newer one: it is handed back to be closed
    #[cfg(feature = "notify")]
    #[test]
    fn a_late_notification_does_not_take_a_newer_slot() {
        let mut notices: BTreeMap<u64, NoticeSlot> = BTreeMap::new();
        // Showing 2 is wanted; showing 1 was cleared before it came
        notices.insert(
            1,
            NoticeSlot {
                generation: 2,
                shown: None,
            },
        );
        let late = (5, notify_rust::Notification::new());
        assert!(accept_notice(&mut notices, 1, 1, late).is_some());
        assert!(!notice_on_record(&notices, 1, 5));
        let wanted = (6, notify_rust::Notification::new());
        assert!(accept_notice(&mut notices, 1, 2, wanted).is_none());
        assert!(notice_on_record(&notices, 1, 6));
        // Once shown, another for the same showing is a duplicate
        let again = (8, notify_rust::Notification::new());
        assert!(accept_notice(&mut notices, 1, 2, again).is_some());
    }

    /// §5.1: copies share; a move shares with nothing; a delete shares with
    /// neither a copy nor a move
    #[test]
    fn operations_on_the_same_items_collide_as_the_scenarios_say() {
        let copy = |from: &str, to: &str| Operation::Copy {
            paths: vec![from.into()],
            to: to.into(),
        };
        let move_ = |from: &str, to: &str| Operation::Move {
            paths: vec![from.into()],
            to: to.into(),
            cross_device_copy: false,
        };
        let delete = |path: &str| Operation::Delete {
            paths: vec![path.into()],
        };
        let a = PathBuf::from("/a/x");
        assert_eq!(collision(&copy("/a/x", "/b"), &copy("/a/x", "/c")), None);
        assert_eq!(collision(&copy("/a/x", "/b"), &move_("/a/x", "/c")), None);
        assert_eq!(
            collision(&move_("/a/x", "/b"), &copy("/a/x", "/c")),
            Some(a.clone())
        );
        assert_eq!(
            collision(&move_("/a/x", "/b"), &delete("/a/x")),
            Some(a.clone())
        );
        assert_eq!(
            collision(&copy("/a/x", "/b"), &delete("/a/x")),
            Some(a.clone())
        );
        assert_eq!(
            collision(&delete("/a/x"), &move_("/a/x", "/c")),
            Some(a.clone())
        );
        // What a copy makes counts, and a folder holds what is in it
        assert_eq!(
            collision(&copy("/a/x", "/b"), &delete("/b")),
            Some(PathBuf::from("/b"))
        );
        assert_eq!(
            collision(&move_("/a", "/b"), &copy("/a/x/y", "/c")),
            Some(PathBuf::from("/a/x/y"))
        );
        // Other items: no collision
        assert_eq!(collision(&move_("/a/x", "/b"), &copy("/a/y", "/c")), None);
    }

    /// A question about `/root/secrets.env`, and where its answer arrives.
    fn question(same_for_rest: bool) -> (Question, mpsc::Receiver<BlockedAnswer>) {
        let (tx, rx) = mpsc::channel(1);
        let question = Question {
            blocked: Blocked::Read(PathBuf::from("/root/secrets.env")),
            tx: Some(tx),
            same_for_rest,
            ask: Ask {
                count: 2,
                retry: true,
                root: None,
                not_granted: false,
                skip: true,
                abort: false,
                permanently: false,
            },
            item: None,
        };
        (question, rx)
    }

    /// The messages `question`'s dialog, drawn on its own, sends for
    /// `events`. The pointer is where the last `CursorMoved` put it.
    fn question_messages(
        question: &Question,
        events: &[crate::ui::iced_core::Event],
    ) -> Vec<Message> {
        use crate::ui::iced_core::{Event, Size, clipboard, mouse};
        use crate::ui::iced_runtime::user_interface::{Cache, UserInterface};

        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(
            Element::from(App::blocked_dialog(7, question)),
            Size::new(QUESTION_AREA.0, QUESTION_AREA.1),
            Cache::default(),
            &mut renderer,
        );
        let mut cursor = mouse::Cursor::Unavailable;
        let mut messages = Vec::new();
        for event in events {
            if let Event::Mouse(mouse::Event::CursorMoved { position }) = event {
                cursor = mouse::Cursor::Available(*position);
            }
            let _ = ui.update(
                std::slice::from_ref(event),
                cursor,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
        }
        messages
    }

    /// The size the question's dialog is drawn at in these tests.
    const QUESTION_AREA: (f32, f32) = (640.0, 320.0);

    /// A left click at every point of an 8 px grid over the question.
    fn click_everywhere() -> Vec<crate::ui::iced_core::Event> {
        use crate::ui::iced_core::{Event, Point, mouse};

        let mut events = Vec::new();
        for y in (4..QUESTION_AREA.1 as u32).step_by(8) {
            for x in (4..QUESTION_AREA.0 as u32).step_by(8) {
                events.push(Event::Mouse(mouse::Event::CursorMoved {
                    position: Point::new(x as f32, y as f32),
                }));
                events.push(Event::Mouse(mouse::Event::ButtonPressed(
                    mouse::Button::Left,
                )));
                events.push(Event::Mouse(mouse::Event::ButtonReleased(
                    mouse::Button::Left,
                )));
            }
        }
        events
    }

    /// The distinct answers among `messages`, for operation 7, in a fixed
    /// order.
    fn answers(messages: &[Message]) -> Vec<BlockedAnswer> {
        let mut answers: Vec<BlockedAnswer> = messages
            .iter()
            .filter_map(|message| match message {
                Message::BlockedAnswer(7, answer) => Some(*answer),
                _ => None,
            })
            .collect();
        answers.sort_by_key(|answer| format!("{answer:?}"));
        answers.dedup();
        answers
    }

    /// The question never takes the keyboard: Enter, Escape and Space typed
    /// while it is up answer nothing.
    #[test]
    fn a_question_takes_no_keys() {
        use crate::ui::iced_core::Event;
        use crate::ui::iced_core::keyboard::{self, Key, key};

        let press = |named: key::Named| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(named),
                modified_key: Key::Named(named),
                physical_key: key::Physical::Code(key::Code::Enter),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            })
        };
        let (question, _rx) = question(false);
        let messages = question_messages(
            &question,
            &[
                press(key::Named::Enter),
                press(key::Named::Escape),
                press(key::Named::Space),
            ],
        );
        assert!(messages.is_empty(), "{messages:?}");
    }

    /// Asked, its three buttons answer Cancel, Retry, and Skip carrying the
    /// "Same for the rest" box.
    #[test]
    fn a_question_answers_with_its_buttons() {
        for same_for_rest in [false, true] {
            let (question, _rx) = question(same_for_rest);
            let messages = question_messages(&question, &click_everywhere());
            let found = answers(&messages);
            let mut expected = vec![
                BlockedAnswer::Cancel,
                BlockedAnswer::Retry,
                BlockedAnswer::Skip(same_for_rest),
            ];
            expected.sort_by_key(|answer| format!("{answer:?}"));
            assert_eq!(found, expected);
        }
    }

    /// With nothing after it to ask about, there is no "Same for the rest":
    /// Skip skips this one, whatever the box last said.
    #[test]
    fn the_last_question_has_no_same_for_the_rest() {
        let (mut question, _rx) = question(true);
        question.ask.count = 1;
        let messages = question_messages(&question, &click_everywhere());
        assert!(
            !messages
                .iter()
                .any(|message| matches!(message, Message::BlockedSameForRest(..))),
            "{messages:?}"
        );
        assert!(answers(&messages).contains(&BlockedAnswer::Skip(false)));
        assert!(!answers(&messages).contains(&BlockedAnswer::Skip(true)));
    }

    /// A link the drive cannot hold is never retried: no Retry button.
    #[test]
    fn a_link_question_has_no_retry() {
        let (mut question, _rx) = question(false);
        // As the engine asks it before the operation runs
        question.ask.retry = false;
        question.blocked = Blocked::Link {
            path: PathBuf::from("/p/result"),
            fs: Some("exFAT"),
        };
        let found = answers(&question_messages(&question, &click_everywhere()));
        assert_eq!(found, [BlockedAnswer::Cancel, BlockedAnswer::Skip(false)]);
        let (text, _) = question_text(&question.blocked);
        assert_eq!(text, fl!("blocked-link-fs", name = "result", fs = "exFAT"));
    }

    /// A move that could not remove its originals offers Skip and Cancel,
    /// never Copy instead or Retry, and says why.
    #[test]
    fn a_move_question_offers_skip_and_cancel() {
        for same_for_rest in [false, true] {
            let (mut question, _rx) = question(same_for_rest);
            question.ask.retry = false;
            question.blocked = Blocked::Move {
                path: PathBuf::from("/p/aplin"),
                folder: PathBuf::from("/p"),
                read_only: false,
            };
            let found = answers(&question_messages(&question, &click_everywhere()));
            let mut expected = vec![BlockedAnswer::Cancel, BlockedAnswer::Skip(same_for_rest)];
            expected.sort_by_key(|answer| format!("{answer:?}"));
            assert_eq!(found, expected);
        }
        let detail = question_detail(&Blocked::Move {
            path: PathBuf::from("/p/aplin"),
            folder: PathBuf::from("/p"),
            read_only: true,
        });
        assert_eq!(
            detail,
            Some(fl!(
                "blocked-move-reason",
                folder = "p",
                reason = fl!("reason-read-only")
            ))
        );
    }

    /// Answered, the question stays on its row to roll up, but its buttons
    /// send nothing more.
    #[test]
    fn an_answered_question_has_no_live_buttons() {
        let (mut question, _rx) = question(false);
        assert!(question.answer(BlockedAnswer::Retry));
        let messages = question_messages(&question, &click_everywhere());
        assert!(answers(&messages).is_empty(), "{messages:?}");
    }

    /// A notification's buttons answer like the card's; clicking the
    /// notification itself or closing it answers nothing.
    #[cfg(feature = "notify")]
    #[test]
    fn notification_actions_answer_like_the_card() {
        assert_eq!(notice_answer(NOTICE_SKIP), Some(BlockedAnswer::Skip(false)));
        assert_eq!(notice_answer(NOTICE_RETRY), Some(BlockedAnswer::Retry));
        assert_eq!(notice_answer(NOTICE_CANCEL), Some(BlockedAnswer::Cancel));
        assert_eq!(notice_answer("default"), None);
        assert_eq!(notice_answer("__closed"), None);
    }

    /// An original a move could not remove is kept, not skipped.
    #[test]
    fn an_original_left_behind_is_kept() {
        let (text, skip) = question_text(&Blocked::Remove(PathBuf::from("/srv/build")));
        assert_eq!(text, fl!("blocked-remove", name = "build"));
        assert_eq!(skip, fl!("keep-original"));
        let (_, skip) = question_text(&Blocked::List(PathBuf::from("/srv/pg")));
        assert_eq!(skip, fl!("skip"));
    }

    /// An answer goes to the operation once.
    #[test]
    fn a_question_is_answered_once() {
        let (mut question, mut rx) = question(false);
        assert!(question.answer(BlockedAnswer::Skip(false)));
        assert!(!question.answer(BlockedAnswer::Retry));
        assert_eq!(rx.try_recv().ok(), Some(BlockedAnswer::Skip(false)));
        assert!(rx.try_recv().is_err());
    }

    /// Clearing a focused search and typing at once, while its field springs
    /// shut: the keys do not reach the old term.
    #[test]
    fn a_closing_search_field_takes_no_keys() {
        use crate::ui::iced_core::keyboard::{self, Key, key};
        use crate::ui::iced_core::widget::operation::focusable;
        use crate::ui::iced_core::{Event, Size, clipboard, mouse};
        use crate::ui::iced_runtime::user_interface::{Cache, UserInterface};

        let id = widget::Id::new("search");
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let window = Size::new(400.0, 100.0);
        let press = |text: &str| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Character(text.into()),
                modified_key: Key::Character(text.into()),
                physical_key: key::Physical::Code(key::Code::KeyA),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: Some(text.into()),
                repeat: false,
            })
        };
        let view: Element<'_, Message> = search_text_input(&id, "old", false).into();
        let mut ui = UserInterface::build(view, window, Cache::default(), &mut renderer);
        ui.operate(&renderer, &mut focusable::focus(id.clone()));
        let cache = ui.into_cache();

        let mut send = |cache: Cache, closing: bool, event: Event| {
            let view: Element<'_, Message> = search_text_input(&id, "old", closing).into();
            let mut ui = UserInterface::build(view, window, cache, &mut renderer);
            let mut messages = Vec::new();
            let _ = ui.update(
                &[event],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            (ui.into_cache(), messages)
        };

        let (cache, messages) = send(cache, false, press("x"));
        assert!(
            matches!(messages.as_slice(), [Message::SearchInput(term)] if term == "oldx"),
            "open, it takes keys: {messages:?}"
        );

        let (cache, messages) = send(cache, true, press("a"));
        assert!(messages.is_empty(), "closing: {messages:?}");
        let (_, messages) = send(cache, true, press("b"));
        assert!(messages.is_empty(), "still closing: {messages:?}");
    }

    /// The action card's height for `action`, laid out on its own, and the
    /// room it recorded for the tab to keep under its files.
    fn action_card_heights(action: tab::TabAction) -> (f32, f32) {
        use crate::ui::iced_core::widget::{Id as WidgetId, Operation};
        use crate::ui::iced_runtime::user_interface::{Cache, UserInterface};

        struct Bounds(Option<Rectangle>);
        impl Operation for Bounds {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn container(&mut self, id: Option<&WidgetId>, bounds: Rectangle) {
                if id == Some(&WidgetId::new("card")) {
                    self.0 = Some(bounds);
                }
            }
        }

        let shown = Shown {
            entity: Entity::default(),
            action,
            leaving: false,
        };
        let recorded = Cell::new(0.0);
        let card = widget::id_container(
            App::action_card_view(shown, false, &recorded),
            WidgetId::new("card"),
        );
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(
            Element::from(card),
            Size::new(800.0, 600.0),
            Cache::default(),
            &mut renderer,
        );
        // The card springs open from nothing: run frames until it has
        // settled at its natural height.
        let mut now = Instant::now();
        for _ in 0..200 {
            now += Duration::from_millis(16);
            let _ = ui.update(
                &[Event::Window(WindowEvent::RedrawRequested(now))],
                iced::mouse::Cursor::Unavailable,
                &mut renderer,
                &mut crate::ui::iced_core::clipboard::Null,
                &mut Vec::new(),
            );
        }
        let mut bounds = Bounds(None);
        ui.operate(&renderer, &mut bounds);
        drop(ui);
        (
            bounds.0.expect("the card is laid out").height,
            recorded.get(),
        )
    }

    #[test]
    fn the_card_records_its_height_and_margin() {
        use tab::{TabAction, TrashSize};
        for action in [
            TabAction::EmptyTrash(None),
            TabAction::EmptyTrash(Some((12, TrashSize::Calculating))),
            TabAction::EmptyTrash(Some((12, TrashSize::Partial(340_000_000)))),
            TabAction::ClearRecents(None),
            TabAction::ClearRecents(Some(137)),
            TabAction::AddNetworkDrive(0),
            TabAction::AddNetworkDrive(2),
        ] {
            let (height, recorded) = action_card_heights(action);
            assert_eq!(recorded, height + widget::toaster::OFFSET, "{action:?}");
        }
    }

    #[test]
    fn the_detail_drops_under_the_title_when_it_does_not_fit() {
        // At 360 px, "Recents · 3 items" beside "Clear Recents history" does
        // not fit on one line (measured 2026-09-28: 70 px against 56 px for
        // the title alone; one line from 420 px).
        let title_only = action_card_heights(tab::TabAction::ClearRecents(None)).0;
        let recents = action_card_heights(tab::TabAction::ClearRecents(Some(3))).0;
        assert!(recents > title_only, "{recents} > {title_only}");
        // With nothing connected, the network card has no detail: one line
        assert_eq!(
            action_card_heights(tab::TabAction::AddNetworkDrive(0)).0,
            title_only
        );
    }

    #[test]
    fn a_uri_is_shown_without_its_credentials() {
        assert_eq!(uri_for_display("smb://u:p@host/share"), "smb://host/share");
        assert_eq!(uri_for_display("sftp://u@host"), "sftp://host");
        assert_eq!(uri_for_display("smb://host/share"), "smb://host/share");
        assert_eq!(uri_for_display("smb://host/a@b"), "smb://host/a@b");
        assert_eq!(uri_for_display("host/share"), "host/share");
    }

    #[test]
    fn the_compositor_closing_the_main_window_lets_go_of_it() {
        let main = window::Id::unique();
        let mut core = Core::default();
        core.set_main_window_id(Some(main));

        assert!(
            !main_window_closed(&mut core, window::Id::unique()),
            "another window"
        );
        assert_eq!(core.main_window_id(), Some(main));

        assert!(main_window_closed(&mut core, main));
        assert_eq!(core.main_window_id(), None);

        // `WindowClose` let go of it before asking for the close, so the
        // `Closed` that follows is not a second exit request.
        assert!(!main_window_closed(&mut core, main));
    }

    #[test]
    fn keep_both_move_points_favorites_at_the_renamed_item() {
        let from = PathBuf::from("/src/file.txt");
        let to = Path::new("/dest");
        let renamed = to.join("file (copy).txt");
        let moved = vec![(from.clone(), renamed.clone())];

        let changes = move_path_changes(&moved, std::slice::from_ref(&from), to);

        let (_, first) = changes
            .iter()
            .find(|(f, _)| from.starts_with(f))
            .expect("the moved path is rewritten");
        assert_eq!(
            first, &renamed,
            "the recorded pair wins over the basename guess"
        );
    }

    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// Opening an executable asks before running it, whether or not the
    /// file is already marked executable
    #[test]
    fn executable_is_confirmed_before_launch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("downloaded");
        fs::write(&path, b"#!/bin/sh\n").unwrap();

        assert!(matches!(
            App::launch_executable_dialog(path.clone()),
            DialogPage::SetExecutableAndLaunch { .. }
        ));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        match App::launch_executable_dialog(path.clone()) {
            DialogPage::LaunchExecutable { path: p, command } => {
                assert_eq!(p, path);
                assert_eq!(command, path.to_string_lossy());
            }
            _ => panic!("an executable file should get the launch confirmation"),
        }
    }
}

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
