// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::ui::Element;
use crate::ui::app::Task;
use crate::ui::iced::futures::{self, SinkExt};
use crate::ui::iced::keyboard::key::{Named, Physical};
use crate::ui::iced::keyboard::{Event as KeyEvent, Key, Modifiers};
use crate::ui::iced::{self, Alignment, Event, Length, Size, Subscription, event, stream, window};
use crate::ui::iced_core::SmolStr;
use crate::ui::iced_core::widget::Operation;
use crate::ui::iced_core::widget::operation;
use crate::ui::shell::context_drawer;
use crate::ui::shell::{Application, Core, Shell};
use crate::ui::widget::menu::key_bind::Modifier;
use crate::ui::widget::menu::{Action as MenuAction, KeyBind};
use crate::ui::widget::scrollable;
use crate::ui::widget::scrollable::AbsoluteOffset;
use crate::ui::widget::{self, segmented_button};
use mime_guess::{Mime, mime};
use notify_debouncer_full::notify::{self, RecommendedWatcher};
use notify_debouncer_full::{DebouncedEvent, Debouncer, RecommendedCache, new_debouncer};
use rustc_hash::{FxHashMap, FxHashSet};
use std::any::TypeId;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{self, Instant};
use std::{env, fmt, fs};

use crate::app::{
    Action, ContextPage, Message as AppMessage, PreviewItem, PreviewKind, REPLACE_BUTTON_ID,
};
use crate::config::{Config, DialogConfig, Store, ThumbCfg, TypeToSearch};
use crate::key_bind::key_binds;
use crate::localize::LANGUAGE_SORTER;
use crate::mounter::{MOUNTERS, MounterItem, MounterItems, MounterKey, MounterMessage};
use crate::tab::{self, ItemMetadata, Location, SearchLocation, Tab};
use crate::ui::convert::{ToLength, ToPixels};
use crate::ui::theme::{Container, Layer, Spacing, spacing};
use crate::zoom::{zoom_in_view, zoom_out_view, zoom_to_default};
use crate::{fl, home_dir, menu, mime_icon};

#[derive(Clone, Debug)]
pub struct DialogMessage(crate::ui::Action<Message>);

#[derive(Clone, Debug)]
pub enum DialogResult {
    Cancel,
    Open(Vec<PathBuf>),
}

#[derive(Clone, Debug)]
pub enum DialogKind {
    OpenFile,
    OpenFolder,
    OpenMultipleFiles,
    OpenMultipleFolders,
    SaveFile { filename: String },
}

impl DialogKind {
    pub fn title(&self) -> String {
        match self {
            Self::OpenFile => fl!("open-file"),
            Self::OpenFolder => fl!("open-folder"),
            Self::OpenMultipleFiles => fl!("open-multiple-files"),
            Self::OpenMultipleFolders => fl!("open-multiple-folders"),
            Self::SaveFile { .. } => fl!("save-file"),
        }
    }

    pub fn accept_label(&self) -> String {
        match self {
            Self::SaveFile { .. } => fl!("save"),
            _ => fl!("open"),
        }
    }

    pub const fn is_dir(&self) -> bool {
        matches!(self, Self::OpenFolder | Self::OpenMultipleFolders)
    }

    pub const fn multiple(&self) -> bool {
        matches!(self, Self::OpenMultipleFiles | Self::OpenMultipleFolders)
    }

    pub const fn save(&self) -> bool {
        matches!(self, Self::SaveFile { .. })
    }
}

#[derive(Clone, Debug)]
pub struct DialogChoiceOption {
    pub id: String,
    pub label: String,
}

impl AsRef<str> for DialogChoiceOption {
    fn as_ref(&self) -> &str {
        &self.label
    }
}

#[derive(Clone, Debug)]
pub enum DialogChoice {
    CheckBox {
        id: String,
        label: String,
        value: bool,
    },
    ComboBox {
        id: String,
        label: String,
        options: Vec<DialogChoiceOption>,
        selected: Option<usize>,
    },
}

#[derive(Clone, Debug)]
pub enum DialogFilterPattern {
    Glob(String),
    Mime(String),
}

#[derive(Clone, Debug)]
pub struct DialogFilter {
    pub label: String,
    pub patterns: Vec<DialogFilterPattern>,
}

impl AsRef<str> for DialogFilter {
    fn as_ref(&self) -> &str {
        &self.label
    }
}

/// The selected [`DialogFilter`], parsed once and ready to test items against.
struct ItemFilter {
    globs: Vec<glob::Pattern>,
    mimes: Vec<Mime>,
}

impl ItemFilter {
    fn admits(&self, item: &tab::Item) -> bool {
        // Directories are always shown
        item.metadata.is_dir()
            // Check for mime type match (first because it is faster)
            || self.mimes.iter().any(|filter_mime| {
                if filter_mime.subtype() == mime::STAR {
                    filter_mime.type_() == item.mime.type_()
                } else {
                    *filter_mime == item.mime
                        || mime_icon::is_mime_subclass_of(&item.mime, filter_mime)
                }
            })
            // Check for glob match (last because it is slower)
            || self.globs.iter().any(|glob| glob.matches(&item.name))
    }
}

#[derive(Clone, Debug)]
pub struct DialogLabelSpan {
    pub text: String,
    pub underline: bool,
}

#[derive(Clone, Debug)]
pub struct DialogLabel {
    pub spans: Vec<DialogLabelSpan>,
    pub key_bind_opt: Option<KeyBind>,
}

impl<T: AsRef<str>> From<T> for DialogLabel {
    fn from(text: T) -> Self {
        let mut spans = Vec::<DialogLabelSpan>::new();
        let mut key_bind_opt = None;
        let mut next_underline = false;
        for c in text.as_ref().chars() {
            let underline = next_underline;
            next_underline = false;

            if c == '_' && !underline {
                next_underline = true;
                continue;
            }

            if underline && key_bind_opt.is_none() {
                key_bind_opt = Some(KeyBind {
                    modifiers: vec![Modifier::Alt],
                    key: Key::Character(c.to_lowercase().to_string().into()),
                });
            }

            if let Some(span) = spans.last_mut()
                && underline == span.underline
            {
                span.text.push(c);
                continue;
            }

            spans.push(DialogLabelSpan {
                text: String::from(c),
                underline,
            });
        }

        Self {
            spans,
            key_bind_opt,
        }
    }
}

impl<'a, M: Clone + 'static> From<&'a DialogLabel> for Element<'a, M> {
    fn from(label: &'a DialogLabel) -> Self {
        let mut iced_spans: Vec<crate::ui::iced_core::text::Span<'_, ()>> =
            Vec::with_capacity(label.spans.len());
        for span in &label.spans {
            iced_spans.push(crate::ui::iced::widget::span(&span.text).underline(span.underline));
        }
        crate::ui::iced::widget::rich_text(iced_spans).into()
    }
}

/// The title of window `id` for a host that owns choosers: a chooser's own
/// when `id` is its window, the host's otherwise.
///
/// A chooser keeps its title in its own nested shell, but the compositor asks
/// the host, whose default [`Application::title`](crate::ui::shell::Application::title)
/// looks only in the host's `Core`.
#[must_use]
pub fn window_title<'a, S: std::hash::BuildHasher>(
    choosers: impl IntoIterator<Item = (window::Id, &'a str)>,
    host: &'a HashMap<window::Id, String, S>,
    id: window::Id,
) -> &'a str {
    choosers
        .into_iter()
        .find_map(|(window, title)| (window == id).then_some(title))
        .unwrap_or_else(|| host.get(&id).map_or("", String::as_str))
}

pub struct DialogSettings {
    kind: DialogKind,
    path_opt: Option<PathBuf>,
}

impl DialogSettings {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn kind(mut self, kind: DialogKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn path(mut self, path: PathBuf) -> Self {
        self.path_opt = Some(path);
        self
    }
}

impl Default for DialogSettings {
    fn default() -> Self {
        Self {
            kind: DialogKind::OpenFile,
            path_opt: None,
        }
    }
}

/// Wraps a chooser's own messages for the host that owns it.
///
/// A closure rather than a function pointer so a host running several
/// choosers at once, as a portal backend does, can capture which one a
/// message belongs to.
pub type DialogMapper<M> = Arc<dyn Fn(DialogMessage) -> M + Send + Sync>;

pub struct Dialog<M> {
    shell: Shell<App>,
    mapper: DialogMapper<M>,
    on_result: Box<dyn Fn(DialogResult) -> M>,
}

impl<M: Send + 'static> Dialog<M> {
    pub fn new(
        dialog_settings: DialogSettings,
        mapper: impl Fn(DialogMessage) -> M + Send + Sync + 'static,
        on_result: impl Fn(DialogResult) -> M + 'static,
    ) -> (Self, Task<M>) {
        crate::localize::localize();

        let (config_handler, config) = Config::load();
        // See the same calls in `lib.rs`: the dialog can be built by a host that
        // never went through `crate::main`, such as the file chooser a portal
        // backend puts on screen. Each of these is idempotent.
        crate::ui::theme::custom::load();
        crate::ui::font::set_families(&config);
        crate::ui::icon_theme::set_from_config(&config);

        let settings = window::Settings {
            decorations: false,
            exit_on_close_request: false,
            min_size: Some(Size::new(360.0, 180.0)),
            resizable: true,
            size: Size::new(1024.0, 640.0),
            transparent: true,
            ..Default::default()
        };

        // Create the surface through exwlshell's action, as `ui::shell::runner` does
        // for the main window. `iced_exwlshell`'s `Action::Window` dispatcher has no
        // `Open` arm and ends in `_ => {}`, so it silently discards `window::open`
        // requests. That prevented the "Move to…", "Copy to…" and "Extract to…" dialogs
        // from appearing.
        let window_id = crate::ui::iced::window::Id::unique();
        let window_command = iced::Task::done(crate::ui::action::exwl::base_window(
            window_id,
            iced_exwlshell::actions::IcedXdgWindowSettings {
                size: Some(iced_exwlshell::reexport::PixelSize::px(
                    settings.size.width.max(1.0) as u32,
                    settings.size.height.max(1.0) as u32,
                )),
                // `settings.decorations = false` above asks for no server-side
                // decorations, i.e. this window draws its own header bar.
                client_side_decorations: true,
            },
        ));

        let mut core = Core::default();
        core.set_main_window_id(Some(window_id));
        // This shell is embedded in a host's daemon, and its "main window" is
        // just this chooser. Left at the default, the host would call
        // `iced::exit` when the compositor closed the chooser, taking the
        // whole process with it: the file manager, or a portal backend along
        // with every other request it was serving.
        core.exit_on_main_window_closed = false;
        let flags = Flags {
            kind: dialog_settings.kind,
            path_opt: dialog_settings.path_opt.as_ref().and_then(|path| {
                match fs::canonicalize(path) {
                    Ok(ok) => Some(ok),
                    Err(err) => {
                        log::warn!("failed to canonicalize {}: {}", path.display(), err);
                        None
                    }
                }
            }),
            window_id,
            config_handler,
            config,
        };

        // The nested shell renders into the outer application's daemon, so
        // its own `theme()`/`style()` are never called; the theme it is handed
        // here is only what its config watchers update.
        let (shell, shell_command) = Shell::<App>::init(core, flags, crate::ui::theme::active());
        let mapper: DialogMapper<M> = Arc::new(mapper);
        let for_command = Arc::clone(&mapper);
        (
            Self {
                shell,
                mapper,
                on_result: Box::new(on_result),
            },
            Task::batch([
                window_command,
                shell_command
                    .map(DialogMessage)
                    .map(move |message| crate::ui::action::app(for_command(message))),
            ]),
        )
    }

    pub fn set_title(&mut self, title: impl Into<String>) -> Task<M> {
        let mapper = Arc::clone(&self.mapper);
        self.shell.app.title = title.into();
        self.shell
            .app
            .update_title()
            .map(DialogMessage)
            .map(move |message| crate::ui::action::app(mapper(message)))
    }

    pub fn set_accept_label(&mut self, accept_label: impl AsRef<str>) {
        self.shell.app.accept_label = DialogLabel::from(accept_label);
    }

    pub fn choices(&self) -> &[DialogChoice] {
        &self.shell.app.choices
    }

    pub fn set_choices(&mut self, choices: impl Into<Vec<DialogChoice>>) {
        self.shell.app.choices = choices.into();
    }

    pub fn filters(&self) -> (&[DialogFilter], Option<usize>) {
        (&self.shell.app.filters, self.shell.app.filter_selected)
    }

    pub fn set_filters(
        &mut self,
        filters: impl Into<Vec<DialogFilter>>,
        filter_selected: Option<usize>,
    ) -> Task<M> {
        let mapper = Arc::clone(&self.mapper);
        self.shell.app.filters = filters.into();
        self.shell.app.filter_selected = filter_selected;
        self.shell.app.update_item_filter();
        self.shell
            .app
            .rescan_tab(None)
            .map(DialogMessage)
            .map(move |message| crate::ui::action::app(mapper(message)))
    }

    /// The chooser's own events, for the host to wrap.
    ///
    /// Left untagged because a subscription is identified by a hash of what
    /// it carries, and a closure cannot be hashed. A host running several
    /// choosers tags these itself with something that can be, such as the
    /// request they belong to.
    pub fn subscription(&self) -> Subscription<DialogMessage> {
        self.shell.subscription().map(DialogMessage)
    }

    pub fn update(&mut self, message: DialogMessage) -> Task<M> {
        let mapper = Arc::clone(&self.mapper);
        let command = self
            .shell
            .update(message.0)
            .map(DialogMessage)
            .map(move |message| crate::ui::action::app(mapper(message)));
        if let Some(result) = self.shell.app.result_opt.take() {
            // Every surface our shell tracks is a Wayland popup, so there is
            // no surface kind to switch on here.
            let mut tasks: Vec<Task<M>> = self
                .shell
                .popup_view_ids()
                .map(|id| Task::done(crate::ui::action::exwl::remove_window::<M>(id)))
                .collect();
            if !tasks.is_empty() {
                log::debug!("waiting for surfaces to close...");
                let on_result_message = (self.on_result)(result);

                tasks.push(Task::future(async move {
                    crate::ui::action::app(on_result_message)
                }));
                tasks.push(command);
                return Task::batch(tasks);
            }
            let on_result_message = (self.on_result)(result);

            Task::batch([
                command,
                Task::future(async move { crate::ui::action::app(on_result_message) }),
            ])
        } else {
            command
        }
    }

    pub fn view(&self, window_id: window::Id) -> Element<'_, M> {
        let mapper = Arc::clone(&self.mapper);
        self.shell
            .view(window_id)
            .map(DialogMessage)
            .map(move |message| mapper(message))
    }

    pub const fn window_id(&self) -> window::Id {
        self.shell.app.flags.window_id
    }

    /// The chooser's current title: as last set with [`Dialog::set_title`], else its kind's default.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.shell.app.title
    }

    pub fn contains_surface(&self, id: &window::Id) -> bool {
        self.shell.has_popup_view(id)
    }
}

#[derive(Clone, Debug)]
enum DialogPage {
    NewFolder { parent: PathBuf, name: String },
    Replace { filename: String },
}

#[derive(Clone, Debug)]
struct Flags {
    kind: DialogKind,
    path_opt: Option<PathBuf>,
    window_id: window::Id,
    config_handler: Store,
    config: Config,
}

/// Messages that are used specifically by our [`App`].
#[derive(Clone, Debug)]
enum Message {
    None,
    Cancel,
    Choice(usize, usize),
    Config(Config),
    DialogCancel,
    DialogComplete,
    DialogUpdate(DialogPage),
    Escape,
    Filename(String),
    /// A window event, tagged with the window it came from. The chooser shares
    /// the runtime with its host, so events for other windows arrive here too
    /// and must not drive it.
    ForWindow(window::Id, Box<Message>),
    Filter(usize),
    Key(Modifiers, Key, Physical, Option<SmolStr>),
    ModifiersChanged(Modifiers),
    MounterItems(MounterKey, MounterItems),
    NavBarClose(segmented_button::Entity),
    NewFolder,
    /// A folder the chooser asked for now exists, tagged with where the
    /// chooser had been sent when it was asked for.
    NewFolderCreated(u64, PathBuf),
    NotifyEvents(Vec<DebouncedEvent>),
    NotifyWatcher(WatcherWrapper),
    Open,
    Preview,
    /// Items re-read off the event loop after the filesystem changed beneath
    /// them, tagged with the location and listing they were read from and the
    /// number of the batch that asked for them.
    RefreshedItems(Location, u64, u64, Vec<(PathBuf, Box<tab::Item>)>),
    Save(bool),
    ScrollTab(i16),
    SearchActivate,
    SearchClear,
    SearchInput(String),
    /// Widen the search to subfolders, or narrow it back to this folder.
    SetSearchRecursive(bool),
    Surface(crate::ui::surface::Action<Message>),
    #[allow(clippy::enum_variant_names)]
    TabMessage(tab::Message),
    TabRescan(
        Location,
        Option<Box<tab::Item>>,
        Vec<tab::Item>,
        Option<Vec<PathBuf>>,
    ),
    TabView(tab::View),
    ToggleFoldersFirst,
    ToggleShowHidden,
    ZoomDefault,
    ZoomIn,
    ZoomOut,
}

impl From<AppMessage> for Message {
    fn from(app_message: AppMessage) -> Self {
        match app_message {
            AppMessage::None => Self::None,
            AppMessage::Preview => Self::Preview,
            AppMessage::SearchActivate => Self::SearchActivate,
            AppMessage::ScrollTab(scroll_speed) => Self::ScrollTab(scroll_speed),
            AppMessage::TabMessage(_entity_opt, tab_message) => Self::TabMessage(tab_message),
            AppMessage::TabView(_entity_opt, view) => Self::TabView(view),
            AppMessage::ToggleFoldersFirst => Self::ToggleFoldersFirst,
            AppMessage::ToggleShowHidden => Self::ToggleShowHidden,
            AppMessage::ZoomDefault(_entity_opt) => Self::ZoomDefault,
            AppMessage::ZoomIn(_entity_opt) => Self::ZoomIn,
            AppMessage::ZoomOut(_entity_opt) => Self::ZoomOut,
            AppMessage::NewItem(_entity_opt, true) => Self::NewFolder,
            AppMessage::Surface(action) => Self::Surface(action.map(Self::from)),
            unsupported => {
                log::warn!("{unsupported:?} not supported in dialog mode");
                Self::None
            }
        }
    }
}

pub struct MounterData(MounterKey, MounterItem);

struct WatcherWrapper {
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

/// The [`App`] stores application-specific state.
struct App {
    core: Core,
    flags: Flags,
    title: String,
    accept_label: DialogLabel,
    choices: Vec<DialogChoice>,
    context_page: ContextPage,
    dialog_pages: VecDeque<DialogPage>,
    dialog_text_input: widget::Id,
    filters: Vec<DialogFilter>,
    filter_selected: Option<usize>,
    /// The folder a chosen search result came from, and the tab navigation it
    /// was chosen in. See [`App::save_dir`].
    save_dir_opt: Option<(u64, PathBuf)>,
    filename_id: widget::Id,
    modifiers: Modifiers,
    mounter_items: FxHashMap<MounterKey, MounterItems>,
    nav_model: segmented_button::SingleSelectModel,
    result_opt: Option<DialogResult>,
    search_id: widget::Id,
    tab: Tab,
    key_binds: HashMap<KeyBind, Action>,
    watcher_opt: Option<(
        Debouncer<RecommendedWatcher, RecommendedCache>,
        FxHashSet<PathBuf>,
    )>,
    auto_scroll_speed: Option<i16>,
    type_select_prefix: String,
    type_select_last_key: Option<Instant>,
}

impl App {
    /// Whether pressing Open does something: it returns the selection, enters
    /// the single selected folder, or returns the current folder in a folder
    /// chooser. Mirrors the checks in the `Message::Open` handler.
    fn can_open(&self) -> bool {
        let want_dir = self.flags.kind.is_dir();
        // From what the scan already read, not from the disk: this decides
        // whether a button is enabled, so it is answered on every frame, and
        // stat-ing each selected path that often makes the chooser as slow as
        // the filesystem it is browsing.
        let selected_dirs: Vec<bool> = self
            .tab
            .items_opt()
            .into_iter()
            .flatten()
            .filter(|item| item.selected && item.path_opt().is_some())
            .map(|item| item.metadata.is_dir())
            .collect();
        if selected_dirs.is_empty() {
            return want_dir && matches!(self.tab.location, Location::Path(_));
        }
        let mismatched = selected_dirs
            .iter()
            .filter(|is_dir| **is_dir != want_dir)
            .count();
        // A lone folder in a file chooser is entered instead of returned
        mismatched == 0 || (!want_dir && selected_dirs.len() == 1 && selected_dirs[0])
    }

    fn button_view(&self) -> Element<'_, Message> {
        let Spacing {
            space_xxxs,
            space_xxs,
            space_xs,
            space_s,
            space_l,
            ..
        } = spacing();
        let is_condensed = self.core().is_condensed();

        let mut col = widget::Column::with_capacity(2).spacing(space_xxs.to_pixels());
        if let DialogKind::SaveFile { filename } = &self.flags.kind {
            col = col.push(
                widget::text_input("", filename)
                    .id(self.filename_id.clone())
                    .double_click_select_delimiter('.')
                    .on_input(Message::Filename)
                    .on_submit(|_| Message::Save(false)),
            );
        }

        let mut row = widget::Row::with_capacity(
            usize::from(!self.filters.is_empty())
                + self.choices.len() * 2
                + if is_condensed { 0 } else { 3 },
        )
        .align_y(Alignment::Center)
        .spacing(space_xxs.to_pixels());
        if !self.filters.is_empty() {
            row = row.push(widget::dropdown(
                &self.filters,
                self.filter_selected,
                Message::Filter,
            ));
        }
        for (choice_i, choice) in self.choices.iter().enumerate() {
            match choice {
                DialogChoice::CheckBox { label, value, .. } => {
                    row = row.push(
                        widget::checkbox(*value)
                            .label(label)
                            .on_toggle(move |checked| {
                                Message::Choice(choice_i, usize::from(checked))
                            }),
                    );
                }
                DialogChoice::ComboBox {
                    label,
                    options,
                    selected,
                    ..
                } => {
                    row = row.push(widget::text::heading(label));
                    row = row.push(widget::dropdown(options, *selected, move |option_i| {
                        Message::Choice(choice_i, option_i)
                    }));
                }
            }
        }

        if is_condensed {
            col = col.push(row);
            row = widget::Row::with_capacity(3)
                .align_y(Alignment::Center)
                .spacing(space_xxs.to_pixels());
        }
        row = row.push(widget::space::horizontal());
        row = row.push(widget::button::standard(fl!("cancel")).on_press(Message::Cancel));

        row = row.push(
            widget::button::custom(
                widget::Row::with_children([Element::from(&self.accept_label)])
                    .padding([0, space_s])
                    .width(Length::Shrink)
                    .height(space_l.to_length())
                    .spacing(space_xxxs.to_pixels())
                    .align_y(Alignment::Center),
            )
            .padding(0)
            .on_press_maybe(if self.flags.kind.save() {
                if let DialogKind::SaveFile { filename } = &self.flags.kind {
                    (!filename.is_empty()).then_some(Message::Save(false))
                } else {
                    None
                }
            } else if self.can_open() {
                Some(Message::Open)
            } else {
                None
            })
            .class(widget::button::ButtonClass::Suggested),
        );

        col = col.push(row);

        widget::layer_container(col)
            .layer(Layer::Primary)
            .padding([8, space_xs])
            .into()
    }

    fn preview<'a>(&'a self, kind: &'a PreviewKind) -> Element<'a, tab::Message> {
        let mut children = Vec::with_capacity(1);
        match kind {
            PreviewKind::Custom(PreviewItem(item)) => {
                children.push(item.preview_view(None));
            }
            PreviewKind::Location(location) => {
                if let Some(items) = self.tab.items_opt() {
                    for item in items {
                        if item.location_opt.as_ref() == Some(location) {
                            children.push(item.preview_view(None));
                            // Only show one property view to avoid issues like hangs when generating
                            // preview images on thousands of files
                            break;
                        }
                    }
                }
            }
            PreviewKind::Selected => {
                if let Some(items) = self.tab.items_opt() {
                    let preview_opt = {
                        let mut selected = items.iter().filter(|item| item.selected);

                        match (selected.next(), selected.next()) {
                            // At least two selected items
                            (Some(_), Some(_)) => Some(self.tab.multi_preview_view(None)),
                            // Exactly one selected item
                            (Some(item), None) => Some(item.preview_view(None)),
                            // No selected items
                            _ => None,
                        }
                    };

                    if let Some(preview) = preview_opt {
                        children.push(preview);
                    }

                    if children.is_empty()
                        && let Some(item) = &self.tab.parent_item_opt
                    {
                        children.push(item.preview_view(None));
                    }
                }
            }
        }
        widget::Column::with_children(children).into()
    }

    /// The selected file type filter, ready to test items against, or `None`
    /// when every file is allowed through.
    fn item_filter(&self) -> Option<ItemFilter> {
        let filter = self.filters.get(self.filter_selected?)?;
        let mut globs = Vec::new();
        let mut mimes = Vec::new();
        for pattern in &filter.patterns {
            match pattern {
                DialogFilterPattern::Glob(value) => match glob::Pattern::new(value) {
                    Ok(glob) => globs.push(glob),
                    Err(err) => log::warn!("failed to parse glob {value:?}: {err}"),
                },
                DialogFilterPattern::Mime(value) => match value.parse::<Mime>() {
                    Ok(parsed) => mimes.push(parsed),
                    Err(err) => log::warn!("failed to parse mime {value:?}: {err}"),
                },
            }
        }
        Some(ItemFilter { globs, mimes })
    }

    /// Hand the tab the filter to apply as items arrive.
    ///
    /// Not applied here afterwards: search results come in one at a time and
    /// are capped, so results this filter rejects would already have taken the
    /// places of ones it admits, and moved the cutoff that stops older results
    /// being sent at all.
    fn update_item_filter(&mut self) {
        self.tab.item_filter = self
            .item_filter()
            .map(|filter| Box::new(move |item: &tab::Item| filter.admits(item)) as tab::ItemFilter);
    }

    /// Drop whatever the selected file type filter does not admit, for a whole
    /// listing that arrived at once.
    fn filter_items(&self, items: &mut Vec<tab::Item>) {
        if let Some(keep) = self.tab.item_filter.as_ref() {
            items.retain(|item| keep(item));
        }
    }

    /// The folder a save lands in.
    ///
    /// Normally the location on screen. A search lists results from anywhere
    /// beneath its root, though, so the name in the filename box belongs to the
    /// folder the result that filled it came from; joining it to the root
    /// instead would save beside the root and test a different file for
    /// overwriting.
    ///
    /// Remembered rather than read back from the selection, which editing the
    /// name clears: picking a result and then changing one letter of its name
    /// would otherwise move the save to a different folder without a word. It
    /// lasts until the tab goes somewhere else, which includes clearing or
    /// changing the search.
    fn save_dir(&self) -> Option<PathBuf> {
        if let Some((navigation, dir)) = &self.save_dir_opt
            && *navigation == self.tab.navigation()
        {
            return Some(dir.clone());
        }
        self.tab.location.path_opt().cloned()
    }

    fn rescan_tab(&self, selection_paths: Option<Vec<PathBuf>>) -> Task<Message> {
        let location = self.tab.location.clone();
        let icon_sizes = self.tab.config.icon_sizes;
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

    /// What sits at the right-hand end of the search field. As in the main
    /// window: the text input has one trailing slot and `on_clear` is that
    /// slot, so the eye shares it with the clear button.
    /// The eye, when there is a search it can act on. As in the main window,
    /// its state comes from the search that is running rather than from the
    /// config, so it describes what is on screen.
    fn search_scope_button(&self) -> Option<Element<'_, Message>> {
        let recursive = match &self.tab.location {
            Location::Search(tab::SearchLocation::Path(..), _, options, _) => options.recursive,
            _ => return None,
        };
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

    fn search_get(&self) -> Option<&str> {
        match &self.tab.location {
            Location::Search(_, term, ..) => Some(term),
            _ => None,
        }
    }

    fn search_set(&mut self, term_opt: Option<String>) -> Task<Message> {
        let location_opt = match term_opt {
            Some(term) => {
                let search_location = if let Some(path) = self.tab.location.path_opt() {
                    Some(SearchLocation::Path(path.clone()))
                } else if self.tab.location.is_recents() {
                    Some(SearchLocation::Recents)
                } else if self.tab.location.is_trash() {
                    Some(SearchLocation::Trash)
                } else {
                    None
                };

                search_location.map(|search_location| {
                    (
                        Location::Search(
                            search_location,
                            term,
                            self.tab.search_options_for_query(),
                            Instant::now(),
                        ),
                        true,
                    )
                })
            }
            None => match &self.tab.location {
                Location::Search(search_location, ..) => match search_location {
                    SearchLocation::Path(path) => Some((Location::Path(path.clone()), false)),
                    SearchLocation::Recents => Some((Location::Recents, false)),
                    SearchLocation::Trash => Some((Location::Trash, false)),
                },
                _ => None,
            },
        };
        if let Some((location, focus_search)) = location_opt {
            self.tab.change_location(&location, None);
            return Task::batch([
                self.update_title(),
                self.update_watcher(),
                self.rescan_tab(None),
                if focus_search {
                    widget::text_input::focus(self.search_id.clone())
                } else {
                    Task::none()
                },
            ]);
        }
        Task::none()
    }

    fn update_config(&mut self) -> Task<Message> {
        crate::ui::theme::set_density(self.flags.config.density);
        crate::ui::theme::set_header_size(self.flags.config.header_size);
        self.core.set_nav_bar_width(self.flags.config.nav_bar_width);
        self.core.window.show_context = self.flags.config.dialog.show_details;
        let config = self.flags.config.dialog_tab();
        self.tab.config.view = config.view;
        self.update_nav_model();
        self.update(Message::TabMessage(tab::Message::Config(config)))
    }

    fn with_dialog_config<F: Fn(&mut DialogConfig)>(&mut self, f: F) -> Task<Message> {
        let mut dialog = self.flags.config.dialog;
        f(&mut dialog);
        if dialog == self.flags.config.dialog {
            Task::none()
        } else {
            self.flags.config.dialog = dialog;
            if let Err(err) = self
                .flags
                .config_handler
                .save_in_background(&self.flags.config)
            {
                log::warn!("failed to save config \"dialog\": {err}");
            }
            self.update_config()
        }
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
        let mut nav_model = segmented_button::ModelBuilder::default();

        if self.flags.config.show_recents {
            nav_model = nav_model.insert(|b| {
                b.text(fl!("recents"))
                    .icon(widget::icon::from_name("document-open-recent-symbolic"))
                    .data(Location::Recents)
            });
        }

        for favorite in &self.flags.config.favorites {
            if let Some(path) = favorite.path_opt() {
                let Some(name) = favorite.display_name() else {
                    continue;
                };
                nav_model = nav_model.insert(move |b| {
                    b.text(name.clone())
                        .icon(
                            widget::icon::icon(if path.is_dir() {
                                widget::icon::from_name(format!(
                                    "{}-symbolic",
                                    tab::folder_icon_name(&path)
                                ))
                                .size(16)
                                .handle()
                            } else {
                                widget::icon::from_name("text-x-generic-symbolic")
                                    .size(16)
                                    .handle()
                            })
                            .size(16),
                        )
                        .data(Location::Path(path.clone()))
                });
            }
        }

        // Collect all mounter items
        let mut nav_items = Vec::new();
        for (key, items) in &self.mounter_items {
            nav_items.extend(items.iter().map(|item| (*key, item)));
        }
        // Sort by name lexically
        nav_items.sort_unstable_by(|a, b| LANGUAGE_SORTER.compare(&a.1.name(), &b.1.name()));
        // Add items to nav model
        for (i, (key, item)) in nav_items.into_iter().enumerate() {
            nav_model = nav_model.insert(|mut b| {
                b = b.text(item.name()).data(MounterData(key, item.clone()));
                if let Some(path) = item.path() {
                    b = b.data(Location::Path(path));
                }
                if let Some(icon_path) = item.icon_path(true) {
                    b = b.icon(widget::icon::icon(widget::icon::from_path(icon_path)).size(16));
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

        self.activate_nav_model_location(&self.tab.location.clone());
    }

    fn update_title(&mut self) -> Task<Message> {
        self.set_header_title(self.title.clone());
        self.set_window_title(self.title.clone(), self.flags.window_id)
    }

    fn update_watcher(&mut self) -> Task<Message> {
        if let Some((mut watcher, old_paths)) = self.watcher_opt.take() {
            let mut new_paths = FxHashSet::default();
            if let Some(path) = &self.tab.location.path_opt() {
                new_paths.insert((*path).clone());
            }

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
}

/// Implement this app's [`Application`] to integrate with the shell.
impl Application for App {
    /// Argument received
    type Flags = Flags;

    /// Message type specific to our [`App`].
    type Message = Message;

    /// The unique application ID to supply to the window manager.
    const APP_ID: &'static str = "com.owlm.EarthFilesDialog";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    /// Creates the application, and optionally emits command on initialize.
    fn init(mut core: Core, flags: Self::Flags) -> (Self, Task<Message>) {
        core.window.context_is_overlay = false;
        core.window.show_close = false;
        core.window.show_maximize = false;
        core.window.show_minimize = false;

        let title = flags.kind.title();
        let accept_label = flags.kind.accept_label();

        let location = Location::Path(match &flags.path_opt {
            Some(path) => path.clone(),
            None => match env::current_dir() {
                Ok(path) => path,
                Err(_) => home_dir(),
            },
        });

        let mut tab = Tab::new(
            location,
            flags.config.dialog_tab(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.mode = tab::Mode::Dialog(flags.kind.clone());
        tab.sort_name = tab::HeadingOptions::Modified;
        tab.sort_direction = false;

        let key_binds = key_binds(&tab.mode, &flags.config.key_binds);

        let mut app = Self {
            core,
            flags,
            title,
            accept_label: DialogLabel::from(accept_label),
            choices: Vec::new(),
            context_page: ContextPage::Preview(None, PreviewKind::Selected),
            dialog_pages: VecDeque::new(),
            dialog_text_input: widget::Id::new("Dialog Text Input"),
            filters: Vec::new(),
            filter_selected: None,
            save_dir_opt: None,
            filename_id: widget::Id::new("Dialog Filename"),
            modifiers: Modifiers::empty(),
            mounter_items: FxHashMap::default(),
            nav_model: segmented_button::ModelBuilder::default().build(),
            result_opt: None,
            search_id: widget::Id::new("Dialog File Search"),
            tab,
            key_binds,
            watcher_opt: None,
            auto_scroll_speed: None,
            type_select_prefix: String::new(),
            type_select_last_key: None,
        };

        let commands = Task::batch([
            app.update_config(),
            app.update_title(),
            app.update_watcher(),
            app.rescan_tab(None),
        ]);

        (app, commands)
    }

    fn drawer_slide_fits_columns(&self, extent: f32, opening: bool) -> bool {
        self.tab.column_slide_fits(extent, opening)
    }

    fn context_drawer(&self) -> Option<context_drawer::ContextDrawer<'_, Message>> {
        match &self.context_page {
            ContextPage::Preview(_, kind) => {
                let actions = self
                    .tab
                    .items_opt()
                    .and_then(|items| {
                        items
                            .iter()
                            .find(|item| item.selected)
                            .map(|item| item.preview_actions().map(Message::TabMessage))
                    })
                    .unwrap_or_else(|| widget::space::horizontal().into());
                Some(
                    context_drawer::context_drawer(
                        self.preview(kind).map(Message::TabMessage),
                        Message::Preview,
                    )
                    .actions(actions),
                )
            }
            _ => None,
        }
    }

    fn dialog(&self) -> Option<Element<'_, Message>> {
        let Spacing { space_xxs, .. } = spacing();

        if self.tab.gallery {
            return Some(
                widget::Column::with_children([
                    self.tab.gallery_view().map(Message::TabMessage),
                    // Draw button row as part of the overlay
                    widget::container(self.button_view())
                        .width(Length::Fill)
                        .padding(space_xxs)
                        .class(Container::WindowBackground)
                        .into(),
                ])
                .into(),
            );
        }

        let dialog_page = self.dialog_pages.front()?;

        let dialog = match dialog_page {
            DialogPage::NewFolder { parent, name } => {
                let mut dialog = widget::dialog().title(fl!("create-new-folder"));

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
                    let path = parent.join(name);
                    if path.exists() {
                        if path.is_dir() {
                            dialog = dialog
                                .tertiary_action(widget::text::body(fl!("folder-already-exists")));
                        } else {
                            dialog = dialog
                                .tertiary_action(widget::text::body(fl!("file-already-exists")));
                        }
                        None
                    } else {
                        if name.starts_with('.') {
                            dialog = dialog.tertiary_action(widget::text::body(fl!("name-hidden")));
                        }
                        Some(Message::DialogComplete)
                    }
                };

                dialog
                    .primary_action(
                        widget::button::suggested(fl!("save"))
                            .on_press_maybe(complete_maybe.clone()),
                    )
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    )
                    .control(
                        widget::Column::with_children([
                            widget::text::body(fl!("folder-name")).into(),
                            widget::text_input("", name.as_str())
                                .id(self.dialog_text_input.clone())
                                .on_input(move |name| {
                                    Message::DialogUpdate(DialogPage::NewFolder {
                                        parent: parent.clone(),
                                        name,
                                    })
                                })
                                .on_submit_maybe(complete_maybe.map(|maybe| move |_| maybe.clone()))
                                .into(),
                        ])
                        .spacing(space_xxs.to_pixels()),
                    )
            }
            DialogPage::Replace { filename } => widget::dialog()
                .title(fl!("replace-title", filename = filename.as_str()))
                .icon(widget::icon::from_name("dialog-question").size(64))
                .body(fl!("replace-warning"))
                .primary_action(
                    widget::button::suggested(fl!("replace"))
                        .on_press(Message::DialogComplete)
                        .id(REPLACE_BUTTON_ID.clone()),
                )
                .secondary_action(
                    widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                ),
        };

        Some(dialog.into())
    }

    fn footer(&self) -> Option<Element<'_, Message>> {
        Some(self.button_view())
    }

    fn header_end(&self) -> Vec<Element<'_, Message>> {
        let mut elements = Vec::with_capacity(3);

        if let Some(term) = self.search_get() {
            if self.core.is_condensed() {
                // The field itself is collapsed here, so the eye stands on
                // its own beside the button that clears the search.
                elements.extend(self.search_scope_button());
                elements.push(
                    widget::button::icon(widget::icon::from_name("system-search-symbolic"))
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
                        .trailing_icon(self.search_trailing())
                        .on_input(Message::SearchInput)
                        .into(),
                );
            }
        } else {
            elements.push(
                widget::button::icon(widget::icon::from_name("system-search-symbolic"))
                    .on_press(Message::SearchActivate)
                    .padding(8)
                    .into(),
            );
        }

        if self.flags.kind.save() {
            elements.push(
                widget::button::icon(widget::icon::from_name("folder-new-symbolic"))
                    .on_press(Message::NewFolder)
                    .padding(8)
                    .into(),
            );
        }

        let show_details = match self.context_page {
            ContextPage::Preview(..) => self.core.window.show_context,
            _ => false,
        };
        elements
            .push(menu::dialog_menu(&self.tab, &self.key_binds, show_details).map(Message::from));

        elements
    }

    fn nav_bar(&self) -> Option<Element<'_, crate::ui::Action<Self::Message>>> {
        if !self.core().nav_bar_active() {
            return None;
        }

        let nav_model = self.nav_model()?;

        let mut nav = widget::nav_bar(nav_model, |entity| {
            crate::ui::Action::Cosmic(crate::ui::app::Action::NavBar(entity))
        })
        .on_close(|entity| crate::ui::Action::App(Message::NavBarClose(entity)))
        .close_icon(
            widget::icon::from_name("media-eject-symbolic")
                .size(16)
                .icon(),
        )
        .into_container();

        if !self.core().is_condensed() {
            nav = nav.max_width(widget::nav_bar::MAX_WIDTH);
        }

        Some(Element::from(
            nav.width(Length::Shrink).height(Length::Fill),
        ))
    }

    fn nav_model(&self) -> Option<&segmented_button::SingleSelectModel> {
        Some(&self.nav_model)
    }

    fn on_app_exit(&mut self) -> Option<Message> {
        self.result_opt = Some(DialogResult::Cancel);
        None
    }

    /// The chooser shares the sidebar width with the main window
    fn on_nav_bar_resized(&mut self, width: u16) -> Task<Message> {
        self.flags.config.nav_bar_width = Some(width);
        if let Err(err) = self
            .flags
            .config_handler
            .save_in_background(&self.flags.config)
        {
            log::warn!("failed to save config \"nav_bar_width\": {err}");
        }
        Task::none()
    }

    fn on_nav_select(&mut self, entity: segmented_button::Entity) -> Task<Message> {
        self.nav_model.activate(entity);
        if let Some(location) = self.nav_model.data::<Location>(entity) {
            let message = Message::TabMessage(tab::Message::Location(location.clone()));
            return self.update(message);
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

    fn on_escape(&mut self) -> Task<Message> {
        if self.tab.gallery {
            // Close gallery if open
            self.tab.gallery = false;
            return Task::none();
        }

        if self.tab.edit_location.is_some() {
            // Close location editing if enabled
            self.tab.dismiss_edit_location();
            return Task::none();
        }

        if self.search_get().is_some() {
            // Close search if open
            return self.search_set(None);
        }

        let had_focused_button = self.tab.select_focus_id().is_some();
        if self.tab.select_none() {
            if had_focused_button {
                // Unfocus if there was a focused button
                return widget::button::focus(widget::Id::unique());
            }
            return Task::none();
        }

        // Close the dialog if the focused widget is the dialog's main text input instead of
        // unfocussing the widget.
        if let operation::Outcome::Some(focused) = operation::focusable::find_focused().finish()
            && self.dialog_text_input == focused
        {
            return self.update(Message::Cancel);
        }

        self.update(Message::Cancel)
    }

    /// Handle application events here.
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::None => {}
            Message::Cancel => {
                self.result_opt = Some(DialogResult::Cancel);
                return window::close(self.flags.window_id);
            }
            Message::Choice(choice_i, option_i) => {
                if let Some(choice) = self.choices.get_mut(choice_i) {
                    match choice {
                        DialogChoice::CheckBox { value, .. } => *value = option_i > 0,
                        DialogChoice::ComboBox {
                            options, selected, ..
                        } => {
                            if option_i < options.len() {
                                *selected = Some(option_i);
                            } else {
                                *selected = None;
                            }
                        }
                    }
                }
            }
            Message::Config(config) => {
                if config != self.flags.config {
                    log::info!("update config");
                    self.flags.config = config;
                    return self.update_config();
                }
            }
            Message::DialogCancel => {
                self.dialog_pages.pop_front();
            }
            Message::DialogComplete => {
                if let Some(dialog_page) = self.dialog_pages.pop_front() {
                    match dialog_page {
                        DialogPage::NewFolder { parent, name } => {
                            let path = parent.join(name);
                            // Created off the event loop, and navigated into
                            // only once it exists. On a slow or remote
                            // destination the `mkdir` alone can take long
                            // enough to be felt as the dialog hanging.
                            let navigation = self.tab.navigation();
                            return Task::future(async move {
                                let created = tokio::task::spawn_blocking({
                                    let path = path.clone();
                                    move || fs::create_dir(&path)
                                })
                                .await;
                                match created {
                                    Ok(Ok(())) => crate::ui::action::app(
                                        Message::NewFolderCreated(navigation, path),
                                    ),
                                    Ok(Err(err)) => {
                                        log::warn!("failed to create {}: {}", path.display(), err);
                                        crate::ui::action::none()
                                    }
                                    Err(err) => {
                                        log::warn!("failed to create {}: {}", path.display(), err);
                                        crate::ui::action::none()
                                    }
                                }
                            });
                        }
                        DialogPage::Replace { .. } => {
                            return self.update(Message::Save(true));
                        }
                    }
                }
            }
            Message::DialogUpdate(dialog_page) => {
                if !self.dialog_pages.is_empty() {
                    self.dialog_pages[0] = dialog_page;
                }
            }
            Message::Escape => return self.on_escape(),
            Message::Filename(new_filename) => {
                // Select based on filename
                self.tab.select_name(&new_filename);

                if let DialogKind::SaveFile { filename } = &mut self.flags.kind {
                    *filename = new_filename;
                }
            }
            Message::Filter(filter_i) => {
                if filter_i < self.filters.len() {
                    self.filter_selected = Some(filter_i);
                } else {
                    self.filter_selected = None;
                }
                self.update_item_filter();
                // A search has no listing to rescan: `Location::scan` answers
                // with nothing for one, because its results arrive from a
                // subscription keyed on the location. A plain rescan would
                // empty the view and leave it empty, the subscription having
                // already finished and nothing restarting it. Ask the search
                // again instead, so the results come back through the new
                // filter.
                if let Location::Search(search_location, term, options, _) = &self.tab.location {
                    let location = Location::Search(
                        search_location.clone(),
                        term.clone(),
                        *options,
                        Instant::now(),
                    );
                    self.tab.change_location(&location, None);
                    return Task::batch([
                        self.update_title(),
                        self.update_watcher(),
                        self.rescan_tab(None),
                    ]);
                }
                return self.rescan_tab(None);
            }
            Message::Key(modifiers, key, physical_key, text) => {
                for (key_bind, action) in &self.key_binds {
                    if key_bind.matches(modifiers, &key, Some(&physical_key)) {
                        return self.update(Message::from(action.message()));
                    }
                }

                // Check key binds from accept label
                if let Some(key_bind) = &self.accept_label.key_bind_opt
                    && key_bind.matches(modifiers, &key, Some(&physical_key))
                {
                    return self.update(if self.flags.kind.save() {
                        Message::Save(false)
                    } else {
                        Message::Open
                    });
                }

                // Uncaptured keys with only shift modifiers go to the search or location box
                if !modifiers.logo()
                    && !modifiers.control()
                    && !modifiers.alt()
                    && matches!(key, Key::Character(_))
                    && let Some(text) = text
                {
                    match self.flags.config.type_to_search {
                        TypeToSearch::Recursive => {
                            let mut term = self.search_get().unwrap_or_default().to_string();
                            term.push_str(&text);
                            return self.search_set(Some(term));
                        }
                        TypeToSearch::EnterPath => {
                            let location = (self.tab.edit_location)
                                .as_ref()
                                .map_or_else(|| &self.tab.location, |x| &x.location);
                            // Try to add text to end of location
                            if let Some(path) = location.path_opt() {
                                let mut path_string = path.to_string_lossy().to_string();
                                path_string.push_str(&text);
                                self.tab.edit_location =
                                    Some(location.with_path(PathBuf::from(path_string)).into());
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

                            self.tab.select_by_prefix(&self.type_select_prefix);
                            if let Some(offset) = self.tab.select_focus_scroll() {
                                return scrollable::scroll_to(
                                    self.tab.scrollable_id.clone(),
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
            Message::ForWindow(window_id, message) => {
                if window_id == self.flags.window_id {
                    return self.update(*message);
                }
            }
            Message::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers;
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
                                unmounted.push(Location::Path(old_path));
                            }
                        }
                    }
                }

                // Go back to home in any tabs that were unmounted
                let mut commands = Vec::new();
                {
                    let home_location = Location::Path(home_dir());
                    if unmounted.contains(&self.tab.location) {
                        self.tab.change_location(&home_location, None);
                        commands.push(self.update_watcher());
                        commands.push(self.rescan_tab(None));
                    }
                }

                // Insert new items
                self.mounter_items.insert(mounter_key, mounter_items);

                // Update nav bar
                self.update_nav_model();

                return Task::batch(commands);
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
            Message::NewFolder => {
                if let Some(path) = self.tab.location.path_opt() {
                    self.dialog_pages.push_back(DialogPage::NewFolder {
                        parent: path.clone(),
                        name: String::new(),
                    });
                    return widget::text_input::focus(self.dialog_text_input.clone());
                }
            }
            Message::NewFolderCreated(navigation, path) => {
                // Created either way, but only entered if the chooser has not
                // been sent somewhere else since. On slow storage the user can
                // navigate away while the folder is being made, and being
                // taken back is not what they asked for.
                //
                // Against the navigation count, not the listing: making the
                // folder is itself a change the watcher reports, so the
                // listing is replaced as a matter of course here and testing
                // it would refuse to enter the folder almost every time.
                if self.tab.navigation() == navigation {
                    return self.update(Message::TabMessage(tab::Message::Location(
                        Location::Path(path),
                    )));
                }
                log::info!(
                    "created {} after the chooser moved on; not entering it",
                    path.display()
                );
            }
            Message::NotifyEvents(events) => {
                log::debug!("{events:?}");

                if let Some(path) = self.tab.location.path_opt() {
                    let mut contains_change = false;
                    // Deduplicated: a burst names the same path many times,
                    // and re-reading it once per mention is work with nothing
                    // to show for it.
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
                                    // matching item is out of date. Note it;
                                    // re-reading it is disk work and happens
                                    // on a worker.
                                    let known = self.tab.items_opt.as_ref().is_some_and(|items| {
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
                    // Not while searching: a rescan of a search location comes
                    // back empty and would throw the results away, and nothing
                    // would ask for them again.
                    if contains_change && !matches!(self.tab.location, Location::Search(..)) {
                        return self.rescan_tab(None);
                    }

                    let sizes = self.tab.config.icon_sizes;
                    let mut tasks = Vec::new();

                    if !changed.is_empty() {
                        let location = self.tab.location.clone();
                        let listing = self.tab.listing();
                        let batch = self.tab.next_refresh_batch();
                        tasks.push(Task::future(async move {
                            let rebuilt = tokio::task::spawn_blocking(move || {
                                changed
                                    .into_iter()
                                    .filter_map(|path| match tab::item_from_path(&path, sizes) {
                                        Ok(item) => Some((path, Box::new(item))),
                                        Err(err) => {
                                            log::warn!(
                                                "failed to reload {}: {err}",
                                                path.display()
                                            );
                                            None
                                        }
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .await;

                            // Answered even when it found nothing, so the count
                            // of batches still out stays honest.
                            let items = rebuilt.unwrap_or_else(|err| {
                                log::warn!("failed to reload changed items: {err}");
                                Vec::new()
                            });
                            crate::ui::action::app(Message::RefreshedItems(
                                location, listing, batch, items,
                            ))
                        }));
                    }

                    // Icons still standing in as placeholders from an earlier
                    // scan. Items being re-read above are warmed after they are
                    // adopted instead, because until then the tab still holds
                    // their old types.
                    let warmup = self.tab.refresh_icons(sizes);
                    if !warmup.is_empty() {
                        tasks.push(Task::future(async move {
                            if tokio::task::spawn_blocking(move || {
                                tab::warm_icons(&warmup, sizes);
                            })
                            .await
                            .is_err()
                            {
                                return crate::ui::action::none();
                            }
                            crate::ui::action::app(Message::TabMessage(tab::Message::IconsReady))
                        }));
                    }

                    if !tasks.is_empty() {
                        return Task::batch(tasks);
                    }
                }
            }
            Message::RefreshedItems(location, listing, batch, rebuilt) => {
                let mut adopted = false;
                // The chooser may have been sent somewhere else, or rescanned
                // from scratch, while these were being read; either way they
                // describe a listing it is no longer showing.
                if self.tab.location == location && self.tab.listing() == listing {
                    let mut keep = Vec::new();
                    for (path, fresh) in rebuilt {
                        // Batches overlap: two bursts naming the same file
                        // start two workers, and the older one can finish
                        // last. Whatever was read more recently for a path is
                        // what the chooser keeps.
                        if self.tab.accept_refresh(listing, batch, &path) {
                            keep.push((path, fresh));
                        }
                    }
                    if let Some(items) = &mut self.tab.items_opt {
                        for (path, fresh) in keep {
                            // One item per path in a listing, so the first
                            // match is the only one.
                            if let Some(item) =
                                items.iter_mut().find(|item| item.path_opt() == Some(&path))
                            {
                                item.adopt(*fresh);
                                adopted = true;
                            }
                        }
                    }
                }
                if adopted {
                    // Asked for after adopting, not before: a refreshed item
                    // may now be a type whose icon has never been resolved,
                    // and until it is adopted the tab still holds the old one.
                    let sizes = self.tab.config.icon_sizes;
                    let warmup = self.tab.refresh_icons(sizes);
                    if !warmup.is_empty() {
                        return Task::future(async move {
                            if tokio::task::spawn_blocking(move || {
                                tab::warm_icons(&warmup, sizes);
                            })
                            .await
                            .is_err()
                            {
                                return crate::ui::action::none();
                            }
                            crate::ui::action::app(Message::TabMessage(tab::Message::IconsReady))
                        });
                    }
                }
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
            Message::Open => {
                let mut paths = Vec::new();
                if let Some(items) = self.tab.items_opt() {
                    for item in items {
                        if item.selected
                            && let Some(path) = item.path_opt()
                        {
                            paths.push(path.clone());
                            if self.flags.config.show_recents {
                                crate::recents::record(
                                    path.clone(),
                                    Self::APP_ID.to_string(),
                                    "earth-files".to_string(),
                                );
                            }
                        }
                    }
                }

                // Ensure selection is allowed; `can_open` keeps the button disabled otherwise
                for path in &paths {
                    let path_is_dir = path.is_dir();
                    if path_is_dir != self.flags.kind.is_dir() {
                        if path_is_dir && paths.len() == 1 {
                            // If the only selected item is a directory and we are selecting files, cd to it
                            let message = Message::TabMessage(tab::Message::Location(
                                Location::Path(path.clone()),
                            ));
                            return self.update(message);
                        }

                        // Otherwise, this is not a legal selection
                        return Task::none();
                    }
                }

                // If there are proper matching items, return them
                if !paths.is_empty() {
                    self.result_opt = Some(DialogResult::Open(paths));
                    return window::close(self.flags.window_id);
                }

                // If we are in directory mode, return the current directory
                if self.flags.kind.is_dir()
                    && let Location::Path(tab_path) = &self.tab.location
                {
                    self.result_opt = Some(DialogResult::Open(vec![tab_path.clone()]));
                    return window::close(self.flags.window_id);
                }
            }
            Message::Preview => {
                self.context_page = ContextPage::Preview(None, PreviewKind::Selected);
                return self.with_dialog_config(|config| {
                    config.show_details = !config.show_details;
                });
            }
            Message::Save(replace) => {
                if let DialogKind::SaveFile { filename } = &self.flags.kind
                    && !filename.is_empty()
                    && let Some(tab_path) = self.save_dir()
                {
                    let path = tab_path.join(filename);
                    if path.is_dir() {
                        // cd to directory
                        let message =
                            Message::TabMessage(tab::Message::Location(Location::Path(path)));
                        return self.update(message);
                    } else if !replace && path.exists() {
                        self.dialog_pages.push_back(DialogPage::Replace {
                            filename: filename.clone(),
                        });
                        return widget::button::focus(REPLACE_BUTTON_ID.clone());
                    }
                    self.result_opt = Some(DialogResult::Open(vec![path]));
                    return window::close(self.flags.window_id);
                }
            }
            Message::ScrollTab(scroll_speed) => {
                return self.update(Message::TabMessage(tab::Message::ScrollTab(
                    f32::from(scroll_speed) / 10.0,
                )));
            }
            Message::SearchActivate => {
                let mut tasks = vec![];

                if self.search_get().is_none() {
                    tasks.push(self.search_set(Some(String::new())));
                } else {
                    tasks.push(widget::text_input::focus(self.search_id.clone()));
                }

                return Task::batch(tasks);
            }
            Message::SetSearchRecursive(recursive) => {
                // Shared with the main window rather than kept separately:
                // whether a search looks into subfolders is one intent, and
                // two of them that drift apart is a difference nobody asked
                // for.
                self.flags.config.tab.search_recursive = recursive;
                if let Err(err) = self
                    .flags
                    .config_handler
                    .save_in_background(&self.flags.config)
                {
                    log::warn!("failed to save config \"search_recursive\": {err}");
                }
                return Task::batch([
                    self.update_config(),
                    self.update(Message::TabMessage(tab::Message::SetSearchRecursive(
                        recursive,
                    ))),
                ]);
            }
            Message::SearchClear => {
                return self.search_set(None);
            }
            Message::SearchInput(input) => {
                return self.search_set(Some(input));
            }
            Message::TabMessage(tab_message) => {
                let click_i_opt = match tab_message {
                    tab::Message::Click(click_i_opt) => click_i_opt,
                    _ => None,
                };
                let tab_commands = self.tab.update(tab_message, self.modifiers);

                // Update filename box when anything is selected
                if let DialogKind::SaveFile { filename } = &mut self.flags.kind
                    && let Some(click_i) = click_i_opt
                    && let Some(items) = self.tab.items_opt()
                    && let Some(item) = items.get(click_i)
                    && item.selected
                    && !item.metadata.is_dir()
                {
                    filename.clone_from(&item.name);
                    // The name alone does not say where it came from, and a
                    // search lists results from any folder under its root
                    self.save_dir_opt = item
                        .path_opt()
                        .and_then(|path| path.parent())
                        .map(|parent| (self.tab.navigation(), parent.to_path_buf()));
                }

                let mut commands = Vec::new();
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
                                    tab::Message::IconsReady,
                                ))
                            }));
                        }
                        tab::Command::Action(action) => {
                            commands.push(self.update(Message::from(action.message())));
                        }
                        tab::Command::ChangeLocation(_tab_title, _tab_path, selection_paths) => {
                            commands.push(Task::batch([
                                self.update_watcher(),
                                self.rescan_tab(selection_paths),
                            ]));
                        }
                        tab::Command::Surface(action) => {
                            let action = action.map(Message::TabMessage);
                            commands.push(self.update(Message::Surface(action)));
                        }
                        tab::Command::Iced(iced_command) => {
                            commands.push(iced_command.0.map(|tab_message| {
                                crate::ui::action::app(Message::TabMessage(tab_message))
                            }));
                        }
                        tab::Command::OpenFile(_item_path) => {
                            if self.flags.kind.save() {
                                commands.push(self.update(Message::Save(false)));
                            } else {
                                commands.push(self.update(Message::Open));
                            }
                        }
                        tab::Command::Preview(kind) => {
                            self.context_page = ContextPage::Preview(None, kind);
                            commands.push(self.with_dialog_config(|config| {
                                config.show_details = true;
                            }));
                        }
                        tab::Command::WindowDrag => {
                            commands.push(window::drag(self.flags.window_id));
                        }
                        tab::Command::WindowToggleMaximize => {
                            commands.push(window::toggle_maximize(self.flags.window_id));
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
                                    tab::Message::NetworkResolved(request, uri, resolved),
                                ))
                            }));
                        }
                        unsupported => {
                            log::warn!("{unsupported:?} not supported in dialog mode");
                        }
                    }
                }
                return Task::batch(commands);
            }
            Message::TabRescan(location, parent_item_opt, mut items, selection_paths) => {
                if location == self.tab.location {
                    self.filter_items(&mut items);

                    // Select based on filename
                    if let DialogKind::SaveFile { filename } = &self.flags.kind {
                        for item in &mut items {
                            item.selected = &item.name == filename;
                        }
                    }

                    self.tab.parent_item_opt = parent_item_opt;
                    self.tab.set_items(items);
                    // Apply a scroll offset restored from history, now that the items exist
                    let restore_scroll =
                        self.update(Message::TabMessage(tab::Message::ScrollRestore));

                    // Resolve this listing's icons on a worker. The listing is
                    // already drawn with placeholders; the real icons replace
                    // them when they arrive, so opening a folder never waits
                    // on the icon theme.
                    let sizes = self.tab.config.icon_sizes;
                    let wanted = self.tab.refresh_icons(sizes);
                    let warm_icons = if wanted.is_empty() {
                        Task::none()
                    } else {
                        Task::future(async move {
                            if tokio::task::spawn_blocking(move || tab::warm_icons(&wanted, sizes))
                                .await
                                .is_err()
                            {
                                return crate::ui::action::none();
                            }
                            crate::ui::action::app(Message::TabMessage(tab::Message::IconsReady))
                        })
                    };

                    if let Some(mut selection_paths) = selection_paths {
                        if !self.flags.kind.multiple() {
                            selection_paths.truncate(1);
                        }
                        self.tab.select_paths(selection_paths);
                    }

                    // Reset focus on location change
                    let focus = if self.search_get().is_some() {
                        widget::text_input::focus(self.search_id.clone())
                    } else if let DialogKind::SaveFile { filename } = &self.flags.kind {
                        Task::batch([
                            widget::text_input::focus(self.filename_id.clone()),
                            widget::text_input::select_until_last(
                                self.filename_id.clone(),
                                filename,
                                '.',
                            ),
                        ])
                    } else {
                        widget::text_input::focus(self.filename_id.clone())
                    };
                    return Task::batch([restore_scroll, warm_icons, focus]);
                }
            }
            Message::TabView(view) => {
                return self.with_dialog_config(|config| {
                    config.view = view;
                });
            }
            Message::ToggleFoldersFirst => {
                return self.with_dialog_config(|config| {
                    config.folders_first = !config.folders_first;
                });
            }
            Message::ToggleShowHidden => {
                return self.with_dialog_config(|config| {
                    config.show_hidden = !config.show_hidden;
                });
            }
            Message::ZoomDefault => {
                return self.with_dialog_config(|config| {
                    zoom_to_default(config.view, &mut config.icon_sizes);
                });
            }
            Message::ZoomIn => {
                return self.with_dialog_config(|config| {
                    zoom_in_view(config.view, &mut config.icon_sizes);
                });
            }
            Message::ZoomOut => {
                return self.with_dialog_config(|config| {
                    zoom_out_view(config.view, &mut config.icon_sizes);
                });
            }
            Message::Surface(action) => {
                return crate::ui::task::message(crate::ui::Action::Surface(action));
            }
        }

        Task::none()
    }

    /// Creates a view after each update.
    fn view(&self) -> Element<'_, Message> {
        let Spacing { space_xxs, .. } = spacing();

        let mut col = widget::Column::with_capacity(2);

        if self.core.is_condensed()
            && let Some(term) = self.search_get()
        {
            col = col.push(
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

        col = col.push(
            self.tab
                .view(
                    &self.key_binds,
                    &self.modifiers,
                    false,
                    &[],
                    self.core.drawer_slide.column_slide(),
                )
                .map(Message::TabMessage),
        );

        col.into()
    }

    fn subscription(&self) -> Subscription<Message> {
        struct WatcherSubscription;
        let mut subscriptions = vec![
            // The chooser shares the runtime with its host, so every window's
            // events arrive here. Only this window's may drive the chooser,
            // otherwise typing in the host window works the dialog too.
            event::listen_with(|event, status, window_id| match event {
                Event::Keyboard(KeyEvent::KeyPressed {
                    key,
                    physical_key,
                    modifiers,
                    text,
                    ..
                }) => match status {
                    event::Status::Ignored => Some(Message::ForWindow(
                        window_id,
                        Box::new(Message::Key(modifiers, key, physical_key, text)),
                    )),
                    event::Status::Captured => {
                        if key == Key::Named(Named::Escape) {
                            Some(Message::ForWindow(window_id, Box::new(Message::Escape)))
                        } else {
                            None
                        }
                    }
                },
                Event::Keyboard(KeyEvent::ModifiersChanged(modifiers)) => Some(Message::ForWindow(
                    window_id,
                    Box::new(Message::ModifiersChanged(modifiers)),
                )),
                _ => None,
            }),
            self.flags
                .config_handler
                .subscription::<Config>()
                .map(Message::Config),
            Subscription::run_with(TypeId::of::<WatcherSubscription>(), |_| {
                stream::channel(100, {
                    |mut output: futures::channel::mpsc::Sender<_>| async move {
                        let watcher_res = {
                            let mut output = output.clone();
                            new_debouncer(
                                time::Duration::from_millis(250),
                                Some(time::Duration::from_millis(250)),
                                move |events_res: notify_debouncer_full::DebounceEventResult| {
                                    match events_res {
                                        Ok(mut events) => {
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
                    }
                })
            }),
            self.tab
                .subscription(
                    self.core.window.show_context
                        && matches!(
                            self.context_page,
                            ContextPage::Preview(_, PreviewKind::Selected)
                        ),
                )
                .map(Message::TabMessage),
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
                .map(|(key, mounter_message)| {
                    if let MounterMessage::Items(items) = mounter_message {
                        Message::MounterItems(key, items)
                    } else {
                        log::warn!("{mounter_message:?} not supported in dialog mode");
                        Message::None
                    }
                })
        }));

        Subscription::batch(subscriptions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chooser_answers_for_its_own_window() {
        let chooser = window::Id::unique();
        let main = window::Id::unique();
        let host: HashMap<window::Id, String> = [
            (main, "Home — Earth Files".to_owned()),
            (chooser, "stale".to_owned()),
        ]
        .into();

        assert_eq!(
            window_title([(chooser, "Open File")], &host, chooser),
            "Open File",
            "the chooser's own title wins"
        );
    }

    #[test]
    fn other_windows_keep_the_hosts_title() {
        let chooser = window::Id::unique();
        let main = window::Id::unique();
        let host: HashMap<window::Id, String> = [(main, "Home — Earth Files".to_owned())].into();

        assert_eq!(
            window_title([(chooser, "Open File")], &host, main),
            "Home — Earth Files"
        );
        assert_eq!(
            window_title([], &host, window::Id::unique()),
            "",
            "an unknown window has no title"
        );
    }
}
