use crate::mime_app::Mime;
use crate::trashing::{TrashItem, TrashItemMetadata, TrashItemSize};
use crate::ui::Element;
use crate::ui::iced::advanced::text;
use crate::ui::iced::futures::{self, SinkExt};
use crate::ui::iced::keyboard::Modifiers;
use crate::ui::iced::widget::stack;
use crate::ui::iced::{
    Alignment, Color, ContentFit, Length, Point, Rectangle, Size, Subscription, padding, stream,
    window,
};
use crate::ui::iced_core::mouse::ScrollDelta;
use crate::ui::theme;
use crate::ui::widget::menu::action::MenuAction;
use crate::ui::widget::menu::key_bind::KeyBind;
use crate::ui::widget::scrollable::{self, AbsoluteOffset, Viewport};
use crate::ui::widget::{self, Id, space};
use i18n_embed::LanguageLoader;
use icu::datetime::input::DateTime;
use icu::datetime::options::TimePrecision;
use icu::datetime::{DateTimeFormatter, DateTimeFormatterPreferences, fieldsets};
use image::{DynamicImage, ImageReader};
use jiff_icu::ConvertFrom;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::fmt::{self, Display};
use std::fs::{self, File, Metadata};
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{self, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, RwLock, atomic};
use std::time::{Duration, Instant, SystemTime};
use tempfile::NamedTempFile;
use tokio::sync::mpsc;
use xdg_kit::basedir::UserDirKind;
use xdg_kit::desktop_entry::{DesktopEntry, Locales};

use crate::app::{Action, PreviewItem, PreviewKind};
use crate::config::{ICON_SCALE_MAX, ICON_SIZE_GRID, IconSizes, TabConfig, ThumbCfg};
use crate::dialog::DialogKind;
use crate::handoff::Handoff;
use crate::large_image::{
    Decoded, LargeImageManager, StartDecode, decode_large_image, exceeds_memory_limit,
    should_use_dedicated_worker, should_use_tiling,
};
use crate::localize::{LANGUAGE_SORTER, LOCALE};
use crate::mime_icon::mime_for_path;
use crate::mounter::MOUNTERS;
use crate::operation::{CancelOnDrop, Controller, ControllerState, OperationError};
use crate::thumbnail_cacher::{self, CachedThumbnail, ThumbnailCacher};
use crate::thumbnailer::thumbnailers;
use crate::trash::{Trash, TrashExt};
use crate::ui::convert::{ToColor, ToPixels, ToRadius};
use crate::ui::shell::panel_slide::ColumnSlide;
use crate::ui::theme::{Button, Container, Spacing, spacing};
use crate::{FxOrderMap, fl, menu, mime_app, mouse_area};
use iced_texture_cache::{TextureCache, cached};

pub const DOUBLE_CLICK_DURATION: Duration = Duration::from_millis(500);

/// How long a rectangle selection over blank space, where no item is crossed
/// to report it, goes without updating the selection at most.
pub const DRAG_REARM: Duration = Duration::from_millis(50);
pub const TYPE_SELECT_TIMEOUT: Duration = Duration::from_millis(1000);
const MAX_SEARCH_LATENCY: Duration = Duration::from_millis(20);
const THUMBNAIL_SIZE: u32 = (ICON_SIZE_GRID as u32) * (ICON_SCALE_MAX as u32);

/// The side of an item's preview in a dialog about it, such as the one for
/// a name already taken.
const DIALOG_PREVIEW: f32 = 128.0;
/// How far the path field's text is in from its ends: as far as the menus'
/// labels are from their pill's (2 + 12).
const FIELD_INSET: u16 = 14;
/// How long an external thumbnailer may run before it is killed.
///
/// Thumbnail work is bounded by [`THUMB_SEMAPHORE`], so a thumbnailer that
/// never exits does not merely lose its own picture: it holds one of the few
/// permits for as long as this process lives, and the pane stops filling in.
/// A deadline is the only thing that gets the permit back.
const THUMBNAILER_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum bytes to read from a file for its preview. Only the first
/// [`TEXT_PREVIEW_LINES`] of what is read are ever shaped, so this needs to be
/// no more than enough to contain them.
const TEXT_PREVIEW_MAX_BYTES: usize = 64 * 1024; // 64 KiB
/// Lines of a text file the preview keeps.
///
/// The preview pane is a [`THUMBNAIL_SIZE`] square, which fits about fifteen
/// rows of wrapped text. Every character beyond what it can show is shaped and
/// then never drawn, and shaping takes the process-wide font system lock that
/// the event loop needs to draw anything at all, so an over-long preview
/// freezes the whole application rather than merely wasting work.
const TEXT_PREVIEW_LINES: usize = 16;
/// Characters of each preview line that are kept, for the same reason: a
/// minified file is a single line thousands of columns wide, of which the pane
/// can show about thirty before wrapping.
const TEXT_PREVIEW_LINE_CHARS: usize = 80;
/// Spaces a tab becomes in a preview.
const TAB_PREVIEW_WIDTH: usize = 4;
/// Maximum file size (bytes) to attempt text preview; files larger than this are skipped entirely.
const TEXT_PREVIEW_MAX_FILE_BYTES: u64 = 8 * 1000 * 1000; // 8 MiB

/// Cut `text` down to what the preview pane can actually show, and to what is
/// cheap to shape.
///
/// Both dimensions are bounded, because shaping cost grows with the characters
/// handed to the editor and is paid under a lock the event loop also needs.
///
/// Control characters are dropped, and not only because they draw as tofu: a
/// character with no glyph in any loaded font sends the shaper searching every
/// installed font for one, and it finds nothing to remember, so it searches
/// again for the next. A log file's colour escapes cost about a hundred times
/// what their letters do.
fn preview_text(text: &str) -> String {
    let mut preview = String::new();
    for (index, line) in text.lines().take(TEXT_PREVIEW_LINES).enumerate() {
        // By index, not by whether anything has been written: a file that
        // opens with blank lines opens with them in its preview too.
        if index > 0 {
            preview.push('\n');
        }

        let mut kept = 0;
        for character in line.chars() {
            if kept >= TEXT_PREVIEW_LINE_CHARS {
                break;
            }
            match character {
                // Indentation is worth keeping; a tab is a control character
                // and shapes like one.
                '\t' => {
                    let spaces = TAB_PREVIEW_WIDTH.min(TEXT_PREVIEW_LINE_CHARS - kept);
                    preview.extend(std::iter::repeat_n(' ', spaces));
                    kept += spaces;
                }
                _ if character.is_control() => {}
                _ => {
                    preview.push(character);
                    kept += 1;
                }
            }
        }
    }
    preview
}

// Thumbnail generation semaphore - limits parallel thumbnail workers
// Uses 4 workers for balanced throughput and memory usage
pub static THUMB_SEMAPHORE: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(*THUMB_PERMITS as usize)));

/// How many permits [`THUMB_SEMAPHORE`] has: what a job takes to run alone.
static THUMB_PERMITS: LazyLock<u32> = LazyLock::new(|| {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(4) as u32
});

/// Runs `work` on a blocking thread, no more than `semaphore` allows at once.
///
/// The permit travels into the blocking task rather than being held by this
/// future, because the two do not end together. Tokio cannot interrupt a
/// blocking task, so when an item scrolls out of view and its subscription is
/// dropped, the work carries on regardless. A permit held out here would be
/// returned at that moment and the next item would start its own job beside
/// the one still running, until a scroll through a directory of videos had
/// every thumbnailer on the machine going at once.
///
/// Held inside, the bound is honest: a job cancelled before its turn never
/// starts, and one already running keeps its place until it truly ends.
pub(crate) async fn bounded_blocking<T, F>(
    semaphore: Arc<tokio::sync::Semaphore>,
    work: F,
) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    bounded_blocking_taking(semaphore, 1, work).await
}

/// As [`bounded_blocking`], taking `permits` of the semaphore's permits for
/// the work rather than one.
pub(crate) async fn bounded_blocking_taking<T, F>(
    semaphore: Arc<tokio::sync::Semaphore>,
    permits: u32,
    work: F,
) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let permit = semaphore.acquire_many_owned(permits).await.ok()?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .ok()
}

/// Cancels a [`Controller`] when dropped.
///
/// Dropping the future that awaits a blocking job does not stop the job, so
/// the drop has to reach it some other way. Scrolling a selected folder out of
/// view drops its subscription, and this turns that into a cancellation the
/// walk itself notices.
/// Which entries of a listing are selected, and which are highlighted, by
/// each entry's own id. Kept by the tab rather than the entries, so an entry
/// read again in place keeps it, and so do the ones search results arrive
/// around. A listing read anew is new entries: its selection is restored
/// by path where asked for.
#[derive(Debug, Default)]
pub struct Selection {
    selected: rustc_hash::FxHashSet<widget::Id>,
    highlighted: rustc_hash::FxHashSet<widget::Id>,
    /// Cut to the clipboard, drawn dimmed until pasted.
    cut: rustc_hash::FxHashSet<widget::Id>,
    /// Crossed by the rectangle being dragged out, which toggles what it
    /// newly crosses or leaves.
    dragged_over: rustc_hash::FxHashSet<widget::Id>,
}

impl Selection {
    pub fn is_selected(&self, item: &Item) -> bool {
        self.selected.contains(&item.button_id)
    }

    pub fn select(&mut self, item: &Item, selected: bool) {
        if selected {
            self.selected.insert(item.button_id.clone());
        } else {
            self.selected.remove(&item.button_id);
        }
    }

    pub fn is_highlighted(&self, item: &Item) -> bool {
        self.highlighted.contains(&item.button_id)
    }

    pub fn highlight(&mut self, item: &Item, highlighted: bool) {
        if highlighted {
            self.highlighted.insert(item.button_id.clone());
        } else {
            self.highlighted.remove(&item.button_id);
        }
    }

    /// Nothing selected; what is highlighted stays.
    pub fn deselect_all(&mut self) {
        self.selected.clear();
    }

    /// Whether anything is selected.
    pub fn any(&self) -> bool {
        !self.selected.is_empty()
    }

    pub fn is_cut(&self, item: &Item) -> bool {
        self.cut.contains(&item.button_id)
    }

    pub fn set_cut(&mut self, item: &Item, cut: bool) {
        if cut {
            self.cut.insert(item.button_id.clone());
        } else {
            self.cut.remove(&item.button_id);
        }
    }

    pub fn is_dragged_over(&self, item: &Item) -> bool {
        self.dragged_over.contains(&item.button_id)
    }

    pub fn set_dragged_over(&mut self, item: &Item, over: bool) {
        if over {
            self.dragged_over.insert(item.button_id.clone());
        } else {
            self.dragged_over.remove(&item.button_id);
        }
    }

    /// Nothing selected or highlighted.
    pub fn clear(&mut self) {
        self.selected.clear();
        self.highlighted.clear();
    }

    /// Nothing at all, for a listing read anew: its entries are new, and
    /// what is cut is marked on them again from the clipboard.
    pub fn forget_entries(&mut self) {
        self.clear();
        self.cut.clear();
        self.dragged_over.clear();
    }
}

/// What is known about an item from the disk itself, as opposed to from the
/// listing it came in.
///
/// Every item carries listing metadata -- name, type, size, whatever the
/// scan handed over -- and that is what the pane draws first. Trashed and
/// GVFS items have nothing more than that until something asks the disk, and
/// asking the disk is what this tracks: not yet, on its way, here, or refused.
/// The three cases are kept apart deliberately. "Not here yet" and "could not
/// be read" have to draw differently, and neither may be drawn as a plausible
/// zero.
#[derive(Clone, Debug)]
pub enum MetadataState {
    /// Nothing has been asked yet. While the item is previewed, a request is
    /// made for it.
    Pending,
    /// A read is on its way. Started once per incarnation and never again
    /// while it runs, however the selection changes in the meantime: the
    /// read holds one of the few permits, and a second read of the same item
    /// would hold another. Its answer arrives by message whether or not the
    /// item is still previewed by then.
    Loading,
    Ready(Metadata),
    Failed(String),
}

/// Detail reads for the preview pane allowed at once.
///
/// Small on purpose, and separate from every other semaphore, because of what
/// a dead mount does: a `stat` on it may not return for as long as the mount
/// stays dead, and the worker holding that permit stays with it. Each dead
/// mount touched costs one permit for the session. The pane has to be, and
/// is, fine with that: the listing still draws it, and a request that never
/// answers is drawn as pending rather than as anything else.
static DETAILS_SEMAPHORE: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

/// Folder-size traversals allowed at once.
///
/// Kept apart from [`THUMB_SEMAPHORE`] so that selecting a few large folders
/// cannot starve the thumbnails the same pane is trying to draw. Smaller,
/// because these are whole-tree walks and running many of them at once turns
/// a disk's seek pattern against itself.
static DIR_SIZE_SEMAPHORE: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

/// How long a traversal that finds its controller paused waits before looking
/// again. Nothing pauses a folder-size job today; this keeps the meaning of a
/// pause the same as everywhere else in case something ever does.
const DIR_SIZE_PAUSE_POLL: Duration = Duration::from_millis(50);

pub(crate) static SORT_OPTION_FALLBACK: LazyLock<FxHashMap<String, (HeadingOptions, bool)>> =
    LazyLock::new(|| {
        FxHashMap::from_iter(
            crate::xdg_dirs::user_dir(&UserDirKind::Download)
                .into_iter()
                .map(|dir| {
                    (
                        Location::Path(dir).normalize().to_string(),
                        (HeadingOptions::Modified, false),
                    )
                }),
        )
    });

static MODE_NAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
    vec![
        // Mode 0
        fl!("none"),
        // Mode 1
        fl!("execute-only"),
        // Mode 2
        fl!("write-only"),
        // Mode 3
        fl!("write-execute"),
        // Mode 4
        fl!("read-only"),
        // Mode 5
        fl!("read-execute"),
        // Mode 6
        fl!("read-write"),
        // Mode 7
        fl!("read-write-execute"),
    ]
});

/// Names for user and group ids, remembered for the life of the process.
///
/// Resolving an id can reach a directory service over the network, and these
/// names are read while building the details pane, which happens on every
/// frame and once per selected item. The set of distinct ids in a directory
/// is small, so the map stays small; names rarely change within a session.
static USER_NAMES: LazyLock<RwLock<FxHashMap<u32, String>>> = LazyLock::new(Default::default);
static GROUP_NAMES: LazyLock<RwLock<FxHashMap<u32, String>>> = LazyLock::new(Default::default);

fn cached_name(
    cache: &LazyLock<RwLock<FxHashMap<u32, String>>>,
    id: u32,
    lookup: impl FnOnce(u32) -> String,
) -> String {
    if let Ok(names) = cache.read()
        && let Some(name) = names.get(&id)
    {
        return name.clone();
    }
    let name = lookup(id);
    if let Ok(mut names) = cache.write() {
        names.insert(id, name.clone());
    }
    name
}

pub fn user_name(uid: u32) -> String {
    cached_name(&USER_NAMES, uid, |uid| {
        uzers::get_user_by_uid(uid)
            .and_then(|user| user.name().to_str().map(ToOwned::to_owned))
            .unwrap_or_default()
    })
}

pub fn group_name(gid: u32) -> String {
    cached_name(&GROUP_NAMES, gid, |gid| {
        uzers::get_group_by_gid(gid)
            .and_then(|group| group.name().to_str().map(ToOwned::to_owned))
            .unwrap_or_default()
    })
}

static SPECIAL_DIRS: LazyLock<FxHashMap<PathBuf, &'static str>> = LazyLock::new(|| {
    let mut special_dirs = FxHashMap::default();
    if let Some(user_dirs) = crate::xdg_dirs::user_dirs() {
        for (kind, icon) in [
            (UserDirKind::Documents, "folder-documents"),
            (UserDirKind::Download, "folder-download"),
            (UserDirKind::Music, "folder-music"),
            (UserDirKind::Pictures, "folder-pictures"),
            (UserDirKind::PublicShare, "folder-publicshare"),
            (UserDirKind::Templates, "folder-templates"),
            (UserDirKind::Videos, "folder-videos"),
            (UserDirKind::Desktop, "user-desktop"),
        ] {
            if let Some(dir) = user_dirs.get(&kind) {
                special_dirs.insert(dir.to_owned(), icon);
            }
        }
    }
    if let Some(dir) = crate::xdg_dirs::home() {
        special_dirs.insert(dir, "user-home");
    }
    special_dirs
});

fn button_appearance(
    theme: &theme::Theme,
    selected: bool,
    highlighted: bool,
    cut: bool,
    focused: bool,
    accent: bool,
    condensed_radius: bool,
) -> widget::button::Style {
    let cosmic = theme.cosmic();
    let mut appearance = widget::button::Style::new();
    if selected {
        if accent {
            appearance.background = Some(cosmic.accent_color().to_color().into());
            appearance.icon_color = Some(cosmic.on_accent_color().to_color());
            if cut {
                appearance.text_color = Some(cosmic.accent.on_disabled.to_color());
            } else {
                appearance.text_color = Some(cosmic.on_accent_color().to_color());
            }
        } else {
            appearance.background = Some(cosmic.bg_component_color().to_color().into());
        }
    } else if highlighted {
        if accent {
            appearance.background = Some(cosmic.bg_component_color().to_color().into());
            appearance.icon_color = Some(cosmic.on_bg_component_color().to_color());
            appearance.text_color = Some(cosmic.on_bg_component_color().to_color());
            if cut {
                appearance.text_color = Some(
                    cosmic
                        .background(theme.transparent)
                        .component
                        .on_disabled
                        .to_color(),
                );
            } else {
                appearance.text_color = Some(cosmic.on_bg_component_color().to_color());
            }
        } else {
            appearance.background = Some(cosmic.bg_component_color().to_color().into());
        }
    } else if cut {
        appearance.text_color = Some(
            cosmic
                .background(theme.transparent)
                .component
                .on_disabled
                .to_color(),
        );
    }
    if focused && accent {
        appearance.outline_width = 1.0;
        appearance.outline_color = cosmic.accent_color().to_color();
        appearance.border_width = 2.0;
        appearance.border_color = Color::TRANSPARENT;
    }
    if condensed_radius {
        appearance.border_radius = cosmic.radius_xs().to_radius();
    } else {
        appearance.border_radius = cosmic.radius_s().to_radius();
    }
    appearance
}

fn button_style(
    selected: bool,
    highlighted: bool,
    cut: bool,
    accent: bool,
    condensed_radius: bool,
) -> Button {
    Button::Custom {
        active: Box::new(move |focused, theme| {
            button_appearance(
                theme,
                selected,
                highlighted,
                cut,
                focused,
                accent,
                condensed_radius,
            )
        }),
        disabled: Box::new(move |theme| {
            button_appearance(
                theme,
                selected,
                highlighted,
                cut,
                false,
                accent,
                condensed_radius,
            )
        }),
        hovered: Box::new(move |focused, theme| {
            button_appearance(
                theme,
                selected,
                highlighted,
                cut,
                focused,
                accent,
                condensed_radius,
            )
        }),
        pressed: Box::new(move |focused, theme| {
            button_appearance(
                theme,
                selected,
                highlighted,
                cut,
                focused,
                accent,
                condensed_radius,
            )
        }),
    }
}

/// XDG icon name for a directory, without the `-symbolic` suffix.
///
/// Split out of `folder_icon`/`folder_icon_symbolic` so `src/dialog.rs` can
/// build its icon from the same name without duplicating the `SPECIAL_DIRS`
/// lookup.
pub fn folder_icon_name(path: &PathBuf) -> &'static str {
    SPECIAL_DIRS.get(path).map_or("folder", |x| *x)
}

/// Folder icons, kept by name and size.
///
/// Resolving one searches the icon theme on disk and costs tens of
/// milliseconds. Without this a directory of a hundred folders pays that for
/// every folder at every size, although they nearly all share the one name.
/// Folder icon name, size, and whether the symbolic variant is wanted
type FolderIconKey = (&'static str, u16, bool);

static FOLDER_ICONS: LazyLock<Mutex<FxHashMap<FolderIconKey, widget::icon::Handle>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));

/// The folder icon for `name`, only if it is already resolved
fn try_folder_icon(name: &'static str, icon_size: u16) -> Option<widget::icon::Handle> {
    let cache = FOLDER_ICONS.lock().unwrap_or_else(|err| err.into_inner());
    cache.get(&(name, icon_size, false)).cloned()
}

fn cached_folder_icon(name: &'static str, icon_size: u16, symbolic: bool) -> widget::icon::Handle {
    let mut cache = FOLDER_ICONS.lock().unwrap_or_else(|err| err.into_inner());
    cache
        .entry((name, icon_size, symbolic))
        .or_insert_with(|| {
            if symbolic {
                widget::icon::from_name(format!("{name}-symbolic"))
                    .size(icon_size)
                    .handle()
            } else {
                widget::icon::from_name(name)
                    .prefer_svg(true)
                    .size(icon_size)
                    .handle()
            }
        })
        .clone()
}

pub fn folder_icon(path: &PathBuf, icon_size: u16) -> widget::icon::Handle {
    cached_folder_icon(folder_icon_name(path), icon_size, false)
}

/// The folder icon if it is cached, else the generic placeholder. Used while
/// scanning, which must not wait on the icon theme.
fn folder_icon_or_placeholder(path: &PathBuf, icon_size: u16) -> widget::icon::Handle {
    try_folder_icon(folder_icon_name(path), icon_size)
        .unwrap_or_else(|| crate::mime_icon::placeholder_icon(icon_size))
}

pub fn folder_icon_symbolic(path: &PathBuf, icon_size: u16) -> widget::icon::Handle {
    cached_folder_icon(folder_icon_name(path), icon_size, true)
}

fn has_trailing_sep(path: &Path) -> bool {
    path.as_os_str()
        .as_encoded_bytes()
        .last()
        .copied()
        .is_some_and(|b| path::is_separator(b as char))
}

fn tab_complete(path: &Path) -> Result<Vec<(String, PathBuf)>, Box<dyn Error>> {
    let parent = if has_trailing_sep(path) && path.is_dir() {
        // Show completions inside existing child directory instead of parent
        path
    } else {
        path.parent()
            .ok_or_else(|| format!("path has no parent {}", path.display()))?
    };

    let child_os = path.strip_prefix(parent)?;
    let child = child_os
        .to_str()
        .ok_or_else(|| format!("invalid UTF-8 {}", child_os.display()))?;

    let pattern = format!("^{}", regex::escape(child));
    let regex = regex::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()?;

    let mut completions = Vec::new();
    for entry_res in fs::read_dir(parent)? {
        let entry = entry_res?;
        let file_name_os = entry.file_name();
        let Some(file_name) = file_name_os.to_str() else {
            continue;
        };
        // Don't list hidden files before entering a pattern
        if pattern == "^" && file_name.starts_with('.') {
            continue;
        }
        if regex.is_match(file_name) {
            completions.push((file_name.to_string(), entry.path()));
        }
    }

    completions.sort_by(|a, b| LANGUAGE_SORTER.compare(&a.0, &b.0));
    completions.truncate(8);
    Ok(completions)
}

pub(crate) fn format_size(size: u64) -> String {
    const KB: u64 = 1000;
    const MB: u64 = 1000 * KB;
    const GB: u64 = 1000 * MB;
    const TB: u64 = 1000 * GB;

    if size >= TB {
        format!("{:.1} TB", size as f64 / TB as f64)
    } else if size >= GB {
        format!("{:.1} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.1} KB", size as f64 / KB as f64)
    } else {
        format!("{size} B")
    }
}

const MODE_SHIFT_USER: u32 = 6;
const MODE_SHIFT_GROUP: u32 = 3;
const MODE_SHIFT_OTHER: u32 = 0;

const fn get_mode_part(mode: u32, shift: u32) -> u32 {
    (mode >> shift) & 0o7
}

fn set_mode_part(mode: u32, shift: u32, bits: u32) -> u32 {
    assert!(bits <= 0o7);
    (mode & !(0o7 << shift)) | (bits << shift)
}

fn date_time_formatter() -> DateTimeFormatter<fieldsets::YMDT> {
    let prefs = DateTimeFormatterPreferences::from(LOCALE.clone());

    let mut fs = fieldsets::YMDT::medium();
    fs = fs.with_time_precision(TimePrecision::Minute);

    DateTimeFormatter::try_new(prefs, fs).expect("failed to create DateTimeFormatter")
}

fn time_formatter() -> DateTimeFormatter<fieldsets::T> {
    let prefs = DateTimeFormatterPreferences::from(LOCALE.clone());

    let mut fs = fieldsets::T::medium();
    fs = fs.with_time_precision(TimePrecision::Minute);

    DateTimeFormatter::try_new(prefs, fs).expect("failed to create DateTimeFormatter")
}

struct FormatTime<'a> {
    pub time: SystemTime,
    pub date_time_formatter: &'a DateTimeFormatter<fieldsets::YMDT>,
    pub time_formatter: &'a DateTimeFormatter<fieldsets::T>,
}

impl<'a> FormatTime<'a> {
    fn from_secs(
        secs: i64,
        date_time_formatter: &'a DateTimeFormatter<fieldsets::YMDT>,
        time_formatter: &'a DateTimeFormatter<fieldsets::T>,
    ) -> Option<Self> {
        // This looks convoluted because we need to ensure the units match up
        let secs: u64 = secs.try_into().ok()?;
        let now = SystemTime::now();
        let filetime_diff = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|from_epoch| from_epoch.as_secs())
            .ok()
            .and_then(|now_secs| now_secs.checked_sub(secs))
            .map(Duration::from_secs)?;
        now.checked_sub(filetime_diff).map(|time| Self {
            time,
            date_time_formatter,
            time_formatter,
        })
    }
}

impl Display for FormatTime<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Ok(zoned) = jiff::Zoned::try_from(self.time) else {
            return Ok(());
        };
        let now = jiff::Zoned::now();
        let icu_datetime = DateTime::convert_from(zoned.datetime());
        if zoned.date() == now.date() {
            f.write_str(fl!("today").as_str())?;
            f.write_str(", ")?;
            self.time_formatter.format(&icu_datetime).fmt(f)
        } else {
            self.date_time_formatter.format(&icu_datetime).fmt(f)
        }
    }
}

const fn format_time<'a>(
    time: SystemTime,
    date_time_formatter: &'a DateTimeFormatter<fieldsets::YMDT>,
    time_formatter: &'a DateTimeFormatter<fieldsets::T>,
) -> FormatTime<'a> {
    FormatTime {
        time,
        date_time_formatter,
        time_formatter,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsKind {
    Local,
    Remote,
    Gvfs,
}

/// Whether a filesystem type is local, remote, or GVFS's FUSE bridge.
pub fn fs_type_kind(fs_type: &str) -> FsKind {
    match fs_type {
        // SMB/CIFS variants
        "cifs" | "smb" | "smb2" | "smbfs" => FsKind::Remote,

        // NFS variants
        "nfs" | "nfs4" => FsKind::Remote,

        // FUSE-based remote filesystems
        "fuse.rclone" | "fuse.sshfs" | "fuse.davfs2" | "fuse.ceph" | "fuse.glusterfs"
        | "fuse.s3fs" | "fuse.goofys" | "fuse.gcsfuse" | "fuse.afp" | "fuse.afpfs" => {
            FsKind::Remote
        }

        // Other network protocols
        "afs" | "coda" | "ncpfs" | "davfs" | "davfs2" | "shfs" => FsKind::Remote,

        // Cluster/distributed filesystems
        "ceph" | "glusterfs" | "lustre" | "gfs" | "gfs2" | "ocfs2" => FsKind::Remote,

        // GVFS (GNOME Virtual File System)
        "fuse.gvfsd-fuse" => FsKind::Gvfs,

        // Everything else is local
        _ => FsKind::Local,
    }
}

/// Device numbers of the mounted filesystems and their kind, read from
/// `/proc/self/mountinfo` and refreshed after [`FS_KINDS_TTL`] so mounts made
/// after startup are classified too.
static FS_KINDS: Mutex<Option<(Instant, FxHashMap<u64, FsKind>)>> = Mutex::new(None);
const FS_KINDS_TTL: Duration = Duration::from_secs(5);

pub fn fs_kind(metadata: &Metadata) -> FsKind {
    let mut guard = FS_KINDS.lock().unwrap_or_else(|e| e.into_inner());
    if guard
        .as_ref()
        .is_none_or(|(read_at, _)| read_at.elapsed() >= FS_KINDS_TTL)
    {
        *guard = Some((Instant::now(), read_fs_kinds()));
    }
    let (_, devices) = guard.as_ref().expect("filled above");
    devices.get(&metadata.dev()).map_or(FsKind::Local, |x| *x)
}

fn read_fs_kinds() -> FxHashMap<u64, FsKind> {
    match fs::read_to_string("/proc/self/mountinfo") {
        Ok(mountinfo) => fs_kinds_in(&mountinfo),
        Err(err) => {
            log::warn!("failed to read mount info: {err}");
            FxHashMap::default()
        }
    }
}

/// Each mounted device's kind, from `mountinfo`'s text: its `major:minor`
/// third field, and the filesystem type first after the ` - ` separator.
fn fs_kinds_in(mountinfo: &str) -> FxHashMap<u64, FsKind> {
    mountinfo
        .lines()
        .filter_map(|line| {
            let (major, minor) = line.split(' ').nth(2)?.split_once(':')?;
            let dev = libc::makedev(major.parse().ok()?, minor.parse().ok()?);
            let fs_type = line.split_once(" - ")?.1.split(' ').next()?;
            Some((dev, fs_type_kind(fs_type)))
        })
        .collect()
}

fn get_desktop_file_display_name(path: &Path) -> Option<String> {
    let entry = match DesktopEntry::read(path) {
        Ok(ok) => ok,
        Err(err) => {
            log::warn!("failed to read {}: {}", path.display(), err);
            return None;
        }
    };

    entry.name(&Locales::from_env())
}

fn get_desktop_file_icon(path: &Path) -> Option<String> {
    let entry = match DesktopEntry::read(path) {
        Ok(ok) => ok,
        Err(err) => {
            log::warn!("failed to read {}: {}", path.display(), err);
            return None;
        }
    };

    entry.icon(&Locales::from_env())
}

/// Creates an icon handle from a desktop file's Icon field value.
/// Supports both icon names (looked up in theme) and absolute paths (used directly).
fn desktop_icon_handle(icon: &str, size: u16) -> widget::icon::Handle {
    let icon_path = Path::new(icon);
    if icon_path.is_absolute() && icon_path.exists() {
        widget::icon::from_path(icon_path.to_path_buf())
    } else {
        widget::icon::from_name(icon)
            .prefer_svg(true)
            .size(size)
            .handle()
    }
}

pub fn parse_desktop_file(path: &Path) -> (Option<String>, Option<String>) {
    let entry = match DesktopEntry::read(path) {
        Ok(ok) => ok,
        Err(err) => {
            log::warn!("failed to read {}: {}", path.display(), err);
            return (None, None);
        }
    };
    let locales = Locales::from_env();
    (entry.name(&locales), entry.icon(&locales))
}

pub(crate) fn display_name_for_file(
    path: &Path,
    name: &str,
    get_from_gvfs: bool,
    is_desktop: bool,
) -> String {
    if is_desktop {
        return get_desktop_file_display_name(path).map_or_else(
            || Item::display_name(name),
            |desktop_name| Item::display_name(desktop_name.as_str()),
        );
    } else if get_from_gvfs {
        #[cfg(feature = "gvfs")]
        {
            let file = gio::File::for_path(path);
            if let Ok(info) = gio::prelude::FileExt::query_info(
                &file,
                "standard::display-name",
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            ) {
                return Item::display_name(info.display_name().as_str());
            }
        }
    }
    Item::display_name(name)
}

// Whether a dir lives on a remote filesystem, according to GIO.
#[cfg(feature = "gvfs")]
fn gvfs_dir_is_remote(dir: &Path) -> bool {
    static REMOTE_CACHE: LazyLock<RwLock<FxHashMap<PathBuf, bool>>> =
        LazyLock::new(|| RwLock::new(FxHashMap::default()));

    if let Some(remote) = REMOTE_CACHE.read().unwrap().get(dir) {
        return *remote;
    }

    let remote = match gio::prelude::FileExt::query_filesystem_info(
        &gio::File::for_path(dir),
        gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE,
        gio::Cancellable::NONE,
    ) {
        Ok(info) => info.boolean(gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE),
        Err(err) => {
            log::warn!(
                "failed to get GIO filesystem info for {}: {}",
                dir.display(),
                err
            );

            //Assume remote so that the "expensive tasks" are rather skipped then actually executed.
            true
        }
    };

    REMOTE_CACHE
        .write()
        .unwrap()
        .insert(dir.to_path_buf(), remote);
    remote
}

/// Icons for a file item, using the launcher's own icon when `path` is a
/// desktop entry. Returns whether it was one, so callers can also take the
/// display name from the entry.
/// Positions the content check takes as on screen before the listing has
/// been drawn.
const DEFAULT_SHOWN: usize = 64;

/// What a layout was made for. While it holds, every item's position and
/// rect are still where the layout put them, and a view need not visit
/// them again.
#[derive(Clone, Debug)]
struct LayoutKey {
    /// The kept order, by identity: a new order is a new one, so anything
    /// that drops the order also drops the layout made from it
    order: Arc<[usize]>,
    show_hidden: bool,
    columns: usize,
    width: f32,
    pitch: f32,
}

impl PartialEq for LayoutKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.order, &other.order)
            && self.show_hidden == other.show_hidden
            && self.columns == other.columns
            && self.width == other.width
            && self.pitch == other.pitch
    }
}

/// How a view laid the listing out: the indices of the items it shows, in
/// order, hidden ones left out while hidden, in rows of `columns` items
/// starting `top` pixels down and `pitch` pixels apart.
#[derive(Clone, Debug)]
struct ShownLayout {
    key: LayoutKey,
    shown: Arc<[usize]>,
    columns: usize,
    top: f32,
    pitch: f32,
}

impl ShownLayout {
    /// The positions of `shown` whose rows meet `visible`, worked out from
    /// the rows' geometry alone: what is on screen costs the same however
    /// long the listing is.
    fn visible(&self, visible: Rectangle) -> std::ops::Range<usize> {
        if self.pitch <= 0.0 {
            return 0..0;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped at zero; a row number"
        )]
        let row = |y: f32| ((y - self.top) / self.pitch).max(0.0);
        let len = self.shown.len();
        // The first row starting above the top edge, to the last starting
        // above the bottom one
        let start = (row(visible.y).floor() as usize * self.columns).min(len);
        let end = (row(visible.y + visible.height).ceil() as usize * self.columns).min(len);
        start..end.max(start)
    }
}

/// A content check job: the files to read, in order, and the key that says
/// when it must be built again.
#[derive(Clone, Debug)]
struct TypeCheckWindow {
    key: u64,
    files: Arc<[crate::type_check::Unchecked]>,
}

/// The indices of `order` to check around `visible`: the visible positions,
/// then a screenful below them, then a screenful above, nearest first; and
/// the window of `order` they come from.
///
/// The window runs from the screenful above the one where the view starts to
/// three screenfuls past it: what is shown spans at most two, so at least one
/// more is below. Snapped that way, scrolling within a screenful leaves the
/// window as it was. A screenful is the visible count rounded up to a power
/// of two: a row scrolled half out of view changes that count by one, and
/// would otherwise move the window back and forth.
fn check_order(
    order: &[usize],
    visible: std::ops::Range<usize>,
) -> (Vec<usize>, std::ops::Range<usize>) {
    let len = order.len();
    let visible = visible.start.min(len)..visible.end.min(len);
    let page = visible.len().max(1).next_power_of_two();
    let first = visible.start / page;
    let start = first.saturating_sub(1) * page;
    let end = ((first + 3) * page).min(len);
    let indices = order[visible.clone()]
        .iter()
        .chain(&order[visible.end..end])
        .chain(order[start..visible.start].iter().rev())
        .copied()
        .collect();
    (indices, start..end)
}

/// File types and folder names whose icons still need resolving
#[derive(Clone, Debug, Default)]
pub struct IconWarmup {
    mimes: Vec<Mime>,
    folders: Vec<&'static str>,
}

impl IconWarmup {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mimes.is_empty() && self.folders.is_empty()
    }
}

/// Take any icons the cache has learned since these items were built.
///
/// Cheap: cache lookups only, never resolution. Called wherever items are
/// installed as well as when a worker reports back, because another tab may
/// have warmed the same types in between, in which case there is nothing to
/// warm and no report would arrive to swap the placeholders out.
///
/// Items showing a thumbnail or a desktop entry's own icon are left alone:
/// theirs did not come from these caches.
pub fn refresh_icons(items: &mut [Item], sizes: IconSizes) -> IconWarmup {
    let mut collect = IconCollect::default();
    for item in items {
        collect.add(item, sizes);
    }
    collect.finish()
}

/// [`refresh_icons`] one item at a time, for callers that know which items
/// changed and need not walk the whole listing.
#[derive(Default)]
pub struct IconCollect {
    mimes: std::collections::HashSet<Mime>,
    folders: std::collections::HashSet<&'static str>,
}

impl IconCollect {
    /// Takes any icons the cache has for `item`, and notes what it lacks.
    pub fn add(&mut self, item: &mut Item, sizes: IconSizes) {
        if item.mime == Mime::APPLICATION_X_DESKTOP
            || matches!(
                item.thumbnail_opt,
                Some(ItemThumbnail::Image(..) | ItemThumbnail::Svg(..))
            )
        {
            return;
        }
        // The same lookups decide both what to draw now and what still needs
        // resolving. Asking twice would leave a gap in which another tab's
        // worker fills the cache: the refresh would keep its placeholders and
        // the second question would answer "nothing missing", so no report
        // would ever come back to swap them out.
        let sizes_wanted = [sizes.grid(), sizes.list()];
        if item.metadata.is_dir() {
            let Some(path) = item.path_opt() else {
                return;
            };
            let name = folder_icon_name(path);
            let found = sizes_wanted.map(|size| try_folder_icon(name, size));
            if found.iter().any(Option::is_none) {
                self.folders.insert(name);
            }
            let [grid, list] = found;
            if let Some(handle) = grid {
                item.icon_handle_grid = handle;
            }
            if let Some(handle) = list {
                item.icon_handle_list = handle;
            }
        } else {
            // One look per size answers both questions, so the cache cannot
            // change between them and strand this item with a placeholder
            // nobody will come back to replace
            let found =
                sizes_wanted.map(|size| crate::mime_icon::lookup_mime_icon(&item.mime, size));
            if found
                .iter()
                .any(|state| matches!(state, crate::mime_icon::CachedIcon::Unknown))
            {
                self.mimes.insert(item.mime.clone());
            }
            let [grid, list] = found;
            if let crate::mime_icon::CachedIcon::Found(handle) = grid {
                item.icon_handle_grid = handle;
            }
            if let crate::mime_icon::CachedIcon::Found(handle) = list {
                item.icon_handle_list = handle;
            }
        }
    }

    /// What the items added still lack.
    #[must_use]
    pub fn finish(self) -> IconWarmup {
        IconWarmup {
            mimes: self.mimes.into_iter().collect(),
            folders: self.folders.into_iter().collect(),
        }
    }
}

/// Resolve everything in `warmup` into the icon caches. Each lookup searches
/// the icon theme on disk, so this belongs on a worker.
pub fn warm_icons(warmup: &IconWarmup, sizes: IconSizes) {
    let all = [sizes.grid(), sizes.list()];
    crate::mime_icon::warm_mime_icons(&warmup.mimes, &all);
    for name in &warmup.folders {
        for size in all {
            let _ = cached_folder_icon(name, size, false);
        }
    }
}

/// The icon for `mime`, or the generic one when it has not been resolved yet
fn cached_or_placeholder(mime: &Mime, size: u16) -> widget::icon::Handle {
    crate::mime_icon::try_mime_icon(mime, size)
        .unwrap_or_else(|| crate::mime_icon::placeholder_icon(size))
}

pub(crate) fn file_icons(
    path: &Path,
    mime: &Mime,
    sizes: IconSizes,
) -> (bool, widget::icon::Handle, widget::icon::Handle) {
    let is_desktop = *mime == Mime::APPLICATION_X_DESKTOP;
    match is_desktop.then(|| get_desktop_file_icon(path)).flatten() {
        Some(icon_name) => (
            true,
            desktop_icon_handle(&icon_name, sizes.grid()),
            desktop_icon_handle(&icon_name, sizes.list()),
        ),
        // Only what is already cached. Resolving an icon searches the icon
        // theme on disk and costs tens of milliseconds, and a directory of a
        // hundred files holds enough distinct types to add seconds to the
        // scan before anything can be drawn. The placeholder stands in until
        // `Message::IconsReady` swaps in the real one.
        None => (
            is_desktop,
            cached_or_placeholder(mime, sizes.grid()),
            cached_or_placeholder(mime, sizes.list()),
        ),
    }
}

/// Read an image's pixel size into its cache.
///
/// Only scans call this. They run on a worker, where a slow header read costs
/// a slower listing; item constructors do not, because items are also built
/// while handling input, filesystem notifications and search results, and an
/// image on a stalled mount would freeze the window. An item built outside a
/// scan keeps an empty cache and shows no size. A name-first scan skips it
/// too; there the content check fills the cache (`Item::take_check`).
pub fn fill_image_dimensions(item: &Item) {
    if !item.mime.is_media("image") {
        return;
    }
    // For a trashed item this is the copy inside the trash, not the path it
    // came from, which by now holds nothing or something else
    let Some(path) = item.path_opt() else {
        return;
    };
    let _ = item
        .image_dimensions
        .set(image::image_dimensions(path).ok());
}

#[cfg(feature = "gvfs")]
pub fn item_from_gvfs_info(path: PathBuf, file_info: gio::FileInfo, sizes: IconSizes) -> Item {
    let file_name = file_info
        .attribute_as_string(gio::FILE_ATTRIBUTE_STANDARD_NAME)
        .unwrap_or_default();
    let mtime = file_info.attribute_uint64(gio::FILE_ATTRIBUTE_TIME_MODIFIED);
    let remote = path.parent().is_none_or(gvfs_dir_is_remote);
    let is_dir = matches!(file_info.file_type(), gio::FileType::Directory);

    let size_opt = (!is_dir).then_some(file_info.size() as u64);

    let (is_desktop, mime, icon_handle_grid, icon_handle_list) = if is_dir {
        (
            false,
            Mime::INODE_DIRECTORY,
            folder_icon_or_placeholder(&path, sizes.grid()),
            folder_icon_or_placeholder(&path, sizes.list()),
        )
    } else {
        // ALWAYS assume we're remote for mime guessing here, since gvfs reading can be expensive
        // @todo - expose this as a config option?
        let mime = mime_for_path(&path, true);
        let (is_desktop, grid, list) = file_icons(&path, &mime, sizes);
        (is_desktop, mime, grid, list)
    };

    // Children are counted in the background by the tab's subscription
    let children_opt = None;
    let dir_size = if is_dir && !remote {
        DirSize::Calculating(Controller::default())
    } else {
        DirSize::NotDirectory
    };

    let display_name = display_name_for_file(&path, &file_info.display_name(), false, is_desktop);
    let hidden = file_name.starts_with('.');

    Item {
        name: file_name.into(),
        display_name,
        is_mount_point: false,
        metadata: ItemMetadata::GvfsPath {
            mtime,
            size_opt,
            children_opt,
            is_dir,
        },
        hidden,
        // Only the gvfs listing builds these, and it runs on a worker.
        // Remote paths are left alone: reading content over the network to
        // learn a pixel size is what the remote guard exists to avoid.
        details: MetadataState::Pending,
        details_epoch: 0,
        image_dimensions: OnceCell::from(
            (!remote && mime.is_media("image"))
                .then(|| image::image_dimensions(&path).ok())
                .flatten(),
        ),
        location_opt: Some(Location::Path(path)),
        mime,
        type_checked: true,
        icon_handle_grid,
        icon_handle_list,
        thumbnail_opt: if remote {
            Some(ItemThumbnail::NotImage)
        } else {
            None
        },
        button_id: widget::Id::unique(),
        pos_opt: Cell::new(None),
        rect_opt: Cell::new(None),
        dir_size,
        checksums: ChecksumState::default(),
    }
}

pub fn item_from_search_item(search_item: SearchItem, sizes: IconSizes) -> Item {
    match search_item {
        SearchItem::Path(path, name, metadata) => item_from_entry(path, name, metadata, sizes),
        SearchItem::Trash(entry, metadata) => item_from_trash_entry(entry, metadata, sizes),
    }
}

/// How a folder scan works out each file's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Typing {
    /// Read the head of each file, as everything that builds items always
    /// has.
    Content,
    /// Go by the name only and leave the reading to the content check
    /// (`crate::type_check`), so a folder on a slow drive is shown before its
    /// files are opened -- apart from `.desktop` launchers, which are still
    /// read for their icon and name.
    NameFirst,
}

#[must_use]
pub fn item_from_entry(
    path: PathBuf,
    name: String,
    metadata: fs::Metadata,
    sizes: IconSizes,
) -> Item {
    item_from_entry_typed(path, name, metadata, sizes, Typing::Content)
}

/// As [`item_from_entry`], typing the file as `typing` says.
fn item_from_entry_typed(
    path: PathBuf,
    name: String,
    metadata: fs::Metadata,
    sizes: IconSizes,
    typing: Typing,
) -> Item {
    let mut is_gvfs = false;

    let hidden = name.starts_with('.');

    let remote = match fs_kind(&metadata) {
        FsKind::Local => false,
        FsKind::Remote => true,
        #[cfg(feature = "gvfs")]
        FsKind::Gvfs => {
            is_gvfs = true;
            path.parent().is_none_or(gvfs_dir_is_remote)
        }
        #[cfg(not(feature = "gvfs"))]
        FsKind::Gvfs => {
            log::info!(
                "gvfs feature not enabled, info may be inaccurate for {}",
                path.display()
            );
            true
        }
    };

    // Left for the content check: a local file, when the caller asked for
    // names first. A remote file is always typed by name, as it was before.
    let by_name = typing == Typing::NameFirst && !remote && !metadata.is_dir();

    let (is_desktop, mime, icon_handle_grid, icon_handle_list) = if metadata.is_dir() {
        (
            false,
            Mime::INODE_DIRECTORY,
            folder_icon_or_placeholder(&path, sizes.grid()),
            folder_icon_or_placeholder(&path, sizes.list()),
        )
    } else {
        let mime = mime_for_path(&path, remote || by_name);
        let (is_desktop, grid, list) = file_icons(&path, &mime, sizes);
        (is_desktop, mime, grid, list)
    };

    // Children are counted in the background by the tab's subscription
    let children_opt = None;
    let dir_size = if metadata.is_dir() && !remote {
        DirSize::Calculating(Controller::default())
    } else {
        DirSize::NotDirectory
    };

    let display_name = display_name_for_file(&path, &name, is_gvfs, is_desktop);

    Item {
        name,
        display_name,
        is_mount_point: false,
        metadata: ItemMetadata::Path {
            metadata,
            children_opt,
        },
        hidden,
        image_dimensions: OnceCell::new(),
        details: MetadataState::Pending,
        details_epoch: 0,
        location_opt: Some(Location::Path(path)),
        mime,
        type_checked: !by_name,
        icon_handle_grid,
        icon_handle_list,
        thumbnail_opt: remote.then_some(ItemThumbnail::NotImage),
        button_id: widget::Id::unique(),
        pos_opt: Cell::new(None),
        rect_opt: Cell::new(None),
        dir_size,
        checksums: ChecksumState::default(),
    }
}

pub fn item_from_trash_entry(
    entry: TrashItem,
    metadata: TrashItemMetadata,
    sizes: IconSizes,
) -> Item {
    let name = entry.name.to_string_lossy().into_owned();
    let display_name = Item::display_name(&name);

    let trash_path = crate::trash::trash_item_path(&entry);
    let location = trash_path.clone().map(Location::Path);
    // With no record of where it came from, the type is read from its name
    // in the trash, which is all there is
    let original_path = entry.original_path().unwrap_or_else(|| {
        trash_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(&entry.name))
    });

    let (mime, icon_handle_grid, icon_handle_list) = match metadata.size {
        crate::trashing::TrashItemSize::Entries(_) => (
            Mime::INODE_DIRECTORY,
            folder_icon_or_placeholder(&original_path, sizes.grid()),
            folder_icon_or_placeholder(&original_path, sizes.list()),
        ),
        crate::trashing::TrashItemSize::Bytes(_) => {
            // This passes remote = true so it does not read from the original path
            let mime = mime_for_path(&original_path, true);
            // The launcher icon is read from the copy inside the trash
            let icon_path = trash_path.as_deref().unwrap_or(&original_path);
            let (_, grid, list) = file_icons(icon_path, &mime, sizes);
            (mime, grid, list)
        }
    };

    Item {
        name,
        display_name,
        is_mount_point: false,
        metadata: ItemMetadata::Trash { metadata, entry },
        hidden: false,
        location_opt: location,
        image_dimensions: OnceCell::new(),
        details: MetadataState::Pending,
        details_epoch: 0,
        mime,
        type_checked: true,
        icon_handle_grid,
        icon_handle_list,
        thumbnail_opt: Some(ItemThumbnail::NotImage),
        button_id: widget::Id::unique(),
        pos_opt: Cell::new(None),
        rect_opt: Cell::new(None),
        dir_size: DirSize::NotDirectory,
        checksums: ChecksumState::default(),
    }
}

fn item_from_trash_child(
    path: PathBuf,
    name: String,
    metadata: fs::Metadata,
    sizes: IconSizes,
) -> Option<Item> {
    // Inside an entry with no record of where it came from, nothing has one
    let original_parent = crate::trash::original_path_for_trash_child(&path)
        .and_then(|original| original.parent().map(Path::to_path_buf));
    let entry = crate::trashing::TrashItem {
        id: crate::trashing::TrashId::Child(path.clone()),
        name: std::ffi::OsString::from(&name),
        original_parent,
        time_deleted: 0,
    };
    let size = if metadata.is_dir() {
        crate::trashing::TrashItemSize::Entries(0)
    } else {
        crate::trashing::TrashItemSize::Bytes(metadata.len())
    };
    Some(item_from_trash_entry(
        entry,
        crate::trashing::TrashItemMetadata { size },
        sizes,
    ))
}

fn get_filename_from_path(path: &Path) -> Result<String, String> {
    Ok(match path.file_name() {
        Some(name_os) => name_os
            .to_str()
            .ok_or_else(|| {
                format!(
                    "failed to parse file name for {}: {name_os:?} is not valid UTF-8",
                    path.display()
                )
            })?
            .to_string(),
        None => fl!("filesystem"),
    })
}

pub fn item_from_path<P: Into<PathBuf>>(path: P, sizes: IconSizes) -> Result<Item, String> {
    let path = path.into();
    let name = get_filename_from_path(&path)?;
    let metadata = fs::metadata(&path)
        .map_err(|err| format!("failed to read metadata for {}: {}", path.display(), err))?;
    Ok(item_from_entry(path, name, metadata, sizes))
}

/// The folder's items, or an empty list if it could not be read (logged).
pub fn scan_path(tab_path: &PathBuf, sizes: IconSizes) -> Vec<Item> {
    try_scan_path(tab_path, sizes).unwrap_or_else(|err| {
        log::warn!("failed to read directory {}: {}", tab_path.display(), err);
        Vec::new()
    })
}

/// The folder's items, or why it could not be read, so a caller can tell an
/// unreadable folder from an empty one. Only the final `read_dir` counts: a
/// GVFS folder that gio fails to enumerate falls back to it.
pub fn try_scan_path(tab_path: &PathBuf, sizes: IconSizes) -> io::Result<Vec<Item>> {
    scan_folder(tab_path, sizes, Typing::Content)
}

/// As [`try_scan_path`], typing files as `typing` says.
fn scan_folder(tab_path: &PathBuf, sizes: IconSizes, typing: Typing) -> io::Result<Vec<Item>> {
    let mut items = Vec::new();
    let mut hidden_files = Box::from([]);
    let mut remote_scannable = false;

    #[cfg(feature = "gvfs")]
    {
        if let Ok(path_meta) = fs::metadata(tab_path)
            && fs_kind(&path_meta) == FsKind::Gvfs
        {
            let file = gio::File::for_path(tab_path);

            // gio crate expects a comma delimited string
            let attr_string = [
                gio::FILE_ATTRIBUTE_STANDARD_DISPLAY_NAME.as_str(),
                gio::FILE_ATTRIBUTE_TIME_MODIFIED.as_str(),
                gio::FILE_ATTRIBUTE_STANDARD_SIZE.as_str(),
                gio::FILE_ATTRIBUTE_STANDARD_TYPE.as_str(),
                gio::FILE_ATTRIBUTE_STANDARD_NAME.as_str(),
            ]
            .join(",");

            match gio::prelude::FileExt::enumerate_children(
                &file,
                attr_string.as_str(),
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            ) {
                Ok(res) => {
                    remote_scannable = true;
                    items = res
                        .filter_map(|file| {
                            let file = file.ok()?;
                            Some(item_from_gvfs_info(tab_path.join(file.name()), file, sizes))
                        })
                        .collect();
                }
                Err(err) => {
                    log::warn!(
                        "could not enumerate {} via gio: {}",
                        tab_path.display(),
                        err
                    );
                }
            }
        }
    }

    if !remote_scannable {
        let entries = fs::read_dir(tab_path)?;
        let trash = crate::trash::is_trash_path(tab_path);
        items = entries
            .filter_map(|entry_res| {
                let entry = entry_res
                    .inspect_err(|err| {
                        log::warn!("failed to read entry in {}: {}", tab_path.display(), err)
                    })
                    .ok()?;

                let path = entry.path();

                let name = entry
                    .file_name()
                    .into_string()
                    .inspect_err(|name_os| {
                        log::warn!(
                            "failed to parse entry at {}: {:?} is not valid UTF-8",
                            path.display(),
                            name_os
                        )
                    })
                    .ok()?;

                if name == ".hidden" && path.is_file() {
                    hidden_files = parse_hidden_file(&path);
                }

                // Fall back to the link itself when the target cannot
                // be read, so a broken symlink is still listed and can
                // be selected and deleted instead of being invisible
                let metadata = match fs::metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(err) => match fs::symlink_metadata(&path) {
                        Ok(metadata) => metadata,
                        Err(_) => {
                            log::warn!(
                                "failed to read metadata for entry at {}: {}",
                                path.display(),
                                err
                            );
                            return None;
                        }
                    },
                };

                let item = if trash {
                    item_from_trash_child(path, name, metadata, sizes)
                } else {
                    Some(item_from_entry_typed(path, name, metadata, sizes, typing))
                };
                // An unchecked file is left to the content check, which reads
                // its size along with its type.
                if let Some(item) = &item
                    && item.type_checked
                {
                    fill_image_dimensions(item);
                }
                item
            })
            .collect();
    }
    items.sort_unstable_by(|a, b| match (a.metadata.is_dir(), b.metadata.is_dir()) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => LANGUAGE_SORTER.compare(&a.display_name, &b.display_name),
    });
    for item in &mut items {
        if hidden_files.contains(&item.name) {
            item.hidden = true;
        }
    }
    Ok(items)
}

/// The eye that says whether a search descends into subfolders.
///
/// `view-reveal-symbolic` and `view-conceal-symbolic` are the standard names
/// for showing and hiding a password, drawn as an open and a shut eye, so a
/// theme that has them already has exactly this pair. The Cosmic theme has
/// neither, and the theme is the user's to choose, so copies are bundled and
/// used when the lookup comes back empty. The bundled ones are marked
/// symbolic, so they take the colour of the button they sit in.
pub fn search_scope_icon(recursive: bool) -> widget::icon::Handle {
    static REVEAL_SVG: &[u8] = include_bytes!("../res/icons/symbolic/view-reveal-symbolic.svg");
    static CONCEAL_SVG: &[u8] = include_bytes!("../res/icons/symbolic/view-conceal-symbolic.svg");

    let (name, bundled) = if recursive {
        ("view-reveal-symbolic", REVEAL_SVG)
    } else {
        ("view-conceal-symbolic", CONCEAL_SVG)
    };

    let theme = crate::ui::icon_theme::default();
    let themed = crate::ui::icon_theme::lookup_in(&theme, name, 16, 1, true);
    match themed {
        Some(path) => widget::icon::from_path(path),
        None => {
            let mut handle = widget::icon::from_svg_bytes(bundled);
            handle.symbolic = true;
            handle
        }
    }
}

/// Searches `search_location` for names matching `term`, handing each
/// match to `callback`, which returns whether to go on. `wanted` is asked
/// at every entry visited, matching or not, so that a search nobody waits
/// for any more stops even where nothing matches.
pub fn scan_search<F: Fn(SearchItem) -> bool + Sync>(
    search_location: &SearchLocation,
    term: &str,
    options: SearchOptions,
    wanted: &(dyn Fn() -> bool + Sync),
    callback: F,
) {
    if term.is_empty() {
        return;
    }

    let pattern = regex::escape(term);
    let regex = match regex::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
    {
        Ok(ok) => ok,
        Err(err) => {
            log::warn!("failed to parse regex {pattern:?}: {err}");
            return;
        }
    };

    match search_location {
        SearchLocation::Path(tab_path) => {
            let mut builder = ignore::WalkBuilder::new(tab_path);
            builder
                .standard_filters(false)
                .hidden(!options.show_hidden)
                .same_file_system(true);
            if !options.recursive {
                // The folder itself is depth 0 and what is in it is depth 1,
                // so this lists the folder and descends no further.
                builder.max_depth(Some(1));
            }
            builder.build_parallel().run(|| {
                Box::new(|entry_res| {
                    if !wanted() {
                        return ignore::WalkState::Quit;
                    }
                    let Ok(entry) = entry_res else {
                        // Skip invalid entries
                        return ignore::WalkState::Skip;
                    };

                    let Some(file_name) = entry.file_name().to_str() else {
                        // Skip anything with an invalid name
                        return ignore::WalkState::Skip;
                    };

                    if regex.is_match(file_name) {
                        let path = entry.path();

                        // What a link points at, as the folder listing
                        // reads it, the link itself only when broken: the
                        // permissions shown, and changed, are the target's
                        let metadata = match fs::metadata(path).or_else(|_| entry.metadata()) {
                            Ok(ok) => ok,
                            Err(err) => {
                                log::warn!(
                                    "failed to read metadata for entry at {}: {}",
                                    path.display(),
                                    err
                                );
                                return ignore::WalkState::Continue;
                            }
                        };

                        if !callback(SearchItem::Path(
                            path.to_path_buf(),
                            file_name.to_string(),
                            metadata,
                        )) {
                            return ignore::WalkState::Quit;
                        }
                    }

                    ignore::WalkState::Continue
                })
            });
        }
        SearchLocation::Recents => {
            let recent_files = match crate::recents::load() {
                Ok(recent_files) => recent_files,
                Err(err) => {
                    log::warn!("Error reading recent files: {err}");
                    return;
                }
            };

            for bookmark in recent_files.bookmarks() {
                if !wanted() {
                    break;
                }
                let path = uri_to_path(bookmark.href());
                if let Some(path) = path
                    && path.exists()
                {
                    let file_name = path.file_name();
                    if let Some(file_name) = file_name {
                        let file_name = file_name.to_string_lossy();
                        if regex.is_match(&file_name) {
                            match path.metadata() {
                                Ok(metadata) => {
                                    if !callback(SearchItem::Path(
                                        path.to_path_buf(),
                                        file_name.to_string(),
                                        metadata,
                                    )) {
                                        break;
                                    }
                                }
                                Err(err) => {
                                    log::warn!(
                                        "failed to read metadata for entry at {}: {}",
                                        path.display(),
                                        err
                                    );
                                }
                            };
                        }
                    }
                }
            }
        }
        SearchLocation::Trash => {
            Trash::scan_search(callback, &regex);
        }
    }
}

/// The path a recent file's URI is listed at: a local file's own, or a
/// remote one's GVFS mount.
pub(crate) fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let url = uri.parse::<url::Url>().ok()?;
    if url.scheme() == "file" {
        return url.to_file_path().ok();
    }
    // Anything else, such as a document on a share, is reachable through
    // its GVFS mount when it has one
    #[cfg(feature = "gvfs")]
    {
        gio::prelude::FileExt::path(&gio::File::for_uri(url.as_str()))
    }
    #[cfg(not(feature = "gvfs"))]
    {
        None
    }
}

pub fn has_recents() -> bool {
    crate::recents::load().is_ok_and(|recent_files| recent_files.bookmarks().next().is_some())
}

pub fn scan_recents(sizes: IconSizes) -> Vec<Item> {
    let recent_files = match crate::recents::load() {
        Ok(recent_files) => recent_files,
        Err(err) => {
            log::warn!("Error reading recent files: {err}");
            return Vec::new();
        }
    };
    let mut recents: Vec<_> = recent_files
        .bookmarks()
        .filter_map(|bookmark| {
            let path = uri_to_path(bookmark.href())?;
            let last_edit = jiff::Timestamp::try_from(bookmark.modified()?).ok()?;
            let last_visit = jiff::Timestamp::try_from(bookmark.visited()?).ok()?;

            if path.exists() {
                let file_name = path.file_name()?;
                let name = file_name.to_string_lossy().to_string();

                let metadata = match path.metadata() {
                    Ok(ok) => ok,
                    Err(err) => {
                        log::warn!(
                            "failed to read metadata for entry at {}: {}",
                            path.display(),
                            err
                        );
                        return None;
                    }
                };

                let item = item_from_entry(path, name, metadata, sizes);
                fill_image_dimensions(&item);
                Some((item, last_edit.min(last_visit)))
            } else {
                log::warn!("recent file path does not exist: {}", path.display());
                None
            }
        })
        .collect();

    recents.sort_by_key(|recent| Reverse(recent.1));

    recents.into_iter().take(50).map(|(item, _)| item).collect()
}

/// The network location's items, or an empty list if no mounter could read
/// it (each failure logged).
pub fn scan_network(uri: &str, sizes: IconSizes) -> Vec<Item> {
    try_scan_network(uri, sizes).unwrap_or_default()
}

/// The network location's items from the first mounter that reads it, or
/// the last mounter's error if none did (each failure logged), so a caller
/// can tell a read that failed or timed out from an empty location.
///
/// Blocking: it waits for the mounter, which may wait for a server.
pub fn try_scan_network(uri: &str, sizes: IconSizes) -> Result<Vec<Item>, String> {
    let mut last_err = None;
    for mounter in MOUNTERS.values() {
        match crate::ui::iced::futures::executor::block_on(mounter.network_scan(uri)) {
            Some(Ok(listing)) => {
                return Ok(listing
                    .entries
                    .into_iter()
                    .map(|entry| network_item(entry, listing.all_dirs, listing.remote, sizes))
                    .collect());
            }
            Some(Err(err)) => {
                log::warn!("failed to scan {uri:?}: {err}");
                last_err = Some(err);
            }
            None => {}
        }
    }
    Err(last_err.unwrap_or_else(|| "no mounter could read it".to_owned()))
}

/// The path of a mounter's `icon` at `size`: the first of its theme names
/// the theme has, else its image file.
fn mounter_icon_path(icon: &crate::mounter::IconRef, size: u16) -> Option<PathBuf> {
    icon.names
        .iter()
        .find_map(|name| widget::icon::from_name(name.as_str()).size(size).path())
        .or_else(|| icon.file.clone())
}

/// `entry` of a network location as the tab lists it. `all_dirs`: every
/// entry is a place to go into; `remote`: the location's folders are not
/// walked for their size.
fn network_item(
    entry: crate::mounter::NetworkEntry,
    all_dirs: bool,
    remote: bool,
    sizes: IconSizes,
) -> Item {
    let location = Location::Network(entry.uri, entry.display_name.clone(), entry.path);
    // Children are counted in the background by the tab's subscription
    #[cfg(feature = "gvfs")]
    let listed = ItemMetadata::GvfsPath {
        mtime: entry.mtime,
        size_opt: entry.size,
        children_opt: None,
        is_dir: entry.is_dir,
    };
    // No mounter lists anything without GVFS
    #[cfg(not(feature = "gvfs"))]
    let listed = ItemMetadata::SimpleDir { entries: 0 };
    let metadata = if all_dirs {
        ItemMetadata::SimpleDir { entries: 0 }
    } else {
        listed
    };
    // Local directories get their size summed in the background like any
    // other local folder; remote ones would cost a listing per entry
    let dir_size = if metadata.is_dir() && !remote && location.path_opt().is_some() {
        DirSize::Calculating(Controller::default())
    } else {
        DirSize::NotDirectory
    };
    let file_icon = |size| {
        mounter_icon_path(&entry.icon, size)
            .map(widget::icon::from_path)
            .unwrap_or(
                widget::icon::from_name(if metadata.is_dir() {
                    "folder"
                } else {
                    "text-x-generic"
                })
                .size(size)
                .handle(),
            )
    };
    // Directories keep the mime every directory item carries
    let mime = if metadata.is_dir() { None } else { entry.mime }.unwrap_or(Mime::INODE_DIRECTORY);
    let (icon_handle_grid, icon_handle_list) = (file_icon(sizes.grid()), file_icon(sizes.list()));
    Item {
        name: entry.name,
        is_mount_point: false,
        display_name: entry.display_name,
        metadata,
        hidden: entry.hidden,
        location_opt: Some(location),
        image_dimensions: OnceCell::new(),
        details: MetadataState::Pending,
        details_epoch: 0,
        mime,
        type_checked: true,
        icon_handle_grid,
        icon_handle_list,
        thumbnail_opt: Some(ItemThumbnail::NotImage),
        button_id: widget::Id::unique(),
        pos_opt: Cell::new(None),
        rect_opt: Cell::new(None),
        dir_size,
        checksums: ChecksumState::default(),
    }
}

#[derive(Clone, Debug)]
pub struct EditLocation {
    pub location: Location,
    pub completions: Option<Vec<(String, PathBuf)>>,
    /// The previous list, shown while this edit's own list is being looked
    /// up, so the dropdown does not vanish and unroll again on every
    /// keystroke. It is for show only: never chosen from, never submitted.
    pub stale: Option<Vec<(String, PathBuf)>>,
    pub selected: Option<usize>,
}

impl EditLocation {
    /// What this edit names, as far as can be told without leaving the process.
    ///
    /// A network location is not resolved here. Naming its directory means
    /// asking GVFS, which means waiting on a server that may be slow or gone;
    /// done from an input handler that wait is the whole interface stopping.
    /// [`Self::network_uri`] gives the URI to resolve through a task instead.
    pub fn resolve(&self) -> Option<Location> {
        if matches!(self.location, Location::Network(..)) {
            return None;
        }
        let Some(selected) = self.selected else {
            return Some(self.location.clone());
        };
        let completions = self.completions.as_ref()?;
        let completion = completions.get(selected)?;
        Some(
            self.location
                .with_path(completion.1.clone())
                .normalize_lexically(),
        )
    }

    /// The URI to resolve through a mounter, when this edit names a network
    /// location rather than a path.
    pub fn network_uri(&self) -> Option<&str> {
        match &self.location {
            Location::Network(uri, ..) => Some(uri),
            _ => None,
        }
    }

    /// The list the dropdown shows, and whether its rows can be chosen.
    ///
    /// A stale list is only ever a stand-in for one being looked up, so this
    /// edit's own list wins whenever there is one, even an empty one: an
    /// empty answer means there is nothing to offer.
    pub fn shown(&self) -> Option<(&[(String, PathBuf)], bool)> {
        let (list, choosable) = match (&self.completions, &self.stale) {
            (Some(completions), _) => (completions, true),
            (None, Some(stale)) => (stale, false),
            (None, None) => return None,
        };
        (!list.is_empty()).then_some((list.as_slice(), choosable))
    }

    pub fn select(&mut self, forwards: bool) {
        if let Some(completions) = &self.completions {
            if completions.is_empty() {
                self.selected = None;
            } else {
                let mut selected = if forwards {
                    self.selected.and_then(|x| x.checked_add(1)).unwrap_or(0)
                } else {
                    self.selected
                        .and_then(|x| x.checked_sub(1))
                        .unwrap_or(completions.len() - 1)
                };
                if selected >= completions.len() {
                    selected = 0;
                }
                self.selected = Some(selected);

                // Automatically resolve if there is only one completion
                if completions.len() == 1
                    && let Some(resolved) = self.resolve()
                {
                    self.location = resolved;
                    self.selected = None;
                }
            }
        } else {
            self.selected = None;
        }
    }
}

/// Whether a search keeps its top result selected, so that Enter in the
/// search field opens it. Results arrive newest first, a newer one taking the
/// top, and the selection follows the top until the user makes one of their
/// own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchSelect {
    Off,
    /// A search with nothing selected yet.
    Waiting,
    /// The top result, at this index, is selected for the search.
    Top(usize),
}

/// How a search is run.
///
/// A struct rather than two more positional fields on [`Location::Search`]:
/// they are both booleans, and next to each other they would be a standing
/// invitation to pass them the wrong way round.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SearchOptions {
    pub show_hidden: bool,
    /// Whether to descend into subfolders. Only meaningful for a path: trash
    /// and recent files are flat lists with no subfolders to descend into.
    pub recursive: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SearchLocation {
    Path(PathBuf),
    Recents,
    Trash,
}

impl std::fmt::Display for SearchLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::Recents => write!(f, "recents"),
            Self::Trash => write!(f, "trash"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum SearchItem {
    Path(PathBuf, String, fs::Metadata),
    Trash(TrashItem, TrashItemMetadata),
}

impl From<Location> for EditLocation {
    fn from(location: Location) -> Self {
        Self {
            location,
            completions: None,
            stale: None,
            selected: None,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Location {
    Network(String, String, Option<PathBuf>),
    Path(PathBuf),
    Recents,
    Search(SearchLocation, String, SearchOptions, Instant),
    Trash,
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(uri, ..) => write!(f, "{uri}"),
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::Recents => write!(f, "recents"),
            Self::Search(location, term, ..) => {
                write!(f, "search {} for {}", location, term)
            }
            Self::Trash => write!(f, "trash"),
        }
    }
}

#[cfg(test)]
impl Tab {
    /// [`Self::update`], with each step to a location it asks to resolve
    /// resolved at once, as the app's worker would, and its answer handled.
    pub fn update_resolved(&mut self, message: Message, modifiers: Modifiers) -> Vec<Command> {
        let mut done = Vec::new();
        let mut pending = self.update(message, modifiers);
        while !pending.is_empty() {
            let mut next = Vec::new();
            for command in pending {
                match command {
                    Command::ResolveLocation(request, location_request) => {
                        let resolved = resolve_location(&location_request);
                        next.extend(
                            self.update(Message::LocationResolved(request, resolved), modifiers),
                        );
                    }
                    other => done.push(other),
                }
            }
            pending = next;
        }
        done
    }
}

/// What a step to a file, rather than a folder, does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnFile {
    /// Go to its folder with it selected.
    Select,
    /// Open it, staying where the tab is.
    Open,
}

/// A step to a location, for [`resolve_location`] to work out.
#[derive(Clone, Debug)]
pub struct LocationRequest {
    pub location: Location,
    /// The history entry stepped to, for Back and Forward.
    pub history_i_opt: Option<usize>,
    /// Where to go instead if `location` is not there: the first completion
    /// of an address typed but not finished.
    pub fallback: Option<Location>,
    pub on_file: OnFile,
}

impl LocationRequest {
    fn new(location: Location, history_i_opt: Option<usize>) -> Self {
        Self {
            location,
            history_i_opt,
            fallback: None,
            on_file: OnFile::Select,
        }
    }
}

/// Where a step to a location goes, as the filesystem has it.
#[derive(Clone, Debug)]
pub struct ResolvedLocation {
    /// The location made canonical: a file's folder, for a file.
    pub location: Location,
    /// The file stepped to, to select in its folder.
    pub selected_paths: Option<Vec<PathBuf>>,
    /// Whether `location` is a folder that can be gone to.
    pub is_dir: bool,
    /// The history entry stepped to, for Back and Forward.
    pub history_i_opt: Option<usize>,
    /// A file to open instead of going anywhere.
    pub open_file: Option<PathBuf>,
}

/// What `request` means: its location's canonical form, or for a file,
/// opening it or its folder with it selected, as the request says. Asks the
/// filesystem, so it is for workers.
pub fn resolve_location(request: &LocationRequest) -> ResolvedLocation {
    let history_i_opt = request.history_i_opt;
    let mut location = &request.location;
    if let Some(fallback) = &request.fallback
        && location.path_opt().is_some_and(|path| !path.exists())
    {
        location = fallback;
    }
    if request.on_file == OnFile::Open
        && let Some(path) = location.path_opt()
        && !path.is_dir()
    {
        return ResolvedLocation {
            location: location.clone(),
            selected_paths: None,
            is_dir: false,
            history_i_opt,
            open_file: Some(path.clone()),
        };
    }
    let mut location = location.normalize();
    let mut selected_paths = None;
    if let Some(path) = location.path_opt()
        && !path.is_dir()
        && let Some(parent) = path.parent()
    {
        selected_paths = Some(vec![path.clone()]);
        location = location.with_path(parent.to_path_buf());
    }
    let is_dir = location.path_opt().is_none_or(|path| path.is_dir());
    ResolvedLocation {
        location,
        selected_paths,
        is_dir,
        history_i_opt,
        open_file: None,
    }
}

impl Location {
    /// The location in its canonical form: a network URI ending in a
    /// slash, and a path made absolute with its links resolved, ending in a
    /// slash if it is a folder.
    ///
    /// This asks the filesystem, which a mount that has stopped answering
    /// does not do, so it is for workers only; the event loop has
    /// [`Self::normalize_lexically`].
    pub fn normalize(&self) -> Self {
        if let Location::Network(uri, ..) = self {
            if !uri.ends_with('/') {
                let mut uri = uri.clone();
                uri.push('/');
                self.with_uri(uri)
            } else {
                self.clone()
            }
        } else if let Some(mut path) = self.path_opt().cloned() {
            // Canonicalize path, if possible
            if let Ok(canonical) = fs::canonicalize(&path) {
                path = canonical;
            }
            // Add trailing slash if location is a directory
            if path.is_dir() {
                path.push("");
            }
            self.with_path(path)
        } else {
            self.clone()
        }
    }

    /// As much of [`Self::normalize`] as needs no filesystem: a network URI
    /// ends in a slash, and a path is left as it is, to be made canonical by
    /// the rescan that reads it.
    pub fn normalize_lexically(&self) -> Self {
        match self {
            Self::Network(..) => self.normalize(),
            _ => self.clone(),
        }
    }

    /// The breadcrumbs up to home or the root, named as [`folder_name`] does
    /// for `look`.
    pub fn ancestors(&self, look: bool) -> Vec<(Self, String)> {
        self.path_opt().map_or_else(Default::default, |path| {
            path.ancestors()
                .scan(false, |found_home, ancestor| {
                    (!*found_home).then(|| {
                        let (name, is_home) = folder_name(ancestor, look);
                        *found_home = is_home;
                        (self.with_path(ancestor.to_path_buf()), name)
                    })
                })
                .collect()
        })
    }

    pub const fn path_opt(&self) -> Option<&PathBuf> {
        match self {
            Self::Path(path) => Some(path),
            Self::Search(SearchLocation::Path(path), ..) => Some(path),
            Self::Network(_, _, path) => path.as_ref(),
            _ => None,
        }
    }

    /// The folder an empty search of it, eye shut, lists in full: with no
    /// term there is nothing to narrow it down by, and it shows the folder as
    /// it is. An open eye searches the tree under it, which has no such
    /// answer, and stays empty.
    pub fn empty_search_folder(&self) -> Option<&PathBuf> {
        match self {
            Self::Search(SearchLocation::Path(path), term, options, _)
                if term.is_empty() && !options.recursive =>
            {
                Some(path)
            }
            _ => None,
        }
    }

    /// The folder this lists in full: a folder's own, or the one an empty
    /// search of it shows.
    fn listed_folder(&self) -> Option<&PathBuf> {
        match self {
            Self::Path(path) => Some(path),
            _ => self.empty_search_folder(),
        }
    }

    /// Whether `other` lists what this does: the same location, or a folder
    /// and the empty search of it.
    pub fn lists_same_as(&self, other: &Self) -> bool {
        self == other
            || self
                .listed_folder()
                .is_some_and(|folder| other.listed_folder() == Some(folder))
    }

    /// Where the listing's sort is kept: the folder's own, which an empty
    /// search of it shares, or none for a search, whose results are always
    /// newest first.
    pub fn sort_key(&self) -> Option<String> {
        match self {
            Self::Search(..) => self
                .empty_search_folder()
                .map(|path| Self::Path(path.clone()).normalize_lexically().to_string()),
            _ => Some(self.normalize_lexically().to_string()),
        }
    }

    pub(crate) fn into_path_opt(self) -> Option<PathBuf> {
        match self {
            Self::Path(path) => Some(path),
            Self::Search(SearchLocation::Path(path), ..) => Some(path),
            Self::Network(_, _, path) => path,
            _ => None,
        }
    }

    pub fn with_path(&self, path: PathBuf) -> Self {
        let path = Self::expand_tilde(path);
        match self {
            Self::Path(..) => Self::Path(path),
            Self::Search(SearchLocation::Path(_), term, options, time) => {
                Self::Search(SearchLocation::Path(path), term.clone(), *options, *time)
            }

            other => other.clone(),
        }
    }

    pub fn with_uri(&self, uri: String) -> Self {
        if let Self::Network(_, name, path) = self {
            Self::Network(uri, name.clone(), path.clone())
        } else {
            self.clone()
        }
    }

    pub fn scan(&self, sizes: IconSizes) -> (Option<Box<Item>>, Vec<Item>) {
        let (parent_item_opt, items, _) = self.try_scan(sizes, Typing::Content);
        (parent_item_opt, items)
    }

    /// As [`Self::scan`], typing a folder's files as `typing` says, and
    /// whether the read worked: false when a folder or network location could
    /// not be read, whose listing is then empty rather than truly empty. A
    /// search, the trash and recents always count as read.
    pub fn try_scan(
        &self,
        sizes: IconSizes,
        typing: Typing,
    ) -> (Option<Box<Item>>, Vec<Item>, bool) {
        let read_path = |path: &PathBuf| match scan_folder(path, sizes, typing) {
            Ok(items) => (items, true),
            Err(err) => {
                log::warn!("failed to read directory {}: {}", path.display(), err);
                (Vec::new(), false)
            }
        };
        let (items, read_ok) = match self {
            Self::Path(path) => read_path(path),
            Self::Search(..) => match self.empty_search_folder() {
                Some(path) => read_path(path),
                // Search is done incrementally
                None => (Vec::new(), true),
            },
            Self::Trash => (Trash::scan(sizes), true),
            Self::Recents => (scan_recents(sizes), true),
            Self::Network(uri, _, _) => match try_scan_network(uri, sizes) {
                Ok(items) => (items, true),
                Err(_) => (Vec::new(), false),
            },
        };
        let parent_item_opt = match self.path_opt() {
            Some(path) => match item_from_path(path, sizes) {
                Ok(item) => Some(Box::new(item)),
                Err(err) => {
                    log::warn!("failed to get item for {}: {}", path.display(), err);
                    None
                }
            },
            None => None,
        };
        (parent_item_opt, items, read_ok)
    }

    /// The tab's title, named as [`folder_name`] does for `look`.
    pub fn title(&self, look: bool) -> String {
        self.title_with(|path| folder_name(path, look).0)
    }

    /// As [`Self::title`], naming a folder with `folder`.
    fn title_with(&self, folder: impl FnOnce(&Path) -> String) -> String {
        match self {
            Self::Path(path) => folder(path),
            Self::Search(location, term, ..) => {
                let name = match location {
                    SearchLocation::Path(path) => folder(path),
                    SearchLocation::Trash => fl!("trash"),
                    SearchLocation::Recents => fl!("recents"),
                };

                fl!("search-title", term = term.as_str(), name = name)
            }
            Self::Trash => {
                fl!("trash")
            }
            Self::Recents => {
                fl!("recents")
            }
            Self::Network(display_name, ..) => display_name.clone(),
        }
    }

    /// Expand a path that starts with "~" with the
    /// user's home directory
    pub fn expand_tilde(path: PathBuf) -> PathBuf {
        let mut components = path.components();
        match components.next() {
            Some(path::Component::Normal(os_str)) if os_str == "~" => {
                if let Some(home) = crate::xdg_dirs::home() {
                    home.join(components.as_path())
                } else {
                    path
                }
            }
            _ => path,
        }
    }

    pub fn is_trash(&self) -> bool {
        matches!(
            self,
            Location::Trash | Location::Search(SearchLocation::Trash, ..)
        )
    }

    pub fn is_recents(&self) -> bool {
        matches!(
            self,
            Location::Recents | Location::Search(SearchLocation::Recents, ..)
        )
    }

    /// Returns true if this location supports paste operations (not Trash)
    pub fn supports_paste(&self) -> bool {
        matches!(
            self,
            Self::Path(..) | Self::Search(..) | Self::Recents | Self::Network(_, _, Some(_))
        )
    }
}

pub struct TaskWrapper(pub crate::ui::Task<Message>);

impl From<crate::ui::Task<Message>> for TaskWrapper {
    fn from(task: crate::ui::Task<Message>) -> Self {
        Self(task)
    }
}

impl fmt::Debug for TaskWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskWrapper").finish()
    }
}

#[derive(Debug)]
pub enum Command {
    Action(Action),
    /// Resolve these icons on a worker, then send [`Message::IconsReady`].
    /// Scanning and search both leave placeholders rather than waiting on the
    /// icon theme, which costs tens of milliseconds per icon.
    WarmIcons(IconWarmup, IconSizes),
    Surface(crate::ui::surface::Action<Message>),
    AddNetworkDrive,
    AddToSidebar(PathBuf),
    AutoScroll(Option<f32>),
    ChangeLocation(String, Location, Option<Vec<PathBuf>>),
    /// Ask a mounter what this URI names, off the event loop, and answer with
    /// [`Message::NetworkResolved`] carrying the same request number back.
    ResolveNetwork(u64, String),
    /// Ask the filesystem what a location to go to names, off the event
    /// loop, with [`resolve_location`], and answer with
    /// [`Message::LocationResolved`] carrying the same request number back.
    ResolveLocation(u64, LocationRequest),
    Delete(Vec<PathBuf>),
    /// Files were dropped on this tab: read the drag payload and move them into
    /// `to`, or copy them when the second field is set.
    DropFiles(PathBuf, bool),
    ClearRecents,
    EmptyTrash,
    ExecEntryAction(crate::desktop_entry::DesktopEntryData, usize),
    Iced(TaskWrapper),
    OpenFile(Vec<PathBuf>),
    OpenInNewTab(PathBuf),
    OpenInNewWindow(PathBuf),
    Preview(PreviewKind),
    RunContextAction(usize),
    SetOpenWith(Mime, String),
    /// A path's new mode, and the bits of it that changed: the rest is
    /// taken as the file has it when the change is made.
    SetPermissions(PathBuf, u32, u32),
    SetMultiplePermissions(Vec<(PathBuf, u32, u32)>),
    SetSort(String, HeadingOptions, bool),
    WindowDrag,
    WindowToggleMaximize,
}

#[derive(Clone, Debug)]
pub enum Message {
    AddNetworkDrive,
    /// What a mounter said a submitted network URI names, or `None` when it
    /// could not say, tagged with the request number it answers.
    NetworkResolved(u64, String, Option<Location>),
    /// What a location to go to names, tagged with the request number it
    /// answers.
    LocationResolved(u64, ResolvedLocation),
    /// Widen the running search to subfolders, or narrow it to this folder.
    SetSearchRecursive(bool),
    /// A previewed item has nothing from the disk yet: start reading it, once.
    /// Tagged with the incarnation and listing it was asked for.
    RequestDetails(PathBuf, u64, u64),
    /// What the disk said about a previewed item, tagged with the incarnation
    /// and listing it was asked about.
    ItemDetails(PathBuf, u64, u64, Result<Metadata, String>),
    /// The icon cache has been filled, so placeholders can be replaced
    IconsReady,
    /// What the content check read for files a name-first scan typed by
    /// name, tagged with the listing they were read for.
    TypesChecked(u64, Vec<crate::type_check::Checked>),
    /// How much a Trash listing holds, tagged with the listing it was
    /// worked out for.
    TrashSize(u64, TrashSize),
    AutoScroll(Option<f32>),
    Click(Option<usize>),
    DoubleClick(Option<usize>),
    ClickRelease(Option<usize>),
    Config(TabConfig),
    ContextAction(Action),
    RightClickBackground,
    Surface(crate::ui::surface::Action<Message>),
    LocationContextMenuIndex(Option<usize>),
    LocationMenuAction(LocationMenuAction),
    Drag(Option<Rectangle>),
    /// Start a Wayland drag carrying the selection, because the pointer moved
    /// away from a press on item `usize`.
    DragFiles(usize),
    /// A Wayland file drag moved over, was dropped on, or left this tab.
    Dnd(crate::mouse_area::DndDrag),
    /// The same, for the breadcrumb segment of the ancestor at `usize`, whose
    /// directory the drop would move the files into.
    DndAncestor(usize, PathBuf, crate::mouse_area::DndDrag),
    /// A file drag has been held over the same folder or crumb for
    /// [`crate::ui::dnd::HOVER_OPEN`], if the `u64` is still the current
    /// [`Tab::drag_hover_generation`].
    DragHoverElapsed(u64),
    DragEnd,
    /// The rectangle selection may report again; see [`DRAG_REARM`].
    DragRearm,
    EditLocation(Option<EditLocation>),
    EditLocationComplete(usize),
    EditLocationEnable,
    /// A press on a row of the stale list shown while the field's own list is
    /// looked up. Taken by the row so it neither falls through to the list
    /// underneath nor closes the field, and it chooses nothing.
    EditLocationStalePressed,
    EditLocationSubmit,
    EditLocationTab,
    OpenInNewTab(PathBuf),
    ClearRecents,
    EmptyTrash,
    ExecEntryAction(Option<PathBuf>, usize),
    Gallery(bool),
    GalleryPrevious,
    GalleryNext,
    GalleryToggle,
    GoNext,
    GoPrevious,
    ItemDown,
    ItemLeft,
    ItemPageDown,
    ItemPageUp,
    ItemRight,
    ItemUp,
    Location(Location),
    LocationUp,
    Open(Option<PathBuf>),
    Reload,
    RightClick(Option<Point>, Option<usize>),
    MiddleClick(usize),
    Resize(Rectangle),
    Scroll(Viewport),
    ScrollTab(f32),
    ScrollToFocused,
    /// Apply the scroll offset restored from history once the items are in
    ScrollRestore,
    SearchContext(Location, Handoff<SearchContext>),
    SearchReady(bool),
    SelectAll,
    SelectFirst,
    SelectLast,
    SetOpenWith(Mime, String),
    RunContextAction(usize),
    ShiftPermissions(Option<(PathBuf, u32)>, u32, u32),
    SetSort(HeadingOptions, bool),
    TabComplete(PathBuf, Vec<(String, PathBuf)>),
    /// A thumbnail of the file at the path, as last modified at the time.
    Thumbnail(PathBuf, Option<SystemTime>, ItemThumbnail),
    ToggleSort(HeadingOptions),
    WindowDrag,
    WindowToggleMaximize,
    ZoomIn,
    ZoomOut,
    HighlightDeactivate(usize),
    HighlightActivate(usize),
    DirectorySize(PathBuf, DirSize),
    /// A folder's entry count: where its item was when asked, its path, and
    /// the count.
    DirectoryChildren(usize, PathBuf, usize),
    Checksums(PathBuf, ChecksumState),
    CalculateChecksums(PathBuf),
    CopyChecksum(String),
    /// path, width, height, pixels, display_size, generation, and which
    /// file the pixels are of
    ImageDecoded(
        PathBuf,
        u32,
        u32,
        Vec<u8>,
        Option<(u32, u32)>,
        u64,
        crate::large_image::FileStamp,
    ),
    /// `path` was replaced while the gallery decoded it, as its decode
    /// numbered so.
    ImageDecodeChanged(PathBuf, u64),
    /// The listing was read again: the image the gallery shows may have
    /// been replaced.
    GalleryRecheck,
    /// The gallery could not decode `path`, as its decode numbered so.
    ImageDecodeFailed(PathBuf, u64),
    /// The gallery moved on before `path`'s decode, numbered so, had its
    /// turn.
    ImageDecodeSuperseded(PathBuf, u64),
}

impl Message {
    /// Whether handling this leaves alone everything the listing is sorted
    /// by: names, types, sizes, dates and which items there are. The ones
    /// that sometimes change it say so themselves, for the sort in use.
    /// Anything only shown or asked for -- menus, popups, the gallery, the
    /// path field, the settings, zoom, the window, a file dragged over the
    /// list -- keeps it: sorting a large folder again for a right-click or
    /// each step of a drag would hold the frames up. A setting the order
    /// does depend on, folders first, is in its key; a drop changes the
    /// listing only through the rescan that follows it.
    fn keeps_order(&self) -> bool {
        matches!(
            self,
            Self::AutoScroll(_)
                | Self::CalculateChecksums(_)
                | Self::Checksums(..)
                | Self::Click(_)
                | Self::ClickRelease(_)
                | Self::Config(_)
                | Self::DirectoryChildren(..)
                | Self::DirectorySize(..)
                | Self::Dnd(_)
                | Self::DndAncestor(..)
                | Self::DoubleClick(_)
                | Self::Drag(_)
                | Self::DragEnd
                | Self::DragRearm
                | Self::DragFiles(_)
                | Self::DragHoverElapsed(_)
                | Self::EditLocation(_)
                | Self::EditLocationComplete(_)
                | Self::EditLocationEnable
                | Self::EditLocationStalePressed
                | Self::EditLocationTab
                | Self::Gallery(_)
                | Self::GalleryNext
                | Self::GalleryPrevious
                | Self::GalleryToggle
                | Self::HighlightActivate(_)
                | Self::HighlightDeactivate(_)
                | Self::IconsReady
                | Self::ImageDecoded(..)
                | Self::ItemDown
                | Self::ItemLeft
                | Self::ItemPageDown
                | Self::ItemPageUp
                | Self::ItemRight
                | Self::ItemUp
                | Self::LocationContextMenuIndex(_)
                | Self::MiddleClick(_)
                | Self::RequestDetails(..)
                | Self::Resize(_)
                | Self::RightClick(..)
                | Self::RightClickBackground
                | Self::Scroll(_)
                | Self::ScrollRestore
                | Self::ScrollTab(_)
                | Self::ScrollToFocused
                | Self::SelectAll
                | Self::SelectFirst
                | Self::SelectLast
                | Self::Surface(_)
                | Self::TabComplete(..)
                | Self::Thumbnail(..)
                | Self::TrashSize(..)
                | Self::TypesChecked(..)
                | Self::WindowDrag
                | Self::WindowToggleMaximize
                | Self::ZoomIn
                | Self::ZoomOut
        )
    }
}

impl Message {
    /// Whether handling this leaves the kept selection right: it changes
    /// nothing selected, or only through methods that keep it themselves,
    /// as the arrows do through [`Tab::select_position`].
    fn keeps_selection(&self) -> bool {
        matches!(
            self,
            Self::Checksums(..)
                | Self::ItemDown
                | Self::ItemLeft
                | Self::ItemPageDown
                | Self::ItemPageUp
                | Self::ItemRight
                | Self::ItemUp
                | Self::DirectoryChildren(..)
                | Self::DirectorySize(..)
                | Self::DragRearm
                | Self::HighlightActivate(_)
                | Self::HighlightDeactivate(_)
                | Self::IconsReady
                | Self::ImageDecoded(..)
                | Self::Resize(_)
                | Self::Scroll(_)
                | Self::ScrollRestore
                | Self::ScrollTab(_)
                | Self::Thumbnail(..)
                | Self::TrashSize(..)
                | Self::TypesChecked(..)
        )
    }
}

/// What a kept order was sorted for: the sort options, the listing and its
/// length.
type SortKey = ((HeadingOptions, bool, bool), u64, usize);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LocationMenuAction {
    OpenInNewTab(usize),
    OpenInNewWindow(usize),
    Preview(usize),
    AddToSidebar(usize),
}

impl MenuAction for LocationMenuAction {
    type Message = Message;

    fn message(&self) -> Self::Message {
        Message::LocationMenuAction(*self)
    }
}

#[derive(Clone, Debug)]
pub enum DirSize {
    Calculating(Controller),
    Directory(u64),
    NotDirectory,
    Error(String),
}

/// Checksums computed for a file. Only SHA256 is currently exposed; see
/// [`calculate_checksums`] for how to add more.
#[derive(Clone, Debug, Default)]
pub struct FileChecksums {
    pub sha256: String,
}

/// State of checksum computation for a file.
#[derive(Clone, Debug, Default)]
pub enum ChecksumState {
    #[default]
    NotCalculated,
    Calculating,
    Calculated(FileChecksums),
    Error(String),
}

#[derive(Clone, Debug)]
pub enum ItemMetadata {
    Path {
        metadata: Metadata,
        children_opt: Option<usize>,
    },
    Trash {
        metadata: crate::trashing::TrashItemMetadata,
        entry: crate::trashing::TrashItem,
    },
    SimpleDir {
        entries: u64,
    },
    SimpleFile {
        size: u64,
    },
    #[cfg(feature = "gvfs")]
    GvfsPath {
        mtime: u64,
        size_opt: Option<u64>,
        children_opt: Option<usize>,
        is_dir: bool,
    },
}

impl ItemMetadata {
    pub fn is_dir(&self) -> bool {
        match self {
            Self::Path { metadata, .. } => metadata.is_dir(),
            Self::Trash { metadata, .. } => match metadata.size {
                crate::trashing::TrashItemSize::Entries(_) => true,
                crate::trashing::TrashItemSize::Bytes(_) => false,
            },
            Self::SimpleDir { .. } => true,
            Self::SimpleFile { .. } => false,
            #[cfg(feature = "gvfs")]
            Self::GvfsPath { is_dir, .. } => *is_dir,
        }
    }

    pub fn modified(&self) -> Option<SystemTime> {
        match self {
            Self::Path { metadata, .. } => metadata.modified().ok(),
            #[cfg(feature = "gvfs")]
            Self::GvfsPath { mtime, .. } => {
                SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(*mtime))
            }
            _ => None,
        }
    }

    pub fn file_size(&self) -> Option<u64> {
        match self {
            Self::Path { metadata, .. } => (!metadata.is_dir()).then_some(metadata.len()),
            Self::Trash { metadata, .. } => match metadata.size {
                TrashItemSize::Bytes(size) => Some(size),
                TrashItemSize::Entries(_) => None,
            },
            #[cfg(feature = "gvfs")]
            Self::GvfsPath { size_opt, .. } => *size_opt,
            _ => None,
        }
    }

    pub fn children_count(&self) -> Option<&usize> {
        match &self {
            ItemMetadata::Path { children_opt, .. } => children_opt.as_ref(),
            #[cfg(feature = "gvfs")]
            ItemMetadata::GvfsPath { children_opt, .. } => children_opt.as_ref(),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum ItemThumbnail {
    NotImage,
    Image(widget::image::Handle, Option<(u32, u32)>),
    Svg(widget::svg::Handle),
    Text(widget::text_editor::Content<crate::ui::Renderer>),
}

impl Clone for ItemThumbnail {
    fn clone(&self) -> Self {
        match self {
            Self::NotImage => Self::NotImage,
            Self::Image(handle, size_opt) => Self::Image(handle.clone(), *size_opt),
            Self::Svg(handle) => Self::Svg(handle.clone()),
            // Content cannot be cloned simply
            Self::Text(content) => {
                Self::Text(widget::text_editor::Content::with_text(&content.text()))
            }
        }
    }
}

impl ItemThumbnail {
    pub fn new(
        path: &Path,
        metadata: ItemMetadata,
        mime: Mime,
        mut thumbnail_size: u32,
        max_mem: u64,
        jobs: usize,
        max_size_mb: u64,
    ) -> Self {
        let thumbnail_cacher =
            ThumbnailCacher::new(path, thumbnail_cacher::size_for(thumbnail_size));
        match thumbnail_cacher.as_ref() {
            Ok(cache) => match cache.get_cached_thumbnail() {
                CachedThumbnail::Valid((thumbnail_path, size)) => {
                    // Check original image dimensions even when loading cached thumbnail
                    // This prevents trying to load huge images in preview mode
                    let original_dims = match image::image_dimensions(path) {
                        Ok((width, height)) => Some((width, height)),
                        Err(_) => size.map(|s| (s.max_dimension(), s.max_dimension())),
                    };

                    return Self::Image(
                        widget::image::Handle::from_path(thumbnail_path),
                        original_dims,
                    );
                }
                CachedThumbnail::Failed => {
                    if !mime.is_media("image") {
                        return Self::NotImage;
                    }
                }
                CachedThumbnail::RequiresUpdate(size) => {
                    thumbnail_size = size.max_dimension();
                }
            },
            Err(err) => {
                log::warn!(
                    "failed to create ThumbnailCache for {}: {}",
                    path.display(),
                    err
                );
            }
        }

        let size = metadata.file_size().unwrap_or_default();
        let check_size = |thumbnailer: &str, max_size| {
            if size <= max_size {
                true
            } else {
                log::warn!(
                    "skipping internal {} thumbnailer for {}: file size {} is larger than {}",
                    thumbnailer,
                    path.display(),
                    format_size(size),
                    format_size(max_size)
                );
                false
            }
        };

        let mut tried_supported_file = false;
        // First try built-in image thumbnailer
        if mime.is_media("image") && check_size("image", max_size_mb * 1000 * 1000) {
            // Check if image dimensions would exceed available memory budget
            // The GPU tiling system can handle large images, but we still need to decode them first
            let dimensions_ok = match image::image_dimensions(path) {
                Ok((width, height)) => {
                    if exceeds_memory_limit(width, height, max_mem) {
                        log::warn!(
                            "skipping thumbnail generation for {}: {}x{} image would exceed {}MB memory budget",
                            path.display(),
                            width,
                            height,
                            max_mem
                        );
                        false
                    } else {
                        if should_use_tiling(width, height) {
                            log::info!(
                                "Large image {}x{} detected, will use GPU tiling for display",
                                width,
                                height
                            );
                        }
                        true
                    }
                }
                Err(err) => {
                    log::debug!(
                        "failed to read dimensions for {}: {}, will try decoding",
                        path.display(),
                        err
                    );
                    true // If we can't read dimensions, try anyway
                }
            };

            if !dimensions_ok {
                // Skip this image entirely since it is too large to safely decode
                return Self::NotImage;
            }

            tried_supported_file = true;
            let dyn_img = match image::ImageReader::open(path)
                .and_then(image::ImageReader::with_guessed_format)
            {
                Ok(mut reader) => {
                    let mut limits = image::Limits::default();
                    let max_ram = max_mem * 1000 * 1000 / jobs as u64;
                    limits.max_alloc = Some(max_ram);
                    reader.limits(limits);
                    match reader.decode() {
                        Ok(reader) => Some(reader),
                        Err(err) => {
                            log::warn!("failed to decode {}: {}", path.display(), err);
                            None
                        }
                    }
                }
                Err(err) => {
                    log::warn!("failed to read {}: {}", path.display(), err);
                    None
                }
            };

            if let Some(dyn_img) = dyn_img {
                let (img_width, img_height) = (dyn_img.width(), dyn_img.height());

                if let Ok(cacher) = thumbnail_cacher.as_ref() {
                    match cacher.update_with_image(dyn_img) {
                        Ok(thumb_path) => {
                            return Self::Image(
                                widget::image::Handle::from_path(thumb_path),
                                Some((img_width, img_height)),
                            );
                        }
                        Err(err) => {
                            log::warn!("cacher failed to decode {}: {}", path.display(), err);
                        }
                    }
                } else {
                    // Fallback for when thumbnail cacher isn't available.
                    let thumbnail = dyn_img
                        .thumbnail(thumbnail_size, thumbnail_size)
                        .into_rgba8();
                    return Self::Image(
                        widget::image::Handle::from_rgba(
                            thumbnail.width(),
                            thumbnail.height(),
                            thumbnail.into_raw(),
                        ),
                        Some((img_width, img_height)),
                    );
                }
            }
        }

        // Try external thumbnailers.
        if let Some((item_thumbnail, temp_file)) =
            Self::generate_thumbnail_external(path, &mime, thumbnail_size)
        {
            if let Ok(cache) = thumbnail_cacher
                && let Err(err) = cache.update_with_temp_file(temp_file)
            {
                log::warn!("failed to update cache for {}: {}", path.display(), err);
            }
            return item_thumbnail;
        }

        tried_supported_file = tried_supported_file || !thumbnailers(&mime).is_empty();

        // Try internal thumbnailers that don't get cached.
        if mime.is_media("image")
            && mime
                .subtype()
                .split('+')
                .next()
                .is_some_and(|subtype| subtype.eq_ignore_ascii_case("svg"))
            && check_size("svg", 8 * 1000 * 1000)
        {
            tried_supported_file = true;
            // Try built-in svg thumbnailer
            match fs::read(path) {
                Ok(data) => {
                    return Self::Svg(widget::svg::Handle::from_memory(data));
                }
                Err(err) => {
                    log::warn!("failed to read {}: {}", path.display(), err);
                }
            }
        } else if mime.is_media("text") && check_size("text", TEXT_PREVIEW_MAX_FILE_BYTES) {
            tried_supported_file = true;
            if size > 0 {
                // Reuse size from metadata above; cap allocation and read
                let read_cap = (size.min(TEXT_PREVIEW_MAX_BYTES as u64)) as usize;
                let mut buf = vec![0u8; read_cap];
                match File::open(path).and_then(|f| {
                    let n = Read::read(&mut f.take(read_cap as u64), &mut buf)?;
                    buf.truncate(n);
                    Ok(())
                }) {
                    Ok(()) => {
                        let text = match std::str::from_utf8(&buf) {
                            Ok(s) => s,
                            Err(e) => {
                                // Use only the valid UTF-8 prefix (slice is guaranteed valid by valid_up_to())
                                std::str::from_utf8(&buf[..e.valid_up_to()]).unwrap_or("")
                            }
                        };
                        // Only the lines the pane can show are shaped: the rest
                        // would cost the event loop the font system lock for
                        // nothing.
                        let preview = preview_text(text);
                        if !preview.is_empty() {
                            return Self::Text(widget::text_editor::Content::with_text(&preview));
                        }
                    }
                    Err(err) => {
                        log::warn!("failed to read {}: {}", path.display(), err);
                    }
                }
            }
            // size == 0: empty file or unknown size; skip read and allocation
        }

        // If we weren't able to create a thumbnail, but we should have
        // been able to, create a fail marker so that it isn't tried the
        // next time.
        if let Ok(cacher) = thumbnail_cacher
            && tried_supported_file
            && let Err(err) = cacher.create_fail_marker()
        {
            log::warn!(
                "failed to create thumbnail fail marker for {}: {}",
                path.display(),
                err
            );
        }

        Self::NotImage
    }

    fn generate_thumbnail_external(
        path: &Path,
        mime: &Mime,
        thumbnail_size: u32,
    ) -> Option<(Self, NamedTempFile)> {
        // Try external thumbnailers
        for thumbnailer in thumbnailers(mime) {
            let is_evince = thumbnailer.exec().starts_with("evince-thumbnailer ");
            let prefix = if is_evince {
                "gnome-desktop-"
            } else {
                "earth-files-"
            };

            // In the system's temporary folder: the cache installs a copy of
            // it, with the standard's keys, so where it is written does not
            // matter, and evince-thumbnailer's AppArmor profile allows only
            // there (and only with its own prefix)
            let file = match tempfile::Builder::new().prefix(prefix).tempfile() {
                Ok(ok) => ok,
                Err(err) => {
                    log::warn!(
                        "failed to create temporary file for thumbnail of {}: {}",
                        path.display(),
                        err
                    );
                    continue;
                }
            };

            let Some(mut command) =
                crate::thumbnailer::command(thumbnailer, path, file.path(), thumbnail_size)
            else {
                continue;
            };
            // Thumbnailers such as ffmpegthumbnailer print harmless notices
            // for every file, so their stderr goes to the debug log rather
            // than straight to our terminal; with debug logging off it is
            // dropped without the cost of a reader thread.
            let log_stderr = log::log_enabled!(log::Level::Debug);
            command.stderr(if log_stderr {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            });
            let status = command.spawn().and_then(|mut child| {
                if log_stderr {
                    crate::child::log_stderr(
                        &mut child,
                        format!("thumbnailer for {}", path.display()),
                    );
                }
                crate::child::wait_with_timeout(&mut child, THUMBNAILER_TIMEOUT)
            });
            match status {
                Ok(None) => {
                    log::warn!(
                        "killed {:?} for {}: still running after {:?}",
                        thumbnailer,
                        path.display(),
                        THUMBNAILER_TIMEOUT
                    );
                }
                Ok(Some(status)) => {
                    if status.success() {
                        match image::ImageReader::open(file.path())
                            .and_then(ImageReader::with_guessed_format)
                        {
                            Ok(reader) => match reader.decode().map(DynamicImage::into_rgba8) {
                                Ok(image) => {
                                    return Some((
                                        Self::Image(
                                            widget::image::Handle::from_rgba(
                                                image.width(),
                                                image.height(),
                                                image.into_raw(),
                                            ),
                                            None,
                                        ),
                                        file,
                                    ));
                                }
                                Err(err) => {
                                    log::warn!("failed to decode {}: {}", path.display(), err);
                                }
                            },
                            Err(err) => {
                                log::warn!("failed to read {}: {}", path.display(), err);
                            }
                        }
                    } else {
                        log::warn!(
                            "failed to run {:?} for {}: {}",
                            thumbnailer,
                            path.display(),
                            status
                        );
                    }
                }
                Err(err) => {
                    log::warn!(
                        "failed to run {thumbnailer:?} for {}: {}",
                        path.display(),
                        err
                    );
                }
            }
        }

        None
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub is_mount_point: bool,
    pub display_name: String,
    pub metadata: ItemMetadata,
    pub hidden: bool,
    pub location_opt: Option<Location>,
    pub mime: Mime,
    /// Whether [`Self::mime`] is final. False only for a file that a
    /// [`Typing::NameFirst`] scan typed by its name, until the content check
    /// has read the file.
    pub type_checked: bool,
    /// Pixel size of an image, filled when it is first needed.
    ///
    /// The scan paths that already know it fill it in up front; a name-first
    /// scan leaves it to the content check, which reads the file anyway. The
    /// rest leave it empty, because items are also built while handling input
    /// and filesystem notifications, where opening a file to read its header
    /// would block the interface. Nothing fills it on demand; the details
    /// pane shows a size only when one is already here.
    pub image_dimensions: OnceCell<Option<(u32, u32)>>,
    /// What the disk says about an item whose [`Self::metadata`] does not
    /// carry a [`Metadata`] of its own -- trashed and GVFS items. Filled in
    /// the background while the item is previewed, never from a view.
    pub details: MetadataState,
    /// Which incarnation of this item `details` describes. Bumped when the
    /// item is rebuilt after a change on disk, so the request for the new
    /// incarnation is a new request and an answer to the old one is ignored.
    pub details_epoch: u64,
    pub icon_handle_grid: widget::icon::Handle,
    pub icon_handle_list: widget::icon::Handle,
    pub thumbnail_opt: Option<ItemThumbnail>,
    pub button_id: widget::Id,
    pub pos_opt: Cell<Option<(usize, usize)>>,
    pub rect_opt: Cell<Option<Rectangle>>,

    pub dir_size: DirSize,
    pub checksums: ChecksumState,
}

impl Item {
    /// Take on `fresh`, which was read from this item's path after its
    /// contents changed, so the mime, icons and thumbnail follow the new data.
    /// Selection and layout state the view relies on is kept.
    ///
    /// Reading `fresh` is the expensive half -- metadata, mime and icons -- and
    /// it is deliberately not done here: a burst of filesystem notifications
    /// names many paths at once, and doing that work between two frames is
    /// what the burst would otherwise cost the interface. See
    /// [`item_from_path`], which callers run on a worker.
    pub fn adopt(&mut self, fresh: Item) {
        // A new incarnation: whatever the disk said about the old one no
        // longer applies, and an answer still on its way for it must not land
        // on this one.
        let details_epoch = self.details_epoch.wrapping_add(1);
        *self = Item {
            details_epoch,
            button_id: self.button_id.clone(),
            pos_opt: Cell::new(self.pos_opt.get()),
            rect_opt: Cell::new(self.rect_opt.get()),
            ..fresh
        };
    }

    /// Take on what the content check found reading this item's file.
    /// Returns whether the type changed, in which case the icons were rebuilt
    /// for the new one and still need refreshing from the icon cache.
    pub fn take_check(&mut self, checked: crate::type_check::Checked) -> bool {
        self.type_checked = true;
        self.image_dimensions = OnceCell::from(checked.dims);
        let Some(retyped) = checked.retyped else {
            return false;
        };
        self.mime = retyped.mime;
        if let Some(display_name) = retyped.display_name {
            self.display_name = display_name;
        }
        self.icon_handle_grid = retyped.icon_handle_grid;
        self.icon_handle_list = retyped.icon_handle_list;
        // Thumbnails wait for a checked type, so there is none yet; cleared
        // anyway so one could never outlive the type it was made for
        self.thumbnail_opt = None;
        true
    }

    /// Whether a thumbnail should be made now: there is none yet, and the
    /// type is final. The type picks the thumbnailer, and an image's size
    /// picks its memory budget, so neither may be a guess from the name.
    fn wants_thumbnail(&self) -> bool {
        self.thumbnail_opt.is_none() && self.type_checked
    }

    fn display_name(name: &str) -> String {
        // In order to wrap at periods and underscores, add a zero width space after each one
        name.replace('.', ".\u{200B}").replace('_', "_\u{200B}")
    }

    /// Text widget for a filename in grid/icon view: word-or-glyph wrapping, middle-ellipsized to 3 lines.
    fn grid_display_name<'a>(
        name: impl Into<Cow<'a, str>> + 'a,
    ) -> widget::Ellipsize<'a, crate::ui::Theme, crate::ui::Renderer> {
        widget::ellipsize::body(name, widget::EllipsizeMode::Middle(3))
            .wrapping(text::Wrapping::WordOrGlyph)
            .align_x(text::Alignment::Center)
    }

    /// Text widget for a filename in list view: word-or-glyph wrapping, middle-ellipsized to 1 line.
    fn list_display_name<'a>(
        name: impl Into<Cow<'a, str>> + 'a,
    ) -> widget::Ellipsize<'a, crate::ui::Theme, crate::ui::Renderer> {
        widget::ellipsize::body(name, widget::EllipsizeMode::Middle(1))
            .wrapping(text::Wrapping::WordOrGlyph)
    }

    /// Whether the item is drawn faded: one that would be hidden, shown
    /// because hidden files are, unless it is `selected`.
    fn faded(&self, selected: bool) -> bool {
        self.hidden && !selected
    }

    pub fn path_opt(&self) -> Option<&PathBuf> {
        self.location_opt.as_ref()?.path_opt()
    }

    pub fn can_gallery(&self) -> bool {
        self.mime.is_media("image") || self.mime.is_media("text")
    }

    /// The full [`Metadata`] for this item, if it is known. Never touches the
    /// disk: a local item carries it from the scan, and any other item has it
    /// only once a background read has answered.
    pub fn file_metadata(&self) -> Option<Metadata> {
        match (&self.metadata, &self.details) {
            (ItemMetadata::Path { metadata, .. }, _) => Some(metadata.clone()),
            (_, MetadataState::Ready(metadata)) => Some(metadata.clone()),
            _ => None,
        }
    }

    fn preview(&self) -> Element<'_, Message> {
        let spacing = spacing();
        // This loads the image only if thumbnailing worked
        let icon = widget::icon::icon(self.icon_handle_grid.clone())
            .content_fit(ContentFit::Contain)
            .size(IconSizes::default().grid())
            .into();
        match self
            .thumbnail_opt
            .as_ref()
            .unwrap_or(&ItemThumbnail::NotImage)
        {
            ItemThumbnail::NotImage => icon,
            ItemThumbnail::Image(handle, _original_dims) => {
                // Preview pane: ALWAYS show thumbnail for instant, responsive UI
                // Full resolution loading happens in gallery mode
                widget::image(handle.clone()).into()
            }
            ItemThumbnail::Svg(handle) => widget::svg(handle.clone()).into(),
            ItemThumbnail::Text(content) => widget::text_editor::text_editor(content)
                .style(text_editor_class)
                .width(THUMBNAIL_SIZE as f32)
                .height(Length::Fixed(THUMBNAIL_SIZE as f32))
                .padding(spacing.space_xxs)
                .into(),
        }
    }

    pub fn preview_actions(&self) -> Element<'_, Message> {
        let mut row = widget::Row::with_capacity(3)
            .align_y(Alignment::Center)
            .spacing((spacing().space_xxs).to_pixels())
            .push(
                widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                    .on_press(Message::ItemLeft),
            )
            .push(
                widget::button::icon(widget::icon::from_name("go-next-symbolic"))
                    .on_press(Message::ItemRight),
            );
        if self.can_gallery()
            && let Some(_path) = self.path_opt()
        {
            row = row.push(
                widget::button::icon(widget::icon::from_name("view-fullscreen-symbolic"))
                    .on_press(Message::Gallery(true)),
            );
        }
        row.into()
    }

    /// `selected`: whether it is selected in its listing, which offers to
    /// open it.
    pub fn preview_view<'a>(
        &'a self,
        mime_app_cache_opt: Option<&'a mime_app::MimeAppCache>,
        selected: bool,
    ) -> Element<'a, Message> {
        let Spacing {
            space_xxxs,
            space_m,
            ..
        } = spacing();

        let mut column = widget::Column::with_capacity(4).spacing(space_m.to_pixels());

        column = column.push(
            widget::container(self.preview())
                .center_x(Length::Fill)
                .max_height(THUMBNAIL_SIZE as f32),
        );

        let mut details = widget::Column::with_capacity(8).spacing(space_xxxs.to_pixels());
        details = details.push(widget::selectable_text::heading(self.name.clone()));
        details = details.push(widget::text::body(fl!(
            "type",
            mime = self.mime.to_string()
        )));
        let mut settings = Vec::new();
        // Choosing here sets the default application for the type, so it is
        // offered only once the type is known rather than guessed from the
        // name; it appears once the check has read the file.
        if self.type_checked
            && let Some(mime_app_cache) = mime_app_cache_opt
        {
            let mime_apps = mime_app_cache.get_apps_for_mime(&self.mime, false);
            if !mime_apps.is_empty() {
                let (names, icons) = mime_apps
                    .iter()
                    .map(|(app, _)| (Cow::Owned(app.name.clone()), app.icon()))
                    .collect::<(Vec<_>, Vec<_>)>();
                settings.push(
                    widget::settings::item::builder(fl!("open-with")).control(
                        Element::from(
                            widget::dropdown(
                                names,
                                mime_apps
                                    .iter()
                                    .position(|(x, _)| mime_app_cache.is_default(x, &self.mime)),
                                move |index| index,
                            )
                            .icons(Cow::Owned(icons)),
                        )
                        .map(move |index| {
                            let mime_app = &mime_apps[index].0;
                            Message::SetOpenWith(self.mime.clone(), mime_app.id.clone())
                        }),
                    ),
                );
            }
        }

        // From the listing: what every item knows about itself, however it
        // was found. Drawn at once, so the pane is never blank while the disk
        // is being asked for the rest.
        if self.metadata.is_dir() {
            if let Some(children) = self.metadata.children_count() {
                details = details.push(widget::text::body(fl!("items", items = children)));
            }
            let size = match &self.dir_size {
                DirSize::Calculating(_) => fl!("calculating"),
                DirSize::Directory(size) => format_size(*size),
                DirSize::NotDirectory => String::new(),
                DirSize::Error(err) => err.clone(),
            };
            if !size.is_empty() {
                details = details.push(widget::text::body(fl!("item-size", size = size)));
            }
        } else if let Some(size) = self.metadata.file_size() {
            details = details.push(widget::text::body(fl!(
                "item-size",
                size = format_size(size)
            )));
        }

        // Where a trashed item came from, which is all a restore goes by
        if let ItemMetadata::Trash { entry, .. } = &self.metadata {
            let location = entry.original_parent.as_ref().map_or_else(
                || fl!("trash-original-unknown"),
                |parent| parent.display().to_string(),
            );
            details = details.push(widget::selectable_text::body(fl!(
                "trash-original-location",
                location = location
            )));
        }

        // An item the listing could not fully describe, while the disk is
        // asked, or after it refused. Never a made-up value in either case.
        if !matches!(self.metadata, ItemMetadata::Path { .. }) {
            match &self.details {
                MetadataState::Pending | MetadataState::Loading => {
                    details = details.push(widget::text::body(fl!("calculating")));
                }
                MetadataState::Failed(err) => {
                    details = details.push(widget::text::body(err.clone()));
                }
                MetadataState::Ready(_) => {}
            }
        }

        // From the disk: times and ownership, which only a `stat` can say.
        if let Some(metadata) = self.file_metadata() {
            let date_time_formatter = date_time_formatter();
            let time_formatter = time_formatter();

            if let Ok(time) = metadata.created() {
                details = details.push(widget::selectable_text::body(fl!(
                    "item-created",
                    created = format_time(time, &date_time_formatter, &time_formatter).to_string()
                )));
            }

            if let Ok(time) = metadata.modified() {
                details = details.push(widget::selectable_text::body(fl!(
                    "item-modified",
                    modified = format_time(time, &date_time_formatter, &time_formatter).to_string()
                )));
            }

            if let Ok(time) = metadata.accessed() {
                details = details.push(widget::selectable_text::body(fl!(
                    "item-accessed",
                    accessed = format_time(time, &date_time_formatter, &time_formatter).to_string()
                )));
            }

            if let Some(path) = self.path_opt() {
                use std::os::unix::fs::MetadataExt;

                let mode = metadata.mode();

                let user_name = user_name(metadata.uid());
                let user_path = path.clone();
                settings.push(
                    widget::settings::item::builder(user_name)
                        .description(fl!("owner"))
                        .control(widget::dropdown(
                            Cow::Borrowed(MODE_NAMES.as_slice()),
                            Some(get_mode_part(mode, MODE_SHIFT_USER).try_into().unwrap()),
                            move |selected| {
                                Message::ShiftPermissions(
                                    Some((user_path.clone(), mode)),
                                    MODE_SHIFT_USER,
                                    selected.try_into().unwrap(),
                                )
                            },
                        )),
                );

                let group_name = group_name(metadata.gid());
                let group_path = path.clone();
                settings.push(
                    widget::settings::item::builder(group_name)
                        .description(fl!("group"))
                        .control(widget::dropdown(
                            Cow::Borrowed(MODE_NAMES.as_slice()),
                            Some(get_mode_part(mode, MODE_SHIFT_GROUP).try_into().unwrap()),
                            move |selected| {
                                Message::ShiftPermissions(
                                    Some((group_path.clone(), mode)),
                                    MODE_SHIFT_GROUP,
                                    selected.try_into().unwrap(),
                                )
                            },
                        )),
                );

                let other_path = path.clone();
                settings.push(widget::settings::item::builder(fl!("other")).control(
                    widget::dropdown(
                        Cow::Borrowed(MODE_NAMES.as_slice()),
                        Some(get_mode_part(mode, MODE_SHIFT_OTHER).try_into().unwrap()),
                        move |selected| {
                            Message::ShiftPermissions(
                                Some((other_path.clone(), mode)),
                                MODE_SHIFT_OTHER,
                                selected.try_into().unwrap(),
                            )
                        },
                    ),
                ));
            }
        }

        // Only what the scan already read. Opening the file here would put a
        // filesystem read in the render pass, which on a slow mount freezes
        // the window; an item built outside a scan simply shows no size.
        if let Some((width, height)) = self.image_dimensions.get().copied().flatten() {
            details = details.push(widget::text::body(format!("{width}x{height}")));
        }
        column = column.push(details);

        if let Some(metadata) = self.file_metadata()
            && !metadata.is_dir()
            && let Some(path) = self.path_opt()
        {
            let control: Element<'_, Message> = match &self.checksums {
                ChecksumState::NotCalculated => widget::button::standard(fl!("calculate"))
                    .on_press(Message::CalculateChecksums(path.clone()))
                    .into(),
                ChecksumState::Calculating => widget::Row::with_capacity(2)
                    .align_y(Alignment::Center)
                    .spacing(space_xxxs.to_pixels())
                    .push(widget::indeterminate_circular().size(16.0))
                    .push(widget::text::body(fl!("calculating")))
                    .into(),
                ChecksumState::Calculated(checksums) => {
                    let value = checksums.sha256.clone();
                    // Middle-ellipsize the digest to fit, full value on hover.
                    let value_text = widget::tooltip(
                        widget::ellipsize::body(value.clone(), widget::EllipsizeMode::Middle(1))
                            .font(crate::ui::font::mono())
                            .width(Length::Fill)
                            .wrapping(text::Wrapping::None),
                        widget::text::body(value.clone()),
                        widget::tooltip::Position::Bottom,
                    );
                    let copy_button = widget::button::icon(
                        widget::icon::from_name("edit-copy-symbolic").size(16),
                    )
                    .on_press(Message::CopyChecksum(value.clone()))
                    .tooltip(fl!("copy"));
                    widget::Row::with_capacity(2)
                        .align_y(Alignment::Center)
                        .spacing(space_xxxs.to_pixels())
                        .push(value_text)
                        .push(copy_button)
                        .into()
                }
                ChecksumState::Error(err) => {
                    widget::text::body(format!("{}: {}", fl!("error"), err)).into()
                }
            };
            settings.push(
                widget::settings::item::builder(fl!("checksum", kind = "SHA256")).control(control),
            );
        }

        if let Some(path) = self.path_opt()
            && selected
        {
            column = column.push(
                widget::button::standard(fl!("open")).on_press(Message::Open(Some(path.clone()))),
            );
        }

        if !settings.is_empty() {
            let mut section = widget::settings::section();
            section = section.extend(settings);
            column = column.push(section);
        }

        column.into()
    }

    /// Gives the item the preview the file view would show, made now: for a
    /// dialog about it, where nothing else will. Blocking: it may decode an
    /// image or run a thumbnailer, so call it on a worker.
    pub fn load_preview(&mut self) {
        let Some(path) = self.path_opt().cloned() else {
            return;
        };
        let limits = crate::config::ThumbCfg::default();
        let thumbnail = ItemThumbnail::new(
            &path,
            self.metadata.clone(),
            self.mime.clone(),
            THUMBNAIL_SIZE,
            u64::from(limits.max_mem_mb.get()),
            usize::from(limits.jobs.get()),
            u64::from(limits.max_size_mb.get()),
        );
        let handle = match &thumbnail {
            ItemThumbnail::Image(handle, _) => Some(widget::icon::Handle {
                symbolic: false,
                data: widget::icon::Data::Image(handle.clone()),
            }),
            ItemThumbnail::Svg(handle) => Some(widget::icon::Handle {
                symbolic: false,
                data: widget::icon::Data::Svg(handle.clone()),
            }),
            ItemThumbnail::NotImage | ItemThumbnail::Text(_) => None,
        };
        if let Some(handle) = handle {
            self.icon_handle_grid.clone_from(&handle);
            self.icon_handle_list = handle;
        }
        self.thumbnail_opt = Some(thumbnail);
    }

    pub fn replace_view(&self, heading: String) -> Element<'_, Message> {
        let Spacing { space_xxxs, .. } = spacing();

        let mut row = widget::Row::with_capacity(2).spacing(space_xxxs.to_pixels());
        // A little larger than a grid icon, and a real preview where there
        // is one; never larger, whatever the image
        row = row.push(
            widget::container(self.preview())
                .width(Length::Fixed(DIALOG_PREVIEW))
                .height(Length::Fixed(DIALOG_PREVIEW))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center),
        );

        let mut column = widget::Column::with_capacity(3).spacing(space_xxxs.to_pixels());
        column = column.push(widget::text::heading(heading));

        if let Some(metadata) = self.file_metadata() {
            if metadata.is_dir() {
                if let Some(children) = self.metadata.children_count() {
                    column = column.push(widget::text::body(fl!("items", items = children)));
                }
            } else {
                column = column.push(widget::text::body(fl!(
                    "item-size",
                    size = format_size(metadata.len())
                )));
            }
            if let Ok(time) = metadata.modified() {
                let date_time_formatter = date_time_formatter();
                let time_formatter = time_formatter();

                column = column.push(widget::text::body(fl!(
                    "item-modified",
                    modified = format_time(time, &date_time_formatter, &time_formatter).to_string()
                )));
            }
        }

        row = row.push(column);
        row.into()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum View {
    Grid,
    List,
}
/// View width under which the list's sort bar shows its icons without their
/// labels.
pub const SORT_LABELS_MIN: f32 = 600.0;

/// Height of the list's sort bar.
const SORT_BAR_HEIGHT: f32 = 44.0;

/// The width `content` takes in the default font family at `size`, shaped
/// the way a label draws it.
fn label_width(content: &str, size: f32, font: crate::ui::font::Font) -> f32 {
    crate::ui::font::text_width(content, size, font)
}

/// Width of a list row's right column, which holds what the list is sorted
/// by (the date when sorted by name).
fn right_column_width(sort: HeadingOptions) -> f32 {
    match sort {
        HeadingOptions::Size => 110.0,
        HeadingOptions::Type => 140.0,
        HeadingOptions::Name | HeadingOptions::Modified | HeadingOptions::TrashedOn => 200.0,
    }
}

/// The least width a list row's name is given. Under it the right column is
/// dropped and its detail leads the second line instead.
const LIST_NAME_MIN: f32 = 160.0;

/// Whether a list row `width` wide (the view's width) has room for its right
/// column next to the icon and at least [`LIST_NAME_MIN`] for the name.
fn right_column_fits(width: f32, icon_size: u16, sort: HeadingOptions) -> bool {
    let Spacing {
        space_s, space_xs, ..
    } = spacing();
    // The list's side padding, the row button's padding, and the gaps after
    // the icon and after the name
    let around = 2.0 * f32::from(space_s) + 4.0 * f32::from(space_xs);
    width - around - f32::from(icon_size) - right_column_width(sort) >= LIST_NAME_MIN
}

/// A row's second line with what its right column would have held put first,
/// for a row too narrow for the right column.
fn line_with_right(right: &str, line: &str) -> String {
    [right, line]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What a list row shows besides its name: its second line and its right
/// column. The right column holds the detail the list is sorted by, the date
/// when sorted by name or date; the second line the other two, in the order
/// type, size, date, leaving out any that is not known.
fn list_details(sort: HeadingOptions, type_: &str, size: &str, date: &str) -> (String, String) {
    let right = match sort {
        HeadingOptions::Type => 0,
        HeadingOptions::Size => 1,
        HeadingOptions::Name | HeadingOptions::Modified | HeadingOptions::TrashedOn => 2,
    };
    let parts = [type_, size, date];
    let line = parts
        .iter()
        .enumerate()
        .filter(|&(i, part)| i != right && !part.is_empty())
        .map(|(_, part)| *part)
        .collect::<Vec<_>>()
        .join(" · ");
    (line, parts[right].to_owned())
}

/// The order a sort gives, in words, as the sort menu says it.
pub fn sort_order_words(sort: HeadingOptions, ascending: bool) -> String {
    match (sort, ascending) {
        (HeadingOptions::Name | HeadingOptions::Type, true) => fl!("sort-a-z"),
        (HeadingOptions::Name | HeadingOptions::Type, false) => fl!("sort-z-a"),
        (HeadingOptions::Modified | HeadingOptions::TrashedOn, true) => fl!("sort-oldest-first"),
        (HeadingOptions::Modified | HeadingOptions::TrashedOn, false) => {
            fl!("sort-newest-first")
        }
        (HeadingOptions::Size, true) => fl!("sort-smallest-to-largest"),
        (HeadingOptions::Size, false) => fl!("sort-largest-to-smallest"),
    }
}

/// The sort bar's icon for `sort`, or its reverse button's for `None`.
/// Bundled and symbolic, so they take the colour of the button they sit in.
fn sort_icon(sort: Option<HeadingOptions>) -> widget::icon::Handle {
    static NAME: &[u8] = include_bytes!("../res/icons/symbolic/sort-name-symbolic.svg");
    static MODIFIED: &[u8] = include_bytes!("../res/icons/symbolic/sort-modified-symbolic.svg");
    static SIZE: &[u8] = include_bytes!("../res/icons/symbolic/sort-size-symbolic.svg");
    static TYPE: &[u8] = include_bytes!("../res/icons/symbolic/sort-type-symbolic.svg");
    static REVERSE: &[u8] = include_bytes!("../res/icons/symbolic/sort-reverse-symbolic.svg");
    let bytes = match sort {
        Some(HeadingOptions::Name) => NAME,
        Some(HeadingOptions::Modified | HeadingOptions::TrashedOn) => MODIFIED,
        Some(HeadingOptions::Size) => SIZE,
        Some(HeadingOptions::Type) => TYPE,
        None => REVERSE,
    };
    let mut handle = widget::icon::from_svg_bytes(bytes);
    handle.symbolic = true;
    handle
}

/// A sort bar button: the active one filled with the component colour and
/// its icon in the accent colour; the others dimmed text until hovered.
fn sort_button_style(active: bool) -> Button {
    fn appearance(theme: &theme::Theme, active: bool, lit: bool) -> widget::button::Style {
        let cosmic = theme.cosmic();
        let mut style = widget::button::Style::new();
        style.border_radius = cosmic.radius_s().to_radius();
        let on = cosmic.background(theme.transparent).on;
        if active {
            style.background = Some(cosmic.bg_component_color().to_color().into());
            style.text_color = Some(cosmic.on_bg_component_color().to_color());
            style.icon_color = Some(cosmic.accent_text_color().to_color());
        } else {
            if lit {
                style.background = Some(cosmic.bg_component_color().to_color().into());
            }
            let mut text = on;
            if !lit {
                text.alpha *= 0.75;
            }
            style.text_color = Some(text.to_color());
            style.icon_color = Some(text.to_color());
        }
        style
    }
    Button::Custom {
        active: Box::new(move |_, theme| appearance(theme, active, false)),
        disabled: Box::new(move |theme| appearance(theme, active, false)),
        hovered: Box::new(move |_, theme| appearance(theme, active, true)),
        pressed: Box::new(move |_, theme| appearance(theme, active, true)),
    }
}

/// Dimmed text: a list row's second line and right column.
fn dim_text_style(theme: &theme::Theme) -> widget::text::Style {
    let mut color = theme.cosmic().background(theme.transparent).on;
    color.alpha *= 0.75;
    widget::text::Style {
        color: Some(color.to_color()),
        ..Default::default()
    }
}

/// How strongly a hidden file shown in the listing is drawn: its icon's
/// opacity, and the share of the usual opacity its text keeps.
const FADED_OPACITY: f32 = 0.5;

/// A hidden file's name, fainter than any other name.
fn faded_text_style(theme: &theme::Theme) -> widget::text::Style {
    let mut color = theme.cosmic().background(theme.transparent).on;
    color.alpha *= FADED_OPACITY;
    widget::text::Style {
        color: Some(color.to_color()),
        ..Default::default()
    }
}

/// A hidden file's details, as much fainter than other details.
fn faded_dim_text_style(theme: &theme::Theme) -> widget::text::Style {
    let mut style = dim_text_style(theme);
    if let Some(color) = &mut style.color {
        color.a *= FADED_OPACITY;
    }
    style
}

/// The faint fill of every other list row, so the eye keeps to a row
/// across a wide window.
fn stripe_style(theme: &theme::Theme) -> widget::container::Style {
    let cosmic = theme.cosmic();
    let mut color = cosmic.background(theme.transparent).on;
    color.alpha = if cosmic.is_dark { 0.025 } else { 0.03 };
    widget::container::Style {
        background: Some(color.to_color().into()),
        border: crate::ui::iced::Border {
            radius: cosmic.radius_s().to_radius(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, PartialOrd, Ord, Eq, Deserialize, Serialize)]
pub enum HeadingOptions {
    Name = 0,
    Modified,
    Size,
    TrashedOn,
    Type,
}

impl fmt::Display for HeadingOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name => write!(f, "{}", fl!("name")),
            Self::Modified => write!(f, "{}", fl!("modified")),
            Self::Size => write!(f, "{}", fl!("size")),
            Self::TrashedOn => write!(f, "{}", fl!("trashed-on")),
            Self::Type => write!(f, "{}", fl!("type-heading")),
        }
    }
}

impl HeadingOptions {
    pub fn names() -> Vec<String> {
        vec![
            Self::Name.to_string(),
            Self::Modified.to_string(),
            Self::Size.to_string(),
            Self::TrashedOn.to_string(),
            Self::Type.to_string(),
        ]
    }
}

#[derive(Clone, Debug)]
pub enum Mode {
    App,
    Dialog(DialogKind),
}

impl Mode {
    /// Whether multiple files can be selected in this mode
    pub fn multiple(&self) -> bool {
        match self {
            Self::App => true,
            Self::Dialog(dialog) => dialog.multiple(),
        }
    }
}

pub struct SearchContext {
    results_rx: mpsc::Receiver<Item>,
    ready: Arc<atomic::AtomicBool>,
    last_modified_opt: Arc<RwLock<Option<SystemTime>>>,
}

/// Keeps only the items a host is willing to show; see [`Tab::item_filter`].
pub(crate) type ItemFilter = Box<dyn Fn(&Item) -> bool>;

/// What a file drag can be held over in a tab to open it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DragHover {
    /// The folder at this path in the list: by path, not position, so a
    /// refresh that moves or removes rows cannot make the hold open another
    Item(PathBuf),
    /// The ancestor crumb at this index of `Tab::location_ancestors`
    Ancestor(usize),
}

/// How fast a file drag at `y`, measured from the top of the visible list
/// `height` tall, scrolls it: pixels per 10 ms tick, negative for up, or
/// `None` outside the edge zones. The zones are the top and bottom
/// [`DRAG_EDGE_ZONE`] of the list, and the speed grows with how far into one
/// the drag is, up to half the zone at the very edge.
///
/// Not the wheel: a Wayland compositor such as Hyprland takes pointer focus
/// from every window for the length of a drag, so no window gets wheel events
/// until it ends.
fn drag_edge_scroll(y: f32, height: f32) -> Option<f32> {
    if !(0.0..=height).contains(&y) {
        None
    } else if y < DRAG_EDGE_ZONE {
        Some(-(DRAG_EDGE_ZONE - y) / 2.0)
    } else if y > height - DRAG_EDGE_ZONE {
        Some((y - (height - DRAG_EDGE_ZONE)) / 2.0)
    } else {
        None
    }
}

/// See [`drag_edge_scroll`].
const DRAG_EDGE_ZONE: f32 = 48.0;

pub struct Tab {
    pub location: Location,
    pub location_ancestors: Vec<(Location, String)>,
    pub location_title: String,
    /// Breadcrumb whose context menu is open, drawn as active while it shows
    pub location_context_menu_index: Option<usize>,
    pub mode: Mode,
    pub scroll_opt: Option<AbsoluteOffset>,
    /// Keeps only the items a host is willing to show, applied as they arrive
    /// rather than afterwards.
    ///
    /// The file chooser's file type filter. Search results come in one at a
    /// time and are capped, so a filter that ran after the fact would already
    /// have let rejected results take the places of ones that would have been
    /// shown, and move the cutoff that stops older results being sent at all.
    pub(crate) item_filter: Option<ItemFilter>,
    pub size_opt: Cell<Option<Size>>,
    pub content_height_opt: Cell<Option<f32>>,
    /// Room kept free under the last item, in pixels, so it can be scrolled
    /// clear of the card the app floats over the bottom-right corner: what
    /// [`Self::view`] was given, read at each layout. 0 without a card.
    bottom_reserve: Cell<f32>,
    pub viewport_opt: Option<Rectangle>,
    pub item_view_size_opt: Cell<Option<Size>>,
    pub edit_location: Option<EditLocation>,
    pub edit_location_id: widget::Id,
    pub history_i: usize,
    pub history: Vec<Location>,
    /// Scroll offset of each history entry when it was left, restored on
    /// going back or forward
    history_scroll: Vec<Option<AbsoluteOffset>>,
    /// Offset to apply once the items of a restored history entry are in
    pending_scroll: Option<AbsoluteOffset>,
    pub config: TabConfig,
    pub thumb_config: ThumbCfg,
    pub sort_name: HeadingOptions,
    pub sort_direction: bool,
    pub gallery: bool,
    pub(crate) parent_item_opt: Option<Box<Item>>,
    pub(crate) items_opt: Option<Vec<Item>>,
    /// Which of the items are selected and highlighted.
    pub selection: Selection,
    // `iced_core::widget::Id` has no `Display` and a private `Internal`, so the
    // name cannot be recovered from an `Id`. Store it alongside the id; every
    // producer already has it as a literal.
    pub(crate) scrollable_id: widget::Id,
    /// The string `scrollable_id` was built from; see the note above.
    pub(crate) scrollable_name: std::borrow::Cow<'static, str>,
    select_focus: Option<usize>,
    select_range: Option<(usize, usize)>,
    clicked: Option<usize>,
    /// Set by a double click so the release that follows it does not open the
    /// item a second time. In single-click mode the first release has already
    /// opened it, and a double click is still two presses and two releases.
    opened_by_this_click: bool,
    /// What the press a release will complete was on, so a listing changed
    /// in between, putting another item under the pointer, does not hand the
    /// click to that item.
    pressed_on: Option<Location>,
    /// An item pressed while already selected. The press keeps the rest of
    /// the selection, so it can be dragged as a whole; if the release comes
    /// on the same item with no drag started in between, the selection
    /// narrows to it.
    narrow_on_release: Option<usize>,
    last_right_click: Option<usize>,
    search_context: Option<SearchContext>,
    search_select: SearchSelect,
    date_time_formatter: DateTimeFormatter<fieldsets::YMDT>,
    time_formatter: DateTimeFormatter<fieldsets::T>,
    watch_drag: bool,
    /// This tab started the file drag currently in flight, so its `on_drag`
    /// must not start a second one for every further pixel of motion.
    dnd_source: bool,
    /// What this tab's drag carries, which is no place to drop it.
    dnd_dragged: Vec<PathBuf>,
    /// Where a live file drag last was over the list, from the top of what
    /// is on screen: the drop target is worked out again from it as the list
    /// scrolls under a still pointer.
    dnd_position: Option<Point>,
    /// How fast a file drag near an edge of the list is scrolling it; see
    /// [`drag_edge_scroll`].
    dnd_auto_scroll: Option<f32>,
    /// The folder or crumb a file drag is held over, to open it.
    drag_hover: Option<DragHover>,
    /// Counts changes of `drag_hover`, so only the timer of the current one
    /// opens anything.
    drag_hover_generation: u64,
    /// The item a live file drag is over and would drop into, drawn as
    /// highlighted. Only ever a directory.
    dnd_target: Option<usize>,
    /// The breadcrumb ancestor under a file drag, indexed by `Location::path_opt()`'s
    /// `ancestors()`. This is the index used by `location_context_menu_index`; the drag
    /// highlight uses the same `Button::LinkActive` style as an open breadcrumb context
    /// menu.
    dnd_ancestor: Option<usize>,
    window_id: Option<window::Id>,
    large_image_manager: LargeImageManager,
    /// The request number of the network resolution this tab is waiting on,
    /// if any.
    ///
    /// A number rather than the URI itself, because the same URI can be
    /// submitted twice and the first answer must not settle the second
    /// request. Cleared whenever the tab goes somewhere or the address field
    /// is put away, so an answer that outlives the question it was asked for
    /// is dropped rather than dragging the tab back.
    resolving_network: Option<u64>,
    /// The request number of the step to a local location being resolved
    /// on a worker, answered by [`Message::LocationResolved`], and the
    /// history entry and location it steps to.
    resolving_location: Option<(u64, Option<usize>, Location)>,
    /// Counts the steps asked of the tab, answered or still being resolved,
    /// so that whatever was waiting to send it somewhere can tell it has
    /// been sent elsewhere since.
    steps: u64,
    next_location_request: u64,
    /// Which listing this tab is showing.
    ///
    /// Bumped whenever the items are replaced wholesale -- a scan, a rescan, a
    /// change of location. An item re-read in the background belongs to the
    /// listing that asked for it, and applying it to a later one would put
    /// back a snapshot the newer scan has already superseded.
    listing: u64,
    /// How the view last laid the listing out, which says what is on
    /// screen at any scroll. Set by the list and grid views; forgotten by
    /// [`Self::new_listing`], as its indices are the old listing's.
    shown_layout: RefCell<Option<ShownLayout>>,
    /// The last content check job, kept while its key holds: the
    /// subscription is asked for after every update, and building the job
    /// clones a path per file.
    type_check_job: RefCell<Option<TypeCheckWindow>>,
    /// The listing's order as last sorted, kept until something it depends
    /// on may have changed: see [`Self::column_sort`].
    sort_cache: RefCell<Option<(SortKey, Arc<[usize]>)>>,
    /// The selected items' indices: see [`Self::selected_indices`].
    selection_cache: RefCell<Option<Arc<[usize]>>>,
    /// The items that may be highlighted, so taking every highlight away
    /// need not visit every item: see [`Self::dehighlight_all`].
    highlight_marks: Vec<usize>,
    /// How much a Trash listing holds; worked out afresh for each listing.
    trash_size: TrashSize,
    /// What working that out needs, taken from the listing once.
    trash_size_input: OnceCell<Arc<TrashSizeInput>>,
    /// How many times this tab has been sent somewhere. See
    /// [`Self::navigation`].
    navigation: u64,
    /// How many folder reads have been started for this tab. See
    /// [`Self::start_scan`].
    scans: u64,
    /// The batch number given to the next set of items sent off to be re-read
    /// for this tab, and the newest batch already adopted per path. Per tab,
    /// because two tabs can show the same folder and each has to see its own
    /// results; forgotten only when the listing is replaced.
    refresh_batch: u64,
    refreshed_at: FxHashMap<PathBuf, u64>,
    /// Detail reads out for this tab, by path, each with whether its answer is
    /// still wanted. Kept here and not on the item, because the item can be
    /// replaced -- by a rescan, or a rebuild -- while the read is still
    /// running, and the replacement must not start a second read of the same
    /// path while the first still holds a permit. An entry lives exactly as
    /// long as the read; the flag goes false when its answer stopped
    /// mattering, so a read that has not started yet declines to.
    details_reads: FxHashMap<PathBuf, Arc<atomic::AtomicBool>>,
    /// The number given to the next network resolution this tab asks for.
    next_network_request: u64,
}

/// Add up the size of everything under `path`.
///
/// The walk runs on a bounded blocking worker. It reads directories and stats
/// every entry, which is blocking work whatever it is wrapped in: awaiting
/// between entries let the runtime schedule something else, but one slow
/// directory read still held the thread it was on, and a selection of several
/// large folders held several.
///
/// `controller` is what stops it. The caller cancels it when it stops caring,
/// because dropping the future that awaits a blocking job does not reach the
/// job, and a walk nobody is waiting for would otherwise run to the end of the
/// tree.
async fn calculate_dir_size(path: &Path, controller: Controller) -> Result<u64, OperationError> {
    let path = path.to_path_buf();
    let walk = {
        let controller = controller.clone();
        bounded_blocking(Arc::clone(&DIR_SIZE_SEMAPHORE), move || {
            let mut total = 0;
            for entry_res in ignore::WalkBuilder::new(&path)
                .standard_filters(false)
                .build()
            {
                // Checked here, inside the work, for the same reason the permit
                // is held here: nothing outside can interrupt it.
                loop {
                    match controller.state() {
                        ControllerState::Running => break,
                        ControllerState::Paused => std::thread::sleep(DIR_SIZE_PAUSE_POLL),
                        state => return Err(OperationError::from_state(state)),
                    }
                }

                if let Ok(entry) = entry_res
                    && let Ok(metadata) = entry.metadata()
                    && metadata.is_file()
                {
                    total += metadata.len();
                }
            }
            Ok(total)
        })
        .await
    };

    walk.unwrap_or_else(|| Err(OperationError::from_state(ControllerState::Failed)))
}

/// Calculate file checksums in a single pass over the file. To add another
/// digest, hash it alongside `sha256_hasher` in the loop below and add a field
/// to [`FileChecksums`]; the file is only read once.
async fn calculate_checksums(path: &Path) -> Result<FileChecksums, String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = File::open(&path).map_err(|e| e.to_string())?;
        let mut sha256_hasher = Sha256::new();

        let mut buffer = [0u8; 8192];
        loop {
            let bytes_read = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if bytes_read == 0 {
                break;
            }
            sha256_hasher.update(&buffer[..bytes_read]);
        }

        Ok(FileChecksums {
            sha256: crate::hex::lower(sha256_hasher.finalize()),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A folder's name for a breadcrumb or tab title, and whether it is home.
///
/// With `look`, the filesystem is asked, so a gvfs folder gets the display
/// name its backend gives it; that is a blocking round trip per folder and
/// belongs on a worker. Without it, the name is the one the path spells,
/// which is the same thing for a local folder and stands in for a remote
/// one until the worker's answer arrives.
fn folder_name<P: AsRef<Path>>(path: P, look: bool) -> (String, bool) {
    let path = path.as_ref();
    let mut found_home = false;
    let name = match path.file_name() {
        Some(name) => {
            if path == crate::home_dir() {
                found_home = true;
                fl!("home")
            } else {
                match (
                    get_filename_from_path(path),
                    look.then(|| fs::metadata(path)),
                ) {
                    (Ok(name), Some(Ok(metadata))) => {
                        let is_gvfs = fs_kind(&metadata) == FsKind::Gvfs;
                        display_name_for_file(path, &name, is_gvfs, false)
                    }
                    (Ok(name), None) => Item::display_name(&name),
                    _ => name.to_string_lossy().into_owned(),
                }
            }
        }
        None => {
            fl!("filesystem")
        }
    };
    (name, found_home)
}

// parse .hidden file and return files path
pub fn parse_hidden_file(path: &PathBuf) -> Box<[String]> {
    let Ok(file) = File::open(path) else {
        return Default::default();
    };

    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let line = line.trim();
            (!line.is_empty()).then_some(line.to_owned())
        })
        .collect()
}

impl Drop for Tab {
    fn drop(&mut self) {
        // A closed tab's queued reads answer nobody. Those already running
        // finish; those not yet started will not start.
        for wanted in self.details_reads.values() {
            wanted.store(false, atomic::Ordering::Relaxed);
        }
    }
}

/// How much the trash holds, as far as it could be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrashSize {
    /// Still being worked out.
    Calculating,
    Known(u64),
    /// At least this much: a trashed folder had no copy to walk, or its
    /// walk failed outright.
    Partial(u64),
}

/// The one button a location offers, shown on a card in the bottom-right
/// corner for as long as the tab is there, with what the card says beside
/// it. The counts are `None` in a search: the listing then holds only the
/// matches, while the button acts on everything.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TabAction {
    EmptyTrash(Option<(usize, TrashSize)>),
    ClearRecents(Option<usize>),
    /// With how many network drives are connected now.
    AddNetworkDrive(usize),
}

impl TabAction {
    /// The location's name and icon, as the sidebar gives them, then the
    /// button's label and message. The trash is never empty while it offers
    /// to be emptied, so its icon is the full one without asking the disk.
    pub fn parts(self) -> (String, &'static str, String, Message) {
        match self {
            Self::EmptyTrash(_) => (
                fl!("trash"),
                "user-trash-full-symbolic",
                fl!("empty-trash"),
                Message::EmptyTrash,
            ),
            Self::ClearRecents(_) => (
                fl!("recents"),
                "document-open-recent-symbolic",
                fl!("clear-recents-history"),
                Message::ClearRecents,
            ),
            Self::AddNetworkDrive(_) => (
                fl!("networks"),
                "network-workgroup-symbolic",
                fl!("add-network-drive"),
                Message::AddNetworkDrive,
            ),
        }
    }

    /// What the card says after the title, if anything.
    pub fn detail(self) -> Option<String> {
        match self {
            Self::EmptyTrash(counts) => counts.map(|(items, size)| {
                let size = match size {
                    TrashSize::Calculating => fl!("calculating"),
                    TrashSize::Known(bytes) => format_size(bytes),
                    TrashSize::Partial(bytes) => format!("{}+", format_size(bytes)),
                };
                format!("{} · {size}", fl!("item-count", count = items))
            }),
            Self::ClearRecents(items) => items.map(|items| fl!("item-count", count = items)),
            // Nothing to say when none is connected
            Self::AddNetworkDrive(connected) => {
                (connected > 0).then(|| fl!("network-drives-connected", count = connected))
            }
        }
    }
}

/// What working out a Trash listing's size needs: the bytes its files
/// already carry, and the copies inside the trash of its folders, to walk
/// (`None` for one whose copy could not be found).
#[derive(Debug, Default, Eq, PartialEq)]
struct TrashSizeInput {
    files: u64,
    folders: Vec<Option<PathBuf>>,
}

impl TrashSizeInput {
    fn new(items: &[Item]) -> Self {
        let mut input = Self::default();
        for item in items {
            if let ItemMetadata::Trash { metadata, entry } = &item.metadata {
                match metadata.size {
                    crate::trashing::TrashItemSize::Bytes(bytes) => input.files += bytes,
                    crate::trashing::TrashItemSize::Entries(_) => {
                        input.folders.push(crate::trash::trash_item_path(entry))
                    }
                }
            }
        }
        input
    }

    /// The total, walking each folder in turn. A folder with no copy to
    /// walk, or whose walk fails outright, is left out and makes the total
    /// partial. What a walk cannot read inside a folder, the folder itself
    /// included, it skips as [`calculate_dir_size`] does everywhere: a
    /// missing copy counts as nothing (decided 2026-09-28). A cancelled
    /// walk stops there, as nobody is waiting for the answer any more.
    async fn total(&self, controller: Controller) -> TrashSize {
        let mut total = self.files;
        let mut partial = false;
        for folder in &self.folders {
            if controller.is_cancelled() {
                break;
            }
            let Some(path) = folder else {
                log::warn!("a trashed folder has no copy in the trash to measure");
                partial = true;
                continue;
            };
            match calculate_dir_size(path, controller.clone()).await {
                Ok(size) => total += size,
                Err(err) => {
                    if !controller.is_cancelled() {
                        log::warn!(
                            "failed to calculate the size of trashed folder {}: {err}",
                            path.display()
                        );
                    }
                    partial = true;
                }
            }
        }
        if partial {
            TrashSize::Partial(total)
        } else {
            TrashSize::Known(total)
        }
    }
}

impl Tab {
    pub fn new(
        location: Location,
        config: TabConfig,
        thumb_config: ThumbCfg,
        sorting_options: Option<&FxOrderMap<String, (HeadingOptions, bool)>>,
        scrollable_name: std::borrow::Cow<'static, str>,
        window_id: Option<window::Id>,
    ) -> Self {
        let scrollable_id = match scrollable_name.clone() {
            std::borrow::Cow::Borrowed(name) => widget::Id::new(name),
            std::borrow::Cow::Owned(name) => widget::Id::from(name),
        };
        let location_str = location.to_string();
        let (sort_name, sort_direction) = sorting_options
            .and_then(|opts| opts.get(&location_str))
            .or_else(|| SORT_OPTION_FALLBACK.get(&location_str))
            .copied()
            .unwrap_or((HeadingOptions::Name, true));
        // Made canonical by the rescan that follows, off this thread
        let location = location.normalize_lexically();
        // Named from the path alone; the rescan that follows names them from
        // the filesystem, off this thread.
        let location_ancestors = location.ancestors(false);
        let location_title = location.title(false);
        let history = vec![location.clone()];
        Self {
            location,
            location_ancestors,
            location_title,
            location_context_menu_index: None,
            resolving_network: None,
            resolving_location: None,
            steps: 0,
            next_location_request: 0,
            listing: 0,
            navigation: 0,
            scans: 0,
            item_filter: None,
            refresh_batch: 0,
            shown_layout: RefCell::new(None),
            type_check_job: RefCell::new(None),
            sort_cache: RefCell::new(None),
            selection_cache: RefCell::new(None),
            highlight_marks: Vec::new(),
            trash_size: TrashSize::Calculating,
            trash_size_input: OnceCell::new(),
            refreshed_at: FxHashMap::default(),
            details_reads: FxHashMap::default(),
            next_network_request: 0,
            mode: Mode::App,
            scroll_opt: None,
            size_opt: Cell::new(None),
            content_height_opt: Cell::new(None),
            bottom_reserve: Cell::new(0.0),
            viewport_opt: None,
            item_view_size_opt: Cell::new(None),
            edit_location: None,
            edit_location_id: widget::Id::unique(),
            history_i: 0,
            history_scroll: vec![None; history.len()],
            pending_scroll: None,
            history,
            config,
            thumb_config,
            sort_name,
            sort_direction,
            gallery: false,
            parent_item_opt: None,
            items_opt: None,
            selection: Selection::default(),
            scrollable_id,
            scrollable_name,
            select_focus: None,
            select_range: None,
            clicked: None,
            opened_by_this_click: false,
            pressed_on: None,
            narrow_on_release: None,
            last_right_click: None,
            search_context: None,
            search_select: SearchSelect::Off,
            date_time_formatter: date_time_formatter(),
            time_formatter: time_formatter(),
            watch_drag: true,
            dnd_source: false,
            dnd_dragged: Vec::new(),
            dnd_position: None,
            dnd_auto_scroll: None,
            drag_hover: None,
            drag_hover_generation: 0,
            dnd_target: None,
            dnd_ancestor: None,
            window_id,
            large_image_manager: LargeImageManager::new(),
        }
    }

    pub fn title(&self) -> String {
        self.location_title.clone()
    }

    pub const fn items_opt(&self) -> Option<&Vec<Item>> {
        self.items_opt.as_ref()
    }

    pub const fn items_opt_mut(&mut self) -> Option<&mut Vec<Item>> {
        self.items_opt.as_mut()
    }

    /// The button this tab's location offers now, if any: emptying a trash
    /// or clearing a recents list that has something in it, and adding a
    /// network drive from the network root. `connected_network_drives` is
    /// what the network root's card counts; the tab does not know the
    /// mounts.
    pub fn action(&self, connected_network_drives: usize) -> Option<TabAction> {
        let items = self.items_opt().map(Vec::len).filter(|&items| items > 0);
        match &self.location {
            Location::Trash => {
                items.map(|items| TabAction::EmptyTrash(Some((items, self.trash_size))))
            }
            Location::Search(SearchLocation::Trash, ..) => {
                items.map(|_| TabAction::EmptyTrash(None))
            }
            Location::Recents => items.map(|items| TabAction::ClearRecents(Some(items))),
            Location::Search(SearchLocation::Recents, ..) => {
                items.map(|_| TabAction::ClearRecents(None))
            }
            Location::Network(uri, ..) if uri == "network:///" => {
                Some(TabAction::AddNetworkDrive(connected_network_drives))
            }
            _ => None,
        }
    }

    /// Take any icons the cache has learned, and report what it still lacks.
    ///
    /// See [`refresh_icons`]; this is the form the hosts use, which keeps the
    /// borrow of the item list inside the tab.
    pub fn refresh_icons(&mut self, sizes: IconSizes) -> IconWarmup {
        self.items_opt
            .as_deref_mut()
            .map(|items| refresh_icons(items, sizes))
            .unwrap_or_default()
    }

    /// The items on screen now, in the order shown, with their indices.
    /// Worked out from the last view's layout and the current scroll rather
    /// than kept from the view, because iced asks for the subscription
    /// before it draws: a scroll would otherwise be seen only at the update
    /// after it, which may never come.
    fn visible_items(&self) -> Vec<(usize, &Item)> {
        let (Some(items), Some(layout)) =
            (self.items_opt.as_ref(), self.shown_layout.borrow().clone())
        else {
            return Vec::new();
        };
        let visible = layout.visible(self.visible_rect(self.size_opt.get().unwrap_or_default()));
        layout.shown[visible]
            .iter()
            .filter_map(|&index| Some((index, items.get(index)?)))
            .collect()
    }

    /// The content check to run for what is shown: the unchecked files on
    /// screen, a screenful below and above them, then any selected ones.
    /// `None` when none of those is left unchecked, which ends the check's
    /// subscription until the view moves or the selection changes.
    fn type_check_job(&self) -> Option<TypeCheckWindow> {
        let items = self.items_opt.as_ref()?;
        let show_hidden = self.config.show_hidden;
        let wanted = |item: &Item| {
            !item.type_checked && (show_hidden || !item.hidden) && item.path_opt().is_some()
        };

        let layout = self.shown_layout.borrow().clone();
        let before_view;
        let (order, visible): (&[usize], std::ops::Range<usize>) = match layout.as_ref() {
            Some(layout) => (
                &layout.shown,
                layout.visible(self.visible_rect(self.size_opt.get().unwrap_or_default())),
            ),
            // Not drawn yet: the top of the listing as it will be shown
            None => {
                before_view = self
                    .column_sort()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                let visible = 0..DEFAULT_SHOWN.min(before_view.len());
                (&before_view, visible)
            }
        };
        let (window, range) = check_order(order, visible);

        // Everything the job depends on, so it is built again only when one
        // of them changes: not when its own answers arrive, and not while
        // the view scrolls within the same window
        let mut key = rustc_hash::FxHasher::default();
        show_hidden.hash(&mut key);
        order[range].hash(&mut key);
        let selected = self.selected_indices();
        selected.hash(&mut key);
        let key = key.finish();

        let mut cached = self.type_check_job.borrow_mut();
        if cached.as_ref().is_none_or(|job| job.key != key) {
            let in_window: rustc_hash::FxHashSet<usize> = window.iter().copied().collect();
            let files = window
                .iter()
                .copied()
                .chain(
                    selected
                        .iter()
                        .copied()
                        .filter(|index| !in_window.contains(index)),
                )
                .filter_map(|index| {
                    let item = items.get(index).filter(|item| wanted(item))?;
                    Some(crate::type_check::Unchecked {
                        index,
                        path: item.path_opt()?.clone(),
                        name: item.name.clone(),
                        mime: item.mime.clone(),
                    })
                })
                .collect();
            *cached = Some(TypeCheckWindow { key, files });
        }
        let job = cached.as_ref()?;
        job.files
            .iter()
            .any(|file| items.get(file.index).is_some_and(wanted))
            .then(|| job.clone())
    }

    /// Note a new set of items, and forget what was being re-read for the old
    /// one: a scan reads everything fresh, so an older re-read of one file has
    /// nothing left to say.
    fn new_listing(&mut self) {
        self.listing = self.listing.wrapping_add(1);
        // The content check of the old listing ends with its subscription;
        // the new one starts from what the new listing shows
        self.shown_layout = RefCell::new(None);
        self.type_check_job = RefCell::new(None);
        // So is the trash's size
        self.trash_size = TrashSize::Calculating;
        self.trash_size_input = OnceCell::new();
        // Reads out for the old listing answer questions nobody is asking any
        // more. Those not yet started will not start; those running finish,
        // and are declined on arrival. Either way each still holds its permit
        // until it ends, which is why the entries stay: they are what stops a
        // second read of the same path being started meanwhile.
        for wanted in self.details_reads.values() {
            wanted.store(false, atomic::Ordering::Relaxed);
        }
        // The only place this is forgotten. Tying it to a count of
        // outstanding work instead would mean the last answer to arrive
        // cleared the record that rejects it -- and the last to arrive is the
        // one most likely to be stale. Keyed by path within one listing, it is
        // bounded by the size of the directory being shown.
        self.refreshed_at.clear();
    }

    /// Where the tab has been sent, counted separately from what it is
    /// showing.
    ///
    /// A rescan replaces the items without the user going anywhere, so
    /// [`Self::listing`] moves far more often than this does. Anything asking
    /// "did the user navigate away while I was working?" has to ask this;
    /// asking the listing would answer yes to an ordinary refresh.
    pub fn navigation(&self) -> u64 {
        self.navigation
    }

    /// How a search started from this tab should be run, from its config.
    pub fn search_options(&self) -> SearchOptions {
        SearchOptions {
            show_hidden: self.config.show_hidden,
            recursive: self.config.search_recursive,
        }
    }

    /// How a search should be run when the query is set from here.
    ///
    /// Each option follows whatever shows it. The eye shows this search's own
    /// scope, so a running search keeps it: the user set it deliberately, and
    /// typing another letter is not a request to change it. Hidden files are
    /// shown by a checkbox in the menu that describes the config rather than
    /// any one search, so that answer comes from the config -- keeping a
    /// restored search's own would leave the checkbox contradicting what is
    /// on screen. A search that is starting takes both from the config.
    pub fn search_options_for_query(&self) -> SearchOptions {
        match &self.location {
            Location::Search(_, _, options, _) => SearchOptions {
                show_hidden: self.config.show_hidden,
                recursive: options.recursive,
            },
            _ => self.search_options(),
        }
    }

    /// A location on its way back from history, with anything it carries that
    /// the config now answers for brought up to date.
    ///
    /// A search remembers the options it ran with, and its scope is its own to
    /// keep. Whether hidden files are shown is not: that is the config's
    /// answer, shown in the menu, and a restored search that disagreed with it
    /// would contradict the checkbox before the user had typed anything.
    fn restored(&self, location: &Location) -> Location {
        match location {
            Location::Search(search_location, term, options, time)
                if options.show_hidden != self.config.show_hidden =>
            {
                Location::Search(
                    search_location.clone(),
                    term.clone(),
                    SearchOptions {
                        show_hidden: self.config.show_hidden,
                        recursive: options.recursive,
                    },
                    *time,
                )
            }
            other => other.clone(),
        }
    }

    /// Which listing is on screen, for tagging background re-reads of it.
    pub fn listing(&self) -> u64 {
        self.listing
    }

    /// Take a batch number for a set of items about to be re-read.
    pub fn next_refresh_batch(&mut self) -> u64 {
        let batch = self.refresh_batch;
        self.refresh_batch = self.refresh_batch.wrapping_add(1);
        batch
    }

    /// Start reading `path` from the disk for the item at it, unless a read of
    /// that path is already out.
    ///
    /// One read per path at a time, however many incarnations of the item come
    /// and go while it runs: the read holds one of the few permits, and a
    /// second one for the same path would hold another, for the same answer.
    /// An item that finds a read already out is marked as waiting on it, and
    /// is read for itself once that read ends and its answer turns out not to
    /// be for this incarnation -- which is why a `Loading` item with no read
    /// out for its path is accepted here: that is what waiting looks like
    /// from the outside, and this is the read it was waiting for.
    fn start_details_read(&mut self, path: PathBuf, epoch: u64, listing: u64) -> Option<Command> {
        let location = Location::Path(path.clone());
        let mut found = false;
        for item in self
            .parent_item_opt
            .iter_mut()
            .map(|item| &mut **item)
            .chain(self.items_opt.iter_mut().flatten())
        {
            if item.location_opt.as_ref() == Some(&location)
                && item.details_epoch == epoch
                && matches!(
                    item.details,
                    // Pending: asked for the first time. Loading with no read
                    // out for the path: told to wait on a read that has since
                    // ended without being for it. Both start a read now.
                    MetadataState::Pending | MetadataState::Loading
                )
            {
                item.details = MetadataState::Loading;
                found = true;
            }
        }
        if !found || self.details_reads.contains_key(&path) {
            return None;
        }

        let wanted = Arc::new(atomic::AtomicBool::new(true));
        self.details_reads.insert(path.clone(), Arc::clone(&wanted));
        // A task, not part of the subscription that asked: it outlives the
        // selection, so its answer lands whether or not the item is still
        // previewed, and the permit it holds is released by the read ending
        // -- nothing else.
        Some(Command::Iced(
            crate::ui::Task::future(async move {
                let result = bounded_blocking(Arc::clone(&DETAILS_SEMAPHORE), {
                    let path = path.clone();
                    move || {
                        // Obsolete before it began -- the listing moved on, or
                        // the tab closed -- so it does not begin. The permit
                        // goes straight back.
                        if !wanted.load(atomic::Ordering::Relaxed) {
                            return Err("no longer wanted".to_owned());
                        }
                        fs::metadata(&path).map_err(|err| err.to_string())
                    }
                })
                .await
                .unwrap_or_else(|| Err("the read was not run".to_owned()));
                Message::ItemDetails(path, epoch, listing, result)
            })
            .into(),
        ))
    }

    /// Whether a re-read of `path` from `listing`/`batch` is still the newest
    /// thing known about it, recording it when it is.
    pub fn accept_refresh(&mut self, listing: u64, batch: u64, path: &Path) -> bool {
        if listing != self.listing {
            return false;
        }
        if self
            .refreshed_at
            .get(path)
            .is_some_and(|seen| batch < *seen)
        {
            return false;
        }
        self.refreshed_at.insert(path.to_path_buf(), batch);
        true
    }

    pub fn set_items(&mut self, items: Vec<Item>) {
        // What the menus read of the selection is worked out again
        self.forget_selection();
        let marks = self.selection_marks();
        self.new_listing();
        let highlighted = self.items_opt.as_ref().and_then(|items| {
            items
                .iter()
                .position(|item| self.selection.is_highlighted(item))
        });
        // The same places stay selected in the listing read anew
        let selected = self.selected_locations();
        self.selection.forget_entries();
        for item in &items {
            if item
                .location_opt
                .as_ref()
                .is_some_and(|location| selected.contains(location))
            {
                self.selection.select(item, true);
            }
        }
        self.items_opt = Some(items);
        self.restore_selection_marks(marks);
        // The old listing's notes name other items
        self.highlight_marks.clear();
        if let Some(item) = highlighted
            .zip(self.items_opt.as_ref())
            .and_then(|(h, items)| items.get(h))
        {
            self.selection.highlight(item, true);
        }
        self.highlight_marks.extend(highlighted);
    }

    pub fn cut_selected(&mut self) {
        if let Some(ref mut items) = self.items_opt {
            for item in items.iter_mut() {
                let selected = self.selection.is_selected(item);
                self.selection.set_cut(item, selected);
            }
        }
    }

    pub fn refresh_cut(&mut self, locations: &[PathBuf]) {
        if let Some(ref mut items) = self.items_opt {
            for item in items.iter_mut() {
                let cut = item
                    .location_opt
                    .as_ref()
                    .and_then(Location::path_opt)
                    .is_some_and(|path| locations.contains(path));
                self.selection.set_cut(item, cut);
            }
        }
    }

    pub fn selected_locations(&self) -> Vec<Location> {
        if let Some(ref items) = self.items_opt {
            items
                .iter()
                .filter(|item| self.selection.is_selected(item))
                .filter_map(|item| item.location_opt.clone())
                .collect()
        } else {
            Vec::new()
        }
    }

    pub fn select_all(&mut self) {
        // What the menus read of the selection is worked out again
        self.forget_selection();
        if let Some(ref items) = self.items_opt {
            for item in items {
                self.selection
                    .select(item, self.config.show_hidden || !item.hidden);
            }
        }
        self.settle_anchor();
    }

    /// Focuses item `i` and makes it where a shift selection starts, or
    /// forgets both when `None`. For a selection that has just been made
    /// anew, of that one item or of nothing.
    fn anchor_selection(&mut self, i: Option<usize>) {
        self.select_focus = i;
        self.select_range = i.map(|i| (i, i));
    }

    /// After a selection made all at once (everything, or a rectangle): the
    /// anchor stays where it is when that item is still selected, else
    /// moves to the first selected item as shown. Nothing selected, no
    /// anchor.
    fn settle_anchor(&mut self) {
        let selected = |i: usize| {
            self.items_opt
                .as_ref()
                .and_then(|items| items.get(i))
                .is_some_and(|item| self.selection.is_selected(item))
        };
        if self
            .select_range
            .is_some_and(|(anchor, _)| selected(anchor))
        {
            return;
        }
        let first = self
            .column_sort()
            .and_then(|sorted| {
                sorted
                    .into_iter()
                    .find(|(_, item)| self.selection.is_selected(item))
            })
            .map(|(i, _)| i);
        self.select_range = first.map(|i| (i, i));
        if first.is_none() {
            self.select_focus = None;
        }
    }

    /// The items at the focus and at the two ends of the shift range, by
    /// location, to find them again once the listing has changed under
    /// them: positions in a listing say nothing about another one.
    fn selection_marks(&self) -> [Option<Location>; 3] {
        let location = |i: Option<usize>| {
            self.items_opt
                .as_ref()
                .and_then(|items| items.get(i?))
                .and_then(|item| item.location_opt.clone())
        };
        [
            location(self.select_focus),
            location(self.select_range.map(|(anchor, _)| anchor)),
            location(self.select_range.map(|(_, end)| end)),
        ]
    }

    /// Puts the focus and the shift range back on the items
    /// [`Self::selection_marks`] named. One that is gone is forgotten; a
    /// range whose anchor is gone goes with it.
    fn restore_selection_marks(&mut self, [focus, anchor, end]: [Option<Location>; 3]) {
        let find = |location: Option<Location>| {
            let location = location?;
            self.items_opt
                .as_ref()?
                .iter()
                .position(|item| item.location_opt.as_ref() == Some(&location))
        };
        let (focus, anchor, end) = (find(focus), find(anchor), find(end));
        self.select_focus = focus;
        self.select_range = anchor.map(|anchor| (anchor, end.unwrap_or(anchor)));
    }

    pub fn select_none(&mut self) -> bool {
        // What the menus read of the selection is worked out again
        self.forget_selection();
        self.anchor_selection(None);
        let mut had_selection = false;
        if let Some(ref mut items) = self.items_opt {
            for item in items.iter_mut() {
                if self.selection.is_selected(item) {
                    self.selection.select(item, false);
                    had_selection = true;
                }
            }
        }
        had_selection
    }

    /// Whether the selection is still the search's own: nothing yet, or the
    /// top result it selected and nothing else. Once the user selects
    /// something, or nothing, it is theirs and stays put.
    pub(crate) fn search_selection_is_own(&self) -> bool {
        let selected = || {
            self.items_opt
                .iter()
                .flatten()
                .enumerate()
                .filter(|(_, item)| self.selection.is_selected(item))
                .map(|(i, _)| i)
        };
        match self.search_select {
            SearchSelect::Off => false,
            SearchSelect::Waiting => selected().next().is_none(),
            SearchSelect::Top(top) => {
                self.select_focus == Some(top) && selected().eq(std::iter::once(top))
            }
        }
    }

    /// Keeps the top result selected while the selection is still the
    /// search's own (see [`Self::search_selection_is_own`], asked before the
    /// listing changed), and gives it up for good once it is not.
    pub(crate) fn follow_search_top(&mut self, own: bool) {
        if own {
            self.select_search_top();
        } else {
            self.search_select = SearchSelect::Off;
        }
    }

    /// Selects the result shown first, as a click on it would.
    fn select_search_top(&mut self) {
        // What the menus read of the selection is worked out again
        self.forget_selection();
        let show_hidden = self.config.show_hidden;
        // A folder chooser selects folders only, as a click in it does
        let folders_only = matches!(&self.mode, Mode::Dialog(dialog) if dialog.is_dir());
        let Some(top) = self.column_sort().and_then(|items| {
            items
                .into_iter()
                .find(|(_, item)| {
                    (show_hidden || !item.hidden) && (!folders_only || item.metadata.is_dir())
                })
                .map(|(i, _)| i)
        }) else {
            // Nothing to select, yet or any more: newer results may have
            // pushed the one selected past the cap. Still the search's to
            // choose once something eligible arrives.
            if let Some(items) = &mut self.items_opt {
                for item in items.iter_mut() {
                    self.selection.select(item, false);
                }
            }
            self.select_focus = None;
            self.select_range = None;
            self.search_select = SearchSelect::Waiting;
            return;
        };
        if let Some(items) = &mut self.items_opt {
            for (i, item) in items.iter_mut().enumerate() {
                self.selection.select(item, i == top);
            }
        }
        self.select_focus = Some(top);
        self.select_range = Some((top, top));
        self.search_select = SearchSelect::Top(top);
    }

    pub fn select_name(&mut self, name: &str) {
        // What the menus read of the selection is worked out again
        self.forget_selection();
        let mut found = None;
        if let Some(ref mut items) = self.items_opt {
            for (i, item) in items.iter_mut().enumerate() {
                self.selection.select(item, item.name == name);
                if self.selection.is_selected(item) {
                    found = Some(i);
                }
            }
        }
        self.anchor_selection(found);
    }

    /// Selects the first item whose name starts with the given prefix (case-insensitive).
    /// Returns true if an item was selected. The item found is where a shift
    /// selection starts.
    pub fn select_by_prefix(&mut self, prefix: &str) -> bool {
        let found = self.select_by_prefix_only(prefix);
        self.anchor_selection(self.select_focus);
        found
    }

    /// [`Self::select_by_prefix`], leaving the shift range alone.
    fn select_by_prefix_only(&mut self, prefix: &str) -> bool {
        // What the menus read of the selection is worked out again
        self.forget_selection();
        self.search_select = SearchSelect::Off;
        let prefix_lower = prefix.to_lowercase();
        let focus = self.select_focus.take();

        if let Some(ref mut items) = self.items_opt {
            // First, deselect all items
            for item in items.iter_mut() {
                self.selection.select(item, false);
            }

            // Determine the start index of the search. When the index is before the currently focused item, it will be
            // considered first, otherwise last. Consider the focused item last when only a single character has been
            // typed, so we eagerly switch focus on the first character and stay on the same item as long as the prefix
            // matches.
            let single_char = prefix_lower.chars().count() == 1;
            let start = if single_char {
                Self::index_after_focus(focus, self.sort_direction)
            } else {
                Self::index_before_focus(focus, self.sort_direction)
            };
            self.select_focus = Self::select_first_prefix_from_index(
                &prefix_lower,
                items,
                &mut self.selection,
                start,
                self.sort_direction,
            );

            if self.select_focus.is_some() || single_char {
                return self.select_focus.is_some();
            }

            let mut chars = prefix_lower.chars();
            let Some(first) = chars.next() else {
                log::error!("search term is empty");
                return self.select_focus.is_some();
            };

            // Check if all entered characters are the same
            if !chars.all(|c| c == first) {
                return self.select_focus.is_some();
            }

            // Search for a single character when all entered characters are the same.
            // This allows cycling through items starting with the same character by repeatedly pressing a key.
            let start = Self::index_after_focus(focus, self.sort_direction);
            self.select_focus = Self::select_first_prefix_from_index(
                &first.to_string(),
                items,
                &mut self.selection,
                start,
                self.sort_direction,
            );

            return self.select_focus.is_some();
        }
        false
    }

    fn index_before_focus(current_focus: Option<usize>, forward: bool) -> usize {
        current_focus.map_or(0, |i| if forward { i } else { i + 1 })
    }

    fn index_after_focus(current_focus: Option<usize>, forward: bool) -> usize {
        current_focus.map_or(0, |i| if forward { i + 1 } else { i })
    }

    fn select_first_prefix_from_index(
        prefix_lower: &str,
        items: &mut [Item],
        selection: &mut Selection,
        start: usize,
        forward: bool,
    ) -> Option<usize> {
        // Order the search item so they begin at `start`.
        let Some((until, after)) = items.split_at_mut_checked(start) else {
            log::error!(
                "invalid start index {start} for items of length {}",
                items.len()
            );
            return None;
        };
        let search_items = after
            .iter_mut()
            .enumerate()
            .map(|(i, item)| (i + start, item))
            .chain(until.iter_mut().enumerate());

        if forward {
            Self::select_first_prefix_match(prefix_lower, search_items, selection)
        } else {
            Self::select_first_prefix_match(prefix_lower, search_items.rev(), selection)
        }
    }

    /// Selects the first item in the given iterator whose name starts with the given prefix.
    ///
    /// The `prefix` must be lowercase.
    fn select_first_prefix_match<'a>(
        prefix: &str,
        items: impl Iterator<Item = (usize, &'a mut Item)>,
        selection: &mut Selection,
    ) -> Option<usize> {
        for (i, item) in items {
            if item.name.to_lowercase().starts_with(prefix) {
                selection.select(item, true);
                return Some(i);
            }
        }
        None
    }

    pub fn select_paths(&mut self, paths: Vec<PathBuf>) {
        // What the menus read of the selection is worked out again
        self.forget_selection();
        let mut found = None;
        if let Some(ref mut items) = self.items_opt {
            for (i, item) in items.iter_mut().enumerate() {
                self.selection.select(item, false);
                if let Some(path) = item.path_opt()
                    && paths.contains(path)
                {
                    self.selection.select(item, true);
                    found = Some(i);
                }
            }
        }
        self.anchor_selection(found);
    }

    fn select_position(&mut self, row: usize, col: usize, mod_shift: bool) -> bool {
        let mut start = (row, col);
        let mut end = (row, col);
        if mod_shift {
            let len = self.items_opt.as_ref().map_or(0, Vec::len);
            // A starting point in this listing is kept, focused or not (a
            // rectangle leaves one without focus); with none, the focused
            // item starts the range
            if self.select_range.is_none_or(|(anchor, _)| anchor >= len) {
                self.select_range = self.select_focus.filter(|&i| i < len).map(|i| (i, i));
            }

            if let Some(pos) = self.select_range_start_pos_opt() {
                if pos.0 < row || (pos.0 == row && pos.1 < col) {
                    start = pos;
                } else {
                    end = pos;
                }
            }
        }

        if let Some(found) = self.select_laid_out(start, end, (row, col), mod_shift) {
            return found;
        }

        // What the menus read of the selection is worked out again
        self.forget_selection();
        let mut found = false;
        if let Some(ref mut items) = self.items_opt {
            for (i, item) in items.iter_mut().enumerate() {
                self.selection.select(item, false);
                let pos = match item.pos_opt.get() {
                    Some(some) => some,
                    None => continue,
                };
                if pos.0 < start.0 || (pos.0 == start.0 && pos.1 < start.1) {
                    // Before start
                    continue;
                }
                if pos.0 > end.0 || (pos.0 == end.0 && pos.1 > end.1) {
                    // After end
                    continue;
                }
                if pos == (row, col) {
                    // Update focus if this is what we wanted to select
                    self.select_focus = Some(i);
                    // With no starting point yet, the item reached is one
                    let anchor = if mod_shift {
                        self.select_range.map_or(i, |(anchor, _)| anchor)
                    } else {
                        i
                    };
                    self.select_range = Some((anchor, i));
                    found = true;
                }
                self.selection.select(item, true);
            }
        }
        found
    }

    /// Where the item at `i` is, if there is one.
    fn location_at(&self, i: Option<usize>) -> Option<Location> {
        self.items_opt.as_ref()?.get(i?)?.location_opt.clone()
    }

    /// Forgets which items the last rectangle crossed, so the next one
    /// toggles only what it crosses itself.
    fn clear_drag_overlaps(&mut self) {
        for item in self.items_opt.iter().flatten() {
            self.selection.set_dragged_over(item, false);
        }
    }

    pub fn select_rect(&mut self, rect: Rectangle, mod_ctrl: bool, mod_shift: bool) {
        // What the menus read of the selection is worked out again
        self.forget_selection();
        if let Some(ref mut items) = self.items_opt {
            for item in items.iter_mut() {
                let was_overlapped = self.selection.is_dragged_over(item);
                let overlapped = item.rect_opt.get().is_some_and(|r| r.intersects(&rect));
                self.selection.set_dragged_over(item, overlapped);

                self.selection.select(
                    item,
                    if mod_ctrl || mod_shift {
                        if was_overlapped == overlapped {
                            self.selection.is_selected(item)
                        } else {
                            !self.selection.is_selected(item)
                        }
                    } else {
                        overlapped
                    },
                );
            }
        }
    }

    /// The paths a file drag out of this tab carries: everything selected that
    /// exists on disk, or the item the drag started on when nothing is.
    fn drag_paths(&self, started_on: usize) -> Vec<PathBuf> {
        let Some(items) = self.items_opt.as_ref() else {
            return Vec::new();
        };
        let selected: Vec<_> = items
            .iter()
            .filter(|item| self.selection.is_selected(item))
            .filter_map(|item| item.path_opt().cloned())
            .collect();
        if selected.is_empty() {
            items
                .get(started_on)
                .and_then(|item| item.path_opt().cloned())
                .into_iter()
                .collect()
        } else {
            selected
        }
    }

    /// The directory item a drag at `point` would drop into.
    ///
    /// `point` is in content coordinates, the space `grid_view` and `list_view`
    /// lay their `Item::rect_opt` out in: measured from the top of the whole
    /// listing, not from the top of what is on screen. See [`Self::scrolled_by`].
    /// Only directories accept drops; a drag over a file or a gap between rows
    /// drops into the displayed directory. So does a drag over what this tab
    /// is itself dragging: let go where it started, it is put back rather than
    /// into one of its folders.
    fn drop_target(&self, point: Point) -> Option<usize> {
        self.items_opt.as_ref()?.iter().position(|item| {
            item.metadata.is_dir()
                && item
                    .path_opt()
                    .is_some_and(|path| !self.dnd_dragged.contains(path))
                && item.rect_opt.get().is_some_and(|rect| rect.contains(point))
        })
    }

    /// A file drag is at `position` over the list, measured from the top of
    /// what is on screen: highlight the folder it would drop into, time a
    /// hold over it, and scroll when it is near an edge.
    fn drag_over_list(&mut self, position: Point) -> Vec<Command> {
        self.dnd_position = Some(position);
        let scrolled_by = self.scrolled_by();
        let target = self.drop_target(Point::new(
            position.x + scrolled_by.x,
            position.y + scrolled_by.y,
        ));
        if target != self.dnd_target {
            self.highlight_drop_target(target);
        }
        let held = target
            .and_then(|i| self.items_opt.as_ref()?.get(i)?.path_opt().cloned())
            .map(DragHover::Item);
        let mut commands: Vec<Command> = self.set_drag_hover(held).into_iter().collect();
        let height = self
            .item_view_size_opt
            .get()
            .map_or(0.0, |size| size.height);
        commands.extend(self.set_drag_auto_scroll(drag_edge_scroll(position.y, height)));
        commands
    }

    /// Scroll at `speed` for a file drag near an edge, or stop.
    fn set_drag_auto_scroll(&mut self, speed: Option<f32>) -> Option<Command> {
        (speed != self.dnd_auto_scroll).then(|| {
            self.dnd_auto_scroll = speed;
            Command::AutoScroll(speed)
        })
    }

    /// The drag is now held over `hover` (`None`: over nothing that opens).
    /// A change starts the timer again: after [`crate::ui::dnd::HOVER_OPEN`]
    /// the timer's message opens it, unless it has changed again by then.
    fn set_drag_hover(&mut self, hover: Option<DragHover>) -> Option<Command> {
        if hover == self.drag_hover {
            return None;
        }
        let starts = hover.is_some();
        self.drag_hover = hover;
        self.drag_hover_generation += 1;
        let generation = self.drag_hover_generation;
        starts.then(|| {
            Command::Iced(
                // Made when the task runs, as it needs the runtime
                crate::ui::iced::Task::future(async {
                    tokio::time::sleep(crate::ui::dnd::HOVER_OPEN).await;
                })
                .map(move |()| Message::DragHoverElapsed(generation))
                .into(),
            )
        })
    }

    /// Clear the drag indicators and end the drag.
    ///
    /// Each watching widget calls this once. The file list and every breadcrumb segment
    /// see the same `ended` state in one event batch, so repeated calls must be safe.
    /// `ui::dnd::end_drag` supports this and retains the offer until the pending drop
    /// read starts.
    fn end_file_drag(&mut self) {
        if self.dnd_target.is_some() {
            self.highlight_drop_target(None);
        }
        self.dnd_ancestor = None;
        self.dnd_source = false;
        self.dnd_dragged.clear();
        self.dnd_position = None;
        self.drag_hover = None;
        self.drag_hover_generation += 1;
        crate::ui::dnd::end_drag();
    }

    /// Draw exactly `target` as the drop target, and nothing else.
    fn highlight_drop_target(&mut self, target: Option<usize>) {
        self.dnd_target = target;
        if let Some(items) = self.items_opt.as_mut() {
            for (i, item) in items.iter_mut().enumerate() {
                self.selection.highlight(item, Some(i) == target);
            }
        }
        // Nothing else is highlighted now
        self.highlight_marks.clear();
        self.highlight_marks.extend(target);
    }

    pub fn select_focus_id(&self) -> Option<widget::Id> {
        let items = self.items_opt.as_ref()?;
        let item = items.get(self.select_focus?)?;
        Some(item.button_id.clone())
    }

    fn select_focus_pos_opt(&self) -> Option<(usize, usize)> {
        let items = self.items_opt.as_ref()?;
        let item = items.get(self.select_focus?)?;
        item.pos_opt.get()
    }

    /// Takes every highlight away. Every place that highlights an item
    /// notes it in `highlight_marks`, so only those are visited: each arrow
    /// press does this, and a pass over every item is milliseconds in a
    /// large folder.
    fn dehighlight_all(&mut self) {
        if let Some(items) = self.items_opt.as_mut() {
            for index in self.highlight_marks.drain(..) {
                if let Some(item) = items.get_mut(index) {
                    self.selection.highlight(item, false);
                }
            }
            // A highlight made without a note shows here
            #[cfg(test)]
            assert!(
                items
                    .iter()
                    .all(|item| !self.selection.is_highlighted(item)),
                "a highlight was left unnoted"
            );
        }
        self.highlight_marks.clear();
    }

    /// Whether the list can follow a drawer slide by moving its right column
    /// and the sort bar's order button alone: list view, not a search, at
    /// least one row shown (an empty list centres its empty-state message,
    /// which would re-centre with a jump when a column slide ends), and the
    /// sort bar laid out the same way and the rows' right column shown now
    /// and once the drawer has taken (or given back) `extent`, so that only
    /// the names' room changes.
    /// `size_opt` is the width of the last layout, so this assumes no tab
    /// switch or resize since; if one happened in the same update, at worst
    /// one slide runs on the wrong path and the relayout at its end corrects
    /// it. See `crate::ui::shell::panel_slide`.
    #[must_use]
    pub fn column_slide_fits(&self, extent: f32, opening: bool) -> bool {
        if self.config.view != View::List || self.is_search() {
            return false;
        }
        let show_hidden = self.config.show_hidden;
        let shows_a_row = self
            .items_opt()
            .is_some_and(|items| items.iter().any(|item| !item.hidden || show_hidden));
        if !shows_a_row {
            return false;
        }
        let Some(width) = self.size_opt.get().map(|size| size.width) else {
            return false;
        };
        let then = if opening {
            width - extent
        } else {
            width + extent
        };
        then > 0.0
            && self.sort_labels_fit(width) == self.sort_labels_fit(then)
            && self.shows_right_column(width)
            && self.shows_right_column(then)
    }

    /// Whether list rows in a view `width` wide have their right column.
    fn shows_right_column(&self, width: f32) -> bool {
        let (sort_name, _, _) = self.sort_options();
        right_column_fits(width, self.config.icon_sizes.list(), sort_name)
    }

    /// Whether the tab is showing search results, for the row shape (a
    /// simpler layout, path shown under the name) and the drawer slide
    /// (columns never follow, since a search's Name is `Length::Fill` next
    /// to the fixed columns rather than shrinking with them). An empty
    /// search that lists its folder is shown as the folder is.
    fn is_search(&self) -> bool {
        matches!(self.location, Location::Search(..))
            && self.location.empty_search_folder().is_none()
    }

    /// The part of the item view currently on screen, for a view of `size`.
    /// The scroll offset is clamped with the cached content height so a
    /// stale offset after a resize does not point past the end.
    fn visible_rect(&self, size: Size) -> Rectangle {
        let max_scroll_y = self
            .content_height_opt
            .get()
            .map(|ch| (ch - size.height).max(0.0))
            .unwrap_or(f32::MAX);
        let scroll_y = self
            .scroll_opt
            .map(|o| o.y.min(max_scroll_y).max(0.0))
            .unwrap_or(0.0);
        Rectangle::new(Point::new(0.0, scroll_y), size)
    }

    /// How far the listing is scrolled: what to add to a position measured from
    /// the top of the visible area to get a content coordinate.
    fn scrolled_by(&self) -> Point {
        self.visible_rect(self.item_view_size_opt.get().unwrap_or_default())
            .position()
    }

    /// Position of the first item on screen, where keyboard navigation starts
    /// when nothing is selected.
    fn first_visible_pos_opt(&self) -> Option<(usize, usize)> {
        let visible_rect = self.visible_rect(self.item_view_size_opt.get().unwrap_or_default());
        self.items_opt
            .as_ref()?
            .iter()
            .filter(|item| {
                item.rect_opt
                    .get()
                    .is_some_and(|rect| rect.intersects(&visible_rect))
            })
            .filter_map(|item| item.pos_opt.get())
            .min()
    }

    pub(crate) fn select_focus_scroll(&mut self) -> Option<AbsoluteOffset> {
        let items = self.items_opt.as_ref()?;
        let item = items.get(self.select_focus?)?;
        let rect = item.rect_opt.get()?;

        let visible_rect = self.visible_rect(self.item_view_size_opt.get().unwrap_or_default());

        if rect.y < visible_rect.y {
            // Scroll up to rect
            self.scroll_opt = Some(AbsoluteOffset { x: 0.0, y: rect.y });
            self.scroll_opt
        } else if (rect.y + rect.height) > (visible_rect.y + visible_rect.height) {
            // Scroll down to rect
            self.scroll_opt = Some(AbsoluteOffset {
                x: 0.0,
                y: rect.y + rect.height - visible_rect.height,
            });
            self.scroll_opt
        } else {
            // Do not scroll
            None
        }
    }

    fn rows_per_page(&self) -> usize {
        let viewport_height = self.item_view_size_opt.get().map_or(0.0, |s| s.height);
        let row_height = self
            .select_focus
            .and_then(|i| self.items_opt.as_ref()?.get(i))
            .and_then(|item| item.rect_opt.get())
            .map_or(0.0, |r| r.height);
        if row_height > 0.0 && viewport_height > 0.0 {
            (viewport_height / row_height).floor() as usize
        } else {
            1
        }
    }

    fn select_range_start_pos_opt(&self) -> Option<(usize, usize)> {
        let items = self.items_opt.as_ref()?;
        let item = items.get(self.select_range.map(|r| r.0)?)?;
        item.pos_opt.get()
    }

    fn select_first_pos_opt(&self) -> Option<(usize, usize)> {
        let items = self.items_opt.as_ref()?;
        let mut first = None;
        for item in self
            .selected_indices()
            .iter()
            .filter_map(|&index| items.get(index))
        {
            let (row, col) = match item.pos_opt.get() {
                Some(some) => some,
                None => continue,
            };

            first = Some(match first {
                Some((first_row, first_col)) => match row.cmp(&first_row) {
                    Ordering::Less => (row, col),
                    Ordering::Equal => (row, col.min(first_col)),
                    Ordering::Greater => (first_row, first_col),
                },
                None => (row, col),
            });
        }
        first
    }

    /// [`Self::select_position`] through the current layout: the items
    /// from `start` to `end` are selected and the one at `target` focused,
    /// touching only them and what was selected before, rather than every
    /// item. `None` when there is no layout of the current order to go by.
    fn select_laid_out(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        target: (usize, usize),
        mod_shift: bool,
    ) -> Option<bool> {
        let layout = self.shown_layout.borrow().clone()?;
        let current = self
            .sort_cache
            .borrow()
            .as_ref()
            .is_some_and(|(_, order)| Arc::ptr_eq(order, &layout.key.order));
        if !current {
            return None;
        }
        // What was selected, to be unselected: kept from the last press
        let previous = self.selected_indices();
        let items = self.items_opt.as_mut()?;
        let (cols, len) = (layout.columns.max(1), layout.shown.len());
        // A column past the row's end comes after the whole row, as the
        // positions compare
        let first = |(row, col): (usize, usize)| {
            if col >= cols {
                (row + 1) * cols
            } else {
                row * cols + col
            }
        };
        let last = |(row, col): (usize, usize)| {
            if col >= cols {
                (row + 1) * cols
            } else {
                row * cols + col + 1
            }
        };
        let range = first(start).min(len)..last(end).min(len);

        for &index in previous.iter() {
            if let Some(item) = items.get_mut(index) {
                self.selection.select(item, false);
            }
        }
        let mut selected: Vec<usize> = layout.shown[range.clone()].to_vec();
        for &index in &selected {
            self.selection.select(&items[index], true);
        }
        let target = (target.1 < cols)
            .then(|| target.0 * cols + target.1)
            .filter(|position| range.contains(position));
        if let Some(position) = target {
            let i = layout.shown[position];
            self.select_focus = Some(i);
            // With no starting point yet, the item reached is one
            let anchor = if mod_shift {
                self.select_range.map_or(i, |(anchor, _)| anchor)
            } else {
                i
            };
            self.select_range = Some((anchor, i));
        }
        // What it selected is known: kept, so the next press need not look
        selected.sort_unstable();
        self.selection_cache.replace(Some(selected.into()));
        Some(target.is_some())
    }

    fn select_last_pos_opt(&self) -> Option<(usize, usize)> {
        let items = self.items_opt.as_ref()?;
        let mut last = None;
        for item in self
            .selected_indices()
            .iter()
            .filter_map(|&index| items.get(index))
        {
            let (row, col) = match item.pos_opt.get() {
                Some(some) => some,
                None => continue,
            };

            last = Some(match last {
                Some((last_row, last_col)) => match row.cmp(&last_row) {
                    Ordering::Greater => (row, col),
                    Ordering::Equal => (row, col.max(last_col)),
                    Ordering::Less => (last_row, last_col),
                },
                None => (row, col),
            });
        }
        last
    }

    fn trigger_async_decode(&mut self) -> Vec<Command> {
        // Only trigger decode in gallery mode for the currently selected image
        if !self.gallery {
            return Vec::new();
        }

        let Some(index) = self.select_focus else {
            return Vec::new();
        };

        let Some(items) = &self.items_opt else {
            return Vec::new();
        };

        let Some(item) = items.get(index) else {
            return Vec::new();
        };

        let Some(ItemThumbnail::Image(_, original_dims)) = &item.thumbnail_opt else {
            return Vec::new();
        };

        if let Some((w, h)) = original_dims
            && !should_use_tiling(*w, *h)
        {
            return Vec::new();
        }

        let Some(path) = item.path_opt() else {
            return Vec::new();
        };

        // Clone path to avoid borrow checker issues
        let path = path.to_path_buf();

        // Get display size for adaptive resolution
        let display_dimensions = self
            .size_opt
            .get()
            .map(|size| (size.width as u32, size.height as u32));

        // Try to decode the image using LargeImageManager with adaptive resolution
        if let Some(StartDecode {
            target: target_dimensions,
            generation,
        }) = self
            .large_image_manager
            .try_decode(&path, display_dimensions)
        {
            vec![Command::Iced(
                crate::ui::iced::Task::perform(
                    decode_large_image(
                        path.clone(),
                        target_dimensions,
                        self.large_image_manager.decode_order(),
                    ),
                    move |result| match result {
                        Decoded::Done((path, width, height, pixels, stamp)) => {
                            Message::ImageDecoded(
                                path,
                                width,
                                height,
                                pixels,
                                display_dimensions,
                                generation,
                                stamp,
                            )
                        }
                        // Asked for again, which decodes the new file
                        Decoded::Changed => Message::ImageDecodeChanged(path.clone(), generation),
                        // Said, so that the image stops showing as loading
                        Decoded::Failed => Message::ImageDecodeFailed(path.clone(), generation),
                        Decoded::Superseded => {
                            Message::ImageDecodeSuperseded(path.clone(), generation)
                        }
                    },
                )
                .into(),
            )]
        } else {
            Vec::new()
        }
    }

    /// Put the address field away, and withdraw whatever it was asking for.
    ///
    /// The two belong together. A network address being resolved is a question
    /// the field asked, so every way of dismissing the field -- typing
    /// elsewhere, clicking away, Escape, navigating -- has to withdraw it too,
    /// or a slow answer arrives long after and takes the tab somewhere the
    /// user has left. Clearing `edit_location` on its own is what let that
    /// happen, so it is done here and nowhere else.
    pub fn dismiss_edit_location(&mut self) {
        self.edit_location = None;
        self.resolving_network = None;
    }

    /// As [`Self::change_location`], but a location that lists the same
    /// folder, as an empty search of it does, keeps the listing on screen
    /// until the rescan replaces it, instead of going blank for a frame. The
    /// kept listing has nothing selected, as the rescan's own would not.
    pub fn change_location_keeping_listing(&mut self, location: &Location) {
        // What the menus read of the selection is worked out again
        self.forget_selection();
        let kept = if self.location.lists_same_as(location) {
            self.items_opt.take()
        } else {
            None
        };
        self.change_location(location, None);
        if let Some(items) = kept {
            self.selection.clear();
            self.items_opt = Some(items);
        }
    }

    /// Whether going to `location` only opens the empty search of the folder
    /// on screen, or closes it again: what is listed stays the same.
    pub fn only_switches_search(&self, location: &Location) -> bool {
        self.items_opt.is_some()
            && matches!(self.location, Location::Path(_)) != matches!(location, Location::Path(_))
            && self.location.lists_same_as(location)
    }

    /// Goes to `location`, which [`Self::only_switches_search`] said lists
    /// what is on screen. The listing stays as it is, with everything already
    /// counted, typed and drawn for it, and needs no rescan. As a rescan
    /// would, it leaves nothing selected but the top of an empty search.
    /// The folder names are the ones the last rescan read from the
    /// filesystem, so the title and path bar do not change either.
    pub fn switch_search(&mut self, location: &Location) {
        // What the menus read of the selection is worked out again
        self.forget_selection();
        let names: Vec<String> = self
            .location_ancestors
            .iter()
            .map(|(_, name)| name.clone())
            .collect();
        self.change_location_inner(location, None, false);
        if let Some((ancestors, title)) = self.named_folders(names) {
            self.location_ancestors = ancestors;
            self.location_title = title;
        }
        self.selection.clear();
        self.follow_search_top(self.search_selection_is_own());
    }

    /// The breadcrumbs and title of this location, named with `names` (the
    /// folder's own first) as read from the filesystem for this same folder
    /// in either mode, a folder or its empty search. The breadcrumbs lead to
    /// this location's own kind: a folder's to folders, a search's to
    /// searches. `None` if the names do not fit the path.
    pub fn named_folders(&self, names: Vec<String>) -> Option<(Vec<(Location, String)>, String)> {
        let mut ancestors = self.location.ancestors(false);
        if names.len() != ancestors.len() || names.is_empty() {
            return None;
        }
        for ((_, name), read) in ancestors.iter_mut().zip(names) {
            *name = read;
        }
        let title = self.location.title_with(|_| ancestors[0].1.clone());
        Some((ancestors, title))
    }

    /// Numbers a folder read started for this tab. Only the latest one
    /// started may be shown: an earlier one that lands after it, or before
    /// it, describes the folder as it was, and its arrival does not mean the
    /// latest read is done.
    pub fn start_scan(&mut self) -> u64 {
        self.scans = self.scans.wrapping_add(1);
        self.scans
    }

    /// Takes `canonical` as the name of the folder `requested` named, if
    /// the tab is still there: for a location gone to as given, which only
    /// the rescan's worker could make canonical. The current history entry
    /// takes it too.
    pub fn adopt_canonical(&mut self, requested: &Location, canonical: &Location) {
        if self.location == *canonical || !self.location.lists_same_as(requested) {
            return;
        }
        let Some(path) = canonical.listed_folder().or_else(|| canonical.path_opt()) else {
            return;
        };
        let adopted = self.location.with_path(path.clone());
        if let Some(entry) = self.history.get_mut(self.history_i)
            && *entry == self.location
        {
            *entry = adopted.clone();
        }
        self.location = adopted;
    }

    /// Whether `scan` is the latest read [`Self::start_scan`] numbered.
    pub fn is_latest_scan(&self, scan: u64) -> bool {
        scan == self.scans
    }

    pub fn change_location(&mut self, location: &Location, history_i_opt: Option<usize>) {
        self.change_location_inner(location, history_i_opt, true);
    }

    /// As [`Self::change_location`]; without `new_listing`, what is listed
    /// stays, with its scroll offset.
    fn change_location_inner(
        &mut self,
        location: &Location,
        history_i_opt: Option<usize>,
        new_listing: bool,
    ) {
        // Made canonical by the rescan that follows, off this thread
        self.location = location.normalize_lexically();
        // A location still being resolved for an earlier step is not where
        // this one goes
        self.resolving_location = None;
        self.steps = self.steps.wrapping_add(1);
        // Named from the path alone, so the tab turns at once even when the
        // folder is on a mount that has stopped answering; the rescan that
        // follows names them from the filesystem, off this thread.
        self.location_ancestors = self.location.ancestors(false);
        self.location_title = self.location.title(false);
        // Going somewhere answers the question the address field was asking.
        self.dismiss_edit_location();
        if new_listing {
            self.new_listing();
        }
        self.navigation = self.navigation.wrapping_add(1);
        if new_listing {
            self.items_opt = None;
        }
        // Remember where this entry was scrolled to before leaving it
        if let Some(saved) = self.history_scroll.get_mut(self.history_i) {
            *saved = self.scroll_opt;
        }
        if new_listing {
            self.scroll_opt = None;
        }
        self.pending_scroll = None;
        // Positions in the old listing, and clicks still being handled on it,
        // mean nothing in the new one
        self.anchor_selection(None);
        self.clicked = None;
        self.narrow_on_release = None;
        self.last_right_click = None;
        self.search_context = None;
        self.search_select = if matches!(self.location, Location::Search(..)) {
            SearchSelect::Waiting
        } else {
            SearchSelect::Off
        };
        if let Some(history_i) = history_i_opt {
            // Navigating in history
            self.history_i = history_i;
            if let Some(offset) = self.history_scroll.get(history_i).copied().flatten() {
                self.scroll_opt = Some(offset);
                self.pending_scroll = Some(offset);
            }
        } else {
            // Truncate history to remove next entries
            self.history.truncate(self.history_i + 1);
            self.history_scroll.truncate(self.history_i + 1);

            // Compact consecutive matching paths
            {
                let mut remove = false;
                if let Some(last_location) = self.history.last() {
                    if let Location::Network(last_uri, ..) = last_location
                        && let Location::Network(uri, ..) = location
                    {
                        remove = last_uri == uri;
                    } else if let Some(last_path) = last_location.path_opt()
                        && let Some(path) = location.path_opt()
                    {
                        remove = last_path == path;
                    }
                }
                if remove {
                    self.history.pop();
                    self.history_scroll.pop();
                }
            }

            // Push to the front of history
            self.history_i = self.history.len();
            self.history.push(location.clone());
            self.history_scroll.push(None);
        }
    }

    pub fn update(&mut self, message: Message, modifiers: Modifiers) -> Vec<Command> {
        // Sorting a large folder takes tens of milliseconds, and the view
        // asks for the order every frame. It is kept across the messages
        // that cannot change what it sorts by; any other may have, before
        // or while it is handled.
        let keeps_order = message.keeps_order();
        if !keeps_order {
            self.invalidate_order();
        }
        if !message.keeps_selection() {
            self.forget_selection();
        }
        let commands = self.update_inner(message, modifiers);
        if !keeps_order {
            self.invalidate_order();
        }
        commands
    }

    /// Forgets the kept order, for a change to what it sorts by.
    pub(crate) fn invalidate_order(&self) {
        self.sort_cache.replace(None);
    }

    /// The indices of the selected items, in listing order.
    ///
    /// The menus are made on every frame and each asks what is selected;
    /// in a large folder a pass over every item is milliseconds, so the
    /// answer is kept. Everything that changes the selection forgets it
    /// first: [`Self::update`], except for the messages that keep it, and
    /// each method that selects.
    pub fn selected_indices(&self) -> Arc<[usize]> {
        let fresh = || -> Arc<[usize]> {
            self.items_opt.as_ref().map_or_else(
                || Arc::from([]),
                |items| {
                    items
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| self.selection.is_selected(item))
                        .map(|(index, _)| index)
                        .collect()
                },
            )
        };
        let cached = self.selection_cache.borrow().clone();
        if let Some(cached) = cached {
            // Anything that selects without forgetting shows here
            #[cfg(test)]
            assert_eq!(*cached, *fresh(), "a stale selection was kept");
            return cached;
        }
        let indices = fresh();
        self.selection_cache.replace(Some(Arc::clone(&indices)));
        indices
    }

    /// Forgets the kept selection, for a change to it.
    pub(crate) fn forget_selection(&self) {
        self.selection_cache.replace(None);
    }

    fn update_inner(&mut self, message: Message, modifiers: Modifiers) -> Vec<Command> {
        let mut commands = Vec::new();
        let mut cd = None;
        // For a step to a file: what to do with it, and where to go if the
        // step's location is not there
        let mut cd_on_file = OnFile::Select;
        let mut cd_fallback = None;
        let mut history_i_opt = None;
        let mod_ctrl = modifiers.contains(Modifiers::CTRL) && self.mode.multiple();
        let mod_shift = modifiers.contains(Modifiers::SHIFT) && self.mode.multiple();
        // The user choosing, even the result the search chose for them, ends
        // the search's choosing: no later result may take it from them.
        if matches!(
            message,
            Message::Click(_)
                | Message::DoubleClick(_)
                | Message::RightClick(..)
                | Message::MiddleClick(..)
                | Message::Drag(_)
                | Message::ItemDown
                | Message::ItemLeft
                | Message::ItemPageDown
                | Message::ItemPageUp
                | Message::ItemRight
                | Message::ItemUp
                | Message::SelectAll
                | Message::SelectFirst
                | Message::SelectLast
                | Message::GalleryPrevious
                | Message::GalleryNext
        ) {
            self.search_select = SearchSelect::Off;
        }
        match message {
            Message::AddNetworkDrive => {
                commands.push(Command::AddNetworkDrive);
            }
            Message::AutoScroll(auto_scroll) => {
                commands.push(Command::AutoScroll(auto_scroll));
            }
            Message::ClickRelease(click_i_opt) => {
                'release: {
                    // Single click to open, once per click sequence: the release
                    // that completes a double click must not open it again
                    let already_opened = std::mem::take(&mut self.opened_by_this_click);
                    // Something else now where the press was is not clicked: the
                    // press, and all it was waiting to do, is dropped
                    if std::mem::take(&mut self.pressed_on) != self.location_at(click_i_opt) {
                        self.clicked = None;
                        self.narrow_on_release = None;
                        break 'release;
                    }
                    // Ctrl and Shift choose what is selected; they do not open
                    if !mod_ctrl && !mod_shift && self.config.single_click && !already_opened {
                        let mut paths_to_open = Vec::new();
                        if let Some(ref mut items) = self.items_opt {
                            for (i, item) in items.iter_mut().enumerate() {
                                if Some(i) == click_i_opt {
                                    if let Some(location) = &item.location_opt {
                                        if item.metadata.is_dir() {
                                            cd = Some(location.clone());
                                        } else if let Some(path) = location.path_opt() {
                                            paths_to_open.push(path.clone());
                                        } else {
                                            log::warn!("no path for item {item:?}");
                                        }
                                    } else {
                                        log::warn!("no location for item {item:?}");
                                    }
                                }
                            }
                        }
                        if !paths_to_open.is_empty() {
                            commands.push(Command::OpenFile(paths_to_open));
                        }
                    }

                    if let Some(narrow_to) = self.narrow_on_release.take()
                        && click_i_opt == Some(narrow_to)
                        && let Some(ref mut items) = self.items_opt
                    {
                        for (i, item) in items.iter_mut().enumerate() {
                            self.selection.select(item, i == narrow_to);
                        }
                        self.select_range = Some((narrow_to, narrow_to));
                    }

                    if click_i_opt != self.clicked.take()
                        && let Some(ref mut items) = self.items_opt
                    {
                        for (i, item) in items.iter_mut().enumerate() {
                            if mod_ctrl
                                && Some(i) == click_i_opt
                                && self.selection.is_selected(item)
                            {
                                self.selection.select(item, false);
                            }
                        }
                        if !items.iter().any(|item| self.selection.is_selected(item)) {
                            // The last one toggled off
                            self.anchor_selection(None);
                        }
                    }
                }
            }
            Message::DragHoverElapsed(generation) => {
                if generation == self.drag_hover_generation {
                    match self.drag_hover.take() {
                        Some(DragHover::Item(path)) => {
                            // The list may have been refreshed under a still
                            // pointer, moving the folder away: only open it if
                            // it is what the drag is over now
                            let scrolled_by = self.scrolled_by();
                            let under = self
                                .dnd_position
                                .map(|p| Point::new(p.x + scrolled_by.x, p.y + scrolled_by.y))
                                .and_then(|point| self.drop_target(point))
                                .and_then(|i| self.items_opt.as_ref()?.get(i));
                            if let Some(item) = under
                                && item.path_opt() == Some(&path)
                            {
                                cd = item.location_opt.clone();
                            } else if let Some(position) = self.dnd_position {
                                // Something else is there now: that is held
                                // over from here on
                                commands.extend(self.drag_over_list(position));
                            }
                        }
                        Some(DragHover::Ancestor(index)) => {
                            cd = self
                                .location_ancestors
                                .get(index)
                                .map(|(location, _)| location.clone());
                        }
                        None => {}
                    }
                }
            }
            Message::DragRearm => {
                self.watch_drag = true;
            }
            Message::DragEnd => {
                self.clicked = None;
                self.watch_drag = true;
                self.clear_drag_overlaps();
                // A rectangle may have changed the selection under the anchor
                self.settle_anchor();
            }
            Message::DoubleClick(click_i_opt) => {
                self.pressed_on = self.location_at(click_i_opt);
                // In single-click mode the first release already opened this
                // item, so the double click adds nothing but must still stop
                // the release behind it from opening it a third time
                self.opened_by_this_click = true;
                if !self.config.single_click
                    && let Some(clicked_item) = self
                        .items_opt
                        .as_ref()
                        .and_then(|items| click_i_opt.and_then(|click_i| items.get(click_i)))
                {
                    if let Some(location) = &clicked_item.location_opt {
                        if clicked_item.metadata.is_dir() {
                            cd = Some(location.clone());
                        } else if let Some(path) = location.path_opt() {
                            commands.push(Command::OpenFile(vec![path.clone()]));
                        } else {
                            log::warn!("no path for item {clicked_item:?}");
                        }
                    } else {
                        log::warn!("no location for item {clicked_item:?}");
                    }
                }
            }
            Message::Click(click_i_opt) => {
                self.dismiss_edit_location();
                self.narrow_on_release = None;
                self.pressed_on = self.location_at(click_i_opt);
                // A new click sequence: a double click whose release never
                // came does not stop this one opening
                self.opened_by_this_click = false;
                // Nor does an earlier rectangle say what this one crosses
                self.clear_drag_overlaps();
                if click_i_opt.is_none() {
                    self.clicked = click_i_opt;
                }

                if mod_shift {
                    if let Some(click_i) = click_i_opt {
                        // A sorted tab's items can't be linearly selected: the
                        // range runs between the two items as shown, not
                        // between their places in `items_opt`
                        let indices: Vec<usize> = self
                            .column_sort()
                            .map(|sorted| sorted.into_iter().map(|(i, _)| i).collect())
                            .unwrap_or_default();
                        // The anchor, where it is shown; with none, or one
                        // not in this listing, the range is the clicked item
                        // alone and starts there from now on
                        let anchor = self
                            .select_range
                            .map(|(anchor, _)| anchor)
                            .filter(|&anchor| indices.contains(&anchor))
                            .unwrap_or(click_i);
                        let shown = |i: usize| indices.iter().position(|&shown| shown == i);
                        if let (Some(from), Some(to)) = (shown(anchor), shown(click_i)) {
                            let in_range = &indices[from.min(to)..=from.max(to)];
                            self.select_range = Some((anchor, click_i));
                            if let Some(ref mut items) = self.items_opt {
                                // Plain shift-click reselects: whatever the
                                // previous, wider range covered is dropped, so
                                // the range can be shrunk as well as grown.
                                // Ctrl with shift keeps the earlier selection
                                // and adds to it.
                                if !mod_ctrl {
                                    for item in items.iter_mut() {
                                        self.selection.select(item, false);
                                    }
                                }
                                for &index in in_range {
                                    if let Some(item) = items.get_mut(index) {
                                        if item.hidden {
                                            if self.config.show_hidden {
                                                self.selection.select(item, true);
                                            }
                                        } else {
                                            self.selection.select(item, true);
                                        }
                                    }
                                }
                            }
                        }
                        self.clicked = click_i_opt;
                        self.select_focus = click_i_opt;
                    }
                } else {
                    // In a file chooser a folder replaces the selection, so that
                    // Open enters it instead of being blocked by a mixed selection
                    let clicked_folder_in_file_dialog = matches!(&self.mode, Mode::Dialog(dialog) if !dialog.is_dir())
                        && click_i_opt
                            .and_then(|i| self.items_opt.as_ref()?.get(i))
                            .is_some_and(|item| item.metadata.is_dir());
                    let dont_unset = !clicked_folder_in_file_dialog
                        && (mod_ctrl
                            || self.column_sort().is_some_and(|l| {
                                l.iter().any(|&(e_i, e)| {
                                    Some(e_i) == click_i_opt && self.selection.is_selected(e)
                                })
                            }));
                    if let Some(ref mut items) = self.items_opt {
                        for (i, item) in items.iter_mut().enumerate() {
                            if Some(i) == click_i_opt {
                                // Filter out selection if it does not match dialog kind
                                if let Mode::Dialog(dialog) = &self.mode {
                                    let item_is_dir = item.metadata.is_dir();
                                    if item_is_dir != dialog.is_dir() {
                                        // Allow selecting folder if dialog is for files to make it
                                        // possible to double click
                                        if !item_is_dir {
                                            continue;
                                        }
                                    }
                                }
                                if !self.selection.is_selected(item) {
                                    self.clicked = click_i_opt;
                                    self.selection.select(item, true);
                                } else if !mod_ctrl {
                                    self.narrow_on_release = click_i_opt;
                                }
                                self.select_range = Some((i, i));
                                self.select_focus = click_i_opt;
                            } else if !dont_unset && self.selection.is_selected(item) {
                                self.clicked = click_i_opt;
                                self.selection.select(item, false);
                            }
                        }
                    }
                    if click_i_opt.is_none() && !dont_unset {
                        // The background: nothing is selected, so nothing
                        // anchors a shift selection either
                        self.anchor_selection(None);
                    }
                }
            }
            Message::Config(config) => {
                // View is preserved for existing tabs
                let view = self.config.view;
                let show_hidden_changed = self.config.show_hidden != config.show_hidden;
                let recursive_changed = self.config.search_recursive != config.search_recursive;
                self.config = config;
                self.config.view = view;
                // Only the setting that actually changed reaches a running
                // search. A search carries the scope it was given, and the
                // config is the default for the next one rather than a
                // standing instruction to this one: without this, turning
                // hidden files on would also quietly widen a search that had
                // been deliberately narrowed.
                if (show_hidden_changed || recursive_changed)
                    && let Location::Search(path, term, current, ..) = &self.location
                {
                    let mut options = *current;
                    if show_hidden_changed {
                        options.show_hidden = self.config.show_hidden;
                    }
                    if recursive_changed {
                        options.recursive = self.config.search_recursive;
                    }
                    if options != *current {
                        cd = Some(Location::Search(
                            path.clone(),
                            term.clone(),
                            options,
                            Instant::now(),
                        ));
                    }
                }
                // Unhighlight all items when config changes
                self.dehighlight_all();
            }
            Message::ContextAction(action) => {
                commands.push(Command::Action(action));
            }
            Message::RunContextAction(action) => {
                commands.push(Command::RunContextAction(action));
            }
            Message::RightClickBackground => {
                self.dismiss_edit_location();

                if self.last_right_click.take().is_none()
                    && let Some(ref mut items) = self.items_opt
                {
                    for item in items.iter_mut() {
                        self.selection.select(item, false);
                    }
                    self.anchor_selection(None);
                }
            }
            Message::Surface(action) => {
                commands.push(Command::Surface(action));
            }
            Message::LocationContextMenuIndex(index) => {
                self.location_context_menu_index = index;
            }
            Message::LocationMenuAction(action) => {
                let path_for_index = |ancestor_index| {
                    self.location
                        .path_opt()
                        .and_then(|path| path.ancestors().nth(ancestor_index))
                        .map(Path::to_path_buf)
                };
                match action {
                    LocationMenuAction::OpenInNewTab(ancestor_index) => {
                        if let Some(path) = path_for_index(ancestor_index) {
                            commands.push(Command::OpenInNewTab(path));
                        }
                    }
                    LocationMenuAction::OpenInNewWindow(ancestor_index) => {
                        if let Some(path) = path_for_index(ancestor_index) {
                            commands.push(Command::OpenInNewWindow(path));
                        }
                    }
                    LocationMenuAction::Preview(ancestor_index) => {
                        if let Some(path) = path_for_index(ancestor_index) {
                            match item_from_path(&path, IconSizes::default()) {
                                Ok(item) => {
                                    commands.push(Command::Preview(PreviewKind::Custom(
                                        PreviewItem(Box::new(item)),
                                    )));
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
                    LocationMenuAction::AddToSidebar(ancestor_index) => {
                        if let Some(path) = path_for_index(ancestor_index) {
                            commands.push(Command::AddToSidebar(path));
                        } else {
                            log::warn!(
                                "no ancestor {ancestor_index} for location {:?}",
                                self.location
                            );
                        }
                    }
                }
            }
            Message::Drag(rect_opt) => {
                // Not every pixel of the gesture rebuilds the view: the next
                // report waits for the pointer to cross onto or off an item,
                // the list to scroll, or this to pass
                if std::mem::replace(&mut self.watch_drag, false) {
                    commands.push(Command::Iced(
                        crate::ui::Task::future(async {
                            tokio::time::sleep(DRAG_REARM).await;
                        })
                        .map(|()| Message::DragRearm)
                        .into(),
                    ));
                }
                self.narrow_on_release = None;
                if let Some(rect) = rect_opt {
                    if self.mode.multiple() {
                        self.select_rect(rect, mod_ctrl, mod_shift);
                    }
                    if self.select_focus.take().is_some() {
                        // Unfocus currently focused button
                        commands.push(Command::Iced(
                            widget::button::focus(widget::Id::unique()).into(),
                        ));
                    }
                }
            }
            Message::DragFiles(i) => {
                // The pressed selection is being dragged: it stays whole
                self.narrow_on_release = None;
                // `on_drag` fires again on every pixel of the gesture; only the
                // first one may start a drag.
                if !self.dnd_source {
                    let paths = self.drag_paths(i);
                    if !paths.is_empty() {
                        // Offer a copy for drops into other applications. For drops
                        // back into this app, the modifier held at the drop determines
                        // whether to move or copy; see `Message::Dnd`.
                        let contents = crate::clipboard::ClipboardCopy::new(
                            crate::clipboard::ClipboardKind::Copy,
                            &paths,
                        );
                        if crate::ui::dnd::start_drag_data(
                            Arc::new(contents),
                            crate::ui::dnd::LocalPayload::Paths(paths.clone()),
                        ) {
                            self.dnd_source = true;
                            log::debug!("started a file drag of {} path(s)", paths.len());
                            self.dnd_dragged = paths;
                        }
                    }
                }
            }
            Message::Dnd(dnd) => {
                // A Wayland drag reports surface-local coordinates, so
                // `dnd.position` is measured from the top of the visible area
                // while item rectangles are in content coordinates. Add back
                // what is scrolled out of sight above, or a drop after
                // scrolling tests whatever happens to sit at the same distance
                // from the top of the listing — a folder that may be offscreen.
                let scrolled_by = self.scrolled_by();
                let target = dnd
                    .position
                    .map(|point| Point::new(point.x + scrolled_by.x, point.y + scrolled_by.y))
                    .and_then(|point| self.drop_target(point));
                if !dnd.ended {
                    if let Some(position) = dnd.position {
                        commands.extend(self.drag_over_list(position));
                    } else {
                        // Off the list: no scrolling, and a hold timed here
                        // ends, though not one a crumb is timing
                        self.dnd_position = None;
                        if target != self.dnd_target {
                            self.highlight_drop_target(target);
                        }
                        commands.extend(self.set_drag_auto_scroll(None));
                        if matches!(self.drag_hover, Some(DragHover::Item(_))) {
                            commands.extend(self.set_drag_hover(None));
                        }
                    }
                }
                if dnd.ended {
                    commands.extend(self.set_drag_auto_scroll(None));
                    if dnd.dropped
                        && dnd.position.is_some()
                        && self.location.supports_paste()
                        && let Some(to) = target
                            .and_then(|i| self.items_opt.as_ref()?.get(i)?.path_opt().cloned())
                            .or_else(|| self.location.path_opt().cloned())
                    {
                        commands.push(Command::DropFiles(
                            to,
                            crate::ui::dnd::drop_copies(modifiers),
                        ));
                    }
                    self.end_file_drag();
                }
            }
            Message::DndAncestor(index, to, dnd) => {
                // No hit test: this message exists only because the drag is
                // inside that breadcrumb's own `MouseArea`, whose bounds are
                // the segment.
                if dnd.position.is_some() {
                    self.dnd_ancestor = Some(index);
                    // Held there, it opens; not the current folder's own crumb
                    if index > 0 && !dnd.ended {
                        commands.extend(self.set_drag_hover(Some(DragHover::Ancestor(index))));
                    }
                } else if self.dnd_ancestor == Some(index) {
                    // Every segment reports each change, but only the highlighted
                    // segment may clear its highlight. Otherwise, a segment the drag
                    // just left could clear the highlight set by the segment it entered.
                    self.dnd_ancestor = None;
                    if self.drag_hover == Some(DragHover::Ancestor(index)) {
                        commands.extend(self.set_drag_hover(None));
                    }
                }
                if dnd.ended {
                    if dnd.dropped && dnd.position.is_some() && self.location.supports_paste() {
                        commands.push(Command::DropFiles(
                            to,
                            crate::ui::dnd::drop_copies(modifiers),
                        ));
                    }
                    self.end_file_drag();
                }
            }
            Message::EditLocation(mut edit_location) => {
                // A keystroke's edit has no list yet; its own is looked up in
                // the background. Until it arrives the previous list stays on
                // show, inert, so the dropdown is not torn down and unrolled
                // again on every keystroke. Only where the typed text is the
                // path that is looked up: in a network share the field holds
                // a URI, the lookup stays keyed on the mount path, and no list
                // would come to replace the stale one. Putting the field away
                // carries nothing.
                if let Some(new) = &mut edit_location
                    && new.completions.is_none()
                    && matches!(
                        new.location,
                        Location::Path(_) | Location::Search(SearchLocation::Path(_), ..)
                    )
                {
                    new.stale = self
                        .edit_location
                        .take()
                        .and_then(|old| old.completions.or(old.stale));
                }
                // Typing something else, or putting the field away, withdraws
                // whatever it was last asked to resolve.
                self.dismiss_edit_location();
                self.edit_location = edit_location;
                if self.edit_location.is_some() {
                    commands.push(Command::Iced(
                        widget::text_input::focus(self.edit_location_id.clone()).into(),
                    ));
                }
            }
            Message::EditLocationComplete(selected) => {
                if let Some(mut edit_location) = self.edit_location.take()
                    && !matches!(edit_location.location, Location::Network(..))
                {
                    edit_location.selected = Some(selected);
                    cd = edit_location.resolve();
                }
            }
            // Swallowed on purpose; see the variant.
            Message::EditLocationStalePressed => {}
            Message::EditLocationEnable => {
                commands.push(Command::Iced(
                    widget::text_input::focus(self.edit_location_id.clone()).into(),
                ));
                self.edit_location = Some(self.location.clone().into());
            }
            Message::EditLocationSubmit => {
                if let Some(mut edit_location) = self.edit_location.take() {
                    let typed_opt = match &edit_location.location {
                        Location::Path(path) => path.to_str().map(str::to_string),
                        Location::Network(uri, ..) => Some(uri.clone()),
                        _ => None,
                    };
                    let mut typed_uri = false;
                    if let Some(typed) = typed_opt {
                        match typed.trim().parse::<url::Url>() {
                            Ok(url) if url.scheme() != "file" && url.has_host() => {
                                let uri = url.as_str().to_string();
                                edit_location = Location::Network(uri.clone(), uri, None)
                                    .normalize_lexically()
                                    .into();
                                typed_uri = true;
                            }
                            Err(_) if matches!(edit_location.location, Location::Network(..)) => {
                                edit_location = Location::Path(PathBuf::from(typed))
                                    .normalize_lexically()
                                    .into();
                            }
                            _ => {}
                        }
                    }

                    // The first completion, if what was typed is not there:
                    // the worker looks
                    if edit_location.selected.is_none()
                        && matches!(edit_location.location, Location::Path(_))
                    {
                        cd_fallback = edit_location
                            .completions
                            .as_ref()
                            .and_then(|completions| completions.first())
                            .map(|(_, path)| {
                                edit_location
                                    .location
                                    .with_path(path.clone())
                                    .normalize_lexically()
                            });
                    }

                    if let Some(uri) = edit_location.network_uri() {
                        // Resolving this means a round trip to GVFS. Ask for
                        // it and keep the field as it was typed, so the bar
                        // still shows where the user is going while it is
                        // being reached; the answer arrives as
                        // `NetworkResolved`.
                        let request = self.next_network_request;
                        self.next_network_request = self.next_network_request.wrapping_add(1);
                        self.resolving_network = Some(request);
                        // The newest step: one still being resolved is
                        // withdrawn, and whatever waits to send the tab
                        // somewhere can tell
                        self.resolving_location = None;
                        self.steps = self.steps.wrapping_add(1);
                        commands.push(Command::ResolveNetwork(request, uri.to_owned()));
                        self.edit_location = Some(edit_location);
                    } else {
                        cd = edit_location.resolve();
                        if cd.is_none() && typed_uri {
                            cd = Some(edit_location.location);
                        }
                    }
                }
            }
            Message::SetSearchRecursive(recursive) => {
                // Told to this search, not just written to the config. The
                // config may already say this -- a search restored from
                // history can disagree with a config that never changed --
                // and then nothing else would carry the change across.
                if let Location::Search(path, term, options, ..) = &self.location
                    && options.recursive != recursive
                {
                    let mut options = *options;
                    options.recursive = recursive;
                    cd = Some(Location::Search(
                        path.clone(),
                        term.clone(),
                        options,
                        Instant::now(),
                    ));
                }
            }
            Message::NetworkResolved(request, uri, resolved) => {
                // Only the answer to the question still being asked. The user
                // may have typed something else, or gone somewhere else
                // entirely, while the server was being waited on.
                if self.resolving_network == Some(request) {
                    self.dismiss_edit_location();
                    // Nothing came back, so go to the URI as typed and let the
                    // scan report what is wrong with it. That is what
                    // submitting an unresolvable URI has always done.
                    cd =
                        Some(resolved.unwrap_or_else(|| Location::Network(uri.clone(), uri, None)));
                }
            }
            Message::LocationResolved(request, resolved) => {
                // Only the answer to the step still being taken: going
                // anywhere since withdrew it
                if self
                    .resolving_location
                    .as_ref()
                    .is_some_and(|(asked, ..)| *asked == request)
                {
                    self.resolving_location = None;
                    self.go_to(resolved, &mut commands);
                }
            }
            Message::EditLocationTab => {
                if let Some(edit_location) = &mut self.edit_location {
                    edit_location.select(!modifiers.contains(Modifiers::SHIFT));
                }
            }
            Message::OpenInNewTab(path) => {
                commands.push(Command::OpenInNewTab(path));
            }
            Message::ClearRecents => {
                commands.push(Command::ClearRecents);
            }
            Message::EmptyTrash => {
                commands.push(Command::EmptyTrash);
            }
            Message::ExecEntryAction(path, action) => {
                let lang_id = crate::localize::LANGUAGE_LOADER.current_language();
                let language = lang_id.language.as_str();
                match path.map_or_else(
                    || {
                        let items = self.items_opt.as_deref()?;
                        items
                            .iter()
                            .find(|&item| self.selection.is_selected(item))
                            .and_then(|item| {
                                let location = item.location_opt.as_ref()?;
                                let path = location.path_opt()?;
                                crate::desktop_entry::load_desktop_file(
                                    &[language.into()],
                                    path.into(),
                                )
                            })
                    },
                    |path| crate::desktop_entry::load_desktop_file(&[language.into()], path),
                ) {
                    Some(entry) => commands.push(Command::ExecEntryAction(entry, action)),
                    None => log::warn!("Invalid desktop entry path passed to ExecEntryAction"),
                }
            }
            Message::Gallery(gallery) => {
                self.gallery = gallery;

                if gallery {
                    commands.extend(self.trigger_async_decode());
                }
            }
            Message::GalleryPrevious | Message::GalleryNext => {
                let mut pos_opt = None;
                if let Some(mut indices) = self.column_sort() {
                    if matches!(message, Message::GalleryPrevious) {
                        indices.reverse();
                    }
                    let mut found = false;
                    for (index, item) in indices {
                        if self.select_focus.is_none() {
                            found = true;
                        }
                        if self.select_focus == Some(index) {
                            found = true;
                            continue;
                        }
                        if found && item.can_gallery() {
                            pos_opt = item.pos_opt.get();
                            if pos_opt.is_some() {
                                break;
                            }
                        }
                    }
                }
                if let Some((row, col)) = pos_opt {
                    // Should mod_shift be available?
                    self.select_position(row, col, mod_shift);

                    commands.extend(self.trigger_async_decode());
                }
                if let Some(offset) = self.select_focus_scroll() {
                    commands.push(Command::Iced(
                        scrollable::glide_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }
                if let Some(id) = self.select_focus_id() {
                    commands.push(Command::Iced(widget::button::focus(id).into()));
                }
            }
            Message::GalleryToggle => {
                if let Some(indices) = self.column_sort() {
                    for (_, item) in &indices {
                        if self.selection.is_selected(item) && item.can_gallery() {
                            self.gallery = !self.gallery;

                            if self.gallery {
                                commands.extend(self.trigger_async_decode());
                            }
                            break;
                        }
                    }
                }
            }
            Message::GoNext => {
                // From the entry a step still being resolved goes to: a
                // second press before the first is answered goes one further
                let from = self.pending_history_i();
                if let Some(history_i) = from.checked_add(1)
                    && let Some(location) = self.history.get(history_i)
                {
                    cd = Some(self.restored(location));
                    history_i_opt = Some(history_i);
                }
            }
            Message::GoPrevious => {
                let from = self.pending_history_i();
                if let Some(history_i) = from.checked_sub(1)
                    && let Some(location) = self.history.get(history_i)
                {
                    cd = Some(self.restored(location));
                    history_i_opt = Some(history_i);
                }
            }
            Message::ItemDown => {
                self.dehighlight_all();
                if let Some(edit_location) = &mut self.edit_location {
                    edit_location.select(true);
                } else if self.gallery {
                    commands.append(&mut self.update(Message::GalleryNext, modifiers));
                } else {
                    if let Some((row, col)) = self
                        .select_focus_pos_opt()
                        .or_else(|| self.select_last_pos_opt())
                    {
                        if self.select_focus.is_none() {
                            // Select last item in current selection to focus it.
                            self.select_position(row, col, mod_shift);
                        }

                        // Try to select item in next row
                        if !self.select_position(row + 1, col, mod_shift) {
                            // Ensure current item is still selected if there are no other items
                            self.select_position(row, col, mod_shift);
                        }
                    } else {
                        // Select the first item on screen
                        let (row, col) = self.first_visible_pos_opt().unwrap_or((0, 0));
                        self.select_position(row, col, mod_shift);
                    }
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::glide_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::ItemPageDown => {
                self.dehighlight_all();
                if let Some((row, col)) = self
                    .select_focus_pos_opt()
                    .or_else(|| self.select_last_pos_opt())
                {
                    if self.select_focus.is_none() {
                        self.select_position(row, col, mod_shift);
                    }

                    let rows_per_page = self.rows_per_page().max(1);
                    let target_row = row.saturating_add(rows_per_page);

                    if !self.select_position(target_row, col, mod_shift) {
                        // Fall back to the last item at or before target_row
                        let best = self.items_opt.as_ref().and_then(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.pos_opt.get())
                                .filter(|(r, _)| *r <= target_row)
                                .max()
                        });
                        if let Some((best_row, best_col)) = best {
                            self.select_position(best_row, best_col, mod_shift);
                        }
                    }
                } else {
                    self.select_position(0, 0, mod_shift);
                }
                if let Some(offset) = self.select_focus_scroll() {
                    commands.push(Command::Iced(
                        scrollable::glide_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }
                if let Some(id) = self.select_focus_id() {
                    commands.push(Command::Iced(widget::button::focus(id).into()));
                }
            }
            Message::ItemPageUp => {
                self.dehighlight_all();
                if let Some((row, col)) = self
                    .select_focus_pos_opt()
                    .or_else(|| self.select_first_pos_opt())
                {
                    if self.select_focus.is_none() {
                        self.select_position(row, col, mod_shift);
                    }

                    let rows_per_page = self.rows_per_page().max(1);
                    let target_row = row.saturating_sub(rows_per_page);

                    if !self.select_position(target_row, col, mod_shift) {
                        // Fall back to the first item at or after target_row
                        let best = self.items_opt.as_ref().and_then(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.pos_opt.get())
                                .filter(|(r, _)| *r >= target_row)
                                .min()
                        });
                        if let Some((best_row, best_col)) = best {
                            self.select_position(best_row, best_col, mod_shift);
                        }
                    }
                } else {
                    self.select_position(0, 0, mod_shift);
                }
                if let Some(offset) = self.select_focus_scroll() {
                    commands.push(Command::Iced(
                        scrollable::glide_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }
                if let Some(id) = self.select_focus_id() {
                    commands.push(Command::Iced(widget::button::focus(id).into()));
                }
            }
            Message::ItemLeft => {
                self.dehighlight_all();
                if self.gallery {
                    commands.append(&mut self.update(Message::GalleryPrevious, modifiers));
                } else {
                    if let Some((row, col)) = self
                        .select_focus_pos_opt()
                        .or_else(|| self.select_first_pos_opt())
                    {
                        if self.select_focus.is_none() {
                            // Select first item in current selection to focus it.
                            self.select_position(row, col, mod_shift);
                        }

                        // Try to select previous item in current row
                        if !col
                            .checked_sub(1)
                            .is_some_and(|col| self.select_position(row, col, mod_shift))
                        {
                            // Try to select last item in previous row
                            if !row.checked_sub(1).is_some_and(|row| {
                                let mut col = 0;
                                if let Some(ref items) = self.items_opt {
                                    for item in items {
                                        match item.pos_opt.get() {
                                            Some((item_row, item_col)) if item_row == row => {
                                                col = col.max(item_col);
                                            }
                                            _ => continue,
                                        }
                                    }
                                }
                                self.select_position(row, col, mod_shift)
                            }) {
                                // Ensure current item is still selected if there are no other items
                                self.select_position(row, col, mod_shift);
                            }
                        }
                    } else {
                        // Select the first item on screen
                        let (row, col) = self.first_visible_pos_opt().unwrap_or((0, 0));
                        self.select_position(row, col, mod_shift);
                    }
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::glide_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::ItemRight => {
                self.dehighlight_all();
                if self.gallery {
                    commands.append(&mut self.update(Message::GalleryNext, modifiers));
                } else {
                    if let Some((row, col)) = self
                        .select_focus_pos_opt()
                        .or_else(|| self.select_last_pos_opt())
                    {
                        if self.select_focus.is_none() {
                            // Select last item in current selection to focus it.
                            self.select_position(row, col, mod_shift);
                        }
                        // Try to select next item in current row
                        if !self.select_position(row, col + 1, mod_shift) {
                            // Try to select first item in next row
                            if !self.select_position(row + 1, 0, mod_shift) {
                                // Ensure current item is still selected if there are no other items
                                self.select_position(row, col, mod_shift);
                            }
                        }
                    } else {
                        // Select the first item on screen
                        let (row, col) = self.first_visible_pos_opt().unwrap_or((0, 0));
                        self.select_position(row, col, mod_shift);
                    }
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::glide_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::ItemUp => {
                self.dehighlight_all();
                if let Some(edit_location) = &mut self.edit_location {
                    edit_location.select(false);
                } else if self.gallery {
                    commands.append(&mut self.update(Message::GalleryPrevious, modifiers));
                } else {
                    if let Some((row, col)) = self
                        .select_focus_pos_opt()
                        .or_else(|| self.select_first_pos_opt())
                    {
                        if self.select_focus.is_none() {
                            // Select first item in current selection to focus it.
                            self.select_position(row, col, mod_shift);
                        }

                        // Try to select item in last row
                        if !row
                            .checked_sub(1)
                            .is_some_and(|row| self.select_position(row, col, mod_shift))
                        {
                            // Ensure current item is still selected if there are no other items
                            self.select_position(row, col, mod_shift);
                        }
                    } else {
                        // Select the first item on screen
                        let (row, col) = self.first_visible_pos_opt().unwrap_or((0, 0));
                        self.select_position(row, col, mod_shift);
                    }
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::glide_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::Location(location) => {
                // Workaround to support favorited files: a file is opened,
                // as the worker that looks at it finds
                cd_on_file = if matches!(location, Location::Path(_)) {
                    OnFile::Open
                } else {
                    OnFile::Select
                };
                cd = Some(location);
            }
            Message::LocationUp => {
                // Sets location to the path's parent
                // Does nothing if path is root or location is Trash
                // From where a step still being resolved goes: a second
                // press before the first is answered goes one further
                if let Location::Path(path) = self.pending_location()
                    && let Some(parent) = path.parent()
                {
                    cd = Some(Location::Path(parent.to_owned()));
                }
            }
            Message::Open(path_opt) => {
                match path_opt {
                    Some(path) => {
                        // Opened if it is a file, as the worker finds
                        cd = Some(Location::Path(path));
                        cd_on_file = OnFile::Open;
                    }
                    // Open selected items
                    None => {
                        enum ResolveResult {
                            Open(Option<PathBuf>),
                            OpenInTab(Option<PathBuf>),
                            OpenProperties,
                            Cd(Location),
                            Skip,
                        }
                        fn resolve_item(
                            item: &Item,
                            selected: bool,
                            mode: &Mode,
                            is_only_one_selected: bool,
                        ) -> ResolveResult {
                            if !selected {
                                return ResolveResult::Skip;
                            }

                            let location = match &item.location_opt {
                                Some(l) => l,
                                None => return ResolveResult::OpenProperties,
                            };

                            let path_opt = location.path_opt();

                            if item.metadata.is_dir() {
                                match mode {
                                    Mode::App => {
                                        if is_only_one_selected {
                                            ResolveResult::Cd(location.clone())
                                        } else {
                                            ResolveResult::OpenInTab(path_opt.cloned())
                                        }
                                    }
                                    Mode::Dialog(_) => {
                                        if is_only_one_selected {
                                            ResolveResult::Cd(location.clone())
                                        } else {
                                            ResolveResult::Skip
                                        }
                                    }
                                }
                            } else {
                                ResolveResult::Open(path_opt.cloned())
                            }
                        }
                        let mut open_files = Vec::new();
                        if let Some(items) = self.items_opt.as_ref() {
                            let selected_count = items
                                .iter()
                                .filter(|i| self.selection.is_selected(i))
                                .count();

                            for item in items.iter() {
                                match resolve_item(
                                    item,
                                    self.selection.is_selected(item),
                                    &self.mode,
                                    selected_count == 1,
                                ) {
                                    ResolveResult::Open(Some(p)) => open_files.push(p),
                                    ResolveResult::OpenInTab(Some(p)) => {
                                        commands.push(Command::OpenInNewTab(p))
                                    }
                                    ResolveResult::Cd(loc) => cd = Some(loc),
                                    ResolveResult::OpenProperties => {}
                                    _ => {}
                                }
                            }
                        }
                        if !open_files.is_empty() {
                            commands.push(Command::OpenFile(open_files));
                        }
                    }
                }
            }
            Message::Reload => {
                let selected_paths = self
                    .selected_locations()
                    .into_iter()
                    .filter_map(Location::into_path_opt)
                    .collect();
                let location = self.location.clone();
                self.change_location(&location, None);
                commands.push(Command::ChangeLocation(
                    self.title(),
                    location,
                    Some(selected_paths),
                ));
            }
            Message::RightClick(_point_opt, click_i_opt) => {
                if mod_ctrl || mod_shift {
                    self.update(Message::Click(click_i_opt), modifiers);
                }
                if let Some(ref mut items) = self.items_opt
                    && !click_i_opt.is_some_and(|click_i| {
                        items
                            .get(click_i)
                            .is_some_and(|x| self.selection.is_selected(x))
                    })
                {
                    // If item not selected, clear selection on other items
                    for (i, item) in items.iter_mut().enumerate() {
                        self.selection.select(item, Some(i) == click_i_opt);
                    }
                    self.anchor_selection(click_i_opt);
                }
                self.last_right_click = click_i_opt;
            }
            Message::MiddleClick(click_i) => {
                if mod_ctrl || mod_shift {
                    self.update(Message::Click(Some(click_i)), modifiers);
                } else {
                    if let Some(ref mut items) = self.items_opt {
                        for (i, item) in items.iter_mut().enumerate() {
                            self.selection.select(item, i == click_i);
                        }
                        self.anchor_selection(Some(click_i));
                    }
                    if let Some(clicked_item) =
                        self.items_opt.as_ref().and_then(|items| items.get(click_i))
                    {
                        if let Some(path) = clicked_item.path_opt() {
                            if clicked_item.metadata.is_dir() {
                                //cd = Some(Location::Path(path.clone()));
                                commands.push(Command::OpenInNewTab(path.clone()));
                            } else {
                                commands.push(Command::OpenFile(vec![path.clone()]));
                            }
                        } else {
                            log::warn!("no path for item {clicked_item:?}");
                        }
                    } else {
                        log::warn!("no item for click index {click_i:?}");
                    }
                }
            }
            Message::HighlightDeactivate(i) => {
                self.watch_drag = true;
                if let Some(item) = self.items_opt.as_mut().and_then(|f| f.get_mut(i)) {
                    self.selection.highlight(item, false);
                    // Notes stay as few as the highlights
                    self.highlight_marks.retain(|&mark| mark != i);
                }
            }
            Message::HighlightActivate(i) => {
                self.watch_drag = true;
                if let Some(item) = self.items_opt.as_mut().and_then(|f| f.get_mut(i)) {
                    self.selection.highlight(item, true);
                    self.highlight_marks.push(i);
                }
            }
            Message::Resize(viewport) => {
                // Scroll to ensure focused item still in view
                if self.viewport_opt.map(|v| v.size()) != Some(viewport.size())
                    && let Some(offset) = self.select_focus_scroll()
                {
                    commands.push(Command::Iced(
                        scrollable::scroll_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }

                self.viewport_opt = Some(viewport);
            }
            Message::Scroll(viewport) => {
                self.scroll_opt = Some(viewport.absolute_offset());
                self.watch_drag = true;
                // The list moved under a still file drag: what it is over now
                if let Some(position) = self.dnd_position {
                    commands.extend(self.drag_over_list(position));
                }
            }
            Message::ScrollTab(scroll_speed) => {
                commands.push(Command::Iced(
                    scrollable::scroll_by(
                        self.scrollable_id.clone(),
                        AbsoluteOffset {
                            x: 0.0,
                            y: scroll_speed,
                        },
                    )
                    .into(),
                ));
            }
            Message::ScrollRestore => {
                if let Some(offset) = self.pending_scroll.take() {
                    commands.push(Command::Iced(
                        scrollable::scroll_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }
            }
            Message::ScrollToFocused => {
                if let Some(offset) = self.select_focus_scroll() {
                    commands.push(Command::Iced(
                        scrollable::scroll_to(
                            self.scrollable_id.clone(),
                            AbsoluteOffset {
                                x: Some(offset.x),
                                y: Some(offset.y),
                            },
                        )
                        .into(),
                    ));
                }
            }
            Message::SearchContext(location, context) => {
                if location == self.location {
                    self.search_context = context.take();
                } else {
                    log::warn!(
                        "search context provided for {:?} instead of {:?}",
                        location,
                        self.location
                    );
                }
            }
            // An empty search lists its folder, and nothing it sends is for
            // that listing: its cap on results would cut the folder short
            Message::SearchReady(_) if self.location.empty_search_folder().is_some() => {
                self.search_context = None;
            }
            Message::SearchReady(finished) => {
                let follow_top = self.search_selection_is_own();
                // Results are inserted by date, moving the items after them
                let marks = self.selection_marks();
                let max_results = usize::from(self.config.max_search_results.get());
                let sizes = self.config.icon_sizes;
                if let Some(context) = &mut self.search_context {
                    if let Some(items) = &mut self.items_opt {
                        if finished || context.ready.swap(false, atomic::Ordering::SeqCst) {
                            let duration = Instant::now();
                            while let Ok(item) = context.results_rx.try_recv() {
                                // Newest first, which is the sort order search locations are
                                // pinned to in `sort_options`
                                let index = if matches!(item.metadata, ItemMetadata::Path { .. }) {
                                    let item_modified = item.metadata.modified();
                                    match items.binary_search_by(|other| {
                                        item_modified.cmp(&other.metadata.modified())
                                    }) {
                                        Ok(index) => index,
                                        Err(index) => index,
                                    }
                                } else {
                                    items.len()
                                };

                                if index < max_results {
                                    // Before the cap and the cutoff below, not
                                    // after them: a result that will not be
                                    // shown must not take the place of one that
                                    // would be, nor push the cutoff up so that
                                    // older results are never sent.
                                    if self.item_filter.as_ref().is_some_and(|keep| !keep(&item)) {
                                        continue;
                                    }
                                    items.insert(index, item);
                                }
                                // Ensure that updates make it to the GUI in a timely manner
                                if !finished && duration.elapsed() >= MAX_SEARCH_LATENCY {
                                    break;
                                }
                            }
                        }
                        if items.len() >= max_results {
                            items.truncate(max_results);
                            if let Some(last_modified) =
                                items.last().and_then(|item| item.metadata.modified())
                            {
                                *context.last_modified_opt.write().unwrap() = Some(last_modified);
                            }
                        }
                    } else {
                        log::warn!("search ready but items array is empty");
                    }
                }
                self.restore_selection_marks(marks);
                // Inserted results moved the items after them
                self.highlight_marks = self.items_opt.as_ref().map_or_else(Vec::new, |items| {
                    items
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| self.selection.is_highlighted(item))
                        .map(|(index, _)| index)
                        .collect()
                });
                self.follow_search_top(follow_top);
                if finished {
                    self.search_context = None;
                }
                // Results arrive item by item rather than through a scan, so
                // their icons need asking for separately or they stay generic
                if let Some(items) = self.items_opt.as_deref_mut() {
                    let warmup = refresh_icons(items, sizes);
                    if !warmup.is_empty() {
                        commands.push(Command::WarmIcons(warmup, sizes));
                    }
                }
            }
            Message::SelectAll => {
                self.select_all();
                if self.select_focus.take().is_some() {
                    // Unfocus currently focused button
                    commands.push(Command::Iced(
                        widget::button::focus(widget::Id::unique()).into(),
                    ));
                }
            }
            Message::SelectFirst => {
                if self.select_position(0, 0, mod_shift) {
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::glide_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::SelectLast => {
                if let Some(ref items) = self.items_opt
                    && let Some(last_pos) = items.iter().filter_map(|item| item.pos_opt.get()).max()
                    && self.select_position(last_pos.0, last_pos.1, mod_shift)
                {
                    if let Some(offset) = self.select_focus_scroll() {
                        commands.push(Command::Iced(
                            scrollable::glide_to(
                                self.scrollable_id.clone(),
                                AbsoluteOffset {
                                    x: Some(offset.x),
                                    y: Some(offset.y),
                                },
                            )
                            .into(),
                        ));
                    }
                    if let Some(id) = self.select_focus_id() {
                        commands.push(Command::Iced(widget::button::focus(id).into()));
                    }
                }
            }
            Message::SetOpenWith(mime, id) => {
                commands.push(Command::SetOpenWith(mime, id));
            }
            Message::ShiftPermissions(path_mode_opt, shift, bits) => match path_mode_opt {
                Some((path, mode)) => commands.push(Command::SetPermissions(
                    path,
                    set_mode_part(mode, shift, bits),
                    0o7 << shift,
                )),
                // Shift permissions on all selected items
                None => {
                    let mut permissions = Vec::new();
                    for item in self.items_opt().map_or(Vec::new(), |items| {
                        items
                            .iter()
                            .filter(|item| self.selection.is_selected(item))
                            .collect()
                    }) {
                        if let (Some(path), Some(mode)) = (
                            item.path_opt(),
                            // Never read here: the controls that send this are
                            // not offered until every item's details are known.
                            item.file_metadata().map(|metadata| metadata.mode()),
                        ) {
                            permissions.push((
                                path.clone(),
                                set_mode_part(mode, shift, bits),
                                0o7 << shift,
                            ));
                        }
                    }
                    commands.push(Command::SetMultiplePermissions(permissions));
                }
            },
            Message::SetSort(heading_option, dir) => {
                if let Some(sort_key) = self.location.sort_key() {
                    self.sort_name = heading_option;
                    self.sort_direction = dir;
                    commands.push(Command::SetSort(
                        sort_key,
                        heading_option,
                        self.sort_direction,
                    ));
                }
            }
            Message::TabComplete(path, completions) => {
                // Compared as text, not as paths: `PathBuf` equality ignores a
                // trailing separator, but `tab_complete` does not -- with one
                // it lists a folder's children, without one its matching
                // siblings -- so an answer for one form is wrong for the other.
                if let Some(edit_location) = &mut self.edit_location
                    && edit_location
                        .location
                        .path_opt()
                        .is_some_and(|current| current.as_os_str() == path.as_os_str())
                {
                    edit_location.completions = Some(completions);
                    edit_location.stale = None;
                    commands.push(Command::Iced(
                        widget::text_input::focus(self.edit_location_id.clone()).into(),
                    ));
                }
            }
            Message::IconsReady => {
                let sizes = self.config.icon_sizes;
                if let Some(ref mut items) = self.items_opt {
                    // Refresh only. Asking for more work here could never
                    // settle for a type the icon theme has nothing for.
                    let _ = refresh_icons(items, sizes);
                }
            }
            Message::TrashSize(listing, size) => {
                if listing == self.listing {
                    self.trash_size = size;
                }
            }
            Message::TypesChecked(listing, checked) => {
                if listing == self.listing
                    && let Some(items) = self.items_opt.as_mut()
                {
                    let sizes = self.config.icon_sizes;
                    let mut collect = IconCollect::default();
                    let mut retyped = false;
                    // A desktop entry shown by its own name
                    let mut reordered = false;
                    for check in checked {
                        // Where the job found it, unless the listing changed
                        // under it since: then it is looked for by its path
                        let index = if items
                            .get(check.index)
                            .is_some_and(|item| item.path_opt() == Some(&check.path))
                        {
                            Some(check.index)
                        } else {
                            items
                                .iter()
                                .position(|item| item.path_opt() == Some(&check.path))
                        };
                        let Some(item) = index.and_then(|index| items.get_mut(index)) else {
                            continue;
                        };
                        // Checked since by other means: a refresh reads the
                        // file by content, and is newer than this answer
                        if item.type_checked {
                            continue;
                        }
                        let renamed = check
                            .retyped
                            .as_ref()
                            .is_some_and(|retyped| retyped.display_name.is_some());
                        if item.take_check(check) {
                            collect.add(item, sizes);
                            retyped = true;
                            reordered |= renamed;
                        }
                    }
                    // A new type moves a file when sorting by type; a new
                    // name, when sorting by name
                    let sort = self.sort_options().0;
                    if (retyped && sort == HeadingOptions::Type)
                        || (reordered && sort == HeadingOptions::Name)
                    {
                        self.invalidate_order();
                    }
                    if retyped {
                        let warmup = collect.finish();
                        if !warmup.is_empty() {
                            commands.push(Command::WarmIcons(warmup, sizes));
                        }
                    }
                }
            }
            Message::Thumbnail(path, modified, thumbnail) => {
                if let Some(ref mut items) = self.items_opt {
                    let location = Location::Path(path);
                    for item in items.iter_mut() {
                        // Not of a file put at the same path since: its
                        // pixels are the old file's
                        if item.location_opt.as_ref() == Some(&location)
                            && item.metadata.modified() == modified
                        {
                            let handle_opt = match &thumbnail {
                                ItemThumbnail::NotImage => None,
                                ItemThumbnail::Image(handle, _) => Some(widget::icon::Handle {
                                    symbolic: false,
                                    data: widget::icon::Data::Image(handle.clone()),
                                }),
                                ItemThumbnail::Svg(handle) => Some(widget::icon::Handle {
                                    symbolic: false,
                                    data: widget::icon::Data::Svg(handle.clone()),
                                }),
                                ItemThumbnail::Text(_text) => None,
                            };
                            if let Some(handle) = handle_opt {
                                item.icon_handle_grid.clone_from(&handle);
                                item.icon_handle_list = handle;
                            }
                            item.thumbnail_opt = Some(thumbnail);
                            break;
                        }
                    }
                }
            }
            Message::ImageDecoded(path, width, height, pixels, display_size, generation, stamp) => {
                // Create handle from pre-decoded RGBA data (fast!)
                let handle = widget::image::Handle::from_rgba(width, height, pixels);

                // Store decoded image handle if generation still matches (not
                // superseded) and the file is the one asked for
                let current = self.large_image_manager.is_current(&path, generation);
                let stored = self.large_image_manager.store_decoded_with_generation(
                    path,
                    handle,
                    display_size,
                    generation,
                    stamp,
                );
                if current && !stored {
                    // Of a file replaced since: the new one is decoded
                    commands.extend(self.trigger_async_decode());
                }
            }
            Message::ImageDecodeChanged(path, generation) => {
                if self.large_image_manager.is_current(&path, generation) {
                    self.large_image_manager
                        .abandon_with_generation(&path, generation);
                    commands.extend(self.trigger_async_decode());
                }
            }
            Message::GalleryRecheck => {
                // Decodes the image shown again only if its file changed
                commands.extend(self.trigger_async_decode());
            }
            Message::ImageDecodeSuperseded(path, generation) => {
                self.large_image_manager
                    .abandon_with_generation(&path, generation);
                // The gallery may have come back to it while it waited, which
                // asked for nothing as it was still marked as loading: asked
                // for now, if it is the image shown
                commands.extend(self.trigger_async_decode());
            }
            Message::ImageDecodeFailed(path, generation) => {
                self.large_image_manager.fail_with_generation(
                    path,
                    generation,
                    "Failed to decode image".to_string(),
                );
            }
            Message::ToggleSort(heading_option) => {
                if let Some(sort_key) = self.location.sort_key() {
                    let heading_sort = if self.sort_name == heading_option {
                        !self.sort_direction
                    } else {
                        // Default modified to descending, and others to ascending.
                        heading_option != HeadingOptions::Modified
                    };

                    commands.push(Command::SetSort(sort_key, heading_option, heading_sort));

                    self.sort_direction = heading_sort;
                    self.sort_name = heading_option;
                }
            }
            Message::WindowDrag => {
                commands.push(Command::WindowDrag);
            }
            Message::WindowToggleMaximize => {
                commands.push(Command::WindowToggleMaximize);
            }
            Message::ZoomIn => {
                commands.push(Command::Action(Action::ZoomIn));
            }
            Message::ZoomOut => {
                commands.push(Command::Action(Action::ZoomOut));
            }
            Message::RequestDetails(path, epoch, listing) => {
                if listing == self.listing
                    && let Some(command) = self.start_details_read(path, epoch, listing)
                {
                    commands.push(command);
                }
            }
            Message::ItemDetails(path, epoch, listing, result) => {
                // The read is over, whatever it found: the path may be read
                // again from here on
                self.details_reads.remove(&path);

                // Only the listing and incarnation that asked. A rescan gives
                // every item a fresh incarnation at the same epoch, so the
                // epoch alone would let an old answer land on a new item.
                let location = Location::Path(path.clone());
                let asked = |item: &Item| {
                    item.location_opt.as_ref() == Some(&location)
                        && item.details_epoch == epoch
                        && matches!(
                            item.details,
                            MetadataState::Pending | MetadataState::Loading
                        )
                };
                if listing == self.listing {
                    let state = match result {
                        Ok(metadata) => MetadataState::Ready(metadata),
                        Err(err) => MetadataState::Failed(err),
                    };
                    if let Some(ref mut item) = self.parent_item_opt
                        && asked(item)
                    {
                        item.details = state.clone();
                    }
                    if let Some(ref mut items) = self.items_opt
                        && let Some(item) = items.iter_mut().find(|item| asked(item))
                    {
                        item.details = state;
                    }
                }

                // Whatever now stands at that path and is still waiting -- a
                // replacement that arrived while this read was out, and was
                // told to wait its turn -- gets its own read now.
                let waiting = self
                    .parent_item_opt
                    .iter()
                    .map(|item| &**item)
                    .chain(self.items_opt.iter().flatten())
                    .find(|item| {
                        item.location_opt.as_ref() == Some(&location)
                            && !matches!(item.metadata, ItemMetadata::Path { .. })
                            && matches!(item.details, MetadataState::Loading)
                    })
                    .map(|item| item.details_epoch);
                if let Some(epoch) = waiting {
                    let listing = self.listing;
                    if let Some(command) = self.start_details_read(path, epoch, listing) {
                        commands.push(command);
                    }
                }
            }
            Message::DirectorySize(path, dir_size) => {
                let location = Location::Path(path);
                if let Some(ref mut item) = self.parent_item_opt
                    && item.location_opt.as_ref() == Some(&location)
                {
                    item.dir_size.clone_from(&dir_size);
                }
                if let Some(ref mut items) = self.items_opt {
                    for item in items.iter_mut() {
                        if item.location_opt.as_ref() == Some(&location) {
                            item.dir_size = dir_size;
                            break;
                        }
                    }
                }
            }
            Message::DirectoryChildren(index, path, children) => {
                if let Some(ref mut items) = self.items_opt {
                    // Where it was asked about, unless the listing changed
                    // since: then it is looked for by its path
                    let index = if items
                        .get(index)
                        .is_some_and(|item| item.path_opt() == Some(&path))
                    {
                        Some(index)
                    } else {
                        items.iter().position(|item| item.path_opt() == Some(&path))
                    };
                    if let Some(item) = index.and_then(|index| items.get_mut(index)) {
                        match &mut item.metadata {
                            ItemMetadata::Path { children_opt, .. } => {
                                *children_opt = Some(children);
                            }
                            #[cfg(feature = "gvfs")]
                            ItemMetadata::GvfsPath { children_opt, .. } => {
                                *children_opt = Some(children);
                            }
                            _ => {}
                        }
                    }
                    // Folders sort by how many entries they hold
                    if self.sort_options().0 == HeadingOptions::Size {
                        self.invalidate_order();
                    }
                }
            }
            Message::Checksums(path, checksum_state) => {
                let location = Location::Path(path);
                if let Some(ref mut item) = self.parent_item_opt
                    && item.location_opt.as_ref() == Some(&location)
                {
                    item.checksums = checksum_state.clone();
                }
                if let Some(ref mut items) = self.items_opt {
                    for item in items.iter_mut() {
                        if item.location_opt.as_ref() == Some(&location) {
                            item.checksums = checksum_state;
                            break;
                        }
                    }
                }
            }
            Message::CalculateChecksums(path) => {
                let location = Location::Path(path.clone());
                if let Some(ref mut item) = self.parent_item_opt
                    && item.location_opt.as_ref() == Some(&location)
                {
                    item.checksums = ChecksumState::Calculating;
                }
                if let Some(ref mut items) = self.items_opt {
                    for item in items.iter_mut() {
                        if item.location_opt.as_ref() == Some(&location) {
                            item.checksums = ChecksumState::Calculating;
                            break;
                        }
                    }
                }
                commands.push(Command::Iced(
                    crate::ui::Task::future(async move {
                        match calculate_checksums(&path).await {
                            Ok(checksums) => {
                                Message::Checksums(path, ChecksumState::Calculated(checksums))
                            }
                            Err(err) => Message::Checksums(path, ChecksumState::Error(err)),
                        }
                    })
                    .into(),
                ));
            }
            Message::CopyChecksum(value) => {
                commands.push(Command::Iced(
                    crate::ui::iced::clipboard::write(value).into(),
                ));
            }
        }

        // Scroll to top if needed
        if self.scroll_opt.is_none() {
            let offset = AbsoluteOffset { x: 0.0, y: 0.0 };
            self.scroll_opt = Some(offset);
            commands.push(Command::Iced(
                scrollable::scroll_to(
                    self.scrollable_id.clone(),
                    AbsoluteOffset {
                        x: Some(0.0),
                        y: Some(0.0),
                    },
                )
                .into(),
            ));
        }

        // Change directory if requested
        if let Some(location) = cd {
            // A step taken withdraws one still waiting for its answer, local
            // or network
            self.steps = self.steps.wrapping_add(1);
            self.resolving_location = None;
            self.resolving_network = None;
            if location.path_opt().is_some() {
                // Whether it is a folder, and what it is called once its
                // links are followed, is the filesystem's to say, which a
                // mount that has stopped answering never does: asked on a
                // worker, and gone to when the answer comes
                let request = self.next_location_request;
                self.next_location_request = self.next_location_request.wrapping_add(1);
                self.resolving_location = Some((request, history_i_opt, location.clone()));
                commands.push(Command::ResolveLocation(
                    request,
                    LocationRequest {
                        fallback: cd_fallback,
                        on_file: cd_on_file,
                        ..LocationRequest::new(location, history_i_opt)
                    },
                ));
            } else {
                self.go_to(
                    ResolvedLocation {
                        location: location.normalize_lexically(),
                        selected_paths: None,
                        is_dir: true,
                        history_i_opt,
                        open_file: None,
                    },
                    &mut commands,
                );
            }
        }

        commands
    }

    /// The history entry the tab is at, or is on its way to while a step
    /// there is being resolved.
    fn pending_history_i(&self) -> usize {
        self.resolving_location
            .as_ref()
            .and_then(|(_, history_i, _)| *history_i)
            .unwrap_or(self.history_i)
    }

    /// Where the tab is, or is on its way to while a step there is being
    /// resolved: what a step relative to it, such as Up, starts from.
    fn pending_location(&self) -> &Location {
        self.resolving_location
            .as_ref()
            .map_or(&self.location, |(_, _, location)| location)
    }

    /// Counts the steps asked of the tab: see the field.
    pub const fn steps(&self) -> u64 {
        self.steps
    }

    /// Goes where `resolved` says, unless that is here already.
    fn go_to(&mut self, resolved: ResolvedLocation, commands: &mut Vec<Command>) {
        let ResolvedLocation {
            location,
            mut selected_paths,
            is_dir,
            history_i_opt,
            open_file,
        } = resolved;
        if let Some(path) = open_file {
            commands.push(Command::OpenFile(vec![path]));
            return;
        }
        // A history step counts even to where the tab is: Back from the
        // second A in [A, B, A] goes to the first
        let moves_in_history = history_i_opt.is_some_and(|history_i| history_i != self.history_i);
        if location != self.location || selected_paths.is_some() || moves_in_history {
            if is_dir {
                if selected_paths.is_none() {
                    selected_paths = self.location.path_opt().map(|path| vec![path.clone()]);
                }
                self.change_location(&location, history_i_opt);
                commands.push(Command::ChangeLocation(
                    self.title(),
                    location,
                    selected_paths,
                ));
            } else {
                log::warn!("tried to cd to {location:?} which is not a directory");
            }
        }
    }

    pub(crate) fn sort_options(&self) -> (HeadingOptions, bool, bool) {
        match self.location {
            Location::Search(..) if self.location.empty_search_folder().is_none() => {
                (HeadingOptions::Modified, false, false)
            }
            _ => (
                self.sort_name,
                self.sort_direction,
                self.config.folders_first,
            ),
        }
    }

    /// Names of the selected items, in the order they are displayed
    pub fn selected_names(&self) -> Vec<String> {
        self.column_sort()
            .map(|items| {
                items
                    .into_iter()
                    .filter(|(_, item)| self.selection.is_selected(item))
                    .map(|(_, item)| item.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The items in the order shown, sorted as the tab is. The order is kept
    /// while the sort options, the listing and its length hold and nothing
    /// has called [`Self::invalidate_order`]: scrolling redraws every frame
    /// and must not sort the folder again each time.
    fn column_sort(&self) -> Option<Vec<(usize, &Item)>> {
        let items = self.items_opt.as_ref()?;
        let order = self.sorted_order()?;
        Some(order.iter().map(|&index| (index, &items[index])).collect())
    }

    /// The layout `key` gives, made by `lay_out` only when the last one was
    /// made for something else.
    fn laid_out(
        &self,
        key: LayoutKey,
        lay_out: impl FnOnce(LayoutKey) -> ShownLayout,
    ) -> ShownLayout {
        if let Some(layout) = self.shown_layout.borrow().as_ref()
            && layout.key == key
        {
            return layout.clone();
        }
        let layout = lay_out(key);
        self.shown_layout.replace(Some(layout.clone()));
        layout
    }

    /// The indices of [`Self::column_sort`], kept: see there.
    fn sorted_order(&self) -> Option<Arc<[usize]>> {
        let items = self.items_opt.as_ref()?;
        let key = (self.sort_options(), self.listing, items.len());
        let cached = self
            .sort_cache
            .borrow()
            .as_ref()
            .filter(|(cached, _)| *cached == key)
            .map(|(_, order)| Arc::clone(order));
        let order = match cached {
            Some(order) => order,
            None => {
                let order: Arc<[usize]> = self
                    .sort_items()?
                    .into_iter()
                    .map(|(index, _)| index)
                    .collect();
                self.sort_cache.replace(Some((key, Arc::clone(&order))));
                order
            }
        };
        Some(order)
    }

    /// Sorts the items afresh: see [`Self::column_sort`].
    fn sort_items(&self) -> Option<Vec<(usize, &Item)>> {
        let check_reverse = |ord: Ordering, sort: bool| {
            if sort { ord } else { ord.reverse() }
        };
        let mut items: Vec<_> = self.items_opt.as_ref()?.iter().enumerate().collect();
        let (sort_name, sort_direction, folders_first) = self.sort_options();
        match sort_name {
            HeadingOptions::Size => {
                items.sort_by(|a, b| {
                    // entries take precedence over size
                    let get_size = |x: &Item| match &x.metadata {
                        ItemMetadata::Path {
                            metadata,
                            children_opt,
                        } => {
                            if metadata.is_dir() {
                                (true, children_opt.unwrap_or_default() as u64)
                            } else {
                                (false, metadata.len())
                            }
                        }
                        ItemMetadata::Trash { metadata, .. } => match metadata.size {
                            crate::trashing::TrashItemSize::Entries(entries) => {
                                (true, entries as u64)
                            }
                            crate::trashing::TrashItemSize::Bytes(bytes) => (false, bytes),
                        },
                        ItemMetadata::SimpleDir { entries } => (true, *entries),
                        ItemMetadata::SimpleFile { size } => (false, *size),
                        #[cfg(feature = "gvfs")]
                        ItemMetadata::GvfsPath {
                            size_opt,
                            children_opt,
                            is_dir,
                            ..
                        } => {
                            if *is_dir {
                                (true, children_opt.unwrap_or_default() as u64)
                            } else {
                                (false, size_opt.unwrap_or_default())
                            }
                        }
                    };
                    let (a_is_entry, a_size) = get_size(a.1);
                    let (b_is_entry, b_size) = get_size(b.1);

                    match (a_is_entry, b_is_entry) {
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        _ => check_reverse(a_size.cmp(&b_size), sort_direction),
                    }
                });
            }
            HeadingOptions::Name => items.sort_by(|a, b| {
                if folders_first {
                    match (a.1.metadata.is_dir(), b.1.metadata.is_dir()) {
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        _ => check_reverse(
                            LANGUAGE_SORTER.compare(&a.1.display_name, &b.1.display_name),
                            sort_direction,
                        ),
                    }
                } else {
                    check_reverse(
                        LANGUAGE_SORTER.compare(&a.1.display_name, &b.1.display_name),
                        sort_direction,
                    )
                }
            }),
            HeadingOptions::Type => items.sort_by(|a, b| {
                let by_type = || {
                    check_reverse(
                        crate::file_category::compare_by_type(
                            a.1.metadata.is_dir(),
                            &a.1.mime,
                            &a.1.display_name,
                            b.1.metadata.is_dir(),
                            &b.1.mime,
                            &b.1.display_name,
                        ),
                        sort_direction,
                    )
                };
                if folders_first {
                    match (a.1.metadata.is_dir(), b.1.metadata.is_dir()) {
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        _ => by_type(),
                    }
                } else {
                    by_type()
                }
            }),
            HeadingOptions::Modified => {
                items.sort_by(|a, b| {
                    let a_modified = a.1.metadata.modified();
                    let b_modified = b.1.metadata.modified();
                    if folders_first {
                        match (a.1.metadata.is_dir(), b.1.metadata.is_dir()) {
                            (true, false) => Ordering::Less,
                            (false, true) => Ordering::Greater,
                            _ => check_reverse(a_modified.cmp(&b_modified), sort_direction),
                        }
                    } else {
                        check_reverse(a_modified.cmp(&b_modified), sort_direction)
                    }
                });
            }
            HeadingOptions::TrashedOn => {
                let time_deleted = |x: &Item| match &x.metadata {
                    ItemMetadata::Trash { entry, .. } => Some(entry.time_deleted),
                    _ => None,
                };

                items.sort_by(|a, b| {
                    let a_time_deleted = time_deleted(a.1);
                    let b_time_deleted = time_deleted(b.1);
                    if folders_first {
                        match (a.1.metadata.is_dir(), b.1.metadata.is_dir()) {
                            (true, false) => Ordering::Less,
                            (false, true) => Ordering::Greater,
                            _ => check_reverse(a_time_deleted.cmp(&b_time_deleted), sort_direction),
                        }
                    } else {
                        check_reverse(b_time_deleted.cmp(&a_time_deleted), sort_direction)
                    }
                });
            }
        }
        Some(items)
    }

    pub fn gallery_view(&self) -> Element<'_, Message> {
        let Spacing {
            space_xxs,
            space_xs,
            space_m,
            ..
        } = spacing();

        let mut name_opt = None;
        let mut element_opt: Option<Element<Message>> = None;
        if let Some(index) = self.select_focus
            && let Some(items) = &self.items_opt
            && let Some(item) = items.get(index)
        {
            name_opt = Some(widget::text::heading(&item.display_name));
            match item
                .thumbnail_opt
                .as_ref()
                .unwrap_or(&ItemThumbnail::NotImage)
            {
                ItemThumbnail::NotImage => {}
                ItemThumbnail::Image(handle, original_dims) => {
                    // Determine which image to show based on async decode state
                    let mut is_loading = false;
                    let mut error_msg_opt = None;
                    let image_handle = if let Some(path) = item.path_opt() {
                        if let Some(error_msg) = self.large_image_manager.get_error(path) {
                            error_msg_opt = Some(error_msg.clone());
                            handle.clone()
                        } else if self.large_image_manager.is_decoding(path) {
                            // Currently decoding (initial or re-decode) --> show cached/thumbnail with loading indicator
                            is_loading = true;
                            // Use decoded handle if available (re-decode), otherwise thumbnail (initial decode)
                            self.large_image_manager
                                .get_decoded(path)
                                .cloned()
                                .unwrap_or_else(|| handle.clone())
                        } else if let Some(decoded_handle) =
                            self.large_image_manager.get_decoded(path)
                        {
                            // Decoded and not currently decoding --> use it
                            decoded_handle.clone()
                        } else if let Some((w, h)) = original_dims {
                            // Check if image needs tiling
                            if should_use_tiling(*w, *h) {
                                // Large image --> show thumbnail only
                                handle.clone()
                            } else {
                                // Normal-sized image --> load full resolution directly
                                widget::image::Handle::from_path(path)
                            }
                        } else {
                            // No dimensions available --> show thumbnail
                            handle.clone()
                        }
                    } else {
                        handle.clone()
                    };

                    let content: crate::ui::Element<'_, Message> = if let Some(error_msg) =
                        error_msg_opt
                    {
                        widget::Column::with_capacity(2)
                            .push(widget::image(image_handle))
                            .push(
                                widget::text(fl!("image-load-error", error = error_msg.clone()))
                                    .size(13),
                            )
                            .padding(space_xs)
                            .align_x(crate::ui::iced::Alignment::Center)
                            .into()
                    } else if is_loading {
                        widget::Column::with_capacity(2)
                            .push(widget::image(image_handle))
                            .push(widget::text(fl!("loading-full-image")).size(14))
                            .padding(space_xs)
                            .align_x(crate::ui::iced::Alignment::Center)
                            .into()
                    } else {
                        crate::load_image::loaded_image(image_handle).into()
                    };

                    element_opt = Some(widget::container(content).center(Length::Fill).into());
                }
                ItemThumbnail::Svg(handle) => {
                    element_opt = Some(
                        widget::svg(handle.clone())
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .into(),
                    );
                }
                ItemThumbnail::Text(text) => {
                    element_opt = Some(
                        widget::container(
                            widget::text_editor::text_editor(text)
                                .padding(space_xxs)
                                .style(text_editor_class),
                        )
                        .center(Length::Fill)
                        .into(),
                    );
                }
            }
        }

        let mut column = widget::Column::with_capacity(2);
        column = column.push(widget::space::vertical().height(Length::Fixed(space_m.into())));
        {
            let mut row = widget::Row::with_capacity(5).align_y(Alignment::Center);
            row = row.push(widget::space::horizontal());
            if let Some(name) = name_opt {
                row = row.push(name);
            }
            row = row.push(widget::space::horizontal());
            row = row.push(
                widget::button::icon(widget::icon::from_name("window-close-symbolic"))
                    .class(Button::Standard)
                    .on_press(Message::Gallery(false)),
            );
            row = row.push(widget::space::horizontal().width(Length::Fixed(space_m.into())));
            // This mouse area provides window drag while the header bar is hidden
            let mouse_area = mouse_area::MouseArea::new(row)
                .on_press(|_| Message::WindowDrag)
                .on_double_click(|_| Message::WindowToggleMaximize);
            column = column.push(mouse_area);
        }
        {
            let mut row = widget::Row::with_capacity(7).align_y(Alignment::Center);
            row = row.push(widget::space::horizontal().width(Length::Fixed(space_m.into())));
            row = row.push(
                widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                    .padding(space_xs)
                    .class(Button::Standard)
                    .on_press(Message::GalleryPrevious),
            );
            row = row.push(widget::space::horizontal().width(Length::Fixed(space_xxs.into())));
            if let Some(element) = element_opt {
                row = row.push(element);
            } else {
                row = row.push(space::horizontal().width(Length::Fill));
                row = row.push(space::vertical().height(Length::Fill));
            }
            row = row.push(widget::space::horizontal().width(Length::Fixed(space_xxs.into())));
            row = row.push(
                widget::button::icon(widget::icon::from_name("go-next-symbolic"))
                    .padding(space_xs)
                    .class(Button::Standard)
                    .on_press(Message::GalleryNext),
            );
            row = row.push(widget::space::horizontal().width(Length::Fixed(space_m.into())));
            column = column.push(row);
        }

        widget::container(column)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|theme| {
                let cosmic = theme.cosmic();
                let mut bg = cosmic.bg_color();
                bg.alpha = 0.75;
                widget::container::Style {
                    background: Some(bg.to_color().into()),
                    ..Default::default()
                }
            })
            .into()
    }

    /// The tab's part of the window's header: the history pill, then the
    /// crumb pill, or the path field while the location is being edited.
    pub fn header_view(&self) -> Element<'_, Message> {
        let Spacing { space_xxs, .. } = spacing();

        // 30 by 28, in a pill 2 px around them
        let history_button = |svg: &'static [u8], message: Option<Message>| {
            widget::button::custom(widget::icon::icon(widget::icon::line::handle(svg)).size(16))
                .padding([6, 7])
                .class(Button::Icon)
                .on_press_maybe(message)
        };
        let history = widget::container(
            widget::Row::with_children([
                history_button(
                    widget::icon::line::PREVIOUS,
                    (self.history_i > 0 && !self.history.is_empty()).then_some(Message::GoPrevious),
                )
                .into(),
                history_button(
                    widget::icon::line::NEXT,
                    (self.history_i + 1 < self.history.len()).then_some(Message::GoNext),
                )
                .into(),
            ])
            .spacing(2),
        )
        .padding(2)
        .class(Container::Pill);

        let mut row = widget::Row::with_capacity(2)
            .align_y(Alignment::Center)
            .spacing(space_xxs.to_pixels())
            .push(history);

        /// The crumb pill around `crumbs`.
        fn pill(crumbs: widget::Crumbs<'_, Message>) -> Element<'_, Message> {
            let Spacing { space_xxxs, .. } = spacing();
            widget::container(crumbs.spacing(space_xxxs.into()))
                // The icon 12 in from the left end; the last name as far in
                // from the right as the menus' labels are from their pill's
                // (2 + 12), its button adding its own 4
                .padding(padding::left(12).right(f32::from(14u16.saturating_sub(space_xxxs))))
                .center_y(Length::Fixed(32.0))
                .class(Container::Pill)
                .into()
        }

        let crumbs = self.crumbs();

        if let Some(edit_location) = &self.edit_location {
            let mut text_input = None;

            if let Location::Network(ref uri, ..) = edit_location.location {
                let location = edit_location.location.clone();
                text_input = Some((
                    uri.clone(),
                    widget::text_input("", uri.clone())
                        .id(self.edit_location_id.clone())
                        .on_input(move |input| {
                            Message::EditLocation(Some(location.with_uri(input).into()))
                        })
                        .on_submit(|_| Message::EditLocationSubmit)
                        .on_unfocus(Message::EditLocation(None))
                        .line_height(1.0),
                ));
            } else if let Some(resolved_location) = edit_location.resolve()
                && let Some(path) = resolved_location.path_opt().cloned()
            {
                let value = path.to_string_lossy().into_owned();
                text_input = Some((
                    value.clone(),
                    widget::text_input("", value)
                        .id(self.edit_location_id.clone())
                        .on_input(move |input| {
                            Message::EditLocation(Some(
                                resolved_location.with_path(PathBuf::from(input)).into(),
                            ))
                        })
                        .on_submit(|_| Message::EditLocationSubmit)
                        .on_tab(Message::EditLocationTab)
                        .on_unfocus(Message::EditLocation(None))
                        .line_height(1.0),
                ));
            }
            if let Some((value, text_input)) = text_input {
                // In place of the crumbs, the names gliding from them into
                // its text, and as wide as that text where the room beside
                // the menus allows (see `widget::path_morph`). With no
                // crumbs, at most 360 wide. Round, as the search field is.
                let text_input = text_input
                    .width(if crumbs.is_some() {
                        Length::Fill
                    } else {
                        Length::Fixed(360.0)
                    })
                    .style(crate::ui::theme::TextInput::Search)
                    // The crumbs' size, which the names gliding into it keep
                    .size(widget::path_morph::TEXT_SIZE)
                    // The text as far in from the ends as the menus' labels
                    // are from their pill's (2 + 12)
                    .padding([space_xxs, FIELD_INSET]);
                let mut popover =
                    widget::popover(text_input).position(widget::popover::Position::Bottom);
                if let Some((completions, choosable)) = edit_location.shown() {
                    let mut column =
                        widget::Column::with_capacity(completions.len()).padding(space_xxs);
                    for (i, (name, _path)) in completions.iter().enumerate() {
                        let text = widget::text::body(name);
                        let item: Element<_> = if choosable {
                            let selected = edit_location.selected == Some(i);
                            widget::button::custom(text)
                                .class(if selected {
                                    Button::Standard
                                } else {
                                    Button::HeaderBar
                                })
                                .on_press(Message::EditLocationComplete(i))
                                .padding(space_xxs)
                                .width(Length::Fill)
                                .into()
                        } else {
                            // The previous list, on show until this edit's own
                            // arrives. Drawn exactly like an unselected real
                            // row, so nothing changes colour or size when the
                            // real list lands, but never highlighted. Its press
                            // is taken and does nothing: without a handler a
                            // button is drawn disabled, and a row that did not
                            // take the press would let it through to the list
                            // underneath and to the field, which closes on a
                            // press outside it.
                            //
                            // Wrapped so the row is a different widget from a
                            // real one in the same place. A button remembers a
                            // press until the release; if the real list lands
                            // in between, the same button state would carry
                            // over and the release would choose a row the user
                            // never saw. A column rather than a container: a
                            // container shares its content's widget state, so
                            // it changes nothing, while a column holds its
                            // children under a node of its own. With no
                            // padding or spacing, and drawing nothing itself,
                            // it looks and measures as the bare button would.
                            widget::Column::new()
                                .push(
                                    widget::button::custom(text)
                                        .class(Button::HeaderBar)
                                        .on_press(Message::EditLocationStalePressed)
                                        .padding(space_xxs)
                                        .width(Length::Fill),
                                )
                                .into()
                        };
                        column = column.push(item);
                    }
                    // Unrolls on a spring, and springs to each new height as
                    // typing changes how many completions there are.
                    popover = popover.popup(widget::spring_height(
                        widget::container(column)
                            .class(Container::Dropdown)
                            .width(Length::Fixed(360.0)),
                    ));
                }
                let field = widget::container(popover).center_y(Length::Fill);
                return match crumbs {
                    Some((crumbs, names)) => {
                        row.push(widget::path_morph(pill(crumbs), names).field(
                            field,
                            value,
                            self.edit_location_id.clone(),
                        ))
                    }
                    None => row.push(field),
                }
                .into();
            }
        }

        if let Some((crumbs, names)) = crumbs {
            row = row.push(widget::path_morph(pill(crumbs), names));
        }
        row.into()
    }

    /// The crumbs of the header's crumb pill, from the outermost folder to
    /// this one, with the icon of the outermost (see [`widget::crumbs`]),
    /// and what the glide to the path field needs of them (see
    /// [`widget::path_morph`]).
    fn crumbs(&self) -> Option<(widget::Crumbs<'_, Message>, widget::path_morph::Names)> {
        // The "›" between crumbs: the header's text colour, faded
        fn separator_style(theme: &crate::ui::Theme) -> crate::ui::widget::text::Style {
            let mut color = theme.cosmic().background(theme.transparent).on;
            color.alpha *= 0.45;
            crate::ui::widget::text::Style {
                color: Some(color.to_color()),
                ..Default::default()
            }
        }

        let Spacing { space_xxxs, .. } = spacing();

        // The location's icon, in the accent colour
        let icon = |handle: widget::icon::Handle| {
            widget::icon::icon(handle)
                .size(16)
                .class(theme::Svg::custom(|theme| {
                    crate::ui::iced::widget::svg::Style {
                        color: Some(theme.cosmic().accent_text_color().to_color()),
                    }
                }))
        };
        // What the glide to the path field needs of the crumbs
        let names = |crumbs: Vec<widget::path_morph::Name>, handle: widget::icon::Handle| {
            widget::path_morph::Names {
                crumbs,
                icon: Some(handle),
                crumb_inset: f32::from(space_xxxs),
                field_inset: f32::from(FIELD_INSET),
            }
        };
        let single = |handle: widget::icon::Handle, label: String, message: Message| {
            let glide = names(
                vec![widget::path_morph::Name {
                    label: label.clone(),
                    path: None,
                }],
                handle.clone(),
            );
            let crumbs = widget::crumbs(
                icon(handle),
                vec![
                    widget::button::custom(widget::text::heading(label))
                        .padding(space_xxxs)
                        .on_press(message)
                        .class(Button::Text)
                        .into(),
                ],
                Vec::new(),
                Vec::new(),
            );
            (crumbs, glide)
        };

        // A crumb's button with everything a crumb does: going to its
        // folder (or, for this folder, opening the path field), its context
        // menu, a middle-click opening it in a new tab and, for an ancestor,
        // taking dropped files. A "…" standing for hidden crumbs is wrapped
        // the same way as the crumb it goes to.
        let crumb = |content,
                     index: usize,
                     ancestor_location: &Location,
                     ancestor: &PathBuf,
                     current: bool| {
            let mouse_area = crate::mouse_area::MouseArea::new(
                widget::button::custom(content)
                    .padding(space_xxxs)
                    // Reuse `LinkActive`, the highlight for an open breadcrumb
                    // context menu, when a drag hovers over a breadcrumb.
                    .class(
                        if self.location_context_menu_index == Some(index)
                            || self.dnd_ancestor == Some(index)
                        {
                            Button::LinkActive
                        } else {
                            Button::Link
                        },
                    )
                    .on_press(if current {
                        Message::EditLocation(Some(self.location.clone().into()))
                    } else {
                        Message::Location(ancestor_location.clone())
                    }),
            );

            let mouse_area = if let Location::Path(_) = &self.location {
                let ancestor = ancestor.clone();
                mouse_area.on_middle_press(move |_| Message::OpenInNewTab(ancestor.clone()))
            } else {
                mouse_area
            };

            // The last segment is the displayed directory; clicking it opens
            // the location editor. Exclude it as a drop target because the
            // files are already there. All ancestor segments are valid
            // destinations.
            let mouse_area = if current {
                mouse_area
            } else {
                let ancestor = ancestor.clone();
                mouse_area.on_dnd(move |dnd| Message::DndAncestor(index, ancestor.clone(), dnd))
            };

            // Each breadcrumb carries the menu for its own ancestor index
            let mut context_menu = widget::context_menu(
                mouse_area,
                Some(menu::location_context_menu(index, &self.mode)),
            )
            .on_open(Message::LocationContextMenuIndex(Some(index)))
            .on_close(Message::LocationContextMenuIndex(None))
            .on_surface_action(Message::Surface);
            if let Some(window_id) = self.window_id {
                context_menu = context_menu.window_id(window_id);
            }
            let crumb: Element<'_, Message> = context_menu.into();
            crumb
        };

        match &self.location {
            Location::Path(_) | Location::Search(SearchLocation::Path(_), ..) => {
                // `location_ancestors` runs from this folder outwards; the
                // pill runs the other way
                let ancestors: Vec<(usize, &Location, &PathBuf, &String)> = self
                    .location_ancestors
                    .iter()
                    .enumerate()
                    .filter_map(|(index, (location, name))| {
                        location
                            .path_opt()
                            .map(|ancestor| (index, location, ancestor, name))
                    })
                    .rev()
                    .collect();
                let (_, _, outermost, _) = ancestors.first()?;
                let handle = folder_icon_symbolic(outermost, 16);
                let glide = names(
                    ancestors
                        .iter()
                        .map(|&(_, _, ancestor, name)| widget::path_morph::Name {
                            label: name.clone(),
                            path: Some(ancestor.to_string_lossy().into_owned()),
                        })
                        .collect(),
                    handle.clone(),
                );
                let icon = icon(handle);

                let mut crumbs = Vec::with_capacity(ancestors.len());
                let mut separators = Vec::with_capacity(ancestors.len());
                let mut more = Vec::with_capacity(ancestors.len());
                let last = ancestors.len() - 1;
                for (position, &(index, ancestor_location, ancestor, name)) in
                    ancestors.iter().enumerate()
                {
                    // The innermost is this folder: `ancestors` starts at it
                    let current = position == last;
                    if position > 0 {
                        separators.push(
                            widget::text::body("›")
                                .class(theme::Text::Custom(separator_style))
                                .into(),
                        );
                    }
                    let name_text: Element<'_, Message> = if current {
                        widget::ellipsize::heading(name.clone(), widget::EllipsizeMode::End(1))
                            .wrapping(text::Wrapping::None)
                            .into()
                    } else {
                        widget::text::body(name.clone())
                            .wrapping(text::Wrapping::None)
                            .into()
                    };
                    if !current {
                        // Stands for this crumb and all before it when they
                        // do not fit, and goes where this one goes
                        more.push(crumb(
                            widget::text::body("...").into(),
                            index,
                            ancestor_location,
                            ancestor,
                            false,
                        ));
                    }

                    crumbs.push(crumb(
                        name_text,
                        index,
                        ancestor_location,
                        ancestor,
                        current,
                    ));
                }
                Some((widget::crumbs(icon, crumbs, separators, more), glide))
            }
            Location::Trash | Location::Search(SearchLocation::Trash, ..) => Some(single(
                Trash::icon_symbolic(16),
                fl!("trash"),
                Message::Location(Location::Trash),
            )),
            Location::Recents | Location::Search(SearchLocation::Recents, ..) => Some(single(
                widget::icon::from_name("document-open-recent-symbolic")
                    .size(16)
                    .handle(),
                fl!("recents"),
                Message::Location(Location::Recents),
            )),
            Location::Network(_, display_name, _) => Some(single(
                widget::icon::from_name("network-workgroup-symbolic")
                    .size(16)
                    .handle(),
                display_name.clone(),
                Message::EditLocation(Some(self.location.clone().into())),
            )),
        }
    }

    /// The top of the tab in list view: the sort bar, or for a search the
    /// count of its results and their order. Nothing in grid view.
    pub fn location_view(&self, column_slide: Option<&ColumnSlide>) -> Element<'_, Message> {
        let Spacing { space_s, .. } = spacing();
        if self.config.view != View::List {
            return widget::Column::new().into();
        }
        let bar = if self.is_search() {
            self.search_bar()
        } else {
            self.sort_bar(column_slide)
        };
        widget::container(bar)
            .padding([0, space_s])
            .height(Length::Fixed(SORT_BAR_HEIGHT))
            .align_y(Alignment::Center)
            .into()
    }

    /// The sorts the bar offers, each with its name.
    fn sort_choices(&self) -> [(HeadingOptions, String); 4] {
        let modified = if self.location.is_trash() {
            (HeadingOptions::TrashedOn, fl!("trashed-on"))
        } else {
            (HeadingOptions::Modified, fl!("modified"))
        };
        [
            (HeadingOptions::Name, fl!("name")),
            modified,
            (HeadingOptions::Size, fl!("size")),
            (HeadingOptions::Type, fl!("type-heading")),
        ]
    }

    /// Whether the sort bar has room for its labels in a view `width` wide:
    /// never under [`SORT_LABELS_MIN`], and never when the labels, measured
    /// in this language, would push the order button out of the bar.
    fn sort_labels_fit(&self, width: f32) -> bool {
        width >= SORT_LABELS_MIN && width >= self.sort_bar_labelled_width()
    }

    /// The width the sort bar takes with its labels shown, padding included.
    /// Every label is measured semibold, as the active one is drawn, and the
    /// order as its longest wording, so the answer does not change with the
    /// sort.
    fn sort_bar_labelled_width(&self) -> f32 {
        let Spacing {
            space_xxxs,
            space_xs,
            space_s,
            ..
        } = spacing();
        let (space_xxxs, space_xs, space_s) = (
            f32::from(space_xxxs),
            f32::from(space_xs),
            f32::from(space_s),
        );
        let choices = self.sort_choices();
        let buttons: f32 = choices
            .iter()
            .map(|(_, label)| {
                2.0 * space_xs + 16.0 + 6.0 + label_width(label, 14.0, crate::ui::font::semibold())
            })
            .sum::<f32>()
            + space_xxxs * (choices.len() - 1) as f32;
        let words = [
            HeadingOptions::Name,
            HeadingOptions::Modified,
            HeadingOptions::Size,
        ]
        .into_iter()
        .flat_map(|sort| [true, false].map(|ascending| sort_order_words(sort, ascending)))
        .map(|words| label_width(&words, 12.0, crate::ui::font::default()))
        .fold(0.0_f32, f32::max);
        let order = 2.0 * space_xs + words + 6.0 + 14.0;
        // The bar's own padding, and a gap between the buttons and the order
        2.0 * space_s + buttons + space_s + order
    }

    /// The four sorts as buttons, each an icon and its name (the icon alone
    /// in a narrow view, the name then in a tooltip), the active one filled;
    /// and at the right end the order in words, which reverses it.
    fn sort_bar(&self, column_slide: Option<&ColumnSlide>) -> Element<'_, Message> {
        let Spacing {
            space_xxxs,
            space_xxs,
            space_xs,
            ..
        } = spacing();
        let (sort_name, sort_direction, _) = self.sort_options();
        let labels = self
            .size_opt
            .get()
            .is_none_or(|size| self.sort_labels_fit(size.width));
        let choices = self.sort_choices();

        let mut buttons = widget::Row::with_capacity(choices.len())
            .spacing(space_xxxs.to_pixels())
            .align_y(Alignment::Center);
        for (option, label) in choices {
            let active = sort_name == option;
            let mut content = widget::Row::with_capacity(2)
                .spacing(6)
                .align_y(Alignment::Center)
                .push(widget::icon::icon(sort_icon(Some(option))).size(16));
            if labels {
                let mut text = widget::text::body(label.clone());
                if active {
                    text = text.font(crate::ui::font::semibold());
                }
                content = content.push(text);
            }
            let button = widget::button::custom(content)
                .padding([space_xxxs + 2, if labels { space_xs } else { space_xxs }])
                .class(sort_button_style(active))
                .on_press(Message::ToggleSort(option));
            buttons = buttons.push(if labels {
                Element::from(button)
            } else {
                widget::tooltip(
                    button,
                    widget::text::body(label),
                    widget::tooltip::Position::Bottom,
                )
                .into()
            });
        }

        // Clicking the active sort again reverses it, as this does
        let order: Element<'_, Message> = widget::button::custom(
            widget::Row::with_capacity(2)
                .spacing(6)
                .align_y(Alignment::Center)
                .push(widget::text::caption(sort_order_words(
                    sort_name,
                    sort_direction,
                )))
                .push(widget::icon::icon(sort_icon(None)).size(14)),
        )
        .padding([space_xxxs + 2, space_xs])
        .class(sort_button_style(false))
        .on_press(Message::ToggleSort(sort_name))
        .into();

        let mut row = widget::Row::with_capacity(4)
            .align_y(Alignment::Center)
            .push(buttons)
            .push(space::horizontal());
        // While the drawer slides, the order button slides over the gap with
        // the rows' right column; see `crate::ui::shell::panel_slide`.
        row = match column_slide {
            Some(slide) => row
                .push(space::horizontal().width(Length::Fixed(slide.extent)))
                .push(cached(TextureCache::new(), order).translate(slide.offset.clone())),
            None => row.push(order),
        };
        row.into()
    }

    /// A search's bar: how many results it has found, and their order, which
    /// is always newest first, so there is nothing to choose.
    fn search_bar(&self) -> Element<'_, Message> {
        let Spacing {
            space_xxs,
            space_xs,
            ..
        } = spacing();
        let show_hidden = self.config.show_hidden;
        let count = self.items_opt().map_or(0, |items| {
            items
                .iter()
                .filter(|item| !item.hidden || show_hidden)
                .count()
        });
        let (sort_name, sort_direction, _) = self.sort_options();
        widget::Row::with_capacity(4)
            .align_y(Alignment::Center)
            .spacing(space_xxs.to_pixels())
            .push(
                widget::icon::from_name("system-search-symbolic")
                    .size(16)
                    .icon(),
            )
            .push(widget::text::body(fl!("item-count", count = count)))
            .push(space::horizontal())
            .push(
                widget::container(
                    widget::text::caption(sort_order_words(sort_name, sort_direction))
                        .class(theme::Text::Custom(dim_text_style)),
                )
                .padding([0, space_xs]),
            )
            .into()
    }

    pub fn empty_view(&self, has_hidden: bool) -> Element<'_, Message> {
        let Spacing { space_xxs, .. } = spacing();

        mouse_area::MouseArea::new(widget::Column::with_children([widget::container(
            widget::Column::with_children([
                widget::icon::from_name("folder-symbolic")
                    .size(64)
                    .icon()
                    .into(),
                widget::text::body(if has_hidden {
                    fl!("empty-folder-hidden")
                } else if self.is_search() {
                    fl!("no-results")
                } else {
                    fl!("empty-folder")
                })
                .into(),
            ])
            .align_x(Alignment::Center)
            .spacing(space_xxs.to_pixels()),
        )
        .center(Length::Fill)
        .into()]))
        // Accept drops into empty folders. Without this handler, the compositor
        // accepted the drop but nobody read it, leaving an external source waiting for
        // `finish`.
        .on_dnd(Message::Dnd)
        .on_press(|_| Message::Click(None))
        .into()
    }

    pub fn grid_view(&self) -> (Element<'_, Message>, bool) {
        let Spacing {
            space_xxs,
            space_xxxs,
            ..
        } = spacing();

        let TabConfig {
            show_hidden,
            icon_sizes,
            ..
        } = self.config;

        // Intentionally the same as space_xxs; named separately to document
        // that this value is the spacing between grid items.
        let grid_spacing = space_xxs;

        let text_height = 3 * 20; // 3 lines of text
        let item_width = (3 * space_xxs + icon_sizes.grid() + 3 * space_xxs) as usize;
        let item_height =
            (space_xxxs + icon_sizes.grid() + space_xxxs + text_height + space_xxxs) as usize;

        let (width, height) = match self.size_opt.get() {
            Some(size) => (
                (size.width.floor() as usize)
                    .saturating_sub(2 * (space_xxs as usize))
                    .max(item_width),
                (size.height.floor() as usize).max(item_height),
            ),
            None => (item_width, item_height),
        };

        let (cols, column_spacing) = {
            let width_m1 = width.saturating_sub(item_width);
            let cols_m1 = width_m1 / (item_width + grid_spacing as usize);
            let cols = cols_m1 + 1;
            let spacing = width_m1
                .checked_div(cols_m1)
                .unwrap_or(0)
                .saturating_sub(item_width);
            (cols, spacing as u16)
        };

        let visible_rect = self.visible_rect(self.size_opt.get().unwrap_or_default());

        let mut grid = widget::grid()
            .column_spacing(column_spacing)
            .row_spacing(grid_spacing)
            .padding(space_xxs.into());

        let mut column = widget::Column::with_capacity(2);
        if let Some(items) = self.items_opt.as_ref()
            && let Some(order) = self.sorted_order()
        {
            let pitch = item_height + grid_spacing as usize;
            let column_pitch = item_width + column_spacing as usize;
            let key = LayoutKey {
                order: Arc::clone(&order),
                show_hidden,
                columns: cols,
                width: column_pitch as f32,
                pitch: pitch as f32,
            };
            // Every item's place, for the keyboard, selection and drops,
            // worked out only when the order or the geometry changed: a
            // scroll moves none of them
            let layout = self.laid_out(key, |key| {
                let mut shown = Vec::with_capacity(order.len());
                for &i in order.iter() {
                    let item = &items[i];
                    if !show_hidden && item.hidden {
                        item.pos_opt.set(None);
                        item.rect_opt.set(None);
                        continue;
                    }
                    let (row, col) = (shown.len() / cols, shown.len() % cols);
                    item.pos_opt.set(Some((row, col)));
                    item.rect_opt.set(Some(Rectangle::new(
                        Point::new(
                            (col * column_pitch + space_xxs as usize) as f32,
                            (row * pitch + space_xxs as usize) as f32,
                        ),
                        Size::new(item_width as f32, item_height as f32),
                    )));
                    shown.push(i);
                }
                ShownLayout {
                    key,
                    shown: shown.into(),
                    columns: cols,
                    top: f32::from(space_xxs),
                    pitch: pitch as f32,
                }
            });

            let count = layout.shown.len();
            if count == 0 {
                return (self.empty_view(!items.is_empty()), false);
            }
            let rows = count.div_ceil(cols);

            // Only the rows on screen are built; those above and below are
            // a spacer each
            let visible = layout.visible(visible_rect);
            if visible.is_empty() {
                // Nothing on screen: the whole grid's height, padding and
                // all
                let height = 2 * space_xxs as usize + rows * pitch - grid_spacing as usize;
                column = column.push(space::vertical().height(Length::Fixed(height as f32)));
            } else {
                let (first, last) = (visible.start / cols, (visible.end - 1) / cols);
                let mut keys = Vec::with_capacity(visible.len());
                if first > 0 {
                    column = column
                        .push(space::vertical().height(Length::Fixed((first * pitch) as f32)));
                }
                for row in first..=last {
                    for &i in &layout.shown[row * cols..((row + 1) * cols).min(count)] {
                        let item = &items[i];
                        let faded = item.faded(self.selection.is_selected(item));
                        let mut name = Item::grid_display_name(&item.display_name);
                        // A cut name takes the button's dimmed colour
                        if faded && !self.selection.is_cut(item) {
                            name = name.class(theme::Text::Custom(faded_text_style));
                        }
                        let buttons: Vec<Element<Message>> = vec![
                            widget::button::custom(
                                widget::icon::icon(item.icon_handle_grid.clone())
                                    .content_fit(ContentFit::Contain)
                                    .size(icon_sizes.grid())
                                    .opacity(if faded { FADED_OPACITY } else { 1.0 }),
                            )
                            .padding(space_xxxs)
                            .class(button_style(
                                self.selection.is_selected(item),
                                self.selection.is_highlighted(item),
                                self.selection.is_cut(item),
                                false,
                                false,
                            ))
                            .into(),
                            widget::tooltip(
                                widget::button::custom(name)
                                    .id(item.button_id.clone())
                                    .padding([0, space_xxxs])
                                    .class(button_style(
                                        self.selection.is_selected(item),
                                        self.selection.is_highlighted(item),
                                        self.selection.is_cut(item),
                                        true,
                                        true,
                                    )),
                                widget::text::body(&item.name),
                                widget::tooltip::Position::Bottom,
                            )
                            .into(),
                        ];

                        let mut column = widget::Column::with_capacity(buttons.len())
                            .align_x(Alignment::Center)
                            .height(Length::Fixed(item_height as f32))
                            .width(Length::Fixed(item_width as f32));
                        for button in buttons {
                            column = column.push(
                                mouse_area::MouseArea::new(button)
                                    .on_right_press_no_capture()
                                    .on_right_press(move |point_opt| {
                                        Message::RightClick(point_opt, Some(i))
                                    }),
                            );
                        }

                        let mouse_area = crate::mouse_area::MouseArea::new(column)
                            .touch_taps()
                            .identity(item.button_id.clone())
                            .on_press(move |_| Message::Click(Some(i)))
                            .on_drag(move |_| Message::DragFiles(i))
                            .on_double_click(move |_| Message::DoubleClick(Some(i)))
                            .on_release(move |_| Message::ClickRelease(Some(i)))
                            .on_middle_press(move |_| Message::MiddleClick(i))
                            .on_enter(move || Message::HighlightActivate(i))
                            .on_exit(move || Message::HighlightDeactivate(i));
                        keys.push(i as u64);
                        grid = grid.push(mouse_area);
                    }
                    grid = grid.insert_row();
                }
                // Each item's state is its own, whichever place it takes
                column = column.push(widget::keyed_children(keys, grid));
                if last + 1 < rows {
                    column = column.push(
                        space::vertical().height(Length::Fixed(((rows - last - 1) * pitch) as f32)),
                    );
                }
            }

            // Pad the content to the bottom of the view so the whole view is scrollable
            {
                // The last row's bottom edge
                let max_bottom = space_xxs as usize + (rows - 1) * pitch + item_height;

                // Room under the last row for the corner card, counted as
                // content so scrolling may reach it
                let reserve = self.bottom_reserve.get().ceil() as usize;

                // Cache content height for scroll clamping on next frame
                self.content_height_opt
                    .set(Some((max_bottom + reserve) as f32));

                let top_deduct = 7 * (space_xxs as usize);

                self.item_view_size_opt
                    .set(self.size_opt.get().map(|s| Size {
                        width: s.width,
                        height: s.height - top_deduct as f32,
                    }));

                let spacer_height = height.saturating_sub(max_bottom + top_deduct).max(reserve);
                if spacer_height > 0 {
                    column = column.push(widget::container(
                        space::vertical().height(Length::Fixed(spacer_height as f32)),
                    ));
                }
            }
        }

        let mut mouse_area = mouse_area::MouseArea::new(column.width(Length::Fill))
            .touch_taps()
            .on_dnd(Message::Dnd)
            .on_press(|_| Message::Click(None))
            .on_auto_scroll(Message::AutoScroll)
            .on_drag_end(|_| Message::DragEnd)
            .on_last_drag(Message::Drag)
            .show_drag_rect(self.mode.multiple())
            .on_release(|_| Message::ClickRelease(None));
        if self.watch_drag {
            mouse_area = mouse_area.on_drag(Message::Drag);
        }

        (mouse_area.into(), true)
    }

    /// Where a search result lives, under its name: its folder, relative to
    /// the folder the search started in.
    fn search_folder_text(&self, item: &Item) -> String {
        // A trashed item's path is in the trash's own storage, the same for
        // every result, so say where it was trashed from
        if let ItemMetadata::Trash { entry, .. } = &item.metadata {
            return entry.original_parent.as_ref().map_or_else(
                || fl!("trash-original-unknown"),
                |parent| parent.display().to_string(),
            );
        }
        let Some(path) = item.path_opt() else {
            return String::new();
        };
        let folder = path.parent().unwrap_or(path);
        let root = match &self.location {
            Location::Search(SearchLocation::Path(root), ..) => Some(root.as_path()),
            _ => None,
        };
        match root.and_then(|root| folder.strip_prefix(root).ok()) {
            Some(relative) if relative.as_os_str().is_empty() => {
                root.and_then(Path::file_name).map_or_else(
                    || folder.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                )
            }
            Some(relative) => relative.display().to_string(),
            None => folder.display().to_string(),
        }
    }

    pub fn list_view(&self, column_slide: Option<&ColumnSlide>) -> (Element<'_, Message>, bool) {
        let Spacing {
            space_s,
            space_xxs,
            space_xs,
            ..
        } = spacing();

        let TabConfig {
            show_hidden,
            icon_sizes,
            ..
        } = self.config;

        let size = self.size_opt.get().unwrap_or_else(|| Size::new(0.0, 0.0));
        let is_search = self.is_search();
        let (sort_name, _, _) = self.sort_options();
        let right_width = right_column_width(sort_name);
        let icon_size = icon_sizes.list();
        // Too narrow for the right column, its detail leads the second line
        let right_shown = self.shows_right_column(size.width);
        // Two lines of text: a body line (21px) over a caption line (17px)
        const TEXT_HEIGHT: u16 = 38;
        let row_height = icon_size.max(TEXT_HEIGHT) + 2 * space_xxs;

        let mut column = widget::Column::with_capacity(3);
        let mut y: f32 = 0.0;

        let visible_rect = self.visible_rect(self.size_opt.get().unwrap_or_default());

        if let Some(items) = self.items_opt.as_ref()
            && let Some(order) = self.sorted_order()
        {
            let pitch = f32::from(row_height);
            let width = size.width - f32::from(2 * space_s);
            let key = LayoutKey {
                order: Arc::clone(&order),
                show_hidden,
                columns: 1,
                width,
                pitch,
            };
            // Every item's place, for the keyboard, selection and drops,
            // worked out only when the order or the geometry changed: a
            // scroll moves none of them
            let layout = self.laid_out(key, |key| {
                let mut shown = Vec::with_capacity(order.len());
                for &i in order.iter() {
                    let item = &items[i];
                    if item.hidden && !show_hidden {
                        item.pos_opt.set(None);
                        item.rect_opt.set(None);
                        continue;
                    }
                    let row = shown.len();
                    item.pos_opt.set(Some((row, 0)));
                    #[allow(clippy::cast_precision_loss, reason = "a row number")]
                    item.rect_opt.set(Some(Rectangle::new(
                        Point::new(f32::from(space_s), row as f32 * pitch),
                        Size::new(width, pitch),
                    )));
                    shown.push(i);
                }
                ShownLayout {
                    key,
                    shown: shown.into(),
                    columns: 1,
                    top: 0.0,
                    pitch,
                }
            });

            if layout.shown.is_empty() {
                return (self.empty_view(!items.is_empty()), false);
            }

            // Only the rows on screen are built; those above and below are
            // a spacer each
            let visible = layout.visible(visible_rect);
            #[allow(clippy::cast_precision_loss, reason = "row counts")]
            let rows_height = |rows: usize| rows as f32 * pitch;
            // The spacers' keys, apart from every item's
            const ABOVE: u64 = u64::MAX;
            const BELOW: u64 = u64::MAX - 1;
            let mut keys = Vec::with_capacity(visible.len() + 2);
            let mut rows_column = widget::Column::with_capacity(visible.len() + 2);
            if visible.start > 0 {
                keys.push(ABOVE);
                rows_column = rows_column
                    .push(space::vertical().height(Length::Fixed(rows_height(visible.start))));
            }
            for count in visible.clone() {
                let i = layout.shown[count];
                let item = &items[i];
                let modified_text = match &item.metadata {
                    ItemMetadata::Path { metadata, .. } => match metadata.modified() {
                        Ok(time) => self.format_time(time).to_string(),
                        Err(_) => String::new(),
                    },
                    // Nothing says when, for what is inside a trashed
                    // folder or has no `.trashinfo`
                    ItemMetadata::Trash { entry, .. } if entry.time_deleted == 0 => String::new(),
                    ItemMetadata::Trash { entry, .. } => FormatTime::from_secs(
                        entry.time_deleted,
                        &self.date_time_formatter,
                        &self.time_formatter,
                    )
                    .map(|t| t.to_string())
                    .unwrap_or_default(),
                    #[cfg(feature = "gvfs")]
                    ItemMetadata::GvfsPath { .. } => match item.metadata.modified() {
                        Some(mtime) => self.format_time(mtime).to_string(),
                        None => String::new(),
                    },
                    _ => String::new(),
                };

                let size_text = match &item.metadata {
                    ItemMetadata::Path {
                        metadata,
                        children_opt,
                    } => {
                        if metadata.is_dir() {
                            children_opt.map_or_else(String::new, |children| {
                                fl!("item-count", count = children)
                            })
                        } else {
                            format_size(metadata.len())
                        }
                    }
                    ItemMetadata::Trash { metadata, .. } => match metadata.size {
                        crate::trashing::TrashItemSize::Entries(entries) => {
                            fl!("item-count", count = entries)
                        }
                        crate::trashing::TrashItemSize::Bytes(bytes) => format_size(bytes),
                    },
                    ItemMetadata::SimpleDir { entries } => fl!("item-count", count = entries),
                    ItemMetadata::SimpleFile { size } => format_size(*size),
                    #[cfg(feature = "gvfs")]
                    ItemMetadata::GvfsPath {
                        size_opt,
                        children_opt,
                        is_dir,
                        ..
                    } => {
                        if *is_dir {
                            // Children are not counted on remote filesystems
                            children_opt.map_or_else(String::new, |children| {
                                fl!("item-count", count = children)
                            })
                        } else {
                            format_size(size_opt.unwrap_or_default())
                        }
                    }
                };

                let type_text =
                    crate::file_category::FileCategory::of(item.metadata.is_dir(), &item.mime)
                        .to_string();

                let (mut second_line, right_text) = if is_search {
                    (self.search_folder_text(item), modified_text)
                } else {
                    list_details(sort_name, &type_text, &size_text, &modified_text)
                };
                if !right_shown {
                    second_line = line_with_right(&right_text, &second_line);
                }

                let mut name = Item::list_display_name(item.display_name.clone());
                if sort_name == HeadingOptions::Name && !is_search {
                    name = name.font(crate::ui::font::semibold());
                }
                // A selected or cut row's button sets the text colour
                // (on the accent fill, or dimmed), so its details take
                // that colour instead of their own
                let own_colours = !self.selection.is_selected(item) && !self.selection.is_cut(item);
                let faded = item.faded(self.selection.is_selected(item));
                if faded && own_colours {
                    name = name.class(theme::Text::Custom(faded_text_style));
                }
                let dim = if !own_colours {
                    theme::Text::Default
                } else if faded {
                    theme::Text::Custom(faded_dim_text_style)
                } else {
                    theme::Text::Custom(dim_text_style)
                };
                let text = widget::Column::with_capacity(2)
                    .width(Length::Fill)
                    .push(name)
                    .push(
                        widget::ellipsize::caption(second_line, widget::EllipsizeMode::Middle(1))
                            .class(dim),
                    );

                // The right column is dim like the details under the
                // name, whatever the list is sorted by
                let right: Element<'_, Message> = widget::text::body(right_text)
                    .width(Length::Fixed(right_width))
                    .align_x(text::Alignment::Right)
                    .wrapping(text::Wrapping::None)
                    .class(dim)
                    .into();

                let mut cells: Vec<Element<'_, Message>> = vec![
                    widget::icon::icon(item.icon_handle_list.clone())
                        .content_fit(ContentFit::Contain)
                        .size(icon_size)
                        .opacity(if faded { FADED_OPACITY } else { 1.0 })
                        .into(),
                    text.into(),
                ];
                if right_shown {
                    if let Some(slide) = column_slide {
                        // The name is held at its narrow width (its width with
                        // the drawer in the layout) and the right column
                        // slides over the gap as one texture, never over the
                        // name. The spacer brings one more row gap with it,
                        // so it is one gap short of `extent`.
                        debug_assert!(
                            slide.extent >= f32::from(space_xs),
                            "a drawer narrower than a gap"
                        );
                        cells.push(
                            space::horizontal()
                                .width(Length::Fixed(slide.extent - f32::from(space_xs)))
                                .into(),
                        );
                        cells.push(
                            cached(TextureCache::new(), right)
                                .translate(slide.offset.clone())
                                .into(),
                        );
                    } else {
                        cells.push(right);
                    }
                }
                let row = widget::Row::with_children(cells)
                    .height(Length::Fixed(f32::from(row_height)))
                    .align_y(Alignment::Center)
                    .spacing(space_xs.to_pixels());

                let mouse_area = crate::mouse_area::MouseArea::new(
                    widget::button::custom(row)
                        .width(Length::Fill)
                        .id(item.button_id.clone())
                        .padding([0, space_xs])
                        .class(button_style(
                            self.selection.is_selected(item),
                            self.selection.is_highlighted(item),
                            self.selection.is_cut(item),
                            true,
                            false,
                        )),
                )
                .touch_taps()
                .identity(item.button_id.clone())
                .on_press(move |_| Message::Click(Some(i)))
                .on_drag(move |_| Message::DragFiles(i))
                .on_double_click(move |_| Message::DoubleClick(Some(i)))
                .on_release(move |_| Message::ClickRelease(Some(i)))
                .on_middle_press(move |_| Message::MiddleClick(i))
                .on_enter(move || Message::HighlightActivate(i))
                .on_exit(move || Message::HighlightDeactivate(i))
                .on_right_press_no_capture()
                .on_right_press(move |point_opt| Message::RightClick(point_opt, Some(i)));

                // Every other row is striped; hover and selection paint
                // over the stripe
                let row: Element<'_, Message> = if count % 2 == 1 {
                    widget::container(mouse_area).style(stripe_style).into()
                } else {
                    mouse_area.into()
                };
                keys.push(i as u64);
                rows_column = rows_column.push(row);
            }
            let below = layout.shown.len() - visible.end;
            if below > 0 {
                keys.push(BELOW);
                rows_column =
                    rows_column.push(space::vertical().height(Length::Fixed(rows_height(below))));
            }
            // Each row's state is its item's, whichever place it takes
            column = column.push(widget::keyed_children(keys, rows_column));
            y = rows_height(layout.shown.len());

            // Cache content height for scroll clamping on next frame, with
            // the room kept under the last row for the corner card
            self.content_height_opt
                .set(Some(y + self.bottom_reserve.get()));
        }
        // Pad the content to the bottom of the view so the whole view is
        // scrollable; the sort bar sits above it
        {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a small, positive constant"
            )]
            let top_deduct = 6 * space_xxs + SORT_BAR_HEIGHT as u16;

            self.item_view_size_opt
                .set(self.size_opt.get().map(|s| Size {
                    width: s.width,
                    height: s.height - f32::from(top_deduct),
                }));

            let spacer_height =
                (size.height - y - f32::from(top_deduct)).max(self.bottom_reserve.get());
            if spacer_height > 0. {
                column = column.push(widget::container(space::vertical().height(spacer_height)));
            }
        }
        let mut mouse_area = mouse_area::MouseArea::new(column.padding([0, space_s]))
            .with_id(Id::new("list-view"))
            .touch_taps()
            .on_dnd(Message::Dnd)
            .on_press(|_| Message::Click(None))
            .on_auto_scroll(Message::AutoScroll)
            .on_drag_end(|_| Message::DragEnd)
            .on_last_drag(Message::Drag)
            .show_drag_rect(self.mode.multiple())
            .on_release(|_| Message::ClickRelease(None));
        if self.watch_drag {
            mouse_area = mouse_area.on_drag(Message::Drag);
        }

        (mouse_area.into(), true)
    }

    pub fn view_responsive<'a>(
        &'a self,
        key_binds: &'a HashMap<KeyBind, Action>,
        modifiers: &'a Modifiers,
        size: Size,
        sources: menu::Sources<'a>,
        column_slide: Option<&ColumnSlide>,
    ) -> Element<'a, Message> {
        // Update cached size
        self.size_opt.set(Some(size));

        let location_view = self.location_view(column_slide);
        let (item_view, can_scroll) = match self.config.view {
            View::Grid => self.grid_view(),
            View::List => self.list_view(column_slide),
        };
        let item_view: Element<'a, Message> =
            widget::container(item_view).width(Length::Fill).into();
        let mouse_area = mouse_area::MouseArea::new(item_view)
            .touch_taps()
            .on_press(move |_point_opt| Message::Click(None))
            .on_release(|_| Message::ClickRelease(None))
            .on_resize(Message::Resize)
            .on_back_press(move |_point_opt| Message::GoPrevious)
            .on_forward_press(move |_point_opt| Message::GoNext)
            .on_right_press(|_| Message::RightClickBackground);

        let items_area: Element<'_, Message> = if can_scroll {
            // FIXME: new responsive widget will remove the state from the scrollable
            // id_container with custom id forces the state to be extracted in a diff
            // pre-processing step
            widget::id_container(
                widget::scrollable(mouse_area)
                    .id(self.scrollable_id.clone())
                    .on_scroll(Message::Scroll)
                    .width(Length::Fill)
                    .height(Length::Fill),
                widget::Id::from(format!("{}-scrollable", self.scrollable_name)),
            )
            .into()
        } else {
            mouse_area.into()
        };
        // Ctrl with the wheel zooms, and only outside the scrollable does it
        // also do so right after a scroll
        let items_area: Element<'_, Message> = mouse_area::MouseArea::new(items_area)
            .scroll_first()
            .on_scroll(|delta| respond_to_scroll_direction(delta, modifiers))
            .into();

        // Wrap the scrollable, not its content, so the popup anchors in window coordinates
        let mut context_menu = widget::context_menu(
            items_area,
            Some(menu::context_menu(self, key_binds, &sources)),
        )
        // Static, so a child popup can be narrower than the menu: a split
        // row's variants, a program's actions, a dimmed row's reason
        .item_width(crate::ui::widget::menu::ItemWidth::Static(360))
        .on_surface_action(Message::Surface);
        if let Some(window_id) = self.window_id {
            context_menu = context_menu.window_id(window_id);
        }

        let mut tab_column = widget::Column::with_capacity(2);
        tab_column = tab_column.push(location_view);
        tab_column = tab_column.push(context_menu);
        let tab_view = widget::container(tab_column)
            .height(Length::Fill)
            .width(Length::Fill);

        tab_view.into()
    }
    pub fn multi_preview_view<'a>(
        &'a self,
        mime_app_cache_opt: Option<&'a mime_app::MimeAppCache>,
    ) -> Element<'a, Message> {
        let Spacing {
            space_xxxs,
            space_m,
            ..
        } = spacing();

        let mut column = widget::Column::with_capacity(4).spacing(space_m.to_pixels());

        let handle = widget::icon::from_name("text-x-generic")
            .size(IconSizes::default().grid())
            .handle();

        let icon = widget::icon::icon(handle.clone())
            .content_fit(ContentFit::Contain)
            .size(IconSizes::default().grid());

        let icon_container1 = widget::container(icon.clone()).padding(padding::bottom(10).left(10));
        let icon_container2 =
            widget::container(icon.clone()).padding(padding::top(5).bottom(5).left(5).right(5));
        let icon_container3 = widget::container(icon).padding(padding::top(10).right(10));
        let stack = stack![icon_container1, icon_container2, icon_container3];

        column = column.push(
            widget::container(stack)
                .center_x(Length::Fill)
                .max_height(THUMBNAIL_SIZE as f32),
        );

        let selected_items: Vec<&Item> = self.items_opt().map_or(Vec::new(), |items| {
            self.selected_indices()
                .iter()
                .filter_map(|&index| items.get(index))
                .filter(|item| {
                    if self.selection.is_selected(item) {
                        item.location_opt
                            .as_ref()
                            .and_then(Location::path_opt)
                            .is_some()
                    } else {
                        false
                    }
                })
                .collect()
        });

        let mut details = widget::Column::with_capacity(3).spacing(space_xxxs.to_pixels());
        details = details.push(widget::text::body(fl!(
            "items",
            items = selected_items.len()
        )));

        let mut total_size: u64 = 0;
        let mut mime_type_counts: BTreeMap<String, u64> = BTreeMap::new();
        let mut user_name: BTreeSet<String> = BTreeSet::new();
        let mut mode_user: BTreeSet<u32> = BTreeSet::new();
        let mut group_name: BTreeSet<String> = BTreeSet::new();
        let mut mode_group: BTreeSet<u32> = BTreeSet::new();
        let mut mode_other: BTreeSet<u32> = BTreeSet::new();
        let mut calculating_dir_size = false;
        let mut dir_size_error: Option<String> = None;
        // Whether every selected item's ownership is known. The permission
        // controls act on all of them, and an item whose details are still
        // out -- or could not be read -- has no mode to shift; offering the
        // controls anyway would either skip it silently or have to read it
        // here, on the event loop, which is the thing the preview avoids.
        let mut permissions_known = true;

        for item in selected_items.iter() {
            *mime_type_counts.entry(item.mime.to_string()).or_insert(0) += 1;

            // Sizes from the listing, which every item has
            if item.metadata.is_dir() {
                match &item.dir_size {
                    DirSize::Calculating(_) => {
                        calculating_dir_size = true;
                    }
                    DirSize::Directory(size) => {
                        total_size = total_size.saturating_add(*size);
                    }
                    DirSize::NotDirectory => (),
                    DirSize::Error(err) => {
                        dir_size_error = Some(err.clone());
                    }
                };
            } else if let Some(size) = item.metadata.file_size() {
                total_size = total_size.saturating_add(size);
            }
            // Ownership from the disk, which some items are still being asked
            // for. A total with some of its items unanswered is shown the way
            // a total with a folder still being walked is: as not ready yet.
            match (item.file_metadata(), &item.details) {
                (Some(metadata), _) => {
                    let mode = metadata.mode();
                    user_name.insert(crate::tab::user_name(metadata.uid()));
                    mode_user.insert(get_mode_part(mode, MODE_SHIFT_USER));
                    group_name.insert(crate::tab::group_name(metadata.gid()));
                    mode_group.insert(get_mode_part(mode, MODE_SHIFT_GROUP));
                    mode_other.insert(get_mode_part(mode, MODE_SHIFT_OTHER));
                }
                (None, MetadataState::Pending | MetadataState::Loading) => {
                    calculating_dir_size = true;
                    permissions_known = false;
                }
                (None, MetadataState::Failed(err)) => {
                    dir_size_error = Some(err.clone());
                    permissions_known = false;
                }
                (None, MetadataState::Ready(_)) => {}
            }
        }
        let mut mime_types: Vec<(String, u64)> = mime_type_counts.into_iter().collect();
        mime_types.sort_by(|(_, v1), (_, v2)| v2.cmp(v1));

        // Limit the number of displayed mime types
        let limit = usize::min(10, mime_types.len());

        let mut mime_type_strings: Vec<String> = mime_types[..limit]
            .iter()
            .map(|(mime, count)| format!("{} ({})", mime, count))
            .collect();

        if mime_types.len() > limit {
            mime_type_strings.push("...".to_string());
        }

        details = details.push(widget::text::body(fl!(
            "type",
            mime = mime_type_strings.join(", ")
        )));

        let size = {
            if calculating_dir_size {
                fl!("calculating")
            } else if let Some(error) = dir_size_error {
                error
            } else {
                format_size(total_size)
            }
        };

        details = details.push(widget::text::body(fl!("item-size", size = size)));

        column = column.push(details);

        column = column.push(widget::button::standard(fl!("open")).on_press(Message::Open(None)));

        let mut settings = Vec::new();
        // Only allow modifying open-with if all mime types are the same, and
        // only once they are known rather than guessed from the names: this
        // sets the default application for the type
        if selected_items.iter().all(|item| item.type_checked)
            && mime_types.len() == 1
            && let Some(mime) = mime_types
                .first()
                .and_then(|(mime, _)| mime.parse::<Mime>().ok())
            && let Some(mime_app_cache) = mime_app_cache_opt
        {
            let mime_apps = mime_app_cache.get_apps_for_mime(&mime, false);
            if !mime_apps.is_empty() {
                let mime_closure = mime.clone();
                let (names, icons) = mime_apps
                    .iter()
                    .map(|(app, _)| (Cow::Owned(app.name.clone()), app.icon()))
                    .collect::<(Vec<_>, Vec<_>)>();
                settings.push(
                    widget::settings::item::builder(fl!("open-with")).control(
                        Element::from(
                            widget::dropdown(
                                names,
                                mime_apps
                                    .iter()
                                    .position(|(x, _)| mime_app_cache.is_default(x, &mime)),
                                move |index| (index, mime_closure.clone()),
                            )
                            .icons(Cow::Owned(icons)),
                        )
                        .map(move |(index, mime)| {
                            let mime_app = &mime_apps[index].0;
                            Message::SetOpenWith(mime, mime_app.id.clone())
                        }),
                    ),
                );
            }
        }

        // Only return mode part if it's the only one
        fn selected_mode_part(mut modes: BTreeSet<u32>) -> Option<usize> {
            match (modes.pop_first(), modes.pop_first()) {
                (Some(mode), None) => Some(mode.try_into().unwrap()),
                _ => None,
            }
        }

        // Convert a limited number of values from a set into a comma separated list
        fn join_set(set: BTreeSet<String>) -> String {
            let limit = 5;
            let mut title = set.into_iter().collect::<Vec<String>>();
            if title.len() > limit {
                title.truncate(limit);
                title.push("...".to_string());
            }
            title.join(", ")
        }

        if permissions_known {
            let mode_part_user = selected_mode_part(mode_user);
            settings.push(
                widget::settings::item::builder(join_set(user_name))
                    .description(fl!("owner"))
                    .control(
                        widget::dropdown(
                            Cow::Borrowed(MODE_NAMES.as_slice()),
                            mode_part_user,
                            move |selected| {
                                Message::ShiftPermissions(
                                    None,
                                    MODE_SHIFT_USER,
                                    selected.try_into().unwrap(),
                                )
                            },
                        )
                        .placeholder(fl!("mixed")),
                    ),
            );

            let mode_part_group = selected_mode_part(mode_group);
            settings.push(
                widget::settings::item::builder(join_set(group_name))
                    .description(fl!("group"))
                    .control(
                        widget::dropdown(
                            Cow::Borrowed(MODE_NAMES.as_slice()),
                            mode_part_group,
                            move |selected| {
                                Message::ShiftPermissions(
                                    None,
                                    MODE_SHIFT_GROUP,
                                    selected.try_into().unwrap(),
                                )
                            },
                        )
                        .placeholder(fl!("mixed")),
                    ),
            );

            let mode_part_other = selected_mode_part(mode_other);
            settings.push(
                widget::settings::item::builder(fl!("other")).control(
                    widget::dropdown(
                        Cow::Borrowed(MODE_NAMES.as_slice()),
                        mode_part_other,
                        move |selected| {
                            Message::ShiftPermissions(
                                None,
                                MODE_SHIFT_OTHER,
                                selected.try_into().unwrap(),
                            )
                        },
                    )
                    .placeholder(fl!("mixed")),
                ),
            );
        } else {
            // Not offered until they can act on every item. Drawn as what is
            // happening rather than as a control that would do the wrong thing.
            settings.push(
                widget::settings::item::builder(fl!("owner"))
                    .control(widget::text::body(fl!("calculating"))),
            );
        }

        if !settings.is_empty() {
            let mut section = widget::settings::section();
            section = section.extend(settings);
            column = column.push(section);
        }

        column.into()
    }
    pub fn view<'a>(
        &'a self,
        key_binds: &'a HashMap<KeyBind, Action>,
        modifiers: &'a Modifiers,
        sources: menu::Sources<'a>,
        column_slide: Option<ColumnSlide>,
        bottom_reserve: Option<&'a Cell<f32>>,
    ) -> Element<'a, Message> {
        widget::responsive(move |size| {
            // Read at layout, not when the view was built: the card it makes
            // room for records its height as it is laid out
            self.bottom_reserve
                .set(bottom_reserve.map_or(0.0, Cell::get));
            widget::id_container(
                self.view_responsive(
                    key_binds,
                    modifiers,
                    size,
                    sources.clone(),
                    column_slide.as_ref(),
                ),
                Id::from(format!(
                    "tab-{}-{}",
                    self.scrollable_name, self.location_title
                )),
            )
            .into()
        })
        .into()
    }

    pub fn subscription(&self, preview: bool) -> Subscription<Message> {
        let jobs = self.thumb_config.jobs.get() as usize;
        let mut subscriptions = Vec::with_capacity(jobs + 3);

        // Read the files this listing typed by name, one at a time on a
        // worker. Keyed by the listing (the app adds the tab), so a rescan or
        // closing the tab drops it, and with it the receiver the worker checks
        // before each file.
        if let Some(job) = self.type_check_job() {
            struct TypeCheckJob {
                listing: u64,
                key: u64,
                files: Arc<[crate::type_check::Unchecked]>,
                sizes: IconSizes,
            }
            impl Hash for TypeCheckJob {
                fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                    self.listing.hash(state);
                    self.key.hash(state);
                }
            }

            subscriptions.push(Subscription::run_with(
                TypeCheckJob {
                    listing: self.listing,
                    key: job.key,
                    files: job.files,
                    sizes: self.config.icon_sizes,
                },
                |job| {
                    let (listing, files, sizes) = (job.listing, Arc::clone(&job.files), job.sizes);
                    stream::channel(
                        1,
                        move |mut output: futures::channel::mpsc::Sender<_>| async move {
                            let (results, mut received) = mpsc::channel(4);
                            tokio::task::spawn_blocking(move || {
                                let start = Instant::now();
                                let checked = crate::type_check::run(&files, sizes, &results);
                                log::debug!(
                                    "checked the types of {checked} of {} files in {:?}",
                                    files.len(),
                                    start.elapsed()
                                );
                            });
                            while let Some(batch) = received.recv().await {
                                if output
                                    .send(Message::TypesChecked(listing, batch))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            std::future::pending::<()>().await;
                        },
                    )
                },
            ));
        }

        // Add up what the trash holds, walking its folders on the same
        // bounded pool as the other folder sizes. Keyed by the listing, so a
        // new one starts over and leaving the trash cancels the walk.
        if self.location == Location::Trash
            && self.trash_size == TrashSize::Calculating
            && let Some(items) = self.items_opt()
        {
            struct TrashSizeJob {
                listing: u64,
                input: Arc<TrashSizeInput>,
            }
            impl Hash for TrashSizeJob {
                fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                    self.listing.hash(state);
                }
            }

            let input = Arc::clone(
                self.trash_size_input
                    .get_or_init(|| Arc::new(TrashSizeInput::new(items))),
            );
            subscriptions.push(Subscription::run_with(
                TrashSizeJob {
                    listing: self.listing,
                    input,
                },
                |job| {
                    let (listing, input) = (job.listing, Arc::clone(&job.input));
                    stream::channel(
                        1,
                        move |mut output: futures::channel::mpsc::Sender<_>| async move {
                            let controller = Controller::default();
                            let _cancel = CancelOnDrop::new(controller.clone());
                            let start = Instant::now();
                            let size = input.total(controller).await;
                            log::debug!(
                                "added up the trash ({} folders) in {:?}",
                                input.folders.len(),
                                start.elapsed()
                            );
                            let _ = output.send(Message::TrashSize(listing, size)).await;
                            std::future::pending::<()>().await;
                        },
                    )
                },
            ));
        }

        if let Some(items) = &self.items_opt {
            let visible_rect = self.visible_rect(self.size_opt.get().unwrap_or_default());
            // Only what is on screen is counted or thumbnailed, so only that
            // is looked at
            let visible = self.visible_items();

            // Count the children of visible directories in the background. Doing it while
            // scanning costs one directory listing per entry, which stalls remote filesystems.
            for &(index, item) in &visible {
                let uncounted_dir = match &item.metadata {
                    ItemMetadata::Path {
                        metadata,
                        children_opt: None,
                    } => metadata.is_dir(),
                    #[cfg(feature = "gvfs")]
                    ItemMetadata::GvfsPath {
                        children_opt: None,
                        is_dir: true,
                        ..
                    } => true,
                    _ => false,
                };
                if !uncounted_dir {
                    continue;
                }

                // Skip items that are not visible, or have no determined rect
                match item.rect_opt.get() {
                    Some(rect) if rect.intersects(&visible_rect) => {}
                    _ => continue,
                }

                let Some(path) = item.path_opt().cloned() else {
                    continue;
                };

                /// The folder to count, and where its item was when asked,
                /// so the answer finds it without a search. Keyed by the
                /// path alone: a move of the item does not restart the count.
                struct ChildrenWrapper(usize, PathBuf);
                impl Hash for ChildrenWrapper {
                    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                        self.1.hash(state);
                    }
                }

                subscriptions.push(Subscription::run_with(
                    ChildrenWrapper(index, path),
                    |ChildrenWrapper(index, path)| {
                        let (index, path) = (*index, path.clone());
                        stream::channel(
                            1,
                            move |mut output: futures::channel::mpsc::Sender<_>| async move {
                                let message = {
                                    let path = path.clone();
                                    tokio::task::spawn_blocking(move || {
                                        let children = match fs::read_dir(&path) {
                                            Ok(entries) => entries.count(),
                                            Err(err) => {
                                                log::warn!(
                                                    "failed to read directory {}: {}",
                                                    path.display(),
                                                    err
                                                );
                                                0
                                            }
                                        };
                                        Message::DirectoryChildren(index, path, children)
                                    })
                                    .await
                                    .unwrap()
                                };

                                if let Err(err) = output.send(message).await {
                                    log::warn!("failed to send directory children: {err}");
                                }

                                std::future::pending().await
                            },
                        )
                    },
                ));
            }

            for &(_, item) in &visible {
                if !item.wants_thumbnail() {
                    // Already thumbnailed, or its type is still a guess from
                    // its name
                    continue;
                }

                match item.rect_opt.get() {
                    Some(rect) => {
                        if !rect.intersects(&visible_rect) {
                            // Skip items that are not visible
                            continue;
                        }
                    }
                    None => {
                        // Skip items with no determined rect (this should include hidden items)
                        continue;
                    }
                }

                let Some(path) = item.path_opt().cloned() else {
                    continue;
                };

                let metadata = item.metadata.clone();
                let can_thumbnail = match metadata {
                    ItemMetadata::Path { .. } => true,
                    #[cfg(feature = "gvfs")]
                    ItemMetadata::GvfsPath { .. } => true,
                    _ => false,
                };
                if can_thumbnail {
                    let mime = item.mime.clone();
                    let modified = item.metadata.modified();
                    let max_jobs = jobs;
                    let max_mb = u64::from(self.thumb_config.max_mem_mb.get());
                    let max_size = u64::from(self.thumb_config.max_size_mb.get());

                    // Determine effective memory budget based on image size
                    let (dedicated, effective_max_mb, effective_jobs) = if mime.is_media("image") {
                        match item.image_dimensions.get().copied().flatten() {
                            Some((width, height)) => {
                                should_use_dedicated_worker(width, height, max_mb, max_jobs)
                            }
                            None => (false, max_mb, max_jobs),
                        }
                    } else {
                        (false, max_mb, max_jobs)
                    };
                    // An image allowed the whole budget runs alone: beside
                    // other jobs, each large one would take the budget again
                    let permits = if dedicated { *THUMB_PERMITS } else { 1 };

                    #[derive(Clone)]
                    struct Wrapper {
                        path: PathBuf,
                        modified: Option<SystemTime>,
                        permits: u32,
                        metadata: ItemMetadata,
                        mime: Mime,
                        effective_max_mb: u64,
                        effective_jobs: usize,
                        max_size: u64,
                    }

                    impl Hash for Wrapper {
                        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                            self.path.hash(state);
                            // A file replaced at the path is thumbnailed
                            // afresh, not given the old one's job
                            self.modified.hash(state);
                        }
                    }

                    subscriptions.push(Subscription::run_with(
                        Wrapper {
                            path: path.clone(),
                            modified,
                            permits,
                            metadata,
                            mime,
                            effective_max_mb,
                            effective_jobs,
                            max_size,
                        },
                        |wrapper| {
                            let Wrapper {
                                path,
                                modified,
                                permits,
                                metadata,
                                mime,
                                effective_max_mb,
                                effective_jobs,
                                max_size,
                            } = wrapper.clone();
                            stream::channel(
                                1,
                                move |mut output: futures::channel::mpsc::Sender<_>| async move {
                                    while crate::operation::is_actively_writing_to(&path) {
                                        crate::operation::actively_writing_tick().await;
                                    }

                                    let message = {
                                        let path = path.clone();

                                        bounded_blocking_taking(
                                            Arc::clone(&THUMB_SEMAPHORE),
                                            permits,
                                            move || {
                                                let start = Instant::now();
                                                let thumbnail = ItemThumbnail::new(
                                                    &path,
                                                    metadata,
                                                    mime,
                                                    THUMBNAIL_SIZE,
                                                    effective_max_mb,
                                                    effective_jobs,
                                                    max_size,
                                                );
                                                log::debug!(
                                                    "thumbnailed {} in {:?}",
                                                    path.display(),
                                                    start.elapsed()
                                                );
                                                Message::Thumbnail(path, modified, thumbnail)
                                            },
                                        )
                                        .await
                                    };

                                    // The job gave up its turn or the pool went
                                    // away; either way nothing is coming.
                                    let Some(message) = message else {
                                        return std::future::pending().await;
                                    };

                                    match output.send(message).await {
                                        Ok(()) => {}
                                        Err(err) => {
                                            log::warn!(
                                                "failed to send thumbnail for {}: {}",
                                                path.display(),
                                                err
                                            );
                                        }
                                    }

                                    std::future::pending().await
                                },
                            )
                        },
                    ));
                }

                if subscriptions.len() >= jobs {
                    break;
                }
            }

            if preview {
                // Load directory size for selected items

                let mut selected_items: Vec<&Item> = self
                    .selected_indices()
                    .iter()
                    .filter_map(|&index| items.get(index))
                    .collect();

                if selected_items.is_empty()
                    && let Some(p) = self.parent_item_opt.as_ref()
                {
                    selected_items.push(p)
                }
                for item in selected_items {
                    // Item must have a path
                    if let Some(path) = item.path_opt().cloned() {
                        // Details the listing does not carry, for items that
                        // have none of their own. This only asks: the read
                        // itself is started by `update`, and answers by message
                        // whether or not the item is still previewed by then.
                        // Declared here, so the asking exists exactly while
                        // the item is previewed with nothing asked yet; keyed
                        // on the incarnation and the listing, so a rebuilt or
                        // rescanned item is asked about afresh.
                        if !matches!(item.metadata, ItemMetadata::Path { .. })
                            && matches!(item.details, MetadataState::Pending)
                        {
                            struct DetailsKey {
                                path: PathBuf,
                                epoch: u64,
                                listing: u64,
                            }
                            impl Hash for DetailsKey {
                                fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                                    self.path.hash(state);
                                    self.epoch.hash(state);
                                    self.listing.hash(state);
                                }
                            }
                            subscriptions.push(Subscription::run_with(
                                DetailsKey {
                                    path: path.clone(),
                                    epoch: item.details_epoch,
                                    listing: self.listing,
                                },
                                |DetailsKey { path, epoch, listing }| {
                                    let (path, epoch, listing) = (path.clone(), *epoch, *listing);
                                    stream::channel(1, move |mut output: futures::channel::mpsc::Sender<_>| async move {
                                        let _ = output
                                            .send(Message::RequestDetails(path, epoch, listing))
                                            .await;
                                        std::future::pending().await
                                    })
                                },
                            ));
                        }
                        // Item must be calculating directory size
                        if let DirSize::Calculating(_) = &item.dir_size {
                            struct Wrapper {
                                path: PathBuf,
                            }
                            impl Hash for Wrapper {
                                fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                                    self.path.hash(state);
                                }
                            }
                            subscriptions.push(Subscription::run_with(
                                Wrapper { path: path.clone() },
                                |Wrapper { path }| {
                                    let path = path.clone();
                                    stream::channel(1, |mut output: futures::channel::mpsc::Sender<_>| async move {
                                        // A controller of this run's own, never the item's.
                                        // The item's outlives every run, so resetting it to
                                        // Running for a new one would also revive the walk the
                                        // previous run had just cancelled: a worker still
                                        // inside a slow read would wake to find itself Running
                                        // again and carry on, and two walks would share the
                                        // permits meant for one. A cancellation must stay
                                        // cancelled, so each run gets a fresh one and the
                                        // item's is left alone.
                                        let controller = Controller::default();
                                        // Cancel the walk when this subscription goes away,
                                        // which is the only notice a blocking job gets.
                                        let _cancel = CancelOnDrop::new(controller.clone());
                                        let message = {
                                            let start = Instant::now();
                                            match calculate_dir_size(&path, controller).await {
                                                Ok(size) => {
                                                    log::debug!(
                                                        "calculated directory size of {} in {:?}",
                                                        path.display(),
                                                        start.elapsed()
                                                    );
                                                    Message::DirectorySize(
                                                        path.clone(),
                                                        DirSize::Directory(size),
                                                    )
                                                }
                                                Err(err) => {
                                                    log::warn!(
                                                        "failed to calculate directory size of {}: {}",
                                                        path.display(),
                                                        err
                                                    );
                                                    Message::DirectorySize(
                                                        path.clone(),
                                                        DirSize::Error(err.to_string()),
                                                    )
                                                }
                                            }
                                        };

                                        match output.send(message).await {
                                            Ok(()) => {}
                                            Err(err) => {
                                                log::warn!(
                                                    "failed to send directory size for {}: {}",
                                                    path.display(),
                                                    err
                                                );
                                            }
                                        }

                                        std::future::pending().await
                                    })
                                }
                            ));
                        }
                    }
                }
            }
        }

        // Load search items incrementally. An empty search of a folder lists
        // the folder instead, in full.
        if let Location::Search(search_location, term, options, start) = &self.location
            && self.location.empty_search_folder().is_none()
        {
            let location = self.location.clone();
            let search_location = search_location.clone();
            let term = term.clone();
            let options = *options;
            let start = *start;
            /// Left out of the key: a zoom while searching takes new icons,
            /// not a new search
            #[derive(Debug, Clone)]
            struct Unkeyed<T>(T);
            impl<T> Hash for Unkeyed<T> {
                fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {}
            }

            #[derive(Debug, Hash, Clone)]
            struct Wrapper {
                location: Location,
                search_location: SearchLocation,
                term: String,
                options: SearchOptions,
                start: Instant,
                sizes: Unkeyed<IconSizes>,
            }

            subscriptions.push(Subscription::run_with(
                Wrapper {
                    location: location.clone(),
                    search_location: search_location.clone(),
                    term: term.clone(),
                    options,
                    start,
                    sizes: Unkeyed(self.config.icon_sizes),
                },
                |wrapper| {
                    let wrapper = wrapper.clone();
                    stream::channel(
                        2,
                        move |mut output: futures::channel::mpsc::Sender<Message>| async move {
                            let Wrapper {
                                location,
                                search_location,
                                term,
                                options,
                                start,
                                sizes: Unkeyed(sizes),
                            } = wrapper;
                            let (results_tx, results_rx) = mpsc::channel(65536);

                            let ready = Arc::new(atomic::AtomicBool::new(false));
                            let last_modified_opt = Arc::new(RwLock::new(None));
                            output
                                .send(Message::SearchContext(
                                    location.clone(),
                                    Handoff::new(SearchContext {
                                        results_rx,
                                        ready: ready.clone(),
                                        last_modified_opt: last_modified_opt.clone(),
                                    }),
                                ))
                                .await
                                .unwrap();

                            let (watch_tx, mut watch_rx) = tokio::sync::watch::channel(true);
                            {
                                tokio::task::spawn_blocking(move || {
                                    // Wanted while the tab still holds the
                                    // other end: a newer search, or leaving,
                                    // lets go of it
                                    let open_tx = results_tx.clone();
                                    scan_search(
                                        &search_location,
                                        &term,
                                        options,
                                        &|| !open_tx.is_closed(),
                                        move |search_item| -> bool {
                                            // Don't send if the result is too old
                                            if let Some(last_modified) =
                                                *last_modified_opt.read().unwrap()
                                                && let SearchItem::Path(_, _, ref metadata) =
                                                    search_item
                                            {
                                                if let Ok(modified) = metadata.modified() {
                                                    if modified < last_modified {
                                                        return true;
                                                    }
                                                } else {
                                                    return true;
                                                }
                                            }

                                            // Built here, not as it is
                                            // shown: typing a file reads it,
                                            // which a slow drive or a
                                            // launcher makes slow
                                            let item = item_from_search_item(search_item, sizes);
                                            match results_tx.blocking_send(item) {
                                                Ok(()) => {
                                                    if ready.swap(true, atomic::Ordering::SeqCst) {
                                                        true
                                                    } else {
                                                        // Wake up update method
                                                        watch_tx.send(false).is_ok()
                                                    }
                                                }
                                                Err(_) => false,
                                            }
                                        },
                                    );
                                    log::info!(
                                        "searched for {:?} in {} in {:?}",
                                        term,
                                        search_location,
                                        start.elapsed(),
                                    );
                                });
                            }

                            while watch_rx.changed().await.is_ok() {
                                let is_ready = *watch_rx.borrow_and_update();
                                let _ = output.send(Message::SearchReady(is_ready)).await;
                            }

                            // Send final ready
                            let _ = output.send(Message::SearchReady(true)).await;

                            std::future::pending().await
                        },
                    )
                },
            ));
        }

        if let Some(path) = self
            .edit_location
            .as_ref()
            .and_then(|x| x.location.path_opt())
        {
            subscriptions.push(tab_complete_subscription(path));
        }

        Subscription::batch(subscriptions)
    }

    const fn format_time(&self, time: SystemTime) -> FormatTime<'_> {
        format_time(time, &self.date_time_formatter, &self.time_formatter)
    }
}

/// Looks up the completions for `path` once, answering with `TabComplete`.
///
/// Keyed on the path's text rather than the `PathBuf`. The runtime restarts a
/// subscription only when its key hashes differently, and a `PathBuf` hashes
/// the same with or without a trailing separator -- yet `tab_complete` answers
/// the two differently (a folder's children, or its matching siblings), so
/// typing `/` after a folder has to start a new lookup.
fn tab_complete_subscription(path: &Path) -> Subscription<Message> {
    Subscription::run_with(
        ("tab_complete", path.as_os_str().to_owned()),
        |(_, text)| {
            let path = PathBuf::from(text);
            stream::channel(
                1,
                |mut output: futures::channel::mpsc::Sender<_>| async move {
                    let message = {
                        let path = path.clone();
                        tokio::task::spawn_blocking(move || {
                            let start = Instant::now();
                            match tab_complete(&path) {
                                Ok(completions) => {
                                    log::info!(
                                        "tab completed {} in {:?}",
                                        path.display(),
                                        start.elapsed()
                                    );
                                    Message::TabComplete(path.clone(), completions)
                                }
                                Err(err) => {
                                    log::warn!(
                                        "failed to tab complete {}: {}",
                                        path.display(),
                                        err
                                    );
                                    Message::TabComplete(path.clone(), Vec::new())
                                }
                            }
                        })
                        .await
                        .unwrap()
                    };

                    match output.send(message).await {
                        Ok(()) => {}
                        Err(err) => {
                            log::warn!(
                                "failed to send tab completion for {}: {}",
                                path.display(),
                                err
                            );
                        }
                    }

                    std::future::pending().await
                },
            )
        },
    )
}

pub fn respond_to_scroll_direction(delta: ScrollDelta, modifiers: &Modifiers) -> Option<Message> {
    if !modifiers.control() {
        return None;
    }

    let delta_y = match delta {
        ScrollDelta::Lines { y, .. } => y,
        ScrollDelta::Pixels { y, .. } => y,
    };

    if delta_y > 0.0 {
        return Some(Message::ZoomIn);
    }

    if delta_y < 0.0 {
        return Some(Message::ZoomOut);
    }

    None
}

fn text_editor_class(
    theme: &crate::ui::Theme,
    status: crate::ui::widget::text_editor::Status,
) -> crate::ui::iced::widget::text_editor::Style {
    let cosmic = theme.cosmic();
    let container = theme.current_container();

    let mut background: crate::ui::iced::Color = container.component.base.to_color();
    background.a = 0.25;
    let selection = cosmic.accent.base.to_color();
    let value = cosmic.palette.neutral_9.to_color();
    let mut placeholder = cosmic.palette.neutral_9;
    placeholder.alpha = 0.7;
    let placeholder = placeholder.to_color();

    match status {
        crate::ui::iced::widget::text_editor::Status::Active
        | crate::ui::iced::widget::text_editor::Status::Disabled => {
            crate::ui::iced::widget::text_editor::Style {
                background: background.into(),
                border: crate::ui::iced::Border {
                    radius: cosmic.corner_radii.radius_m.to_radius(),
                    width: 2.0,
                    color: container.component.divider.to_color(),
                },
                placeholder,
                value,
                selection,
            }
        }
        crate::ui::iced::widget::text_editor::Status::Hovered
        | crate::ui::iced::widget::text_editor::Status::Focused { .. } => {
            crate::ui::iced::widget::text_editor::Style {
                background: background.into(),
                border: crate::ui::iced::Border {
                    radius: cosmic.corner_radii.radius_m.to_radius(),
                    width: 2.0,
                    color: cosmic.accent.base.to_color(),
                },
                placeholder,
                value,
                selection,
            }
        }
    }
}

#[cfg(test)]
mod tests {

    /// The tab keeps the selection by each entry's own id: an entry read
    /// again in place keeps it, and so does one that new entries arrive
    /// around, as search results do; a listing read anew starts afresh,
    /// but for the places selected before
    #[test]
    fn an_entry_keeps_its_selection_however_it_is_moved_or_read_again() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        let sizes = super::IconSizes::default();
        let mut tab = super::Tab::new(
            super::Location::Path(dir.path().to_path_buf()),
            super::TabConfig::default(),
            super::ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(super::scan_path(&dir.path().to_path_buf(), sizes));
        let named = |tab: &super::Tab, name: &str| {
            let items = tab.items_opt.as_ref().unwrap();
            let item = items.iter().find(|item| item.name == name).unwrap();
            tab.selection.is_selected(item)
        };
        let b = tab
            .items_opt
            .as_ref()
            .unwrap()
            .iter()
            .position(|item| item.name == "b")
            .unwrap();
        tab.selection
            .select(&tab.items_opt.as_ref().unwrap()[b], true);
        tab.selection
            .set_cut(&tab.items_opt.as_ref().unwrap()[b], true);

        // Read again in place
        let fresh = super::item_from_path(dir.path().join("b"), sizes).unwrap();
        tab.items_opt.as_mut().unwrap()[b].adopt(fresh);
        assert!(named(&tab, "b"));
        assert!(
            tab.selection.is_cut(&tab.items_opt.as_ref().unwrap()[b]),
            "still cut"
        );

        // Another entry arrives before it
        let c = super::item_from_path(dir.path().join("a"), sizes).unwrap();
        tab.items_opt.as_mut().unwrap().insert(0, c);
        assert!(named(&tab, "b") && !named(&tab, "a"));

        // Read anew: the same place stays selected, nothing else; what is
        // cut is marked again from the clipboard
        tab.set_items(super::scan_path(&dir.path().to_path_buf(), sizes));
        assert!(named(&tab, "b") && !named(&tab, "a"));
        assert!(
            tab.items_opt
                .iter()
                .flatten()
                .all(|item| !tab.selection.is_cut(item))
        );
    }

    /// A network entry becomes a listed item as the mounter described it:
    /// a file with its type, a folder as a folder, and the network's root
    /// as places to go into
    #[test]
    fn a_network_entry_is_listed_as_the_mounter_says() {
        use crate::mounter::{IconRef, NetworkEntry};
        let entry = |name: &str, is_dir, mime| NetworkEntry {
            name: name.to_owned(),
            display_name: name.to_uppercase(),
            uri: format!("smb://host/{name}"),
            path: None,
            is_dir,
            size: (!is_dir).then_some(3),
            mtime: 7,
            mime,
            hidden: name.starts_with('.'),
            icon: IconRef::default(),
        };
        let sizes = super::IconSizes::default();
        let text = crate::mime_app::Mime::from_media_type("text/plain").ok();

        let file = super::network_item(entry(".notes", false, text.clone()), false, true, sizes);
        assert_eq!(file.display_name, ".NOTES");
        assert!(file.hidden && !file.metadata.is_dir());
        assert_eq!(Some(file.mime), text);
        assert!(matches!(
            file.location_opt,
            Some(super::Location::Network(ref uri, _, None)) if uri == "smb://host/.notes"
        ));

        let folder = super::network_item(entry("share", true, None), false, true, sizes);
        assert!(folder.metadata.is_dir());
        assert_eq!(folder.mime, crate::mime_app::Mime::INODE_DIRECTORY);
        assert!(
            matches!(folder.dir_size, super::DirSize::NotDirectory),
            "remote"
        );

        let root = super::network_item(entry("host", false, None), true, true, sizes);
        assert!(matches!(
            root.metadata,
            super::ItemMetadata::SimpleDir { .. }
        ));
    }

    #[tokio::test]
    async fn a_cancelled_job_keeps_its_permit_until_the_work_really_ends() {
        use std::sync::{Arc, mpsc};
        use std::time::Duration;

        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let (started, work_began) = mpsc::channel();
        let (finish, may_finish) = mpsc::channel::<()>();

        let job = tokio::spawn(super::bounded_blocking(Arc::clone(&semaphore), move || {
            started.send(()).unwrap();
            may_finish.recv().unwrap();
        }));

        // The test runtime is single threaded: the job only reaches its
        // blocking half when this task yields, so waiting for it must not
        // block.
        for _ in 0..500 {
            if work_began.try_recv().is_ok() {
                break;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(10));
        }

        // The item scrolled out of view: its subscription goes away, but
        // nothing can interrupt the work already running.
        job.abort();
        tokio::task::yield_now().await;

        assert!(
            semaphore.try_acquire().is_err(),
            "permit was returned while the work it bounds was still running"
        );

        finish.send(()).unwrap();
        drop(finish);
        for _ in 0..500 {
            if semaphore.try_acquire().is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("permit was never returned");
    }

    /// A job taking every permit waits for the others, and holds all of
    /// them while it runs
    #[tokio::test]
    async fn a_job_taking_every_permit_runs_alone() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let semaphore = Arc::new(tokio::sync::Semaphore::new(2));
        let held = Arc::clone(&semaphore).acquire_owned().await.unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let job = tokio::spawn(super::bounded_blocking_taking(Arc::clone(&semaphore), 2, {
            let ran = Arc::clone(&ran);
            move || ran.store(true, Ordering::Relaxed)
        }));
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(!ran.load(Ordering::Relaxed), "ran beside another job");
        drop(held);
        job.await.unwrap();
        assert!(ran.load(Ordering::Relaxed));
        assert_eq!(semaphore.available_permits(), 2, "all given back");
    }

    #[tokio::test]
    async fn a_job_cancelled_before_its_turn_never_runs() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let held = Arc::clone(&semaphore).acquire_owned().await.unwrap();
        let ran = Arc::new(AtomicBool::new(false));

        let job = tokio::spawn(super::bounded_blocking(Arc::clone(&semaphore), {
            let ran = Arc::clone(&ran);
            move || ran.store(true, Ordering::Relaxed)
        }));
        tokio::task::yield_now().await;

        job.abort();
        tokio::task::yield_now().await;
        drop(held);

        for _ in 0..20 {
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !ran.load(Ordering::Relaxed),
            "work ran for an item that had already gone"
        );
    }

    #[tokio::test]
    async fn a_folder_size_walk_stops_when_its_controller_is_cancelled() {
        use crate::operation::Controller;

        let dir = tempfile::tempdir().expect("tempdir");
        // Enough entries that the walk cannot finish before the cancellation
        // is seen, without making the test slow when it works.
        for i in 0..5_000 {
            std::fs::write(dir.path().join(format!("{i}")), b"x").expect("write");
        }

        let controller = Controller::default();
        controller.cancel();

        let result = super::calculate_dir_size(dir.path(), controller).await;

        assert!(
            result.is_err(),
            "a cancelled walk reported a size instead of stopping"
        );
    }

    /// The names a tab shows before its rescan answers are the ones the
    /// path spells; for a local folder, that is what the filesystem says too.
    #[test]
    fn a_local_folder_is_named_the_same_with_or_without_looking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("my_docs.v2");
        std::fs::create_dir(&path).expect("mkdir");
        let location = Location::Path(path);

        assert_eq!(location.ancestors(false), location.ancestors(true));
        assert_eq!(location.title(false), location.title(true));
    }

    #[tokio::test]
    async fn a_folder_size_walk_adds_up_what_is_there() {
        use crate::operation::Controller;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a"), b"12345").expect("write");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub").join("b"), b"678").expect("write");

        let size = super::calculate_dir_size(dir.path(), Controller::default())
            .await
            .expect("the walk should finish");

        assert_eq!(size, 8, "nested files count, directories themselves do not");
    }

    #[test]
    fn preview_text_keeps_only_what_the_pane_can_show() {
        let text = (0..100)
            .map(|line| format!("{line}: {}", "x".repeat(500)))
            .collect::<Vec<_>>()
            .join("\n");

        let preview = super::preview_text(&text);

        assert_eq!(preview.lines().count(), super::TEXT_PREVIEW_LINES);
        for line in preview.lines() {
            assert!(
                line.chars().count() <= super::TEXT_PREVIEW_LINE_CHARS,
                "line kept {} characters",
                line.chars().count()
            );
        }
    }

    #[test]
    fn preview_text_leaves_a_short_file_alone() {
        assert_eq!(super::preview_text("one\ntwo\nthree"), "one\ntwo\nthree");
    }

    #[test]
    fn preview_text_keeps_leading_blank_lines() {
        assert_eq!(super::preview_text("\n\nhello"), "\n\nhello");
    }

    #[test]
    fn preview_text_drops_control_characters() {
        let preview = super::preview_text("\u{1b}[2mdim\u{1b}[0m text\u{7}");

        assert_eq!(preview, "[2mdim[0m text");
    }

    #[test]
    fn preview_text_keeps_indentation() {
        let preview = super::preview_text("\tindented");

        assert_eq!(preview, "    indented");
    }

    #[test]
    fn preview_text_counts_tab_spaces_against_the_line_budget() {
        let text = "\t".repeat(super::TEXT_PREVIEW_LINE_CHARS);

        let preview = super::preview_text(&text);

        assert_eq!(preview.chars().count(), super::TEXT_PREVIEW_LINE_CHARS);
    }

    /// Each mount's device number comes from its third field and its kind
    /// from the type after the separator, however many optional fields
    /// come before it.
    #[test]
    fn mount_kinds_are_read_from_mountinfo() {
        let kinds = super::fs_kinds_in(concat!(
            "42 1 254:0 / / rw,relatime shared:1 - ext4 /dev/mapper/primary rw\n",
            "90 42 0:61 / /mnt/my\\040share rw shared:7 master:3 - cifs //nas/share rw\n",
            "not a mount line\n",
        ));
        assert_eq!(kinds.len(), 2);
        assert_eq!(kinds[&libc::makedev(254, 0)], super::FsKind::Local);
        assert_eq!(kinds[&libc::makedev(0, 61)], super::FsKind::Remote);
    }

    #[test]
    fn preview_text_truncates_a_single_long_line() {
        let text = "y".repeat(10_000);

        let preview = super::preview_text(&text);

        assert_eq!(preview.chars().count(), super::TEXT_PREVIEW_LINE_CHARS);
    }
    use std::path::PathBuf;
    use std::{fs, io};

    use crate::mime_app::Mime;
    use crate::ui::iced::mouse::ScrollDelta;
    use crate::ui::iced_runtime::keyboard::Modifiers;
    use log::{debug, trace};
    use tempfile::TempDir;
    use test_log::test;

    use super::{
        HeadingOptions, ItemMetadata, ItemThumbnail, Location, Message, Tab, Typing,
        respond_to_scroll_direction, scan_path, try_scan_path,
    };
    use crate::app::test_utils::{
        NAME_LEN, NUM_DIRS, NUM_FILES, NUM_HIDDEN, NUM_NESTED, assert_eq_tab_path, empty_fs,
        eq_path_item, filter_dirs, read_dir_sorted, simple_fs, tab_click_new,
    };
    use crate::config::{IconSizes, TabConfig, ThumbCfg};

    /// A tab listing the folder `path` as it is now.
    fn listing_of(path: &std::path::Path) -> Tab {
        let mut tab = Tab::new(
            Location::Path(path.to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(scan_path(&path.to_path_buf(), IconSizes::default()));
        tab
    }

    /// A folder of empty files named `names`, and a tab listing it.
    fn files_tab(names: &[&str]) -> io::Result<(TempDir, Tab)> {
        let fs = empty_fs()?;
        for name in names {
            fs::write(fs.path().join(name), b"")?;
        }
        let tab = listing_of(fs.path());
        Ok((fs, tab))
    }

    /// The names of the selected items, as shown.
    fn selected_names(tab: &Tab) -> Vec<String> {
        tab.column_sort()
            .into_iter()
            .flatten()
            .filter(|(_, item)| tab.selection.is_selected(item))
            .map(|(_, item)| item.name.clone())
            .collect()
    }

    /// Where the item named `name` is in the tab's items.
    fn index_of(tab: &Tab, name: &str) -> usize {
        tab.items_opt
            .as_deref()
            .and_then(|items| items.iter().position(|item| item.name == name))
            .unwrap_or_else(|| panic!("no item named {name}"))
    }

    /// A press and release on the item named `name`.
    fn click(tab: &mut Tab, name: &str, modifiers: Modifiers) {
        let i = index_of(tab, name);
        tab.update(Message::Click(Some(i)), modifiers);
        tab.update(Message::ClickRelease(Some(i)), modifiers);
    }

    fn shift_click(tab: &mut Tab, name: &str) {
        click(tab, name, Modifiers::SHIFT);
    }

    /// Lays the items out one per row, in the order shown, as a list view
    /// does, for the keyboard and the rectangle selection.
    fn lay_out(tab: &Tab) {
        use crate::ui::iced::{Point, Rectangle, Size};
        for (row, (_, item)) in tab.column_sort().into_iter().flatten().enumerate() {
            item.pos_opt.set(Some((row, 0)));
            #[allow(clippy::cast_precision_loss, reason = "a handful of rows")]
            let top = row as f32 * 20.0;
            item.rect_opt.set(Some(Rectangle::new(
                Point::new(0.0, top),
                Size::new(100.0, 20.0),
            )));
        }
    }

    /// Shift with the arrows starts where a shift-click would: at the
    /// anchor, focused or not, and the first one sets the anchor.
    #[test]
    fn shift_with_the_arrows_starts_at_the_anchor() -> io::Result<()> {
        use crate::ui::iced::{Point, Rectangle, Size};

        let (fs, mut tab) = files_tab(&["a", "b", "c", "d", "e"])?;
        lay_out(&tab);

        // A rectangle anchors without focusing
        let rect = Rectangle::new(Point::new(10.0, 25.0), Size::new(10.0, 20.0));
        tab.update(Message::Drag(Some(rect)), Modifiers::empty());
        tab.update(Message::DragEnd, Modifiers::empty());
        assert_eq!(selected_names(&tab), ["b", "c"]);
        tab.update(Message::ItemDown, Modifiers::SHIFT);
        assert_eq!(selected_names(&tab), ["b", "c", "d"], "after a rectangle");

        // A rescan takes the focused end away and keeps the anchor
        click(&mut tab, "b", Modifiers::empty());
        shift_click(&mut tab, "d");
        fs::remove_file(fs.path().join("d"))?;
        tab.set_items(scan_path(&fs.path().to_path_buf(), IconSizes::default()));
        lay_out(&tab);
        assert_eq!(tab.select_focus, None);
        tab.update(Message::ItemDown, Modifiers::SHIFT);
        assert_eq!(
            selected_names(&tab),
            ["b", "c", "e"],
            "after the focused end went"
        );

        // From nothing, the first shift arrow sets the anchor a shift-click
        // goes on from
        tab.select_none();
        tab.update(Message::ItemDown, Modifiers::SHIFT);
        assert_eq!(selected_names(&tab), ["a"]);
        shift_click(&mut tab, "c");
        assert_eq!(
            selected_names(&tab),
            ["a", "b", "c"],
            "from the first arrow"
        );
        Ok(())
    }

    /// A shift range runs between two items as shown, shrinks as well as
    /// grows, and with Ctrl adds to what was selected.
    #[test]
    fn shift_ranges_follow_the_order_shown() -> io::Result<()> {
        let (_fs, mut tab) = files_tab(&["a", "b", "c", "d", "e"])?;

        click(&mut tab, "a", Modifiers::empty());
        shift_click(&mut tab, "d");
        assert_eq!(selected_names(&tab), ["a", "b", "c", "d"]);
        shift_click(&mut tab, "b");
        assert_eq!(selected_names(&tab), ["a", "b"], "shrunk");

        click(&mut tab, "a", Modifiers::empty());
        click(&mut tab, "d", Modifiers::CTRL);
        click(&mut tab, "e", Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(selected_names(&tab), ["a", "d", "e"], "added with Ctrl");

        tab.sort_direction = false;
        click(&mut tab, "d", Modifiers::empty());
        shift_click(&mut tab, "b");
        assert_eq!(selected_names(&tab), ["d", "c", "b"], "reversed");
        Ok(())
    }

    /// Clearing the selection, however it is done, leaves nothing for a
    /// shift-click to start from: it selects only what it clicks.
    #[test]
    fn a_shift_click_after_the_selection_is_cleared_selects_only_its_item() -> io::Result<()> {
        let (_fs, mut tab) = files_tab(&["a", "b", "c", "d"])?;

        click(&mut tab, "a", Modifiers::empty());
        tab.select_none();
        shift_click(&mut tab, "d");
        assert_eq!(selected_names(&tab), ["d"], "after Escape");

        click(&mut tab, "b", Modifiers::empty());
        tab.update(Message::Click(None), Modifiers::empty());
        shift_click(&mut tab, "d");
        assert_eq!(
            selected_names(&tab),
            ["d"],
            "after a click on the background"
        );

        tab.select_none();
        click(&mut tab, "b", Modifiers::CTRL);
        click(&mut tab, "b", Modifiers::CTRL);
        assert!(selected_names(&tab).is_empty());
        shift_click(&mut tab, "d");
        assert_eq!(
            selected_names(&tab),
            ["d"],
            "after Ctrl took the last one off"
        );

        click(&mut tab, "a", Modifiers::empty());
        tab.update(Message::RightClickBackground, Modifiers::empty());
        shift_click(&mut tab, "c");
        assert_eq!(
            selected_names(&tab),
            ["c"],
            "after a right click on the background"
        );
        Ok(())
    }

    /// Going back to a folder selects the one left, and a shift selection
    /// starts there, whatever was clicked in the folder left.
    #[test]
    fn a_shift_selection_starts_at_the_folder_returned_from() -> io::Result<()> {
        let fs = empty_fs()?;
        for folder in ["k", "m"] {
            fs::create_dir(fs.path().join(folder))?;
            fs::write(fs.path().join(folder).join("inside"), b"")?;
        }
        for name in ["a", "b", "c"] {
            fs::write(fs.path().join(name), b"")?;
        }
        let parent = fs.path().to_owned();
        let child = parent.join("m");
        let mut tab = listing_of(&parent);

        // Into `m`, where the first item is clicked, and back
        tab.change_location(&Location::Path(child.clone()), None);
        tab.set_items(scan_path(&child, IconSizes::default()));
        click(&mut tab, "inside", Modifiers::empty());
        tab.change_location(&Location::Path(parent.clone()), None);
        tab.set_items(scan_path(&parent, IconSizes::default()));
        tab.select_paths(vec![child.clone()]);
        assert_eq!(selected_names(&tab), ["m"]);

        shift_click(&mut tab, "b");
        assert_eq!(selected_names(&tab), ["m", "a", "b"]);

        // The keyboard starts there too
        tab.select_paths(vec![child]);
        lay_out(&tab);
        tab.update(Message::ItemDown, Modifiers::SHIFT);
        assert_eq!(selected_names(&tab), ["m", "a"]);
        Ok(())
    }

    /// A place clicked in a longer listing is not a place in a shorter one:
    /// a shift-click there must not run to the end.
    #[test]
    fn an_anchor_from_another_listing_does_not_stretch_a_shift_click() -> io::Result<()> {
        let (_long, mut tab) = files_tab(&["a", "b", "c", "d", "e", "f"])?;
        click(&mut tab, "f", Modifiers::empty());

        let short = empty_fs()?;
        for name in ["x", "y"] {
            fs::write(short.path().join(name), b"")?;
        }
        tab.change_location(&Location::Path(short.path().to_owned()), None);
        tab.set_items(scan_path(&short.path().to_path_buf(), IconSizes::default()));
        shift_click(&mut tab, "x");
        assert_eq!(selected_names(&tab), ["x"]);
        Ok(())
    }

    /// Type-to-select, a right click and a rectangle each start the next
    /// shift selection at what they selected.
    #[test]
    fn other_ways_of_selecting_anchor_the_next_shift_click() -> io::Result<()> {
        use crate::ui::iced::{Point, Rectangle, Size};

        let (_fs, mut tab) = files_tab(&["a", "b", "c", "d", "e"])?;

        click(&mut tab, "a", Modifiers::empty());
        assert!(tab.select_by_prefix("c"));
        shift_click(&mut tab, "e");
        assert_eq!(selected_names(&tab), ["c", "d", "e"], "after typing");

        click(&mut tab, "a", Modifiers::empty());
        let c = index_of(&tab, "c");
        tab.update(Message::RightClick(None, Some(c)), Modifiers::empty());
        shift_click(&mut tab, "e");
        assert_eq!(selected_names(&tab), ["c", "d", "e"], "after a right click");

        click(&mut tab, "a", Modifiers::empty());
        lay_out(&tab);
        // Over the second and third rows
        let rect = Rectangle::new(Point::new(10.0, 25.0), Size::new(10.0, 20.0));
        tab.update(Message::Drag(Some(rect)), Modifiers::empty());
        tab.update(Message::DragEnd, Modifiers::empty());
        assert_eq!(selected_names(&tab), ["b", "c"]);
        shift_click(&mut tab, "e");
        assert_eq!(
            selected_names(&tab),
            ["b", "c", "d", "e"],
            "after a rectangle"
        );

        // Select all keeps an anchor that is still selected
        click(&mut tab, "b", Modifiers::empty());
        tab.update(Message::SelectAll, Modifiers::empty());
        shift_click(&mut tab, "d");
        assert_eq!(selected_names(&tab), ["b", "c", "d"], "after select all");
        Ok(())
    }

    /// A rescan that lists a new file before the anchor keeps the anchor
    /// and the focus on the same files.
    #[test]
    fn a_rescan_keeps_the_anchor_on_its_file() -> io::Result<()> {
        let (fs, mut tab) = files_tab(&["b", "c", "d", "e"])?;
        click(&mut tab, "c", Modifiers::empty());

        fs::write(fs.path().join("a"), b"")?;
        tab.set_items(scan_path(&fs.path().to_path_buf(), IconSizes::default()));
        let focus = tab.select_focus.expect("the focus stays");
        assert_eq!(tab.items_opt.as_deref().expect("listed")[focus].name, "c");

        shift_click(&mut tab, "e");
        assert_eq!(selected_names(&tab), ["c", "d", "e"]);

        // A file that is gone takes its anchor with it
        click(&mut tab, "c", Modifiers::empty());
        fs::remove_file(fs.path().join("c"))?;
        tab.set_items(scan_path(&fs.path().to_path_buf(), IconSizes::default()));
        shift_click(&mut tab, "e");
        assert_eq!(selected_names(&tab), ["e"]);
        Ok(())
    }

    /// Newer search results go in before older ones; the anchor stays on
    /// its result, and goes when the cap pushes that result out.
    #[test]
    fn search_results_arriving_keep_the_anchor_on_its_result() -> io::Result<()> {
        use crate::tab::{SearchContext, SearchItem, SearchLocation, SearchOptions};
        use std::sync::{Arc, RwLock, atomic};
        use std::time::{Duration, SystemTime};

        let fs = empty_fs()?;
        let now = SystemTime::now();
        for (name, age) in [("old", 30), ("mid", 20), ("new", 10)] {
            let path = fs.path().join(name);
            fs::write(&path, b"x")?;
            fs::File::options()
                .write(true)
                .open(&path)?
                .set_modified(now - Duration::from_secs(age))?;
        }
        let mut tab = listing_of(fs.path());
        tab.change_location(
            &Location::Search(
                SearchLocation::Path(fs.path().to_owned()),
                "o".to_string(),
                SearchOptions {
                    show_hidden: false,
                    recursive: false,
                },
                std::time::Instant::now(),
            ),
            None,
        );
        tab.set_items(Vec::new());
        let (results_tx, results_rx) = tokio::sync::mpsc::channel(8);
        tab.search_context = Some(SearchContext {
            results_rx,
            ready: Arc::new(atomic::AtomicBool::new(true)),
            last_modified_opt: Arc::new(RwLock::new(None)),
        });
        let send = |name: &str| -> io::Result<()> {
            let path = fs.path().join(name);
            let metadata = fs::metadata(&path)?;
            results_tx
                .try_send(crate::tab::item_from_search_item(
                    SearchItem::Path(path, name.to_string(), metadata),
                    IconSizes::default(),
                ))
                .expect("room in the channel");
            Ok(())
        };

        send("old")?;
        send("mid")?;
        tab.update(Message::SearchReady(false), Modifiers::empty());
        click(&mut tab, "mid", Modifiers::empty());

        send("new")?;
        tab.search_context
            .as_ref()
            .expect("still searching")
            .ready
            .store(true, atomic::Ordering::SeqCst);
        tab.update(Message::SearchReady(false), Modifiers::empty());
        shift_click(&mut tab, "old");
        assert_eq!(selected_names(&tab), ["mid", "old"]);

        // With room for two, the oldest goes, and its anchor with it
        click(&mut tab, "old", Modifiers::empty());
        tab.config.max_search_results = std::num::NonZeroU16::new(2).expect("nonzero");
        tab.search_context
            .as_ref()
            .expect("still searching")
            .ready
            .store(true, atomic::Ordering::SeqCst);
        tab.update(Message::SearchReady(false), Modifiers::empty());
        shift_click(&mut tab, "mid");
        assert_eq!(selected_names(&tab), ["mid"]);
        Ok(())
    }

    #[test]
    fn the_action_follows_the_location_and_its_items() -> io::Result<()> {
        use super::{SearchLocation, SearchOptions, TabAction, TrashSize};

        let fs = empty_fs()?;
        let file = fs.path().join("a.txt");
        fs::write(&file, b"x")?;

        let action = |location: Location, with_item: bool| {
            let mut tab = Tab::new(
                location,
                TabConfig::default(),
                ThumbCfg::default(),
                None,
                std::borrow::Cow::Borrowed("Undefined"),
                None,
            );
            if with_item {
                let item = super::item_from_path(&file, IconSizes::default())
                    .expect("the file should be read");
                tab.set_items(vec![item]);
            } else {
                tab.set_items(Vec::new());
            }
            tab.action(2)
        };
        let search = |location| {
            Location::Search(
                location,
                "a".into(),
                SearchOptions {
                    show_hidden: false,
                    recursive: false,
                },
                std::time::Instant::now(),
            )
        };

        assert_eq!(
            action(Location::Trash, true),
            Some(TabAction::EmptyTrash(Some((1, TrashSize::Calculating))))
        );
        assert_eq!(action(Location::Trash, false), None);
        assert_eq!(
            action(search(SearchLocation::Trash), true),
            Some(TabAction::EmptyTrash(None))
        );
        assert_eq!(
            action(Location::Recents, true),
            Some(TabAction::ClearRecents(Some(1)))
        );
        assert_eq!(action(Location::Recents, false), None);
        assert_eq!(
            action(search(SearchLocation::Recents), true),
            Some(TabAction::ClearRecents(None))
        );
        assert_eq!(
            action(
                Location::Network("network:///".into(), "Network".into(), None),
                false
            ),
            Some(TabAction::AddNetworkDrive(2))
        );
        assert_eq!(
            action(
                Location::Network("smb://host/".into(), "host".into(), None),
                false
            ),
            None
        );
        assert_eq!(action(Location::Path(fs.path().to_owned()), true), None);
        Ok(())
    }

    #[test]
    fn the_network_card_counts_connected_drives_only_when_there_are_some() {
        use super::TabAction;

        assert_eq!(TabAction::AddNetworkDrive(0).detail(), None);
        let detail = TabAction::AddNetworkDrive(2)
            .detail()
            .expect("two connected drives are counted");
        assert!(detail.contains('2'), "{detail}");
    }

    #[test]
    fn the_bottom_reserve_is_scrollable_room_under_the_last_item() -> io::Result<()> {
        let fs = simple_fs(NUM_FILES, NUM_HIDDEN, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let location = Location::Path(fs.path().to_owned());
        let (_, items) = location.scan(IconSizes::default());
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(items);
        tab.size_opt
            .set(Some(crate::ui::iced::Size::new(800.0, 600.0)));

        let content_height = |tab: &Tab| {
            let _ = tab.list_view(None);
            let list = tab.content_height_opt.get().expect("the list is measured");
            let _ = tab.grid_view();
            let grid = tab.content_height_opt.get().expect("the grid is measured");
            (list, grid)
        };
        let (list, grid) = content_height(&tab);
        tab.bottom_reserve.set(85.0);
        assert_eq!(content_height(&tab), (list + 85.0, grid + 85.0));
        Ok(())
    }

    /// What `input` adds up to, walked on a runtime of its own.
    fn trash_total(input: &super::TrashSizeInput) -> io::Result<super::TrashSize> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(runtime.block_on(input.total(super::Controller::default())))
    }

    /// A trash entry whose `.trashinfo` is `root/info/<name>.trashinfo`, so
    /// its copy is `root/files/<name>`.
    fn trashed(
        root: &std::path::Path,
        name: &str,
        size: crate::trashing::TrashItemSize,
    ) -> super::Item {
        let entry = crate::trashing::TrashItem {
            id: crate::trashing::TrashId::of_record(
                root.join("info").join(format!("{name}.trashinfo")),
            ),
            name: name.into(),
            original_parent: Some("/nonexistent".into()),
            time_deleted: 0,
        };
        super::item_from_trash_entry(
            entry,
            crate::trashing::TrashItemMetadata { size },
            IconSizes::default(),
        )
    }

    #[test]
    fn the_trash_size_counts_files_and_walks_folders() -> io::Result<()> {
        use super::{TrashSize, TrashSizeInput};

        let fs = empty_fs()?;
        let root = fs.path();
        fs::create_dir_all(root.join("files/folder/sub"))?;
        fs::write(root.join("files/folder/a"), [0; 5])?;
        fs::write(root.join("files/folder/sub/b"), [0; 6])?;

        let input = TrashSizeInput::new(&[
            trashed(root, "file", crate::trashing::TrashItemSize::Bytes(7)),
            trashed(root, "folder", crate::trashing::TrashItemSize::Entries(2)),
        ]);
        assert_eq!(input.files, 7);
        assert_eq!(input.folders, [Some(root.join("files/folder"))]);

        assert_eq!(trash_total(&input)?, TrashSize::Known(7 + 5 + 6));
        Ok(())
    }

    /// Inside a trashed folder with no `.trashinfo`, what it holds is still
    /// listed, with nowhere known to put it back
    #[test]
    fn inside_an_entry_of_unknown_origin_items_are_listed() -> io::Result<()> {
        let fs = empty_fs()?;
        let inner = fs.path().join("inner.txt");
        std::fs::write(&inner, "x")?;
        let metadata = std::fs::symlink_metadata(&inner)?;
        let item = super::item_from_trash_child(
            inner.clone(),
            "inner.txt".to_owned(),
            metadata,
            IconSizes::default(),
        )
        .expect("listed");
        let super::ItemMetadata::Trash { entry, .. } = &item.metadata else {
            panic!("a trash item");
        };
        assert!(crate::trashing::origin_unknown(entry));
        assert_eq!(item.location_opt, Some(Location::Path(inner)));
        Ok(())
    }

    #[test]
    fn a_folder_without_a_copy_makes_the_trash_size_partial() -> io::Result<()> {
        use super::{TrashSize, TrashSizeInput};

        let fs = empty_fs()?;
        let root = fs.path();
        // An id with no trash root above it names no copy to walk
        let orphan = super::item_from_trash_entry(
            crate::trashing::TrashItem {
                id: crate::trashing::TrashId::Orphan("orphan.trashinfo".into()),
                name: "orphan".into(),
                original_parent: Some("/nonexistent".into()),
                time_deleted: 0,
            },
            crate::trashing::TrashItemMetadata {
                size: crate::trashing::TrashItemSize::Entries(1),
            },
            IconSizes::default(),
        );
        let input = TrashSizeInput::new(&[
            trashed(root, "file", crate::trashing::TrashItemSize::Bytes(7)),
            orphan,
        ]);
        assert_eq!(input.folders, [None]);

        assert_eq!(trash_total(&input)?, TrashSize::Partial(7));
        Ok(())
    }

    #[test]
    fn a_missing_folder_copy_counts_as_nothing() -> io::Result<()> {
        use super::{TrashSize, TrashSizeInput};

        // As the folder-size walk does everywhere: what it cannot read, it
        // skips (decided 2026-09-28)
        let fs = empty_fs()?;
        let root = fs.path();
        let input = TrashSizeInput::new(&[
            trashed(root, "file", crate::trashing::TrashItemSize::Bytes(7)),
            trashed(root, "gone", crate::trashing::TrashItemSize::Entries(1)),
        ]);

        assert_eq!(trash_total(&input)?, TrashSize::Known(7));
        Ok(())
    }

    #[test]
    fn the_trash_size_of_an_old_listing_is_ignored() {
        use super::{TabAction, TrashSize};

        let file = || {
            trashed(
                std::path::Path::new("/nonexistent"),
                "file",
                crate::trashing::TrashItemSize::Bytes(7),
            )
        };
        let mut tab = Tab::new(
            Location::Trash,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(vec![file()]);
        let listing = tab.listing;

        let _ = tab.update(
            Message::TrashSize(listing.wrapping_sub(1), TrashSize::Known(1)),
            Modifiers::empty(),
        );
        assert_eq!(
            tab.action(0),
            Some(TabAction::EmptyTrash(Some((1, TrashSize::Calculating))))
        );

        let _ = tab.update(
            Message::TrashSize(listing, TrashSize::Known(7)),
            Modifiers::empty(),
        );
        assert_eq!(
            tab.action(0),
            Some(TabAction::EmptyTrash(Some((1, TrashSize::Known(7)))))
        );

        // A new listing works it out again
        tab.set_items(vec![file()]);
        assert_eq!(
            tab.action(0),
            Some(TabAction::EmptyTrash(Some((1, TrashSize::Calculating))))
        );
    }

    // Boilerplate for tab tests. Checks if simulated clicks selected items.
    fn tab_selects_item(
        clicks: &[usize],
        modifiers: Modifiers,
        expected_selected: &[bool],
    ) -> io::Result<()> {
        let (_fs, mut tab) = tab_click_new(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;

        // Simulate clicks by triggering Message::Click
        for &click in clicks {
            debug!("Emitting Message::Click(Some({click})) with modifiers: {modifiers:?}");
            tab.update(Message::Click(Some(click)), modifiers);
        }

        let items = tab
            .items_opt
            .as_deref()
            .expect("tab should be populated with items");

        for (i, (&expected, actual)) in expected_selected.iter().zip(items).enumerate() {
            assert_eq!(
                expected,
                tab.selection.is_selected(actual),
                "expected index {i} to be {}",
                if expected {
                    "selected but it was deselected"
                } else {
                    "deselected but it was selected"
                }
            );
        }

        Ok(())
    }

    fn tab_history() -> io::Result<(TempDir, Tab, Vec<PathBuf>)> {
        let fs = simple_fs(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();
        let mut tab = Tab::new(
            Location::Path(path.into()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );

        // All directories (simple_fs only produces one nested layer)
        let dirs: Vec<PathBuf> = {
            let top_level = filter_dirs(path)?;
            let mut result = Vec::new();
            for dir in top_level {
                let nested_dirs = filter_dirs(&dir)?;
                result.push(dir);
                result.extend(nested_dirs);
            }
            result
        };
        assert!(
            dirs.len() == NUM_DIRS + NUM_DIRS * NUM_NESTED,
            "Sanity check: Have {} dirs instead of {}",
            dirs.len(),
            NUM_DIRS + NUM_DIRS * NUM_NESTED
        );

        debug!("Building history by emitting Message::Location");
        for dir in &dirs {
            debug!(
                "Emitting Message::Location(Location::Path(\"{}\"))",
                dir.display()
            );
            tab.update_resolved(
                Message::Location(Location::Path(dir.clone())),
                Modifiers::empty(),
            );
        }
        trace!("Tab history: {:?}", tab.history);

        Ok((fs, tab, dirs))
    }

    fn tab_at(location: Location) -> Tab {
        Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        )
    }

    /// A step to a folder asks the filesystem on a worker, and the tab goes
    /// there with the answer, not before; an answer to a step since
    /// superseded is dropped
    #[test]
    fn a_step_waits_for_its_folder_to_be_resolved() -> io::Result<()> {
        let fs = empty_fs()?;
        let (first, second) = (fs.path().join("first"), fs.path().join("second"));
        fs::create_dir(&first)?;
        fs::create_dir(&second)?;
        let mut tab = tab_at(Location::Path(fs.path().to_path_buf()));

        let asked = |commands: Vec<crate::tab::Command>| {
            commands.into_iter().find_map(|command| match command {
                crate::tab::Command::ResolveLocation(request, location_request) => {
                    Some((request, crate::tab::resolve_location(&location_request)))
                }
                _ => None,
            })
        };
        let (stale, stale_answer) = asked(tab.update(
            Message::Location(Location::Path(first.clone())),
            Modifiers::empty(),
        ))
        .expect("asked to resolve");
        assert_eq!(
            tab.location.path_opt(),
            Some(&fs.path().to_path_buf()),
            "not yet"
        );

        let (request, answer) = asked(tab.update(
            Message::Location(Location::Path(second.clone())),
            Modifiers::empty(),
        ))
        .expect("asked to resolve");
        tab.update(
            Message::LocationResolved(stale, stale_answer),
            Modifiers::empty(),
        );
        assert_eq!(
            tab.location.path_opt(),
            Some(&fs.path().to_path_buf()),
            "stale"
        );

        tab.update(
            Message::LocationResolved(request, answer),
            Modifiers::empty(),
        );
        assert_eq!(tab.location.path_opt(), Some(&second));
        Ok(())
    }

    /// A step to a file opens it once the worker has looked, without the
    /// tab looking first
    #[test]
    fn a_step_to_a_file_opens_it() -> io::Result<()> {
        let fs = empty_fs()?;
        let file = fs.path().join("note.txt");
        fs::write(&file, b"x")?;
        let mut tab = tab_at(Location::Path(fs.path().to_path_buf()));
        let asked = tab.update(
            Message::Location(Location::Path(file.clone())),
            Modifiers::empty(),
        );
        assert!(
            asked
                .iter()
                .all(|command| !matches!(command, crate::tab::Command::OpenFile(_))),
            "not opened before the worker looked"
        );
        let opened = tab.update_resolved(
            Message::Location(Location::Path(file.clone())),
            Modifiers::empty(),
        );
        assert!(
            opened
                .iter()
                .any(|command| matches!(command, crate::tab::Command::OpenFile(paths) if paths == std::slice::from_ref(&file))),
            "{opened:?}"
        );
        assert_eq!(tab.location.path_opt(), Some(&fs.path().to_path_buf()));
        Ok(())
    }

    /// An address typed but not there goes to its first completion, as the
    /// worker finds
    #[test]
    fn a_missing_address_falls_back_to_its_completion() -> io::Result<()> {
        let fs = empty_fs()?;
        let real = fs.path().join("real");
        fs::create_dir(&real)?;
        let resolved = crate::tab::resolve_location(&crate::tab::LocationRequest {
            location: Location::Path(fs.path().join("rea")),
            history_i_opt: None,
            fallback: Some(Location::Path(real.clone())),
            on_file: crate::tab::OnFile::Select,
        });
        assert_eq!(
            resolved
                .location
                .path_opt()
                .map(|path| path.components().collect::<Vec<_>>()),
            Some(real.canonicalize()?.components().collect())
        );
        Ok(())
    }

    /// A network address submitted after a step to a folder is the newer
    /// step: the folder's late answer neither moves the tab nor withdraws
    /// the address
    #[test]
    fn a_late_folder_answer_does_not_override_a_network_address() -> io::Result<()> {
        use crate::tab::Command;

        let fs = empty_fs()?;
        let folder = fs.path().join("folder");
        fs::create_dir(&folder)?;
        let mut tab = tab_at(Location::Path(fs.path().to_path_buf()));
        let (request, answer) = tab
            .update(
                Message::Location(Location::Path(folder.clone())),
                Modifiers::empty(),
            )
            .into_iter()
            .find_map(|command| match command {
                Command::ResolveLocation(request, location_request) => {
                    Some((request, crate::tab::resolve_location(&location_request)))
                }
                _ => None,
            })
            .expect("asked to resolve");

        let uri = "smb://example/share/".to_owned();
        tab.edit_location = Some(Location::Network(uri.clone(), uri, None).into());
        tab.update(Message::EditLocationSubmit, Modifiers::empty());
        let network = tab.resolving_network;
        assert!(network.is_some());

        tab.update(
            Message::LocationResolved(request, answer),
            Modifiers::empty(),
        );
        assert_eq!(tab.location.path_opt(), Some(&fs.path().to_path_buf()));
        assert_eq!(tab.resolving_network, network, "still asking");
        Ok(())
    }

    /// Two presses of Back before the first is answered go back two steps
    #[test]
    fn quick_back_presses_each_count() -> io::Result<()> {
        let fs = empty_fs()?;
        let (a, b) = (fs.path().join("a"), fs.path().join("b"));
        fs::create_dir(&a)?;
        fs::create_dir(&b)?;
        let mut tab = tab_at(Location::Path(fs.path().to_path_buf()));
        for dir in [&a, &b] {
            tab.update_resolved(
                Message::Location(Location::Path(dir.clone())),
                Modifiers::empty(),
            );
        }
        assert_eq!(tab.history_i, 2);

        let pending: Vec<_> = [Message::GoPrevious, Message::GoPrevious]
            .into_iter()
            .flat_map(|message| tab.update(message, Modifiers::empty()))
            .filter_map(|command| match command {
                crate::tab::Command::ResolveLocation(request, location_request) => {
                    Some((request, crate::tab::resolve_location(&location_request)))
                }
                _ => None,
            })
            .collect();
        assert_eq!(pending.len(), 2);
        for (request, answer) in pending {
            tab.update(
                Message::LocationResolved(request, answer),
                Modifiers::empty(),
            );
        }
        assert_eq!(tab.history_i, 0, "back two steps");
        Ok(())
    }

    /// Back to an entry naming the folder the tab is in still moves along
    /// the history: from the second A of [A, B, A], twice Back is the first
    #[test]
    fn back_to_the_same_folder_still_steps_through_history() -> io::Result<()> {
        let fs = empty_fs()?;
        let (a, b) = (fs.path().join("a"), fs.path().join("b"));
        fs::create_dir(&a)?;
        fs::create_dir(&b)?;
        let mut tab = tab_at(Location::Path(a.clone()));
        for dir in [&b, &a] {
            tab.update_resolved(
                Message::Location(Location::Path(dir.clone())),
                Modifiers::empty(),
            );
        }
        assert_eq!(tab.history_i, 2);
        let pending: Vec<_> = [Message::GoPrevious, Message::GoPrevious]
            .into_iter()
            .flat_map(|message| tab.update(message, Modifiers::empty()))
            .filter_map(|command| match command {
                crate::tab::Command::ResolveLocation(request, location_request) => {
                    Some((request, crate::tab::resolve_location(&location_request)))
                }
                _ => None,
            })
            .collect();
        for (request, answer) in pending {
            tab.update(
                Message::LocationResolved(request, answer),
                Modifiers::empty(),
            );
        }
        assert_eq!(tab.history_i, 0);
        Ok(())
    }

    /// Submitting a network address is a step: whatever waits to send the
    /// tab somewhere can tell it has been sent elsewhere since
    #[test]
    fn a_network_address_counts_as_a_step() -> io::Result<()> {
        let fs = empty_fs()?;
        let mut tab = tab_at(Location::Path(fs.path().to_path_buf()));
        let before = tab.steps();
        let uri = "smb://example/share/".to_owned();
        tab.edit_location = Some(Location::Network(uri.clone(), uri, None).into());
        tab.update(Message::EditLocationSubmit, Modifiers::empty());
        assert!(tab.resolving_network.is_some());
        assert_ne!(tab.steps(), before);
        Ok(())
    }

    /// Two presses of Up before the first is answered go up two folders
    #[test]
    fn quick_up_presses_each_count() -> io::Result<()> {
        let fs = empty_fs()?;
        let deep = fs.path().join("a").join("b");
        fs::create_dir_all(&deep)?;
        let mut tab = tab_at(Location::Path(fs.path().to_path_buf()));
        tab.update_resolved(Message::Location(Location::Path(deep)), Modifiers::empty());
        let pending: Vec<_> = [Message::LocationUp, Message::LocationUp]
            .into_iter()
            .flat_map(|message| tab.update(message, Modifiers::empty()))
            .filter_map(|command| match command {
                crate::tab::Command::ResolveLocation(request, location_request) => {
                    Some((request, crate::tab::resolve_location(&location_request)))
                }
                _ => None,
            })
            .collect();
        for (request, answer) in pending {
            tab.update(
                Message::LocationResolved(request, answer),
                Modifiers::empty(),
            );
        }
        assert_eq!(
            tab.location
                .path_opt()
                .map(|path| path.components().collect::<Vec<_>>()),
            Some(fs.path().canonicalize()?.components().collect())
        );
        Ok(())
    }

    /// A tab sent to a folder by a name the filesystem has not been asked
    /// about takes its canonical name from the rescan
    #[test]
    fn a_rescan_gives_the_tab_its_folders_canonical_name() -> io::Result<()> {
        let fs = empty_fs()?;
        let real = fs.path().join("real");
        fs::create_dir(&real)?;
        std::os::unix::fs::symlink(&real, fs.path().join("link"))?;
        let given = Location::Path(fs.path().join("link"));
        let mut tab = tab_at(given.clone());
        assert_eq!(tab.location, given, "as given, without asking");

        let canonical = given.normalize();
        tab.adopt_canonical(&given, &canonical);
        assert_eq!(tab.location.path_opt(), canonical.path_opt());
        assert_eq!(tab.history.last(), Some(&tab.location));
        Ok(())
    }

    #[test]
    fn scan_path_succeeds_on_valid_path() -> io::Result<()> {
        let fs = simple_fs(NUM_FILES, NUM_HIDDEN, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();

        // Read directory entries and sort as earth-files does
        let entries = read_dir_sorted(path)?;

        debug!("Calling scan_path(\"{}\")", path.display());
        let actual = scan_path(&path.to_owned(), IconSizes::default());

        // scan_path shouldn't skip any entries
        assert_eq!(entries.len(), actual.len());

        // Correct files should be scanned
        assert!(
            entries
                .into_iter()
                .zip(actual.into_iter())
                .all(|(path, item)| eq_path_item(&path, &item))
        );

        Ok(())
    }

    #[test]
    fn scan_path_returns_empty_vec_for_invalid_path() -> io::Result<()> {
        let fs = simple_fs(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();

        // A nonexisting path within the temp dir
        let invalid_path = path.join("ferris");
        assert!(!invalid_path.exists());

        debug!("Calling scan_path(\"{}\")", invalid_path.display());
        let actual = scan_path(&invalid_path, IconSizes::default());

        assert!(actual.is_empty());

        Ok(())
    }

    #[test]
    fn try_scan_path_fails_for_a_missing_path() -> io::Result<()> {
        let fs = empty_fs()?;
        let missing = fs.path().join("ferris");
        assert!(!missing.exists());

        assert!(try_scan_path(&missing, IconSizes::default()).is_err());

        Ok(())
    }

    #[test]
    fn try_scan_path_reads_an_empty_dir_as_empty() -> io::Result<()> {
        let fs = empty_fs()?;

        let items = try_scan_path(&fs.path().to_owned(), IconSizes::default())?;
        assert!(items.is_empty());

        Ok(())
    }

    #[test]
    fn a_location_that_cannot_be_read_says_so() -> io::Result<()> {
        let fs = empty_fs()?;
        let missing = Location::Path(fs.path().join("ferris"));
        let existing = Location::Path(fs.path().to_owned());

        let (_, items, read_ok) = missing.try_scan(IconSizes::default(), Typing::Content);
        assert!(items.is_empty());
        assert!(!read_ok);
        let (_, _, read_ok) = existing.try_scan(IconSizes::default(), Typing::Content);
        assert!(read_ok);

        Ok(())
    }

    #[test]
    fn scan_path_empty_dir_returns_empty_vec() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();

        debug!("Calling scan_path(\"{}\")", path.display());
        let actual = scan_path(&path.to_owned(), IconSizes::default());

        assert_eq!(0, path.read_dir()?.count());
        assert!(actual.is_empty());

        Ok(())
    }

    #[test]
    fn tab_location_changes_location() -> io::Result<()> {
        let fs = simple_fs(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();

        // Next directory in temp directory
        // This does not have to be sorted
        let next_dir = filter_dirs(path)?
            .next()
            .expect("temp directory should have at least one directory");

        let mut tab = Tab::new(
            Location::Path(path.to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        debug!(
            "Emitting Message::Location(Location::Path(\"{}\"))",
            next_dir.display()
        );
        tab.update_resolved(
            Message::Location(Location::Path(next_dir.clone())),
            Modifiers::empty(),
        );

        // Validate that the tab's path updated
        // NOTE: `items_opt` is set to None with Message::Location so this ONLY checks for equal paths
        // If item contents are NOT None then this needs to be reevaluated for correctness
        assert_eq_tab_path(&tab, &next_dir);
        assert!(
            tab.items_opt.is_none(),
            "Tab's `items` is not None which means this test needs to be updated"
        );

        Ok(())
    }

    #[test]
    fn tab_click_single_selects_item() -> io::Result<()> {
        // Select the second directory with no keys held down
        tab_selects_item(&[1], Modifiers::empty(), &[false, true])
    }

    #[test]
    fn tab_click_double_opens_folder() -> io::Result<()> {
        let (fs, mut tab) = tab_click_new(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();

        // Simulate double clicking second directory
        debug!("Emitting double click Message::DoubleClick(Some(1))");
        tab.update_resolved(Message::DoubleClick(Some(1)), Modifiers::empty());

        // Path to second directory
        let second_dir = read_dir_sorted(path)?
            .into_iter()
            .filter(|p| p.is_dir())
            .nth(1)
            .expect("should be at least two directories");

        // Location should have changed to second_dir
        assert_eq_tab_path(&tab, &second_dir);

        Ok(())
    }

    #[test]
    fn tab_click_ctrl_selects_multiple() -> io::Result<()> {
        // Select the first and second directory by holding down ctrl
        tab_selects_item(&[0, 1], Modifiers::CTRL, &[true, true])
    }

    #[test]
    fn a_dragged_folder_is_no_place_to_drop_the_drag() -> io::Result<()> {
        use crate::ui::iced::{Point, Rectangle, Size};

        let (_fs, mut tab) = tab_click_new(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let items = tab
            .items_opt
            .as_mut()
            .expect("tab should be populated with items");
        assert!(items[0].metadata.is_dir());
        items[0]
            .rect_opt
            .set(Some(Rectangle::new(Point::ORIGIN, Size::new(100.0, 20.0))));
        let dragged = items[0].path_opt().cloned().expect("the folder has a path");
        let over_it = Point::new(10.0, 10.0);

        // Someone else's drag: the folder takes the drop
        assert_eq!(tab.drop_target(over_it), Some(0));

        // This tab's drag of it: over itself, the drop goes to the folder it
        // is in, as if on the background
        tab.dnd_dragged = vec![dragged];
        assert_eq!(tab.drop_target(over_it), None);
        Ok(())
    }

    #[test]
    fn tab_click_on_a_selected_item_narrows_to_it_on_release() -> io::Result<()> {
        let (_fs, mut tab) = tab_click_new(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let selected = |tab: &Tab| -> Vec<usize> {
            tab.items_opt
                .as_deref()
                .expect("tab should be populated with items")
                .iter()
                .enumerate()
                .filter_map(|(i, item)| tab.selection.is_selected(item).then_some(i))
                .collect()
        };

        // This tab lists two folders
        for i in 0..2 {
            tab.update(Message::Click(Some(i)), Modifiers::CTRL);
            tab.update(Message::ClickRelease(Some(i)), Modifiers::CTRL);
        }
        assert_eq!(selected(&tab), [0, 1]);

        // The press keeps the other, so the selection can still be dragged
        // as a whole; a release with no drag in between leaves only this one
        tab.update(Message::Click(Some(1)), Modifiers::empty());
        assert_eq!(selected(&tab), [0, 1]);
        tab.update(Message::ClickRelease(Some(1)), Modifiers::empty());
        assert_eq!(selected(&tab), [1]);

        // Ctrl still toggles the clicked one, keeping the rest
        tab.update(Message::Click(Some(0)), Modifiers::CTRL);
        tab.update(Message::ClickRelease(Some(0)), Modifiers::CTRL);
        tab.update(Message::Click(Some(0)), Modifiers::CTRL);
        tab.update(Message::ClickRelease(Some(0)), Modifiers::CTRL);
        assert_eq!(selected(&tab), [1]);
        Ok(())
    }

    /// Scanning must not resolve icons. Each one searches the icon theme on
    /// disk for tens of milliseconds, and a directory holds enough distinct
    /// types that doing it inline delays the listing by seconds.
    #[test]
    fn scanning_leaves_icons_for_the_worker() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        fs::write(path.join("notes.txt"), b"x")?;
        fs::create_dir(path.join("sub"))?;
        let sizes = IconSizes::default();

        let mut items = super::scan_path(&path.to_path_buf(), sizes);
        // One pass both takes what the cache knows and reports what it lacks,
        // so there is no window in which another tab's worker can fill the
        // cache between the two and leave these placeholders stranded
        let warmup = super::refresh_icons(&mut items, sizes);
        assert!(
            !warmup.is_empty(),
            "the scan should have left the icons to be resolved"
        );

        // What the worker does, then the tab takes the results
        super::warm_icons(&warmup, sizes);
        assert!(
            super::refresh_icons(&mut items, sizes).is_empty(),
            "warming should satisfy everything the scan left"
        );

        // A second visit to the same types asks for nothing
        let mut again = super::scan_path(&path.to_path_buf(), sizes);
        assert!(super::refresh_icons(&mut again, sizes).is_empty());

        // And a type the icon theme has nothing for is not asked for twice
        assert!(
            super::refresh_icons(&mut again, sizes).is_empty(),
            "a type with no icon must not be requeued on every pass"
        );
        Ok(())
    }

    /// Dimensions come from the scan, never from drawing. A scanned image
    /// carries its size; an item built any other way carries none, rather
    /// than opening the file while a frame is being drawn.
    #[test]
    fn image_dimensions_are_filled_by_the_scan_only() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        image::RgbaImage::new(7, 3)
            .save(path.join("pic.png"))
            .expect("the test image should be written");
        fs::write(path.join("plain.txt"), b"x")?;

        let (_parent, items) = Location::Path(path.to_owned()).scan(IconSizes::default());
        let by_name = |name: &str| {
            items
                .iter()
                .find(|item| item.name == name)
                .expect("the scan should list it")
        };
        assert_eq!(
            by_name("pic.png").image_dimensions.get().copied().flatten(),
            Some((7, 3))
        );
        assert_eq!(
            by_name("plain.txt")
                .image_dimensions
                .get()
                .copied()
                .flatten(),
            None,
            "a file that is not an image is never opened for its size"
        );

        // Built outside a scan, as search results and refreshes are
        let metadata = fs::metadata(path.join("pic.png"))?;
        let fresh = super::item_from_entry(
            path.join("pic.png"),
            "pic.png".to_string(),
            metadata,
            IconSizes::default(),
        );
        assert!(
            fresh.image_dimensions.get().is_none(),
            "constructing an item must not read the file"
        );
        Ok(())
    }

    /// A fresh folder holding one JPEG with no extension: its name says
    /// nothing about its type, its content says `image/jpeg`.
    fn folder_with_unnamed_jpeg() -> io::Result<TempDir> {
        let fs = empty_fs()?;
        let named = fs.path().join("photo.jpg");
        image::RgbImage::new(4, 2)
            .save(&named)
            .expect("the test image should be written");
        fs::rename(&named, fs.path().join("photo"))?;
        Ok(fs)
    }

    /// A name-first scan types files by their names and leaves images
    /// unopened for their size; only folders count as checked.
    #[test]
    fn a_name_first_scan_types_by_name() -> io::Result<()> {
        let fs = folder_with_unnamed_jpeg()?;
        fs::create_dir(fs.path().join("sub"))?;
        image::RgbImage::new(4, 2)
            .save(fs.path().join("named.jpg"))
            .expect("the test image should be written");

        let (_parent, items, read_ok) =
            Location::Path(fs.path().to_owned()).try_scan(IconSizes::default(), Typing::NameFirst);
        assert!(read_ok);
        let by_name = |name: &str| {
            items
                .iter()
                .find(|item| item.name == name)
                .expect("the scan should list it")
        };

        let photo = by_name("photo");
        assert_eq!(
            photo.mime,
            Mime::APPLICATION_OCTET_STREAM,
            "the name says nothing about the type"
        );
        assert!(!photo.type_checked, "a file typed by name is not checked");
        assert!(
            photo.image_dimensions.get().is_none(),
            "no file is opened for its size"
        );
        let named = by_name("named.jpg");
        assert_eq!(
            named.mime,
            Mime::from_static("image/jpeg"),
            "the name gives the type"
        );
        assert!(!named.type_checked, "but it is still to be checked");
        assert!(
            named.image_dimensions.get().is_none(),
            "an image is not opened for its size until it is checked"
        );
        assert!(
            by_name("sub").type_checked,
            "a folder's type needs no reading"
        );
        Ok(())
    }

    /// Every other scan still reads content for the type, as before. (Its
    /// image size comes from the extension, as it always has; that path is
    /// covered by `image_dimensions_are_filled_by_the_scan_only`.)
    #[test]
    fn a_content_scan_still_reads_the_file() -> io::Result<()> {
        let fs = folder_with_unnamed_jpeg()?;
        let (_parent, items) = Location::Path(fs.path().to_owned()).scan(IconSizes::default());
        let photo = items
            .iter()
            .find(|item| item.name == "photo")
            .expect("the scan should list it");
        assert_eq!(photo.mime, Mime::from_static("image/jpeg"));
        assert!(photo.type_checked);
        Ok(())
    }

    /// The content check's answer retypes a file its name got wrong, fills
    /// in its size and lets it be thumbnailed; an answer for an older listing
    /// is dropped.
    #[test]
    fn the_content_check_retypes_what_the_name_got_wrong() -> io::Result<()> {
        let fs = folder_with_unnamed_jpeg()?;
        let location = Location::Path(fs.path().to_owned());
        let (_parent, items, _) = location.try_scan(IconSizes::default(), Typing::NameFirst);
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(items);
        let find_photo = |tab: &Tab| {
            tab.items_opt()
                .expect("populated")
                .iter()
                .find(|item| item.name == "photo")
                .expect("listed")
                .clone()
        };
        assert!(
            !find_photo(&tab).wants_thumbnail(),
            "a file typed by name only is not thumbnailed"
        );

        let listing = tab.listing();
        let files = unchecked_files(&tab).expect("the photo is unchecked");
        assert_eq!(files.len(), 1);
        let checked = crate::type_check::check(&files[0], IconSizes::default());

        tab.update(
            Message::TypesChecked(listing.wrapping_sub(1), vec![checked.clone()]),
            Modifiers::empty(),
        );
        assert!(
            !find_photo(&tab).type_checked,
            "an answer for an older listing was applied"
        );

        tab.update(
            Message::TypesChecked(listing, vec![checked]),
            Modifiers::empty(),
        );
        let photo = find_photo(&tab);
        assert!(photo.type_checked);
        assert_eq!(photo.mime, Mime::from_static("image/jpeg"));
        assert_eq!(
            photo.image_dimensions.get().copied().flatten(),
            Some((4, 2))
        );
        assert!(photo.wants_thumbnail(), "checked, and no thumbnail yet");
        assert!(unchecked_files(&tab).is_none(), "nothing is left to check");
        Ok(())
    }

    /// What the content check would read now.
    fn unchecked_files(tab: &Tab) -> Option<std::sync::Arc<[crate::type_check::Unchecked]>> {
        tab.type_check_job().map(|job| job.files)
    }

    /// A tab listing `fs` by name first, sorted by name.
    fn name_first_tab(fs: &TempDir) -> Tab {
        let location = Location::Path(fs.path().to_owned());
        let (_parent, items, _) = location.try_scan(IconSizes::default(), Typing::NameFirst);
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.sort_name = HeadingOptions::Name;
        tab.sort_direction = true;
        tab.set_items(items);
        tab
    }

    /// A folder of `count` files named `f000` and on, typed by name only.
    fn folder_of_files(count: usize) -> io::Result<TempDir> {
        let fs = empty_fs()?;
        for n in 0..count {
            fs::write(fs.path().join(format!("f{n:03}.txt")), b"x")?;
        }
        Ok(fs)
    }

    /// Lays the listing out as the list view would, in item order, ten
    /// pixels a row, and scrolls it so the positions `visible` are on
    /// screen.
    fn show(tab: &mut Tab, visible: std::ops::Range<usize>) {
        let items = tab.items_opt().expect("populated");
        for (row, item) in items.iter().enumerate() {
            item.rect_opt.set(Some(super::Rectangle::new(
                super::Point::new(0.0, row as f32 * 10.0),
                super::Size::new(100.0, 10.0),
            )));
        }
        let count = items.len();
        tab.content_height_opt.set(Some(count as f32 * 10.0));
        tab.size_opt
            .set(Some(super::Size::new(100.0, visible.len() as f32 * 10.0)));
        tab.scroll_opt = Some(super::AbsoluteOffset {
            x: 0.0,
            y: visible.start as f32 * 10.0,
        });
        tab.shown_layout.replace(Some(super::ShownLayout {
            key: super::LayoutKey {
                order: std::sync::Arc::from([]),
                show_hidden: false,
                columns: 1,
                width: 0.0,
                pitch: 0.0,
            },
            shown: (0..count).collect(),
            columns: 1,
            top: 0.0,
            pitch: 10.0,
        }));
    }

    /// Before anything is drawn, files are checked in the order they will be
    /// shown, folders not at all, and hidden ones not while hidden.
    #[test]
    fn unchecked_files_come_in_the_order_shown() -> io::Result<()> {
        let fs = empty_fs()?;
        for name in ["b.txt", ".h.txt", "a.txt"] {
            fs::write(fs.path().join(name), b"x")?;
        }
        fs::create_dir(fs.path().join("dir"))?;
        let mut tab = name_first_tab(&fs);

        let files = unchecked_files(&tab).expect("the files are unchecked");
        let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, ["a.txt", "b.txt"]);

        // A new sort is the order shown from then on
        tab.sort_direction = false;
        let files = unchecked_files(&tab).expect("the files are unchecked");
        let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, ["b.txt", "a.txt"]);

        // Shown hidden files are checked where they are shown
        tab.config.show_hidden = true;
        tab.sort_direction = true;
        let files = unchecked_files(&tab).expect("the files are unchecked");
        let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, [".h.txt", "a.txt", "b.txt"]);
        Ok(())
    }

    /// The window: what is on screen, a screenful below it, then a screenful
    /// above it nearest first, snapped to whole screenfuls.
    #[test]
    fn the_check_reads_around_what_is_on_screen() {
        let order: Vec<usize> = (100..200).collect();
        let (indices, range) = super::check_order(&order, 40..48);
        assert_eq!(range, 32..64);
        let expected: Vec<usize> = (140..148).chain(148..164).chain((132..140).rev()).collect();
        assert_eq!(indices, expected);

        // A screenful is rounded up to a power of two: 7 or 8 rows on screen
        // give the same window
        assert_eq!(super::check_order(&order, 41..48).1, 32..64);
        // Clamped to the listing
        assert_eq!(super::check_order(&order, 96..100).1, 92..100);
        assert_eq!(super::check_order(&order, 0..0).1, 0..3);
    }

    /// Scrolling within the window keeps the job, so its worker carries on;
    /// leaving it makes a new one for the files there.
    #[test]
    fn the_check_follows_the_view_a_window_at_a_time() -> io::Result<()> {
        let fs = folder_of_files(100)?;
        let mut tab = name_first_tab(&fs);

        show(&mut tab, 40..48);
        let first = tab.type_check_job().expect("files are unchecked");
        assert_eq!(first.files.len(), 32);
        assert_eq!(first.files[0].index, 40);
        show(&mut tab, 42..50);
        let scrolled = tab.type_check_job().expect("files are unchecked");
        assert_eq!(scrolled.key, first.key, "the window had not moved");
        assert_eq!(scrolled.files[0].index, 40, "the job was rebuilt");

        show(&mut tab, 60..68);
        let moved = tab.type_check_job().expect("files are unchecked");
        assert_ne!(moved.key, first.key);
        assert_eq!(moved.files[0].index, 60);
        Ok(())
    }

    /// iced asks for the subscription after an update and before it draws,
    /// so a scroll must move the window by itself: the view after it may be
    /// the last thing that happens.
    #[test]
    fn a_scroll_moves_the_window_before_the_view_redraws() -> io::Result<()> {
        let fs = folder_of_files(100)?;
        let mut tab = name_first_tab(&fs);
        show(&mut tab, 0..8);
        let top = tab.type_check_job().expect("files are unchecked");
        assert_eq!(top.files[0].index, 0);

        // As `Message::Scroll` leaves it; nothing is drawn in between
        tab.scroll_opt = Some(super::AbsoluteOffset { x: 0.0, y: 800.0 });
        let scrolled = tab.type_check_job().expect("files are unchecked");
        assert_ne!(scrolled.key, top.key);
        assert_eq!(scrolled.files[0].index, 80);
        Ok(())
    }

    fn shown_names(tab: &Tab) -> Vec<String> {
        tab.column_sort()
            .into_iter()
            .flatten()
            .map(|(_, item)| item.display_name.clone())
            .collect()
    }

    /// The order is sorted once and kept while only order-keeping messages
    /// arrive; any other forgets it, so a change made meanwhile shows.
    #[test]
    fn the_order_is_kept_until_something_may_have_changed_it() -> io::Result<()> {
        let fs = empty_fs()?;
        for name in ["a", "b", "c"] {
            fs::write(fs.path().join(name), b"x")?;
        }
        let mut tab = name_first_tab(&fs);
        assert_eq!(shown_names(&tab), ["a", "b", "c"]);

        // Renamed behind the cache's back, as nothing in the tab would do
        for item in tab.items_opt.as_mut().expect("populated") {
            if item.display_name == "a" {
                item.display_name = "z".to_owned();
            }
        }
        tab.update(Message::HighlightActivate(0), Modifiers::empty());
        assert_eq!(shown_names(&tab), ["z", "b", "c"], "the order was not kept");

        // Nor across a menu, the zoom or the settings it changes: nothing
        // they do moves a file
        tab.update(Message::RightClickBackground, Modifiers::empty());
        tab.update(Message::ZoomIn, Modifiers::empty());
        let mut config = tab.config;
        config.icon_sizes = IconSizes::default();
        tab.update(Message::Config(config), Modifiers::empty());
        assert_eq!(shown_names(&tab), ["z", "b", "c"], "the order was not kept");

        tab.update(
            Message::OpenInNewTab(fs.path().join("b")),
            Modifiers::empty(),
        );
        assert_eq!(
            shown_names(&tab),
            ["b", "c", "z"],
            "a stale order outlived it"
        );

        // A new sort is a new order, without being told
        tab.sort_direction = false;
        assert_eq!(shown_names(&tab), ["z", "c", "b"]);
        Ok(())
    }

    /// The kept selection is forgotten by everything that selects: a kept
    /// answer that is stale fails the check inside `selected_indices`.
    #[test]
    fn every_way_of_selecting_forgets_the_kept_selection() -> io::Result<()> {
        let fs = empty_fs()?;
        for name in ["a", "b", "c"] {
            fs::write(fs.path().join(name), b"x")?;
        }
        let mut tab = name_first_tab(&fs);
        let path = fs.path().join("b");
        let count = |tab: &Tab| tab.selected_indices().len();
        assert_eq!(count(&tab), 0);

        tab.select_all();
        assert_eq!(count(&tab), 3);
        tab.select_none();
        assert_eq!(count(&tab), 0);
        tab.select_paths(vec![path.clone()]);
        assert_eq!(count(&tab), 1);
        tab.select_name("c");
        assert_eq!(count(&tab), 1);
        tab.update(Message::SelectAll, Modifiers::empty());
        assert_eq!(count(&tab), 3);
        // Kept across a message that leaves the selection alone
        tab.update(Message::HighlightActivate(0), Modifiers::empty());
        assert_eq!(count(&tab), 3);
        tab.update(Message::Click(None), Modifiers::empty());
        assert_eq!(count(&tab), 0);
        // A new listing keeps what was selected, by location
        tab.select_paths(vec![path]);
        assert_eq!(count(&tab), 1);
        let (_parent, items, _) =
            Location::Path(fs.path().to_owned()).try_scan(IconSizes::default(), Typing::NameFirst);
        tab.set_items(items);
        assert_eq!(count(&tab), 1);
        Ok(())
    }

    /// Selecting with the keyboard through the layout picks exactly what the
    /// walk over every item picks, in a list and in a grid, with and without
    /// shift.
    #[test]
    fn keyboard_selection_through_the_layout_matches_the_walk() -> io::Result<()> {
        use crate::ui::iced::Size;

        let fs = folder_of_files(37)?;
        for grid in [false, true] {
            let tab_for = |fast: bool| {
                let mut tab = name_first_tab(&fs);
                if grid {
                    tab.config.view = super::View::Grid;
                }
                tab.size_opt.set(Some(Size::new(700.0, 400.0)));
                drop(if grid {
                    tab.grid_view()
                } else {
                    tab.list_view(None)
                });
                if !fast {
                    tab.shown_layout.replace(None);
                }
                tab
            };
            let (mut fast, mut walk) = (tab_for(true), tab_for(false));
            if grid {
                let columns = fast.shown_layout.borrow().as_ref().map(|l| l.columns);
                assert!(columns > Some(2), "the grid has a few columns: {columns:?}");
            }
            let shift = Modifiers::SHIFT;
            let presses = [
                (Message::ItemDown, Modifiers::empty()),
                (Message::ItemDown, Modifiers::empty()),
                (Message::ItemRight, Modifiers::empty()),
                (Message::ItemDown, shift),
                (Message::ItemDown, shift),
                (Message::ItemRight, shift),
                (Message::ItemUp, shift),
                (Message::ItemPageDown, Modifiers::empty()),
                (Message::ItemPageDown, shift),
                (Message::ItemLeft, Modifiers::empty()),
                (Message::ItemPageUp, shift),
                (Message::ItemUp, Modifiers::empty()),
                (Message::ItemPageDown, Modifiers::empty()),
                (Message::ItemPageDown, Modifiers::empty()),
                (Message::ItemPageDown, Modifiers::empty()),
                (Message::ItemDown, Modifiers::empty()),
                (Message::ItemRight, Modifiers::empty()),
                (Message::ItemLeft, shift),
            ];
            for (n, (press, modifiers)) in presses.into_iter().enumerate() {
                let label = format!("{press:?}");
                fast.update(press.clone(), modifiers);
                walk.update(press, modifiers);
                assert_eq!(
                    selected_names(&fast),
                    selected_names(&walk),
                    "grid {grid}, press {n} ({label})"
                );
                assert_eq!(
                    fast.select_focus, walk.select_focus,
                    "grid {grid}, press {n}"
                );
                assert_eq!(
                    fast.select_range, walk.select_range,
                    "grid {grid}, press {n}"
                );
            }
            assert!(
                fast.shown_layout.borrow().is_some(),
                "the fast tab lost its layout"
            );
        }
        Ok(())
    }

    /// Settings keep the order, except the one it is sorted by: folders
    /// first is part of what the order was sorted for.
    #[test]
    fn folders_first_from_the_settings_sorts_again() -> io::Result<()> {
        let fs = empty_fs()?;
        fs::write(fs.path().join("a"), b"x")?;
        fs::create_dir(fs.path().join("b"))?;
        let mut tab = name_first_tab(&fs);
        let mut config = tab.config;
        config.folders_first = true;
        tab.update(Message::Config(config), Modifiers::empty());
        assert_eq!(shown_names(&tab), ["b", "a"]);
        config.folders_first = false;
        tab.update(Message::Config(config), Modifiers::empty());
        assert_eq!(shown_names(&tab), ["a", "b"]);
        Ok(())
    }

    /// What is on screen follows from the rows' geometry: rows that start
    /// above the bottom edge and end below the top one.
    #[test]
    fn the_rows_on_screen_follow_from_the_layout() {
        let list = super::ShownLayout {
            key: super::LayoutKey {
                order: std::sync::Arc::from([]),
                show_hidden: false,
                columns: 1,
                width: 0.0,
                pitch: 10.0,
            },
            shown: (0..100).collect(),
            columns: 1,
            top: 0.0,
            pitch: 10.0,
        };
        use crate::ui::iced::{Point, Rectangle, Size};
        let rect =
            |y: f32, height: f32| Rectangle::new(Point::new(0.0, y), Size::new(100.0, height));
        assert_eq!(list.visible(rect(400.0, 80.0)), 40..48);
        assert_eq!(list.visible(rect(405.0, 80.0)), 40..49, "two half rows");
        assert_eq!(list.visible(rect(990.0, 80.0)), 99..100, "clamped");

        let grid = super::ShownLayout {
            key: super::LayoutKey {
                order: std::sync::Arc::from([]),
                show_hidden: false,
                columns: 4,
                width: 0.0,
                pitch: 100.0,
            },
            shown: (0..95).collect(),
            columns: 4,
            top: 4.0,
            pitch: 100.0,
        };
        assert_eq!(grid.visible(rect(0.0, 250.0)), 0..12);
        assert_eq!(
            grid.visible(rect(2100.0, 250.0)),
            80..95,
            "the last row is short"
        );
    }

    /// A count finds its folder where it was asked about, or by its path
    /// once the listing has moved it; and only a sort by size is redone.
    #[test]
    fn a_folder_count_finds_its_item_and_resorts_only_by_size() -> io::Result<()> {
        let fs = empty_fs()?;
        for name in ["a", "b"] {
            fs::create_dir(fs.path().join(name))?;
        }
        let mut tab = name_first_tab(&fs);
        let items = tab.items_opt().expect("populated").clone();
        let index_of = |name: &str| {
            items
                .iter()
                .position(|item| item.name == name)
                .expect("listed")
        };
        let (a, b) = (index_of("a"), index_of("b"));
        let path_of = |index: usize| items[index].path_opt().expect("a path").clone();
        let (a_path, b_path) = (path_of(a), path_of(b));
        let children =
            |tab: &Tab, index: usize| match &tab.items_opt().expect("populated")[index].metadata {
                ItemMetadata::Path { children_opt, .. } => *children_opt,
                _ => None,
            };

        tab.update(Message::DirectoryChildren(a, a_path, 3), Modifiers::empty());
        assert_eq!(children(&tab, a), Some(3));
        // Asked about at a's place, answered for b
        tab.update(Message::DirectoryChildren(a, b_path, 9), Modifiers::empty());
        assert_eq!(children(&tab, b), Some(9));
        assert_eq!(
            children(&tab, a),
            Some(3),
            "the answer went to the wrong item"
        );

        tab.sort_name = HeadingOptions::Size;
        let _ = tab.column_sort();
        assert!(tab.sort_cache.borrow().is_some());
        tab.update(
            Message::DirectoryChildren(a, path_of(a), 1),
            Modifiers::empty(),
        );
        assert!(
            tab.sort_cache.borrow().is_none(),
            "sizes moved, the order stayed"
        );

        tab.sort_name = HeadingOptions::Name;
        let _ = tab.column_sort();
        tab.update(
            Message::DirectoryChildren(a, path_of(a), 2),
            Modifiers::empty(),
        );
        assert!(tab.sort_cache.borrow().is_some(), "a name sort was redone");
        Ok(())
    }

    /// A selected file is checked wherever it is, after the window, so what
    /// waits for the check -- "Open with" -- is offered for it.
    #[test]
    fn a_selected_file_is_checked_outside_the_window() -> io::Result<()> {
        let fs = folder_of_files(100)?;
        let mut tab = name_first_tab(&fs);
        show(&mut tab, 0..8);
        tab.selection
            .select(&tab.items_opt.as_ref().expect("populated")[90], true);

        let files = unchecked_files(&tab).expect("files are unchecked");
        assert_eq!(files.len(), 25);
        assert_eq!(files.last().map(|file| file.index), Some(90));
        Ok(())
    }

    /// Nothing left unchecked in the window: no job, so no worker, until the
    /// view moves.
    #[test]
    fn a_checked_window_has_no_job() -> io::Result<()> {
        let fs = folder_of_files(100)?;
        let mut tab = name_first_tab(&fs);
        show(&mut tab, 0..8);
        let job = tab.type_check_job().expect("files are unchecked");
        let listing = tab.listing();
        let checked = job
            .files
            .iter()
            .map(|file| crate::type_check::check(file, IconSizes::default()))
            .collect();
        tab.update(Message::TypesChecked(listing, checked), Modifiers::empty());
        assert!(tab.type_check_job().is_none());

        show(&mut tab, 50..58);
        assert!(tab.type_check_job().is_some());
        Ok(())
    }

    /// An answer whose index no longer points at its file -- the listing
    /// changed under the job -- still finds it by its path.
    #[test]
    fn a_stale_index_falls_back_to_the_path() -> io::Result<()> {
        let fs = folder_with_unnamed_jpeg()?;
        fs::write(fs.path().join("a.txt"), b"x")?;
        let mut tab = name_first_tab(&fs);
        let listing = tab.listing();
        let files = unchecked_files(&tab).expect("files are unchecked");
        let photo = files
            .iter()
            .find(|file| file.name == "photo")
            .expect("the photo is unchecked");
        let mut checked = crate::type_check::check(photo, IconSizes::default());
        checked.index = (photo.index + 1) % 2;

        tab.update(
            Message::TypesChecked(listing, vec![checked]),
            Modifiers::empty(),
        );
        let items = tab.items_opt().expect("populated");
        let photo = items
            .iter()
            .find(|item| item.name == "photo")
            .expect("listed");
        assert!(photo.type_checked);
        assert_eq!(photo.mime, Mime::from_static("image/jpeg"));
        let other = items
            .iter()
            .find(|item| item.name == "a.txt")
            .expect("listed");
        assert!(!other.type_checked, "the answer went to the wrong item");
        Ok(())
    }

    /// A refresh reads the file by content and is newer than the check, so
    /// the check's answer must not overwrite it.
    #[test]
    fn a_check_does_not_overwrite_a_refresh() -> io::Result<()> {
        let fs = folder_with_unnamed_jpeg()?;
        let location = Location::Path(fs.path().to_owned());
        let (_parent, items, _) = location.try_scan(IconSizes::default(), Typing::NameFirst);
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(items);
        let listing = tab.listing();
        let files = unchecked_files(&tab).expect("the photo is unchecked");
        let checked = crate::type_check::check(&files[0], IconSizes::default());

        {
            let photo = tab
                .items_opt
                .as_mut()
                .expect("populated")
                .iter_mut()
                .find(|item| item.name == "photo")
                .expect("listed");
            photo.type_checked = true;
            photo.mime = Mime::TEXT_PLAIN;
        }

        tab.update(
            Message::TypesChecked(listing, vec![checked]),
            Modifiers::empty(),
        );
        let photo = tab
            .items_opt()
            .expect("populated")
            .iter()
            .find(|item| item.name == "photo")
            .expect("listed");
        assert_eq!(photo.mime, Mime::TEXT_PLAIN, "the refresh was overwritten");
        assert!(
            photo.image_dimensions.get().is_none(),
            "the check's answer was applied"
        );
        Ok(())
    }

    /// A retype clears any thumbnail; an answer that keeps the type keeps it.
    #[test]
    fn take_check_clears_the_thumbnail_only_on_a_retype() -> io::Result<()> {
        let fs = folder_with_unnamed_jpeg()?;
        let (_parent, items, _) =
            Location::Path(fs.path().to_owned()).try_scan(IconSizes::default(), Typing::NameFirst);
        let photo = items
            .iter()
            .find(|item| item.name == "photo")
            .expect("the scan should list it");
        let path = photo.path_opt().expect("a local file").clone();
        let file = crate::type_check::Unchecked {
            index: 0,
            path: path.clone(),
            name: photo.name.clone(),
            mime: photo.mime.clone(),
        };

        let mut retyped = photo.clone();
        retyped.thumbnail_opt = Some(ItemThumbnail::NotImage);
        assert!(
            retyped.take_check(crate::type_check::check(&file, IconSizes::default())),
            "the content says image/jpeg, the name said nothing"
        );
        assert!(retyped.type_checked);
        assert!(retyped.thumbnail_opt.is_none());

        let mut kept = photo.clone();
        kept.thumbnail_opt = Some(ItemThumbnail::NotImage);
        assert!(!kept.take_check(crate::type_check::Checked {
            index: 0,
            path,
            dims: None,
            retyped: None,
        }));
        assert!(kept.type_checked);
        assert!(matches!(kept.thumbnail_opt, Some(ItemThumbnail::NotImage)));
        Ok(())
    }

    /// Submitting a slow network address and then going somewhere else must
    /// not be undone when the address finally resolves.
    #[test]
    fn a_late_network_answer_does_not_drag_the_tab_back() -> io::Result<()> {
        use crate::tab::Command;

        let fs = empty_fs()?;
        let start = Location::Path(fs.path().to_owned());
        let mut tab = Tab::new(
            start,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );

        // Ask for a network address
        let uri = "smb://example/share/".to_owned();
        tab.edit_location = Some(Location::Network(uri.clone(), uri.clone(), None).into());
        let commands = tab.update(Message::EditLocationSubmit, Modifiers::empty());
        let request = commands
            .iter()
            .find_map(|command| match command {
                Command::ResolveNetwork(request, _) => Some(*request),
                _ => None,
            })
            .expect("submitting a network address should ask for it to be resolved");

        // Go somewhere else while the server is being waited on
        let elsewhere = Location::Path(fs.path().join("elsewhere"));
        fs::create_dir(fs.path().join("elsewhere"))?;
        tab.change_location(&elsewhere, None);
        let now = tab.location.clone();

        // The answer finally arrives
        let resolved = Location::Network(uri.clone(), uri.clone(), None);
        tab.update(
            Message::NetworkResolved(request, uri, Some(resolved)),
            Modifiers::empty(),
        );

        assert_eq!(
            tab.location, now,
            "a network answer from before the tab moved took it back"
        );
        Ok(())
    }

    /// A shut eye searches the folder and nothing under it; an open one
    /// searches the whole tree.
    #[test]
    fn search_depth_follows_the_eye() -> io::Result<()> {
        use crate::tab::{SearchItem, SearchLocation, SearchOptions, scan_search};
        use std::sync::Mutex;

        let fs = empty_fs()?;
        let root = fs.path();
        fs::write(root.join("target-here.txt"), b"x")?;
        fs::create_dir(root.join("sub"))?;
        fs::write(root.join("sub").join("target-deep.txt"), b"x")?;
        fs::create_dir(root.join("sub").join("deeper"))?;
        fs::write(
            root.join("sub").join("deeper").join("target-deepest.txt"),
            b"x",
        )?;

        let found = |recursive: bool| -> Vec<String> {
            let names = Mutex::new(Vec::new());
            scan_search(
                &SearchLocation::Path(root.to_path_buf()),
                "target",
                SearchOptions {
                    show_hidden: false,
                    recursive,
                },
                &|| true,
                |item| {
                    if let SearchItem::Path(_, name, _) = item {
                        names.lock().unwrap().push(name);
                    }
                    true
                },
            );
            let mut names = names.into_inner().unwrap();
            names.sort();
            names
        };

        assert_eq!(
            found(false),
            vec!["target-here.txt".to_owned()],
            "a shut eye should find only what is in this folder"
        );
        assert_eq!(
            found(true),
            vec![
                "target-deep.txt".to_owned(),
                "target-deepest.txt".to_owned(),
                "target-here.txt".to_owned(),
            ],
            "an open eye should find everything underneath too"
        );
        Ok(())
    }

    /// A search nobody waits for stops, though nothing it visits matches
    #[test]
    fn an_unwanted_search_stops_where_nothing_matches() -> io::Result<()> {
        use crate::tab::{SearchLocation, SearchOptions, scan_search};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let fs = empty_fs()?;
        for folder in 0..10 {
            let folder = fs.path().join(format!("folder-{folder}"));
            fs::create_dir(&folder)?;
            for file in 0..20 {
                fs::write(folder.join(format!("file-{file}")), b"x")?;
            }
        }
        let asked = AtomicUsize::new(0);
        let matched = AtomicUsize::new(0);
        scan_search(
            &SearchLocation::Path(fs.path().to_path_buf()),
            "nothing-is-called-this",
            SearchOptions {
                show_hidden: false,
                recursive: true,
            },
            &|| {
                asked.fetch_add(1, Ordering::Relaxed);
                false
            },
            |_| {
                matched.fetch_add(1, Ordering::Relaxed);
                true
            },
        );
        let asked = asked.into_inner();
        assert!(asked > 0, "asked at all");
        assert!(
            asked < 50,
            "walked on after it was not wanted: {asked} entries"
        );
        assert_eq!(matched.into_inner(), 0);
        Ok(())
    }

    /// Setting the query of a running search keeps that search's scope, even
    /// when the config has since moved on -- the Back-then-type case.
    #[test]
    fn editing_a_query_keeps_the_searches_scope() -> io::Result<()> {
        use crate::tab::{SearchLocation, SearchOptions};
        use std::time::Instant;

        let fs = empty_fs()?;
        let mut tab = Tab::new(
            Location::Path(fs.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );

        // Not searching yet: a new search takes the config's default
        tab.config.search_recursive = true;
        assert!(
            tab.search_options_for_query().recursive,
            "a search starting fresh should take the config's default"
        );

        // A search restored from history, made while the eye was shut, with
        // the config since moved on to wanting subfolders
        tab.location = Location::Search(
            SearchLocation::Path(fs.path().to_owned()),
            "ter".to_owned(),
            SearchOptions {
                show_hidden: false,
                recursive: false,
            },
            Instant::now(),
        );
        assert!(
            !tab.search_options_for_query().recursive,
            "typing into a restored search silently widened it"
        );
        // Hidden files are the config's answer, not the search's: the menu
        // checkbox describes the config, and the search must not contradict it
        tab.config.show_hidden = true;
        assert!(
            tab.search_options_for_query().show_hidden,
            "typing into a restored search kept hiding files the menu says to show"
        );
        Ok(())
    }

    /// Opening an empty search of the folder on screen, or closing it, lists
    /// the same folder: the listing stays as it is, with what was already
    /// counted for it, and the folder names the last rescan read. As a
    /// rescan would, it selects the top of the search, and nothing once the
    /// search is closed.
    #[test]
    fn opening_or_closing_an_empty_search_keeps_the_listing() -> io::Result<()> {
        use crate::tab::{SearchLocation, SearchOptions};
        use std::time::Instant;

        let (fs, mut tab) = tab_click_new(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path().to_owned();
        let search = |term: &str| {
            Location::Search(
                SearchLocation::Path(path.clone()),
                term.to_owned(),
                SearchOptions {
                    show_hidden: false,
                    recursive: false,
                },
                Instant::now(),
            )
        };
        let counted = |tab: &Tab| {
            tab.items_opt()
                .into_iter()
                .flatten()
                .filter(|item| {
                    matches!(
                        item.metadata,
                        ItemMetadata::Path {
                            children_opt: Some(7),
                            ..
                        }
                    )
                })
                .count()
        };
        let selected = |tab: &Tab| {
            tab.items_opt()
                .into_iter()
                .flatten()
                .filter(|item| tab.selection.is_selected(item))
                .count()
        };
        if let Some(items) = tab.items_opt.as_mut() {
            for item in items.iter_mut() {
                if let ItemMetadata::Path { children_opt, .. } = &mut item.metadata {
                    *children_opt = Some(7);
                }
            }
            tab.selection.select(&items[0], true);
            tab.selection.highlight(&items[1], true);
        }
        // As the rescan named it from the filesystem
        for (_, name) in &mut tab.location_ancestors {
            *name = format!("looked {name}");
        }
        let listing = tab.listing();

        assert!(tab.only_switches_search(&search("")));
        tab.switch_search(&search(""));
        assert_eq!(
            listing,
            tab.listing(),
            "opening the search started a new listing"
        );
        assert_eq!(
            NUM_DIRS,
            counted(&tab),
            "opening the search lost the counts"
        );
        assert_eq!(1, selected(&tab), "the top of the search is selected");
        assert!(
            tab.items_opt()
                .into_iter()
                .flatten()
                .all(|item| !tab.selection.is_highlighted(item))
        );
        assert!(
            tab.location_ancestors
                .iter()
                .all(|(_, name)| name.starts_with("looked "))
        );
        assert!(tab.location_title.contains("looked "));

        // A search with a term, or the same search again, lists anew
        assert!(!tab.only_switches_search(&search("x")));
        assert!(!tab.only_switches_search(&search("")));

        let closed = Location::Path(path.clone());
        assert!(tab.only_switches_search(&closed));
        tab.switch_search(&closed);
        assert_eq!(
            listing,
            tab.listing(),
            "closing the search started a new listing"
        );
        assert_eq!(
            NUM_DIRS,
            counted(&tab),
            "closing the search lost the counts"
        );
        assert_eq!(0, selected(&tab));
        assert!(tab.location_title.starts_with("looked "));
        Ok(())
    }

    /// A read of a folder still counts once its empty search is opened, or
    /// closed, as both list the folder; a search with a term, a recursive
    /// one, or another folder lists something else.
    #[test]
    fn a_folder_and_its_empty_search_list_the_same() {
        use crate::tab::{SearchLocation, SearchOptions};
        use std::time::Instant;

        let search = |path: &str, term: &str, recursive: bool| {
            Location::Search(
                SearchLocation::Path(PathBuf::from(path)),
                term.to_owned(),
                SearchOptions {
                    show_hidden: false,
                    recursive,
                },
                Instant::now(),
            )
        };
        let folder = Location::Path(PathBuf::from("/a"));
        assert!(folder.lists_same_as(&search("/a", "", false)));
        assert!(search("/a", "", false).lists_same_as(&folder));
        assert!(search("/a", "", false).lists_same_as(&search("/a", "", false)));
        assert!(!folder.lists_same_as(&search("/a", "x", false)));
        assert!(!folder.lists_same_as(&search("/a", "", true)));
        assert!(!folder.lists_same_as(&search("/b", "", false)));
        assert!(!folder.lists_same_as(&Location::Trash));
    }

    /// A read made before the search was opened, or closed, keeps the names
    /// it read, but its breadcrumbs and title are the current mode's: a
    /// folder's lead to folders, a search's to searches
    #[test]
    fn a_read_for_the_other_mode_is_named_for_this_one() -> io::Result<()> {
        use crate::tab::{SearchLocation, SearchOptions};
        use std::time::Instant;

        let (fs, mut tab) = tab_click_new(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path().to_owned();
        let folder = Location::Path(path.clone());
        let search = Location::Search(
            SearchLocation::Path(path),
            String::new(),
            SearchOptions {
                show_hidden: false,
                recursive: false,
            },
            Instant::now(),
        );
        let read = |location: &Location| -> Vec<String> {
            location
                .ancestors(false)
                .into_iter()
                .map(|(_, name)| format!("looked {name}"))
                .collect()
        };

        tab.switch_search(&search);
        let (ancestors, title) = tab.named_folders(read(&folder)).expect("same folder");
        assert!(ancestors.iter().all(|(location, name)| {
            matches!(location, Location::Search(..)) && name.starts_with("looked ")
        }));
        assert_eq!(title, search.title_with(|_| ancestors[0].1.clone()));
        assert_ne!(title, ancestors[0].1, "a search titled as its folder");

        tab.switch_search(&folder);
        let (ancestors, title) = tab.named_folders(read(&search)).expect("same folder");
        assert!(ancestors.iter().all(|(location, name)| {
            matches!(location, Location::Path(_)) && name.starts_with("looked ")
        }));
        assert_eq!(title, ancestors[0].1);

        // Names that do not fit the path are not used
        let mut too_many = read(&folder);
        too_many.push("extra".to_owned());
        assert!(tab.named_folders(too_many).is_none());
        Ok(())
    }

    /// Only the latest folder read started for a tab may be shown: an
    /// earlier one is out of date whenever it lands
    #[test]
    fn only_the_latest_read_counts() -> io::Result<()> {
        let (_fs, mut tab) = tab_click_new(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let first = tab.start_scan();
        assert!(tab.is_latest_scan(first));
        let second = tab.start_scan();
        assert!(
            !tab.is_latest_scan(first),
            "an earlier read replaced a later one"
        );
        assert!(tab.is_latest_scan(second));
        Ok(())
    }

    /// Opening the search while scrolled down keeps the offset, so the top
    /// it selects, which Enter opens, has to be scrolled to
    #[test]
    fn the_top_an_opened_search_selects_is_scrolled_to() -> io::Result<()> {
        use crate::tab::{SearchLocation, SearchOptions};
        use crate::ui::iced::{Point, Rectangle, Size};
        use crate::ui::widget::scrollable::AbsoluteOffset;
        use std::time::Instant;

        let (fs, mut tab) = tab_click_new(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let row = 40.0;
        if let Some(items) = tab.items_opt.as_mut() {
            for (i, item) in items.iter().enumerate() {
                item.rect_opt.set(Some(Rectangle::new(
                    Point::new(0.0, i as f32 * row),
                    Size::new(400.0, row),
                )));
            }
        }
        tab.item_view_size_opt.set(Some(Size::new(400.0, row)));
        let below = NUM_DIRS as f32 * row;
        tab.scroll_opt = Some(AbsoluteOffset { x: 0.0, y: below });

        let search = Location::Search(
            SearchLocation::Path(fs.path().to_owned()),
            String::new(),
            SearchOptions {
                show_hidden: false,
                recursive: false,
            },
            Instant::now(),
        );
        tab.switch_search(&search);
        assert_eq!(Some(AbsoluteOffset { x: 0.0, y: below }), tab.scroll_opt);
        let top = tab.select_focus.expect("the search selected its top");
        let top_y = tab.items_opt().unwrap()[top].rect_opt.get().unwrap().y;
        assert_eq!(
            Some(AbsoluteOffset { x: 0.0, y: top_y }),
            tab.select_focus_scroll(),
            "the selected top was left out of view"
        );
        Ok(())
    }

    /// Reading the same folder again keeps the listing on screen until the
    /// rescan replaces it, instead of the view going blank for a frame. It
    /// keeps no selection, as the rescan's own listing would not.
    #[test]
    fn reading_an_empty_search_again_keeps_the_listing() -> io::Result<()> {
        use crate::tab::{SearchLocation, SearchOptions};
        use std::time::Instant;

        let (fs, mut tab) = tab_click_new(NUM_FILES, NUM_NESTED, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path().to_owned();
        let search = |term: &str| {
            Location::Search(
                SearchLocation::Path(path.clone()),
                term.to_owned(),
                SearchOptions {
                    show_hidden: false,
                    recursive: false,
                },
                Instant::now(),
            )
        };
        tab.change_location(&search(""), None);
        tab.set_items(Location::Path(path.clone()).scan(IconSizes::default()).1);
        if let Some(items) = tab.items_opt.as_mut() {
            tab.selection.select(&items[0], true);
            tab.selection.highlight(&items[0], true);
        }

        tab.change_location_keeping_listing(&search(""));
        let items = tab
            .items_opt()
            .expect("reading the search again blanked the listing");
        assert_eq!(NUM_DIRS, items.len());
        assert!(items.iter().all(|item| {
            !tab.selection.is_selected(item) && !tab.selection.is_highlighted(item)
        }));

        // A term lists something else: that listing starts empty
        tab.change_location_keeping_listing(&search("x"));
        assert!(tab.items_opt().is_none());
        Ok(())
    }

    /// Going Back to a search made under a different "show hidden" brings
    /// that much up to date, while leaving the scope the user chose alone.
    #[test]
    fn a_restored_search_follows_the_hidden_files_setting() -> io::Result<()> {
        use crate::tab::{SearchLocation, SearchOptions};
        use std::time::Instant;

        let fs = empty_fs()?;
        let mut tab = Tab::new(
            Location::Path(fs.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.config.show_hidden = true;
        tab.config.search_recursive = true;

        let remembered = Location::Search(
            SearchLocation::Path(fs.path().to_owned()),
            "term".to_owned(),
            SearchOptions {
                show_hidden: false,
                recursive: false,
            },
            Instant::now(),
        );

        match tab.restored(&remembered) {
            Location::Search(_, _, options, _) => {
                assert!(
                    options.show_hidden,
                    "a restored search contradicted the menu's hidden files checkbox"
                );
                assert!(
                    !options.recursive,
                    "a restored search lost the scope the eye had been set to"
                );
            }
            other => panic!("expected a search location, got {other:?}"),
        }
        Ok(())
    }

    /// A search carries the scope it was given. Editing its query is not a
    /// request to change that scope, and neither is an unrelated setting.
    #[test]
    fn a_running_search_keeps_its_scope() -> io::Result<()> {
        use crate::tab::{SearchLocation, SearchOptions};
        use std::time::Instant;

        let fs = empty_fs()?;
        let narrow = SearchOptions {
            show_hidden: false,
            recursive: false,
        };
        let searching = |tab: &Tab| match &tab.location {
            Location::Search(_, term, options, _) => (term.clone(), *options),
            other => panic!("expected a search location, got {other:?}"),
        };
        let restored = |tab: &mut Tab| {
            // As history gives it back: made while the eye was shut, while
            // the config has since moved on to wanting subfolders
            tab.config.search_recursive = true;
            tab.location = Location::Search(
                SearchLocation::Path(fs.path().to_owned()),
                "term".to_owned(),
                narrow,
                Instant::now(),
            );
        };
        let new_tab = || {
            Tab::new(
                Location::Path(fs.path().to_owned()),
                TabConfig::default(),
                ThumbCfg::default(),
                None,
                std::borrow::Cow::Borrowed("Undefined"),
                None,
            )
        };

        // An unrelated setting changing leaves the scope alone
        let mut tab = new_tab();
        restored(&mut tab);
        let config = TabConfig {
            show_hidden: true,
            search_recursive: true,
            ..TabConfig::default()
        };
        tab.update_resolved(Message::Config(config), Modifiers::empty());
        let (_, options) = searching(&tab);
        assert!(
            !options.recursive,
            "turning on hidden files widened a search that had been narrowed"
        );
        assert!(
            options.show_hidden,
            "the setting that did change was applied"
        );

        // The eye changes it, even though the config already says so and is
        // therefore not changing
        let mut tab = new_tab();
        restored(&mut tab);
        tab.update_resolved(Message::SetSearchRecursive(true), Modifiers::empty());
        let (_, options) = searching(&tab);
        assert!(
            options.recursive,
            "the eye did not reach a search whose config already agreed with it"
        );
        Ok(())
    }

    /// The eye starts shut: a search answers about the folder being looked
    /// at, and is widened deliberately.
    #[test]
    fn search_starts_in_this_folder_only() {
        assert!(
            !TabConfig::default().search_recursive,
            "search should not descend into subfolders by default"
        );
    }

    /// A detail read answers the incarnation of the item that asked for it,
    /// and only that one. A rebuilt item is a new incarnation.
    #[test]
    fn details_answer_only_the_incarnation_that_asked() -> io::Result<()> {
        use crate::tab::{Command, Item, ItemMetadata, MetadataState};
        use crate::ui::widget::scrollable::AbsoluteOffset;

        let fs = empty_fs()?;
        fs::write(fs.path().join("a.txt"), b"x")?;
        let location = Location::Path(fs.path().to_owned());
        let (_parent, items) = location.scan(IconSizes::default());
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(items);

        // Make it an item the listing cannot fully describe, as a trashed or
        // GVFS item is
        fn first(tab: &mut Tab) -> &mut Item {
            tab.items_opt
                .as_mut()
                .expect("populated")
                .first_mut()
                .expect("one item")
        }
        let path = {
            let item = first(&mut tab);
            item.metadata = ItemMetadata::SimpleFile { size: 1 };
            item.path_opt().cloned().expect("has a path")
        };
        assert!(
            first(&mut tab).file_metadata().is_none(),
            "nothing is known before the disk answers, and nothing is read here"
        );
        let epoch = first(&mut tab).details_epoch;
        let listing = tab.listing();
        let metadata = fs::metadata(&path)?;

        // The first update of a fresh tab also scrolls it to the top, which is
        // a command like any other. Settle that first, so what a read costs
        // can be counted exactly.
        tab.scroll_opt = Some(AbsoluteOffset { x: 0.0, y: 0.0 });
        let reads = |commands: Vec<Command>| {
            commands
                .into_iter()
                .filter(|command| matches!(command, Command::Iced(_)))
                .count()
        };

        // Asking starts one read, and asking again while it is out starts none
        let started = reads(tab.update(
            Message::RequestDetails(path.clone(), epoch, listing),
            Modifiers::empty(),
        ));
        assert_eq!(started, 1, "asking should start exactly one read");
        assert!(matches!(first(&mut tab).details, MetadataState::Loading));
        let again = reads(tab.update(
            Message::RequestDetails(path.clone(), epoch, listing),
            Modifiers::empty(),
        ));
        assert_eq!(
            again, 0,
            "a second ask for a read already out started another"
        );

        // An answer for another incarnation is ignored
        tab.update(
            Message::ItemDetails(
                path.clone(),
                epoch.wrapping_add(1),
                listing,
                Ok(metadata.clone()),
            ),
            Modifiers::empty(),
        );
        assert!(
            matches!(first(&mut tab).details, MetadataState::Loading),
            "an answer for a different incarnation was applied: state is {:?}",
            first(&mut tab).details
        );
        // And so is one for another listing, even at the same epoch: a rescan
        // hands out fresh items at epoch zero, just like the ones before it
        tab.update(
            Message::ItemDetails(
                path.clone(),
                epoch,
                listing.wrapping_add(1),
                Ok(metadata.clone()),
            ),
            Modifiers::empty(),
        );
        assert!(
            matches!(first(&mut tab).details, MetadataState::Loading),
            "an answer for a different listing was applied"
        );

        // The answer for this one lands
        tab.update(
            Message::ItemDetails(path.clone(), epoch, listing, Ok(metadata)),
            Modifiers::empty(),
        );
        assert!(
            first(&mut tab).file_metadata().is_some(),
            "the answer for this incarnation was not applied"
        );

        // A rebuild is a new incarnation, asked about afresh
        let fresh = super::item_from_path(&path, IconSizes::default()).expect("rebuild");
        first(&mut tab).adopt(fresh);
        assert_eq!(first(&mut tab).details_epoch, epoch.wrapping_add(1));
        assert!(
            matches!(first(&mut tab).details, MetadataState::Pending),
            "a rebuilt item kept the old incarnation's answer"
        );
        // A read for the rebuilt item, left outstanding across the rescan
        // below
        let epoch = first(&mut tab).details_epoch;
        let started = reads(tab.update(
            Message::RequestDetails(path.clone(), epoch, listing),
            Modifiers::empty(),
        ));
        assert_eq!(started, 1, "the rebuilt item should be read afresh");

        // Nor does a rescan that replaces the item: the path is still being
        // read, and the replacement waits for that read rather than adding a
        // second one that would hold a second permit
        let (old_epoch, old_listing) = (epoch, listing);
        tab.set_items(Vec::new());
        let replaced = super::item_from_path(&path, IconSizes::default()).expect("rebuild");
        let mut replaced = replaced;
        replaced.metadata = ItemMetadata::SimpleFile { size: 1 };
        tab.set_items(vec![replaced]);
        let listing = tab.listing();
        let epoch = first(&mut tab).details_epoch;
        let after_rescan = reads(tab.update(
            Message::RequestDetails(path.clone(), epoch, listing),
            Modifiers::empty(),
        ));
        assert_eq!(
            after_rescan, 0,
            "a rescanned item started a second read of a path already being read"
        );
        assert!(
            matches!(first(&mut tab).details, MetadataState::Loading),
            "the replacement should be marked as waiting on the read"
        );

        // The old read ends. Its answer is for the incarnation before the
        // rescan and is not applied -- but the replacement was waiting on
        // exactly this, and must now be read for itself rather than left
        // "Calculating" for good.
        let stale = fs::metadata(&path)?;
        let restarted = reads(tab.update(
            Message::ItemDetails(path.clone(), old_epoch, old_listing, Ok(stale)),
            Modifiers::empty(),
        ));
        assert!(
            !matches!(first(&mut tab).details, MetadataState::Ready(_)),
            "a stale answer was applied to the replacement"
        );
        assert_eq!(
            restarted, 1,
            "the replacement's own read was not started when the read it waited on ended"
        );
        assert!(matches!(first(&mut tab).details, MetadataState::Loading));

        // And that read's answer lands
        let fresh = fs::metadata(&path)?;
        tab.update(
            Message::ItemDetails(path.clone(), epoch, listing, Ok(fresh)),
            Modifiers::empty(),
        );
        assert!(
            first(&mut tab).file_metadata().is_some(),
            "the replacement's own answer was not applied"
        );

        Ok(())
    }

    /// The last answer to arrive is the one most likely to be stale, so the
    /// record that rejects it must outlive the check.
    #[test]
    fn the_last_answer_to_arrive_is_still_checked_against_the_others() -> io::Result<()> {
        let fs = empty_fs()?;
        let mut tab = Tab::new(
            Location::Path(fs.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(Vec::new());
        let listing = tab.listing();

        // Two batches out for the same file
        let older = tab.next_refresh_batch();
        let newer = tab.next_refresh_batch();
        let path = fs.path().join("a.txt");

        // The newer one comes back first and is taken
        assert!(tab.accept_refresh(listing, newer, &path));

        // The older one comes back last. It is still the older one, and
        // nothing about being last may change that.
        assert!(
            !tab.accept_refresh(listing, older, &path),
            "an older answer was taken because it happened to arrive last"
        );
        Ok(())
    }

    /// An answer belonging to a listing that has since been replaced is not
    /// applied to the listing now on screen.
    #[test]
    fn an_old_listings_answer_is_rejected_by_the_new_one() -> io::Result<()> {
        let fs = empty_fs()?;
        let mut tab = Tab::new(
            Location::Path(fs.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(Vec::new());
        let old_listing = tab.listing();
        let stale = tab.next_refresh_batch();
        let path = fs.path().join("a.txt");

        // A rescan replaces the listing while that batch is still out
        tab.set_items(Vec::new());
        let listing = tab.listing();
        assert_ne!(listing, old_listing, "a rescan is a new listing");

        assert!(
            !tab.accept_refresh(old_listing, stale, &path),
            "an answer about a listing that has been replaced was applied to its replacement"
        );
        // And it left nothing behind that would reject the new listing's work
        let fresh = tab.next_refresh_batch();
        assert!(
            tab.accept_refresh(listing, fresh, &path),
            "a stale answer blocked the current listing's own work"
        );
        Ok(())
    }

    /// Every way of putting the address field away withdraws what it asked
    /// for, not just the ones that go through `EditLocation`.
    #[test]
    fn dismissing_the_address_field_withdraws_its_request() -> io::Result<()> {
        use crate::tab::Command;

        for dismiss in ["click", "right click", "navigate"] {
            let fs = empty_fs()?;
            let start = Location::Path(fs.path().to_owned());
            let mut tab = Tab::new(
                start.clone(),
                TabConfig::default(),
                ThumbCfg::default(),
                None,
                std::borrow::Cow::Borrowed("Undefined"),
                None,
            );

            let uri = "smb://example/share/".to_owned();
            tab.edit_location = Some(Location::Network(uri.clone(), uri.clone(), None).into());
            let request = tab
                .update(Message::EditLocationSubmit, Modifiers::empty())
                .iter()
                .find_map(|command| match command {
                    Command::ResolveNetwork(request, _) => Some(*request),
                    _ => None,
                })
                .expect("should ask for resolution");

            match dismiss {
                "click" => {
                    tab.update(Message::Click(None), Modifiers::empty());
                }
                "right click" => {
                    tab.update(Message::RightClickBackground, Modifiers::empty());
                }
                _ => tab.change_location(&start, None),
            }

            let before = tab.location.clone();
            tab.update(
                Message::NetworkResolved(
                    request,
                    uri.clone(),
                    Some(Location::Network(uri.clone(), uri, None)),
                ),
                Modifiers::empty(),
            );

            assert_eq!(
                tab.location, before,
                "dismissing by {dismiss} left the request live, and it navigated"
            );
        }
        Ok(())
    }

    /// A tab in `root` whose address field holds `path`, with `completions`
    /// already looked up for it.
    fn tab_editing(
        root: &std::path::Path,
        path: PathBuf,
        completions: Option<Vec<(String, PathBuf)>>,
    ) -> Tab {
        let mut tab = Tab::new(
            Location::Path(root.to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        let mut edit_location: super::EditLocation = Location::Path(path).into();
        edit_location.completions = completions;
        tab.edit_location = Some(edit_location);
        tab
    }

    /// What typing `path` into the address field sends.
    fn type_path(tab: &mut Tab, path: PathBuf) {
        tab.update(
            Message::EditLocation(Some(Location::Path(path).into())),
            Modifiers::empty(),
        );
    }

    /// The address field's bounds in the header laid out `width` wide.
    fn field_bounds(tab: &Tab, width: f32) -> crate::ui::iced_core::Rectangle {
        use crate::ui::iced_core::widget::{Id, Operation};
        use crate::ui::iced_core::{Rectangle, Size};
        use crate::ui::iced_runtime::user_interface::{Cache, UserInterface};

        struct Containers(Vec<(Option<Id>, Rectangle)>);
        impl Operation for Containers {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
                self.0.push((id.cloned(), bounds));
            }
        }

        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut ui = UserInterface::build(
            tab.header_view(),
            Size::new(width, 40.0),
            Cache::default(),
            &mut renderer,
        );
        let mut containers = Containers(Vec::new());
        ui.operate(&renderer, &mut containers);
        containers
            .0
            .into_iter()
            .find(|(id, _)| id.as_ref() == Some(&tab.edit_location_id))
            .expect("the field is laid out")
            .1
    }

    #[test]
    fn the_address_field_widens_to_its_text_within_the_room() -> io::Result<()> {
        let fs = empty_fs()?;
        let mut tab = tab_editing(fs.path(), fs.path().to_owned(), None);
        let short = field_bounds(&tab, 1600.0);

        type_path(
            &mut tab,
            fs.path()
                .join("a folder with a rather long name, longer than the crumbs"),
        );
        let long = field_bounds(&tab, 1600.0);
        assert!(
            long.width > short.width + 100.0,
            "the field widens with its text: {short:?} then {long:?}"
        );

        // In a narrow header, as wide as the room and no wider. The crumbs
        // shorten there too, so this can be narrower than `short`.
        let window = short.x + long.width / 2.0;
        let narrow = field_bounds(&tab, window);
        assert!(narrow.width < long.width, "{long:?} {narrow:?}");
        assert!(
            (narrow.x + narrow.width - window).abs() < 0.5,
            "fills the room: {narrow:?} in {window}"
        );
        Ok(())
    }

    /// Two folders, `alpha` and `beta`, and the list the field shows for the
    /// root with a trailing separator: both of them.
    fn two_folders() -> io::Result<(TempDir, Vec<(String, PathBuf)>)> {
        let fs = empty_fs()?;
        fs::create_dir(fs.path().join("alpha"))?;
        fs::create_dir(fs.path().join("beta"))?;
        let list = super::tab_complete(&fs.path().join(""))
            .map_err(|err| io::Error::other(err.to_string()))?;
        assert_eq!(list.len(), 2, "the fixture should list both folders");
        Ok((fs, list))
    }

    #[test]
    fn typing_keeps_the_previous_list_on_show_but_not_choosable() -> io::Result<()> {
        let (fs, list) = two_folders()?;
        let mut tab = tab_editing(fs.path(), fs.path().join(""), Some(list.clone()));

        type_path(&mut tab, fs.path().join("b"));

        let edit_location = tab.edit_location.as_ref().expect("the field is still open");
        assert_eq!(
            edit_location.completions, None,
            "the new list is not looked up yet"
        );
        assert_eq!(edit_location.stale, Some(list.clone()));
        assert_eq!(edit_location.shown(), Some((list.as_slice(), false)));
        Ok(())
    }

    #[test]
    fn nothing_stale_is_submitted() -> io::Result<()> {
        use crate::tab::Command;

        let (fs, list) = two_folders()?;
        let mut tab = tab_editing(fs.path(), fs.path().join(""), Some(list));
        let typed = fs.path().join("b");
        type_path(&mut tab, typed.clone());

        // With a list of its own, a path that does not exist would go to the
        // list's first entry. With only a stale list it is taken as typed: it
        // is not a folder, so the tab stays in its parent with it selected.
        let commands = tab.update_resolved(Message::EditLocationSubmit, Modifiers::empty());
        let went_to = commands.iter().find_map(|command| match command {
            Command::ChangeLocation(_, location, selected) => {
                Some((location.clone(), selected.clone()))
            }
            _ => None,
        });
        assert_eq!(
            went_to,
            Some((Location::Path(fs.path().to_owned()), Some(vec![typed])))
        );
        assert_eq!(tab.location, Location::Path(fs.path().to_owned()));
        Ok(())
    }

    #[test]
    fn the_looked_up_list_replaces_the_stale_one() -> io::Result<()> {
        let (fs, list) = two_folders()?;
        let mut tab = tab_editing(fs.path(), fs.path().join(""), Some(list.clone()));
        let typed = fs.path().join("b");
        type_path(&mut tab, typed.clone());
        assert_eq!(
            tab.edit_location.as_ref().and_then(|x| x.stale.clone()),
            Some(list.clone()),
            "the old list should be on show until the new one arrives"
        );

        let looked_up = vec![list[1].clone()];
        tab.update(
            Message::TabComplete(typed, looked_up.clone()),
            Modifiers::empty(),
        );

        let edit_location = tab.edit_location.as_ref().expect("the field is still open");
        assert_eq!(edit_location.completions, Some(looked_up));
        assert_eq!(edit_location.stale, None);
        Ok(())
    }

    #[test]
    fn a_stale_list_is_carried_across_several_keystrokes() -> io::Result<()> {
        let (fs, list) = two_folders()?;
        let mut tab = tab_editing(fs.path(), fs.path().join(""), Some(list.clone()));

        type_path(&mut tab, fs.path().join("b"));
        type_path(&mut tab, fs.path().join("be"));

        let edit_location = tab.edit_location.as_ref().expect("the field is still open");
        assert_eq!(edit_location.completions, None);
        assert_eq!(edit_location.stale, Some(list));
        Ok(())
    }

    #[test]
    fn dismissing_the_address_field_carries_no_list_over() -> io::Result<()> {
        let (fs, list) = two_folders()?;
        let mut tab = tab_editing(fs.path(), fs.path().join(""), Some(list));

        tab.update(Message::EditLocation(None), Modifiers::empty());
        type_path(&mut tab, fs.path().join(""));

        let edit_location = tab.edit_location.as_ref().expect("the field is open again");
        assert_eq!(edit_location.stale, None);
        Ok(())
    }

    #[test]
    fn a_network_location_does_not_inherit_a_stale_list() -> io::Result<()> {
        let (fs, list) = two_folders()?;
        let mut tab = tab_editing(fs.path(), fs.path().join(""), Some(list));

        let uri = "smb://example/share/".to_owned();
        tab.update(
            Message::EditLocation(Some(Location::Network(uri.clone(), uri, None).into())),
            Modifiers::empty(),
        );

        let edit_location = tab.edit_location.as_ref().expect("the field is still open");
        assert_eq!(edit_location.stale, None);
        Ok(())
    }

    /// Inside a browsed share the field holds a URI, and what is typed into it
    /// never changes the mount path the lookup is keyed on, so no list would
    /// ever come to replace a stale one.
    #[test]
    fn a_network_share_with_a_mount_path_does_not_inherit_a_stale_list() -> io::Result<()> {
        let (fs, list) = two_folders()?;
        let mut tab = tab_editing(fs.path(), fs.path().join(""), None);
        let share = |uri: &str| {
            Location::Network(uri.to_owned(), uri.to_owned(), Some(fs.path().to_owned()))
        };
        let mut edit_location: super::EditLocation = share("smb://example/share/").into();
        edit_location.completions = Some(list);
        tab.edit_location = Some(edit_location);

        tab.update(
            Message::EditLocation(Some(share("smb://example/share/x").into())),
            Modifiers::empty(),
        );

        let edit_location = tab.edit_location.as_ref().expect("the field is still open");
        assert_eq!(edit_location.stale, None);
        Ok(())
    }

    /// The tab's location bar, laid out headless the way the app runs it: one
    /// widget cache carried from each build to the next, so widget state
    /// survives the tab changing in between.
    struct LocationBar {
        renderer: crate::ui::Renderer,
        cache: Option<crate::ui::iced_runtime::user_interface::Cache>,
        now: std::time::Instant,
    }

    impl LocationBar {
        const WINDOW: crate::ui::iced::Size = crate::ui::iced::Size::new(800.0, 600.0);

        /// Builds it and lets the dropdown unroll to its full height.
        fn settled(tab: &Tab) -> Self {
            use crate::ui::iced_core::{Event, mouse, window};

            tab.size_opt.set(Some(Self::WINDOW));
            let mut bar = Self {
                renderer: iced_texture_cache::testing::headless_tiny_skia(),
                cache: Some(Default::default()),
                now: std::time::Instant::now(),
            };
            for frame in 0.. {
                assert!(frame < 200, "the dropdown never settled");
                bar.now += std::time::Duration::from_millis(16);
                let (_, redraw) = bar.send(
                    tab,
                    Event::Window(window::Event::RedrawRequested(bar.now)),
                    mouse::Cursor::Unavailable,
                );
                if !redraw {
                    break;
                }
            }
            bar
        }

        /// Sends one event, returning what it published and whether another
        /// frame was asked for.
        fn send(
            &mut self,
            tab: &Tab,
            event: crate::ui::iced_core::Event,
            cursor: crate::ui::iced_core::mouse::Cursor,
        ) -> (Vec<Message>, bool) {
            use crate::ui::iced_core::{clipboard, window};
            use crate::ui::iced_runtime::user_interface::{State, UserInterface};

            let mut messages = Vec::new();
            let mut ui = UserInterface::build(
                tab.header_view(),
                Self::WINDOW,
                self.cache.take().expect("the cache is put back"),
                &mut self.renderer,
            );
            let (state, _) = ui.update(
                &[event],
                cursor,
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            self.cache = Some(ui.into_cache());
            let redraw = matches!(
                state,
                State::Updated {
                    redraw_request: window::RedrawRequest::NextFrame,
                    ..
                }
            );
            (messages, redraw)
        }

        /// The middle of the dropdown's first row: the shortest box below the
        /// field.
        fn first_row(&mut self, tab: &Tab) -> crate::ui::iced_core::mouse::Cursor {
            use crate::ui::iced_core::widget::{Id, Operation};
            use crate::ui::iced_core::{Point, Rectangle, mouse};
            use crate::ui::iced_runtime::user_interface::UserInterface;

            struct Containers(Vec<(Option<Id>, Rectangle)>);
            impl Operation for Containers {
                fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                    operate(self);
                }
                fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
                    self.0.push((id.cloned(), bounds));
                }
            }

            let mut containers = Containers(Vec::new());
            let mut ui = UserInterface::build(
                tab.header_view(),
                Self::WINDOW,
                self.cache.take().expect("the cache is put back"),
                &mut self.renderer,
            );
            ui.operate(&self.renderer, &mut containers);
            self.cache = Some(ui.into_cache());
            let field = containers
                .0
                .iter()
                .find(|(id, _)| id.as_ref() == Some(&tab.edit_location_id))
                .expect("the field is laid out")
                .1;
            let row = containers
                .0
                .iter()
                .map(|(_, bounds)| *bounds)
                .filter(|bounds| bounds.y >= field.y + field.height && bounds.height > 0.0)
                .min_by(|a, b| a.height.total_cmp(&b.height).then(a.y.total_cmp(&b.y)))
                .expect("the dropdown has rows");
            mouse::Cursor::Available(Point::new(
                row.x + row.width / 2.0,
                row.y + row.height / 2.0,
            ))
        }

        fn button(
            &mut self,
            tab: &Tab,
            cursor: crate::ui::iced_core::mouse::Cursor,
            pressed: bool,
        ) -> Vec<Message> {
            use crate::ui::iced_core::{Event, mouse};

            let button = mouse::Button::Left;
            let event = if pressed {
                mouse::Event::ButtonPressed(button)
            } else {
                mouse::Event::ButtonReleased(button)
            };
            self.send(tab, Event::Mouse(event), cursor).0
        }
    }

    /// Presses and releases the middle of the dropdown's first row once it
    /// has unrolled, returning what that published.
    fn press_first_dropdown_row(tab: &Tab) -> Vec<Message> {
        let mut bar = LocationBar::settled(tab);
        let row = bar.first_row(tab);
        let mut published = bar.button(tab, row, true);
        published.extend(bar.button(tab, row, false));
        published
    }

    /// The list arriving between the press and the release does not turn the
    /// press into a choice: the user pressed a row they could not choose, and
    /// the row now under the pointer is one they have not seen.
    #[test]
    fn a_press_on_a_stale_row_released_after_the_list_arrives_chooses_nothing() -> io::Result<()> {
        let (fs, list) = two_folders()?;
        let mut tab = tab_editing(fs.path(), fs.path().join(""), Some(list.clone()));
        tab.config.view = super::View::Grid;
        let typed = fs.path().join("b");
        type_path(&mut tab, typed.clone());

        let mut bar = LocationBar::settled(&tab);
        let row = bar.first_row(&tab);
        let mut published = bar.button(&tab, row, true);
        tab.update(
            Message::TabComplete(typed, vec![list[1].clone()]),
            Modifiers::empty(),
        );
        assert!(
            tab.edit_location
                .as_ref()
                .is_some_and(|x| x.completions.is_some()),
            "the list should have arrived"
        );
        published.extend(bar.button(&tab, row, false));

        assert!(
            !published.iter().any(|message| matches!(
                message,
                Message::EditLocationComplete(_) | Message::EditLocation(None)
            )),
            "{published:?}"
        );
        Ok(())
    }

    /// A press on a stale row neither closes the field nor reaches whatever
    /// lies under the dropdown: it is taken by the row, and chooses nothing.
    #[test]
    fn a_press_on_a_stale_row_is_swallowed() -> io::Result<()> {
        let (fs, list) = two_folders()?;
        let mut tab = tab_editing(fs.path(), fs.path().join(""), Some(list));
        tab.config.view = super::View::Grid;
        type_path(&mut tab, fs.path().join("b"));

        let published = press_first_dropdown_row(&tab);

        assert!(
            matches!(published.as_slice(), [Message::EditLocationStalePressed]),
            "{published:?}"
        );
        Ok(())
    }

    /// The same press on a looked-up row chooses it, so the press above did
    /// land on a row.
    #[test]
    fn a_press_on_a_looked_up_row_chooses_it() -> io::Result<()> {
        let (fs, list) = two_folders()?;
        let mut tab = tab_editing(fs.path(), fs.path().join(""), Some(list));
        tab.config.view = super::View::Grid;

        let published = press_first_dropdown_row(&tab);

        assert!(
            matches!(published.as_slice(), [Message::EditLocationComplete(0)]),
            "{published:?}"
        );
        Ok(())
    }

    #[test]
    fn a_press_on_a_stale_row_changes_nothing() -> io::Result<()> {
        let (fs, list) = two_folders()?;
        let mut tab = tab_editing(fs.path(), fs.path().join(""), Some(list.clone()));
        let typed = fs.path().join("b");
        type_path(&mut tab, typed.clone());
        let before = tab.location.clone();

        let commands = tab.update(Message::EditLocationStalePressed, Modifiers::empty());

        assert!(commands.is_empty(), "{commands:?}");
        assert_eq!(tab.location, before);
        let edit_location = tab.edit_location.as_ref().expect("the field is still open");
        assert_eq!(edit_location.location, Location::Path(typed));
        assert_eq!(edit_location.completions, None);
        assert_eq!(edit_location.stale, Some(list));
        assert_eq!(edit_location.selected, None);
        Ok(())
    }

    #[test]
    fn the_dropdown_shows_the_looked_up_list_over_a_stale_one() {
        let fresh = vec![("fresh".to_owned(), PathBuf::from("/fresh"))];
        let stale = vec![("stale".to_owned(), PathBuf::from("/stale"))];
        let mut edit_location: super::EditLocation = Location::Path(PathBuf::from("/")).into();

        edit_location.stale = Some(stale.clone());
        assert_eq!(edit_location.shown(), Some((stale.as_slice(), false)));

        edit_location.completions = Some(fresh.clone());
        assert_eq!(edit_location.shown(), Some((fresh.as_slice(), true)));
    }

    #[test]
    fn an_empty_list_shows_no_dropdown() {
        let stale = vec![("stale".to_owned(), PathBuf::from("/stale"))];
        let mut edit_location: super::EditLocation = Location::Path(PathBuf::from("/")).into();
        assert_eq!(
            edit_location.shown(),
            None,
            "nothing looked up, nothing stale"
        );

        edit_location.completions = Some(Vec::new());
        assert_eq!(edit_location.shown(), None);

        // An empty answer is the answer, even with an old list to hand.
        edit_location.stale = Some(stale);
        assert_eq!(edit_location.shown(), None);

        edit_location.completions = None;
        edit_location.stale = Some(Vec::new());
        assert_eq!(edit_location.shown(), None);
    }

    /// A trailing separator changes what is looked up -- a folder's children
    /// rather than its matching siblings -- so an answer for one form is
    /// foreign to the other.
    #[test]
    fn a_list_for_a_path_is_not_taken_for_it_with_a_trailing_separator() -> io::Result<()> {
        let (fs, list) = two_folders()?;
        let bare = fs.path().join("alpha");
        let with_sep = fs.path().join("alpha/");

        for (field, answered) in [(&with_sep, &bare), (&bare, &with_sep)] {
            let mut tab = tab_editing(fs.path(), field.clone(), None);
            tab.update(
                Message::TabComplete(answered.clone(), list.clone()),
                Modifiers::empty(),
            );
            let edit_location = tab.edit_location.as_ref().expect("the field is still open");
            assert_eq!(
                edit_location.completions, None,
                "the answer for {answered:?} was taken for {field:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn typing_a_separator_after_a_folder_lists_its_children() -> io::Result<()> {
        let (fs, _) = two_folders()?;
        fs::create_dir(fs.path().join("alpha").join("inner"))?;
        let bare = fs.path().join("alpha");
        let with_sep = fs.path().join("alpha/");
        let lookup = |path: &PathBuf| {
            super::tab_complete(path).map_err(|err| io::Error::other(err.to_string()))
        };
        let siblings = lookup(&bare)?;
        let children = lookup(&with_sep)?;
        assert_ne!(
            siblings, children,
            "the fixture should answer the two forms differently"
        );

        let mut tab = tab_editing(fs.path(), bare, Some(siblings.clone()));
        type_path(&mut tab, with_sep.clone());
        let edit_location = tab.edit_location.as_ref().expect("the field is still open");
        assert_eq!(edit_location.completions, None);
        assert_eq!(edit_location.stale, Some(siblings));

        tab.update(
            Message::TabComplete(with_sep, children.clone()),
            Modifiers::empty(),
        );
        let edit_location = tab.edit_location.as_ref().expect("the field is still open");
        assert_eq!(edit_location.completions, Some(children));
        assert_eq!(edit_location.stale, None);
        Ok(())
    }

    /// The runtime keeps a subscription running for as long as its hash is
    /// unchanged, so the lookup restarts only if the two forms hash apart.
    #[test]
    fn a_trailing_separator_restarts_the_completion_lookup() {
        use crate::ui::iced::advanced::subscription::{Hasher, into_recipes};
        use std::hash::Hasher as _;

        let id = |path: &str| -> Vec<u64> {
            into_recipes(super::tab_complete_subscription(std::path::Path::new(path)))
                .iter()
                .map(|recipe| {
                    let mut hasher = Hasher::default();
                    recipe.hash(&mut hasher);
                    hasher.finish()
                })
                .collect()
        };

        assert_eq!(
            id("/home/user"),
            id("/home/user"),
            "the same text is the same lookup"
        );
        assert_ne!(id("/home/user"), id("/home/user/"));
    }

    /// The same address submitted twice: the first answer settles only the
    /// first request.
    #[test]
    fn a_network_answer_settles_only_the_request_it_belongs_to() -> io::Result<()> {
        use crate::tab::Command;

        let fs = empty_fs()?;
        let mut tab = Tab::new(
            Location::Path(fs.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );

        let uri = "smb://example/share/".to_owned();
        let ask = |tab: &mut Tab| -> u64 {
            tab.edit_location = Some(Location::Network(uri.clone(), uri.clone(), None).into());
            tab.update(Message::EditLocationSubmit, Modifiers::empty())
                .iter()
                .find_map(|command| match command {
                    Command::ResolveNetwork(request, _) => Some(*request),
                    _ => None,
                })
                .expect("should ask for resolution")
        };

        let first = ask(&mut tab);
        let second = ask(&mut tab);
        assert_ne!(first, second, "each request gets its own number");

        // The first answer arrives while the second is outstanding
        tab.update(
            Message::NetworkResolved(first, uri.clone(), None),
            Modifiers::empty(),
        );
        assert_eq!(
            tab.resolving_network,
            Some(second),
            "an answer to a withdrawn request settled the one still waiting"
        );
        Ok(())
    }

    /// The chooser's file type filter has to meet search results as they
    /// arrive. Applied afterwards, results it rejects would already have taken
    /// the places of ones it admits, up to the cap, and pushed up the cutoff
    /// that stops the search sending anything older.
    #[test]
    fn a_filter_keeps_rejected_search_results_out_of_the_listing() -> io::Result<()> {
        use crate::tab::{SearchContext, SearchItem};
        use std::sync::{Arc, RwLock, atomic};

        let fs = empty_fs()?;
        for name in ["one.txt", "two.txt", "keep.png"] {
            fs::write(fs.path().join(name), b"x")?;
        }
        let mut tab = Tab::new(
            Location::Path(fs.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(Vec::new());
        // One result is all the listing will hold, as a capped search is
        tab.config.max_search_results = std::num::NonZeroU16::new(1).expect("nonzero");
        tab.item_filter = Some(Box::new(|item: &super::Item| item.name.ends_with(".png")));

        let (results_tx, results_rx) = tokio::sync::mpsc::channel(8);
        for name in ["one.txt", "two.txt", "keep.png"] {
            let path = fs.path().join(name);
            let metadata = fs::metadata(&path)?;
            results_tx
                .try_send(crate::tab::item_from_search_item(
                    SearchItem::Path(path, name.to_string(), metadata),
                    IconSizes::default(),
                ))
                .expect("the channel holds them all");
        }
        drop(results_tx);
        tab.search_context = Some(SearchContext {
            results_rx,
            ready: Arc::new(atomic::AtomicBool::new(true)),
            last_modified_opt: Arc::new(RwLock::new(None)),
        });

        tab.update(Message::SearchReady(true), Modifiers::empty());

        let names: Vec<&str> = tab
            .items_opt
            .as_deref()
            .expect("the listing exists")
            .iter()
            .map(|item| item.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["keep.png"],
            "the rejected results must not have taken the one place there was"
        );
        Ok(())
    }

    /// Enter in the search field opens the selection, so a search keeps its
    /// top result selected: a newer result takes the top as it arrives and
    /// the selection goes with it, until the user selects something of their
    /// own.
    #[test]
    fn a_search_selects_its_top_result_until_the_user_picks_one() -> io::Result<()> {
        use crate::tab::{SearchContext, SearchItem, SearchLocation, SearchOptions};
        use std::sync::{Arc, RwLock, atomic};
        use std::time::{Duration, SystemTime};

        let fs = empty_fs()?;
        let now = SystemTime::now();
        for (name, age) in [
            ("old.txt", 30),
            ("mid.txt", 20),
            ("new.txt", 10),
            ("newest.txt", 0),
        ] {
            let path = fs.path().join(name);
            fs::write(&path, b"x")?;
            fs::File::options()
                .write(true)
                .open(&path)?
                .set_modified(now - Duration::from_secs(age))?;
        }
        let mut tab = Tab::new(
            Location::Path(fs.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.change_location(
            &Location::Search(
                SearchLocation::Path(fs.path().to_owned()),
                "txt".to_string(),
                SearchOptions {
                    show_hidden: false,
                    recursive: false,
                },
                std::time::Instant::now(),
            ),
            None,
        );
        tab.set_items(Vec::new());

        let (results_tx, results_rx) = tokio::sync::mpsc::channel(8);
        let ready = Arc::new(atomic::AtomicBool::new(false));
        tab.search_context = Some(SearchContext {
            results_rx,
            ready: ready.clone(),
            last_modified_opt: Arc::new(RwLock::new(None)),
        });
        let send = |name: &str| -> io::Result<()> {
            let path = fs.path().join(name);
            let metadata = fs::metadata(&path)?;
            results_tx
                .try_send(crate::tab::item_from_search_item(
                    SearchItem::Path(path, name.to_string(), metadata),
                    IconSizes::default(),
                ))
                .expect("the channel has room");
            Ok(())
        };
        let deliver = |tab: &mut Tab| {
            ready.store(true, atomic::Ordering::SeqCst);
            tab.update(Message::SearchReady(false), Modifiers::empty());
        };
        let selected = |tab: &Tab| -> Vec<String> {
            tab.items_opt
                .iter()
                .flatten()
                .filter(|item| tab.selection.is_selected(item))
                .map(|item| item.name.clone())
                .collect()
        };

        send("mid.txt")?;
        send("old.txt")?;
        deliver(&mut tab);
        assert_eq!(selected(&tab), ["mid.txt"], "the top result is selected");

        send("new.txt")?;
        deliver(&mut tab);
        assert_eq!(
            selected(&tab),
            ["new.txt"],
            "the selection follows a newer result to the top"
        );

        // Clicking the result already selected for the search changes
        // nothing to see, but makes the choice the user's
        let top = tab
            .items_opt
            .iter()
            .flatten()
            .position(|item| item.name == "new.txt");
        tab.update(Message::Click(top), Modifiers::empty());
        send("newest.txt")?;
        deliver(&mut tab);
        assert_eq!(
            selected(&tab),
            ["new.txt"],
            "a result arriving took away the result the user clicked"
        );
        Ok(())
    }

    /// With nothing typed, a search of a folder with the eye shut shows the
    /// whole folder, sorted as the folder is, its top item selected so that
    /// Enter opens it. With the eye open it searches the tree, which an empty
    /// term says nothing about, and shows nothing.
    #[test]
    fn an_empty_search_of_a_folder_shows_the_folder() -> io::Result<()> {
        use crate::tab::{HeadingOptions, Item, SearchLocation, SearchOptions};

        let fs = empty_fs()?;
        for name in ["b.txt", "a.txt", "c.txt"] {
            fs::write(fs.path().join(name), b"x")?;
        }
        let search = |term: &str, recursive: bool| {
            Location::Search(
                SearchLocation::Path(fs.path().to_owned()),
                term.to_string(),
                SearchOptions {
                    show_hidden: false,
                    recursive,
                },
                std::time::Instant::now(),
            )
            .normalize()
        };
        let names = |items: &[Item]| -> Vec<String> {
            let mut names: Vec<_> = items.iter().map(|item| item.name.clone()).collect();
            names.sort();
            names
        };
        let sizes = IconSizes::default();

        let (_, items) = search("", false).scan(sizes);
        assert_eq!(names(&items), ["a.txt", "b.txt", "c.txt"]);
        let (_, items) = search("", true).scan(sizes);
        assert!(
            items.is_empty(),
            "an empty search of the tree listed something"
        );
        let (_, items) = search("a", false).scan(sizes);
        assert!(
            items.is_empty(),
            "a search's results come as it runs, not from a scan"
        );

        let mut tab = Tab::new(
            Location::Path(fs.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.sort_name = HeadingOptions::Name;
        tab.sort_direction = true;
        let location = search("", false);
        tab.change_location(&location, None);
        assert_eq!(
            location.sort_key(),
            Location::Path(fs.path().to_owned()).normalize().sort_key(),
            "the empty search keeps its sort with the folder's"
        );
        assert_eq!(tab.sort_options().0, HeadingOptions::Name);

        let own = tab.search_selection_is_own();
        tab.set_items(location.scan(sizes).1);
        tab.follow_search_top(own);
        let selected: Vec<_> = tab
            .items_opt
            .iter()
            .flatten()
            .filter(|item| tab.selection.is_selected(item))
            .map(|item| item.name.as_str())
            .collect();
        assert_eq!(selected, ["a.txt"], "the folder's first item is selected");

        tab.change_location(&search("a", false), None);
        assert_eq!(
            tab.sort_options().0,
            HeadingOptions::Modified,
            "results are newest first once there is a term"
        );
        Ok(())
    }

    /// An empty search lists its folder in full, which the cap on search
    /// results does not apply to. A search that has run with it must leave
    /// the listing whole whenever it finishes.
    #[test]
    fn a_finished_search_leaves_an_empty_search_listing_whole() -> io::Result<()> {
        use crate::tab::{SearchContext, SearchLocation, SearchOptions};
        use std::sync::{Arc, RwLock, atomic};

        let fs = empty_fs()?;
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(fs.path().join(name), b"x")?;
        }
        let mut tab = Tab::new(
            Location::Path(fs.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.config.max_search_results = std::num::NonZeroU16::new(1).expect("nonzero");
        let location = Location::Search(
            SearchLocation::Path(fs.path().to_owned()),
            String::new(),
            SearchOptions {
                show_hidden: false,
                recursive: false,
            },
            std::time::Instant::now(),
        )
        .normalize();
        tab.change_location(&location, None);
        tab.set_items(location.scan(IconSizes::default()).1);

        let (_results_tx, results_rx) = tokio::sync::mpsc::channel(1);
        tab.search_context = Some(SearchContext {
            results_rx,
            ready: Arc::new(atomic::AtomicBool::new(true)),
            last_modified_opt: Arc::new(RwLock::new(None)),
        });
        tab.update(Message::SearchReady(true), Modifiers::empty());

        assert_eq!(
            tab.items_opt.as_ref().map(Vec::len),
            Some(3),
            "the search cut the folder listing down to its cap"
        );
        Ok(())
    }

    /// A folder chooser cannot select a file, and a search must not select
    /// one for it either: its Open would be refused with folders to offer.
    #[test]
    fn a_folder_chooser_search_selects_the_top_folder() -> io::Result<()> {
        use crate::dialog::DialogKind;
        use crate::tab::{Mode, SearchContext, SearchItem, SearchLocation, SearchOptions};
        use std::sync::{Arc, RwLock, atomic};
        use std::time::{Duration, SystemTime};

        let fs = empty_fs()?;
        let now = SystemTime::now();
        let dir = fs.path().join("match-dir");
        fs::create_dir(&dir)?;
        fs::File::open(&dir)?.set_modified(now - Duration::from_secs(60))?;
        let file = fs.path().join("match-file");
        fs::write(&file, b"x")?;
        fs::File::options()
            .write(true)
            .open(&file)?
            .set_modified(now)?;

        let mut tab = Tab::new(
            Location::Path(fs.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.mode = Mode::Dialog(DialogKind::OpenFolder);
        tab.change_location(
            &Location::Search(
                SearchLocation::Path(fs.path().to_owned()),
                "match".to_string(),
                SearchOptions {
                    show_hidden: false,
                    recursive: false,
                },
                std::time::Instant::now(),
            ),
            None,
        );
        tab.set_items(Vec::new());

        let (results_tx, results_rx) = tokio::sync::mpsc::channel(8);
        for path in [&file, &dir] {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            results_tx
                .try_send(crate::tab::item_from_search_item(
                    SearchItem::Path(path.clone(), name, fs::metadata(path)?),
                    IconSizes::default(),
                ))
                .expect("the channel has room");
        }
        drop(results_tx);
        tab.search_context = Some(SearchContext {
            results_rx,
            ready: Arc::new(atomic::AtomicBool::new(true)),
            last_modified_opt: Arc::new(RwLock::new(None)),
        });
        tab.update(Message::SearchReady(true), Modifiers::empty());

        let selected: Vec<_> = tab
            .items_opt
            .iter()
            .flatten()
            .filter(|item| tab.selection.is_selected(item))
            .map(|item| item.name.as_str())
            .collect();
        assert_eq!(
            selected,
            ["match-dir"],
            "the newer file on top is not the chooser's to select"
        );
        Ok(())
    }

    /// Newer files can push the folder a folder chooser's search selected
    /// past the cap on results, leaving none to select. That is not the user
    /// choosing nothing: a folder arriving later is selected all the same.
    #[test]
    fn a_folder_chooser_search_selects_again_after_its_folder_is_pushed_out() -> io::Result<()> {
        use crate::dialog::DialogKind;
        use crate::tab::{Mode, SearchContext, SearchItem, SearchLocation, SearchOptions};
        use std::sync::{Arc, RwLock, atomic};
        use std::time::{Duration, SystemTime};

        let fs = empty_fs()?;
        let now = SystemTime::now();
        for (name, age, is_dir) in [
            ("old-dir", 30, true),
            ("file", 20, false),
            ("new-dir", 10, true),
        ] {
            let path = fs.path().join(name);
            if is_dir {
                fs::create_dir(&path)?;
                fs::File::open(&path)?.set_modified(now - Duration::from_secs(age))?;
            } else {
                fs::write(&path, b"x")?;
                fs::File::options()
                    .write(true)
                    .open(&path)?
                    .set_modified(now - Duration::from_secs(age))?;
            }
        }
        let mut tab = Tab::new(
            Location::Path(fs.path().to_owned()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.mode = Mode::Dialog(DialogKind::OpenFolder);
        tab.config.max_search_results = std::num::NonZeroU16::new(1).expect("nonzero");
        tab.change_location(
            &Location::Search(
                SearchLocation::Path(fs.path().to_owned()),
                "e".to_string(),
                SearchOptions {
                    show_hidden: false,
                    recursive: false,
                },
                std::time::Instant::now(),
            ),
            None,
        );
        tab.set_items(Vec::new());

        let (results_tx, results_rx) = tokio::sync::mpsc::channel(8);
        let ready = Arc::new(atomic::AtomicBool::new(false));
        tab.search_context = Some(SearchContext {
            results_rx,
            ready: ready.clone(),
            last_modified_opt: Arc::new(RwLock::new(None)),
        });
        let send = |name: &str| -> io::Result<()> {
            let path = fs.path().join(name);
            let metadata = fs::metadata(&path)?;
            results_tx
                .try_send(crate::tab::item_from_search_item(
                    SearchItem::Path(path, name.to_string(), metadata),
                    IconSizes::default(),
                ))
                .expect("the channel has room");
            Ok(())
        };
        let deliver = |tab: &mut Tab| {
            ready.store(true, atomic::Ordering::SeqCst);
            tab.update(Message::SearchReady(false), Modifiers::empty());
        };
        let selected = |tab: &Tab| -> Vec<String> {
            tab.items_opt
                .iter()
                .flatten()
                .filter(|item| tab.selection.is_selected(item))
                .map(|item| item.name.clone())
                .collect()
        };

        send("old-dir")?;
        deliver(&mut tab);
        assert_eq!(selected(&tab), ["old-dir"]);

        send("file")?;
        deliver(&mut tab);
        assert_eq!(
            selected(&tab),
            Vec::<String>::new(),
            "the folder was pushed out"
        );

        send("new-dir")?;
        deliver(&mut tab);
        assert_eq!(
            selected(&tab),
            ["new-dir"],
            "losing its folder to the cap ended the search's selecting"
        );
        Ok(())
    }

    /// A Wayland drag reports a surface-local position, measured from the top
    /// of what is on screen, while item rectangles are laid out in content
    /// coordinates. After scrolling the two differ by the scroll offset, and a
    /// drop tested without it lands on whatever sits the same distance down the
    /// listing — a folder that may not even be on screen.
    #[test]
    fn a_drop_after_scrolling_finds_the_folder_under_the_pointer() -> io::Result<()> {
        use crate::ui::iced::{Point, Rectangle, Size};
        use crate::ui::widget::scrollable::AbsoluteOffset;

        let fs = empty_fs()?;
        fs::create_dir(fs.path().join("first"))?;
        fs::create_dir(fs.path().join("second"))?;
        let location = Location::Path(fs.path().to_owned());
        let (_parent, items) = location.scan(IconSizes::default());
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(items);

        // Two rows of 100, in a view that shows one of them at a time
        let rows = tab.items_opt.as_deref().expect("tab should be populated");
        assert_eq!(rows.len(), 2, "the fixture has two folders");
        for (i, item) in rows.iter().enumerate() {
            item.rect_opt.set(Some(Rectangle::new(
                Point::new(0.0, i as f32 * 100.0),
                Size::new(200.0, 100.0),
            )));
        }
        tab.content_height_opt.set(Some(200.0));
        tab.item_view_size_opt.set(Some(Size::new(200.0, 100.0)));

        let drag_to = |tab: &mut Tab, y: f32| {
            tab.update(
                Message::Dnd(crate::mouse_area::DndDrag {
                    position: Some(Point::new(10.0, y)),
                    dropped: false,
                    ended: false,
                }),
                Modifiers::empty(),
            );
            tab.dnd_target
        };

        assert_eq!(
            drag_to(&mut tab, 50.0),
            Some(0),
            "unscrolled, halfway down the view is the first row"
        );

        tab.scroll_opt = Some(AbsoluteOffset { x: 0.0, y: 100.0 });
        assert_eq!(
            drag_to(&mut tab, 50.0),
            Some(1),
            "scrolled a row down, the same place on screen is the second row"
        );
        Ok(())
    }

    #[test]
    fn a_drag_near_an_edge_scrolls_faster_the_nearer_it_is() {
        use super::drag_edge_scroll;

        // A list 400 high: zones 0 to 48 and 352 to 400
        assert_eq!(drag_edge_scroll(0.0, 400.0), Some(-24.0));
        assert_eq!(drag_edge_scroll(24.0, 400.0), Some(-12.0));
        assert_eq!(drag_edge_scroll(48.0, 400.0), None);
        assert_eq!(drag_edge_scroll(200.0, 400.0), None);
        assert_eq!(drag_edge_scroll(352.0, 400.0), None);
        assert_eq!(drag_edge_scroll(376.0, 400.0), Some(12.0));
        assert_eq!(drag_edge_scroll(400.0, 400.0), Some(24.0));
        // Off the list
        assert_eq!(drag_edge_scroll(-1.0, 400.0), None);
        assert_eq!(drag_edge_scroll(401.0, 400.0), None);
    }

    /// A tab showing two folders, `first` and `second`, each a row 100 high,
    /// in a view 400 high.
    fn drag_tab() -> io::Result<(TempDir, Tab)> {
        use crate::ui::iced::{Point, Rectangle, Size};

        let fs = empty_fs()?;
        fs::create_dir(fs.path().join("first"))?;
        fs::create_dir(fs.path().join("second"))?;
        let location = Location::Path(fs.path().to_owned());
        let (_parent, items) = location.scan(IconSizes::default());
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(items);
        for (i, item) in tab
            .items_opt
            .as_deref()
            .expect("populated")
            .iter()
            .enumerate()
        {
            item.rect_opt.set(Some(Rectangle::new(
                Point::new(0.0, 100.0 + i as f32 * 100.0),
                Size::new(200.0, 100.0),
            )));
        }
        tab.content_height_opt.set(Some(400.0));
        tab.item_view_size_opt.set(Some(Size::new(200.0, 400.0)));
        Ok((fs, tab))
    }

    fn drag_at(tab: &mut Tab, y: Option<f32>, ended: bool) -> Vec<super::Command> {
        tab.update(
            Message::Dnd(crate::mouse_area::DndDrag {
                position: y.map(|y| crate::ui::iced::Point::new(10.0, y)),
                dropped: false,
                ended,
            }),
            Modifiers::empty(),
        )
    }

    fn goes_to(commands: &[super::Command]) -> Option<PathBuf> {
        commands.iter().find_map(|command| match command {
            super::Command::ChangeLocation(_, location, _) => location.path_opt().cloned(),
            _ => None,
        })
    }

    #[test]
    fn a_drag_held_over_a_folder_opens_it() -> io::Result<()> {
        let (fs, mut tab) = drag_tab()?;
        let first = fs.path().join("first");

        // Over `first` (100 to 200): its timer starts
        drag_at(&mut tab, Some(150.0), false);
        let generation = tab.drag_hover_generation;

        // A timer from before is stale and opens nothing
        let stale = tab.update_resolved(
            Message::DragHoverElapsed(generation - 1),
            Modifiers::empty(),
        );
        assert_eq!(goes_to(&stale), None);

        // Still over it when the timer runs out: it opens
        let opened = tab.update_resolved(Message::DragHoverElapsed(generation), Modifiers::empty());
        assert_eq!(goes_to(&opened), Some(first));
        Ok(())
    }

    #[test]
    fn a_hold_follows_its_folder_through_a_refresh() -> io::Result<()> {
        let (fs, mut tab) = drag_tab()?;
        let first = fs.path().join("first");

        // Held over `first`, which is at 0
        drag_at(&mut tab, Some(150.0), false);
        let generation = tab.drag_hover_generation;

        // Refreshed with `second` now at 0: the hold is still on `first`
        let mut items = tab.items_opt.take().expect("populated");
        items.reverse();
        tab.set_items(items);
        let opened = tab.update_resolved(Message::DragHoverElapsed(generation), Modifiers::empty());
        assert_eq!(goes_to(&opened), Some(first.clone()));
        Ok(())
    }

    #[test]
    fn a_hold_whose_folder_moves_from_under_the_pointer_starts_again() -> io::Result<()> {
        use crate::ui::iced::{Point, Rectangle, Size};

        let (fs, mut tab) = drag_tab()?;
        let second = fs.path().join("second");

        // Held over `first`, in the row at 100 to 200
        drag_at(&mut tab, Some(150.0), false);
        let generation = tab.drag_hover_generation;

        // A refresh sorts `second` into that row and `first` below it, as the
        // next layout draws them, with the pointer not moving
        for item in tab.items_opt.as_deref().expect("populated") {
            let row = if item.path_opt() == Some(&second) {
                1.0
            } else {
                2.0
            };
            item.rect_opt.set(Some(Rectangle::new(
                Point::new(0.0, row * 100.0),
                Size::new(200.0, 100.0),
            )));
        }

        // `first` has moved away: nothing opens, and the hold starts again on
        // `second`, which is under the pointer now
        let opened = tab.update_resolved(Message::DragHoverElapsed(generation), Modifiers::empty());
        assert_eq!(goes_to(&opened), None);
        let restarted = tab.drag_hover_generation;
        assert_ne!(restarted, generation);
        let opened = tab.update_resolved(Message::DragHoverElapsed(restarted), Modifiers::empty());
        assert_eq!(goes_to(&opened), Some(second));
        Ok(())
    }

    #[test]
    fn a_hold_over_a_folder_that_goes_opens_nothing() -> io::Result<()> {
        let (fs, mut tab) = drag_tab()?;
        let first = fs.path().join("first");

        drag_at(&mut tab, Some(150.0), false);
        let generation = tab.drag_hover_generation;

        // Refreshed without `first`: `second` takes its place, and is not
        // what was held over
        let items: Vec<_> = tab
            .items_opt
            .take()
            .expect("populated")
            .into_iter()
            .filter(|item| item.path_opt() != Some(&first))
            .collect();
        tab.set_items(items);
        let opened = tab.update(Message::DragHoverElapsed(generation), Modifiers::empty());
        assert_eq!(goes_to(&opened), None);
        Ok(())
    }

    #[test]
    fn a_drag_that_moves_on_or_ends_opens_nothing() -> io::Result<()> {
        let (_fs, mut tab) = drag_tab()?;

        drag_at(&mut tab, Some(150.0), false);
        let over_first = tab.drag_hover_generation;
        // On to `second`: the first timer is stale
        drag_at(&mut tab, Some(250.0), false);
        let opened = tab.update(Message::DragHoverElapsed(over_first), Modifiers::empty());
        assert_eq!(goes_to(&opened), None);

        // Ended: the second timer is stale too
        let over_second = tab.drag_hover_generation;
        drag_at(&mut tab, None, true);
        let opened = tab.update(Message::DragHoverElapsed(over_second), Modifiers::empty());
        assert_eq!(goes_to(&opened), None);
        Ok(())
    }

    #[test]
    fn a_drag_near_the_list_edges_scrolls_and_stops() -> io::Result<()> {
        let (_fs, mut tab) = drag_tab()?;
        let scrolls = |commands: &[super::Command]| -> Vec<Option<f32>> {
            commands
                .iter()
                .filter_map(|command| match command {
                    super::Command::AutoScroll(speed) => Some(*speed),
                    _ => None,
                })
                .collect()
        };

        assert_eq!(
            scrolls(&drag_at(&mut tab, Some(24.0), false)),
            [Some(-12.0)]
        );
        // The same speed again says nothing new
        assert!(scrolls(&drag_at(&mut tab, Some(24.0), false)).is_empty());
        assert_eq!(scrolls(&drag_at(&mut tab, Some(200.0), false)), [None]);
        assert_eq!(
            scrolls(&drag_at(&mut tab, Some(376.0), false)),
            [Some(12.0)]
        );
        // Leaving the list stops it
        assert_eq!(scrolls(&drag_at(&mut tab, None, false)), [None]);
        Ok(())
    }

    #[test]
    fn a_drag_held_over_a_crumb_opens_that_folder() -> io::Result<()> {
        let (fs, tab) = drag_tab()?;
        drop(tab);
        let first = fs.path().join("first");
        let mut tab = Tab::new(
            Location::Path(first.clone()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        // Crumb 0 is `first` itself, 1 its parent
        let parent = fs.path().to_owned();
        let hold = |tab: &mut Tab, index: usize, position: bool| {
            tab.update_resolved(
                Message::DndAncestor(
                    index,
                    PathBuf::new(),
                    crate::mouse_area::DndDrag {
                        position: position.then(|| crate::ui::iced::Point::new(1.0, 1.0)),
                        dropped: false,
                        ended: false,
                    },
                ),
                Modifiers::empty(),
            );
            tab.drag_hover_generation
        };

        // Its own crumb never opens
        let own = hold(&mut tab, 0, true);
        let opened = tab.update_resolved(Message::DragHoverElapsed(own), Modifiers::empty());
        assert_eq!(goes_to(&opened), None);

        // The parent's does
        let generation = hold(&mut tab, 1, true);
        let opened = tab.update_resolved(Message::DragHoverElapsed(generation), Modifiers::empty());
        assert_eq!(goes_to(&opened), Some(parent));

        // Left before the timer runs out, it does not
        let generation = hold(&mut tab, 1, true);
        hold(&mut tab, 1, false);
        let opened = tab.update_resolved(Message::DragHoverElapsed(generation), Modifiers::empty());
        assert_eq!(goes_to(&opened), None);
        Ok(())
    }

    /// A double click is two presses and two releases. In single-click mode
    /// the first release opens the item and nothing after it may open it
    /// again; in double-click mode only the double click opens it.
    #[test]
    fn a_click_sequence_opens_an_item_exactly_once() -> io::Result<()> {
        let opens = |single_click: bool| -> io::Result<usize> {
            let fs = empty_fs()?;
            fs::write(fs.path().join("a.txt"), b"x")?;
            let location = Location::Path(fs.path().to_owned());
            let (_parent, items) = location.scan(IconSizes::default());
            let mut tab = Tab::new(
                location,
                TabConfig::default(),
                ThumbCfg::default(),
                None,
                std::borrow::Cow::Borrowed("Undefined"),
                None,
            );
            tab.set_items(items);
            tab.config.single_click = single_click;
            let file = tab
                .items_opt
                .as_deref()
                .expect("tab should be populated")
                .iter()
                .position(|item| !item.metadata.is_dir())
                .expect("the fixture has files");

            let mut commands = Vec::new();
            for message in [
                Message::Click(Some(file)),
                Message::ClickRelease(Some(file)),
                Message::DoubleClick(Some(file)),
                Message::ClickRelease(Some(file)),
            ] {
                commands.extend(tab.update(message, Modifiers::empty()));
            }
            Ok(commands
                .iter()
                .filter(|command| matches!(command, super::Command::OpenFile(_)))
                .count())
        };

        assert_eq!(
            opens(true)?,
            1,
            "single-click mode opened it more than once"
        );
        assert_eq!(
            opens(false)?,
            1,
            "double-click mode opened it more than once"
        );
        Ok(())
    }

    /// A tab in single-click mode listing `names`, files all, and where each
    /// is in its items.
    fn single_click_tab(fs: &TempDir, names: &[&str]) -> io::Result<(Tab, Vec<usize>)> {
        for name in names {
            fs::write(fs.path().join(name), b"x")?;
        }
        let location = Location::Path(fs.path().to_owned());
        let (_parent, items) = location.scan(IconSizes::default());
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(items);
        tab.config.single_click = true;
        let at = |tab: &Tab, name: &str| {
            tab.items_opt
                .as_deref()
                .and_then(|items| items.iter().position(|item| item.name == name))
        };
        let positions = names.iter().filter_map(|name| at(&tab, name)).collect();
        Ok((tab, positions))
    }

    /// What `commands` open.
    fn opened(commands: &[super::Command]) -> Vec<PathBuf> {
        commands
            .iter()
            .filter_map(|command| match command {
                super::Command::OpenFile(paths) => Some(paths.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Shift chooses a range and Ctrl toggles: in single-click mode neither
    /// opens the item they end on.
    #[test]
    fn selecting_with_modifiers_opens_nothing() -> io::Result<()> {
        let fs = empty_fs()?;
        let (mut tab, at) = single_click_tab(&fs, &["a.txt", "b.txt"])?;
        for modifiers in [Modifiers::SHIFT, Modifiers::CTRL] {
            let mut commands = tab.update(Message::Click(Some(at[1])), modifiers);
            commands.extend(tab.update(Message::ClickRelease(Some(at[1])), modifiers));
            assert_eq!(opened(&commands), Vec::<PathBuf>::new(), "{modifiers:?}");
        }
        Ok(())
    }

    /// A double click whose release never came (the pointer was taken away)
    /// does not keep the next, separate click from opening its item.
    #[test]
    fn a_double_click_cut_short_does_not_stop_the_next_click() -> io::Result<()> {
        let fs = empty_fs()?;
        let (mut tab, at) = single_click_tab(&fs, &["a.txt", "b.txt"])?;
        tab.update(Message::Click(Some(at[0])), Modifiers::empty());
        tab.update(Message::ClickRelease(Some(at[0])), Modifiers::empty());
        tab.update(Message::DoubleClick(Some(at[0])), Modifiers::empty());

        let mut commands = tab.update(Message::Click(Some(at[1])), Modifiers::empty());
        commands.extend(tab.update(Message::ClickRelease(Some(at[1])), Modifiers::empty()));
        assert_eq!(opened(&commands), [fs.path().join("b.txt")]);
        Ok(())
    }

    /// The listing refreshed between a press and its release, putting another
    /// item where the pressed one was: the release clicks neither.
    #[test]
    fn a_refresh_does_not_pass_a_click_to_another_item() -> io::Result<()> {
        let fs = empty_fs()?;
        let (mut tab, at) = single_click_tab(&fs, &["a.txt", "b.txt"])?;
        let pressed = at[0];
        tab.update(Message::Click(Some(pressed)), Modifiers::empty());

        fs::remove_file(fs.path().join("a.txt"))?;
        let (_parent, items) = tab.location.scan(IconSizes::default());
        tab.set_items(items);
        assert_eq!(
            tab.items_opt
                .as_deref()
                .and_then(|items| items.get(pressed))
                .map(|item| item.name.as_str()),
            Some("b.txt"),
            "another item is now where the pressed one was"
        );

        let commands = tab.update(Message::ClickRelease(Some(pressed)), Modifiers::empty());
        assert_eq!(opened(&commands), Vec::<PathBuf>::new());
        Ok(())
    }

    /// A Ctrl rectangle toggles what it crosses, and nothing the rectangle
    /// before it crossed.
    #[test]
    fn a_ctrl_rectangle_toggles_only_what_it_crosses() -> io::Result<()> {
        use crate::ui::iced::{Point, Rectangle, Size};

        let fs = empty_fs()?;
        let (mut tab, at) = single_click_tab(&fs, &["a.txt", "b.txt"])?;
        let rect = |y: f32| Rectangle::new(Point::new(0.0, y), Size::new(100.0, 10.0));
        for (&i, y) in at.iter().zip([0.0, 20.0]) {
            tab.items_opt.as_ref().unwrap()[i]
                .rect_opt
                .set(Some(rect(y)));
        }
        let selected = |tab: &Tab| -> Vec<bool> {
            at.iter()
                .map(|&i| {
                    tab.selection
                        .is_selected(&tab.items_opt.as_ref().unwrap()[i])
                })
                .collect()
        };

        // Over a alone
        tab.update(Message::Click(None), Modifiers::empty());
        tab.update(Message::Drag(Some(rect(0.0))), Modifiers::empty());
        tab.update(Message::DragEnd, Modifiers::empty());
        assert_eq!(selected(&tab), [true, false]);

        // Then with Ctrl over b alone
        tab.update(Message::Click(None), Modifiers::CTRL);
        tab.update(Message::Drag(Some(rect(20.0))), Modifiers::CTRL);
        tab.update(Message::DragEnd, Modifiers::CTRL);
        assert_eq!(
            selected(&tab),
            [true, true],
            "a was toggled by a rectangle not over it"
        );
        Ok(())
    }

    #[test]
    fn the_history_pill_follows_the_history() -> io::Result<()> {
        use crate::ui::iced_core::{Event, Point, clipboard, mouse};
        use crate::ui::iced_runtime::user_interface::{Cache, UserInterface};

        // What a click on the middle of back (x 2 to 32) and of forward (x 34
        // to 64) sends, in a header 32 high
        let clicks = |tab: &Tab| {
            [17.0, 49.0].map(|x| {
                let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
                let mut ui = UserInterface::build(
                    tab.header_view(),
                    crate::ui::iced::Size::new(800.0, 32.0),
                    Cache::default(),
                    &mut renderer,
                );
                let mut messages = Vec::new();
                let _ = ui.update(
                    &[
                        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                    ],
                    mouse::Cursor::Available(Point::new(x, 16.0)),
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
                messages
            })
        };

        let (_fs, mut tab, _dirs) = tab_history()?;
        let [back, forward] = clicks(&tab);
        assert!(matches!(back.as_slice(), [Message::GoPrevious]), "{back:?}");
        assert!(forward.is_empty(), "nothing to go forward to: {forward:?}");

        tab.update_resolved(Message::GoPrevious, Modifiers::empty());
        let [back, forward] = clicks(&tab);
        assert!(matches!(back.as_slice(), [Message::GoPrevious]), "{back:?}");
        assert!(
            matches!(forward.as_slice(), [Message::GoNext]),
            "{forward:?}"
        );

        // At the start of the history there is nothing to go back to
        while tab.history_i > 0 {
            tab.update_resolved(Message::GoPrevious, Modifiers::empty());
        }
        let [back, forward] = clicks(&tab);
        assert!(back.is_empty(), "nothing to go back to: {back:?}");
        assert!(
            matches!(forward.as_slice(), [Message::GoNext]),
            "{forward:?}"
        );
        Ok(())
    }

    #[test]
    fn tab_gonext_moves_forward_in_history() -> io::Result<()> {
        let (fs, mut tab, dirs) = tab_history()?;
        let path = fs.path();

        // Rewind to the start
        for _ in 0..dirs.len() {
            debug!("Emitting Message::GoPrevious to rewind to the start",);
            tab.update_resolved(Message::GoPrevious, Modifiers::empty());
        }
        assert_eq_tab_path(&tab, path);

        // Back to the future. Directories should be in the order they were opened.
        for dir in dirs {
            debug!("Emitting Message::GoNext",);
            tab.update_resolved(Message::GoNext, Modifiers::empty());
            assert_eq_tab_path(&tab, &dir);
        }

        Ok(())
    }

    #[test]
    fn tab_goprev_moves_backward_in_history() -> io::Result<()> {
        let (fs, mut tab, dirs) = tab_history()?;
        let path = fs.path();

        for dir in dirs.into_iter().rev() {
            assert_eq_tab_path(&tab, &dir);
            debug!("Emitting Message::GoPrevious",);
            tab.update_resolved(Message::GoPrevious, Modifiers::empty());
        }
        assert_eq_tab_path(&tab, path);

        Ok(())
    }

    #[test]
    fn tab_scroll_up_with_ctrl_modifier_zooms() -> io::Result<()> {
        let message_maybe =
            respond_to_scroll_direction(ScrollDelta::Pixels { x: 0.0, y: 1.0 }, &Modifiers::CTRL);
        assert!(message_maybe.is_some());
        assert!(matches!(message_maybe.unwrap(), Message::ZoomIn));
        Ok(())
    }

    #[test]
    fn tab_scroll_up_without_ctrl_modifier_does_not_zoom() -> io::Result<()> {
        let message_maybe = respond_to_scroll_direction(
            ScrollDelta::Pixels { x: 0.0, y: 1.0 },
            &Modifiers::empty(),
        );
        assert!(message_maybe.is_none());
        Ok(())
    }

    #[test]
    fn tab_scroll_down_with_ctrl_modifier_zooms() -> io::Result<()> {
        let message_maybe =
            respond_to_scroll_direction(ScrollDelta::Pixels { x: 0.0, y: -1.0 }, &Modifiers::CTRL);
        assert!(message_maybe.is_some());
        assert!(matches!(message_maybe.unwrap(), Message::ZoomOut));
        Ok(())
    }

    #[test]
    fn tab_scroll_down_without_ctrl_modifier_does_not_zoom() -> io::Result<()> {
        let message_maybe = respond_to_scroll_direction(
            ScrollDelta::Pixels { x: 0.0, y: -1.0 },
            &Modifiers::empty(),
        );
        assert!(message_maybe.is_none());
        Ok(())
    }
    #[test]
    fn tab_empty_history_does_nothing_on_prev_next() -> io::Result<()> {
        let fs = simple_fs(0, NUM_NESTED, NUM_DIRS, 0, NAME_LEN)?;
        let path = fs.path();
        let mut tab = Tab::new(
            Location::Path(path.into()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );

        // Tab's location shouldn't change if GoPrev or GoNext is triggered
        debug!("Emitting Message::GoPrevious",);
        tab.update(Message::GoPrevious, Modifiers::empty());
        assert_eq_tab_path(&tab, path);

        debug!("Emitting Message::GoNext",);
        tab.update(Message::GoNext, Modifiers::empty());
        assert_eq_tab_path(&tab, path);

        Ok(())
    }

    #[test]
    fn tab_locationup_moves_up_hierarchy() -> io::Result<()> {
        let fs = simple_fs(0, NUM_NESTED, NUM_DIRS, 0, NAME_LEN)?;
        let path = fs.path();
        let mut next_dir = filter_dirs(path)?
            .next()
            .expect("should be at least one directory");

        let mut tab = Tab::new(
            Location::Path(next_dir.clone()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        // This will eventually yield false once root is hit
        while next_dir.pop() {
            debug!("Emitting Message::LocationUp",);
            tab.update_resolved(Message::LocationUp, Modifiers::empty());
            assert_eq_tab_path(&tab, &next_dir);
        }

        Ok(())
    }

    #[test]
    fn sort_long_number_file_names() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();

        // Create files with names 255 characters long that only contain a single number
        // Example: 0000...0 for 255 characters
        // https://en.wikipedia.org/wiki/Filename#Comparison_of_filename_limitations
        let mut base_nums: Vec<_> = ('0'..='9').collect();
        fastrand::shuffle(&mut base_nums);
        debug!("Shuffled numbers for paths: {base_nums:?}");
        let paths: Vec<_> = base_nums
            .iter()
            .copied()
            .map(|base| path.join(std::iter::repeat_n(base, 255).collect::<String>()))
            .collect();

        for (file, base) in paths.iter().zip(base_nums) {
            trace!("Creating long file name for {base}");
            fs::File::create(file)?;
        }

        debug!("Creating tab for directory of long file names");
        Tab::new(
            Location::Path(path.into()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            // The scrollable's name; see `Tab::scrollable_name`.
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );

        Ok(())
    }

    #[test]
    fn sort_by_type_groups_mime_then_name() -> io::Result<()> {
        use super::HeadingOptions;

        let fs = empty_fs()?;
        let path = fs.path();
        // Two images, one text file and one folder; names chosen so a plain
        // name sort would interleave the two mime groups.
        fs::create_dir(path.join("a-folder"))?;
        fs::write(path.join("b.txt"), b"x")?;
        fs::write(path.join("c.png"), b"x")?;
        fs::write(path.join("a.png"), b"x")?;

        let location = Location::Path(path.to_owned());
        let (parent_item_opt, items) = location.scan(IconSizes::default());
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.parent_item_opt = parent_item_opt;
        tab.set_items(items);

        let names = |tab: &Tab| -> Vec<String> {
            tab.column_sort()
                .expect("tab should have items")
                .into_iter()
                .map(|(_, item)| item.name.clone())
                .collect()
        };

        tab.sort_name = HeadingOptions::Type;
        tab.sort_direction = true;
        assert_eq!(names(&tab), ["a-folder", "a.png", "c.png", "b.txt"]);

        tab.sort_direction = false;
        assert_eq!(names(&tab), ["a-folder", "b.txt", "c.png", "a.png"]);

        // Without folders first, folders still group first as the Folder category
        tab.config.folders_first = false;
        tab.sort_direction = true;
        assert_eq!(names(&tab), ["a-folder", "a.png", "c.png", "b.txt"]);
        tab.sort_direction = false;
        assert_eq!(names(&tab), ["b.txt", "c.png", "a.png", "a-folder"]);

        Ok(())
    }

    #[test]
    fn mode_calculations() {
        use super::{
            MODE_SHIFT_GROUP, MODE_SHIFT_OTHER, MODE_SHIFT_USER, get_mode_part, set_mode_part,
        };
        for user in 0..=7 {
            for group in 0..=7 {
                for other in 0..=7 {
                    let mode = (user << MODE_SHIFT_USER)
                        | (group << MODE_SHIFT_GROUP)
                        | (other << MODE_SHIFT_OTHER);
                    assert_eq!(format!("{mode:03o}"), format!("{user:o}{group:o}{other:o}"),);
                    assert_eq!(get_mode_part(mode, MODE_SHIFT_USER), user);
                    assert_eq!(get_mode_part(mode, MODE_SHIFT_GROUP), group);
                    assert_eq!(get_mode_part(mode, MODE_SHIFT_OTHER), other);

                    let mode_no_user = (group << MODE_SHIFT_GROUP) | (other << MODE_SHIFT_OTHER);
                    assert_eq!(
                        format!("{mode_no_user:03o}"),
                        format!("0{group:o}{other:o}")
                    );
                    assert_eq!(set_mode_part(mode_no_user, MODE_SHIFT_USER, user), mode);

                    let mode_no_group = (user << MODE_SHIFT_USER) | (other << MODE_SHIFT_OTHER);
                    assert_eq!(
                        format!("{mode_no_group:03o}"),
                        format!("{user:o}0{other:o}")
                    );
                    assert_eq!(set_mode_part(mode_no_group, MODE_SHIFT_GROUP, group), mode);

                    let mode_no_other = (user << MODE_SHIFT_USER) | (group << MODE_SHIFT_GROUP);
                    assert_eq!(
                        format!("{mode_no_other:03o}"),
                        format!("{user:o}{group:o}0")
                    );
                    assert_eq!(set_mode_part(mode_no_other, MODE_SHIFT_OTHER, other), mode);
                }
            }
        }
    }

    #[test]
    fn item_thumbnail_text_preview_small_utf8_returns_text() -> io::Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().join("preview.txt");
        fs::write(&path, "Hello, world!")?;
        let metadata = fs::metadata(&path)?;
        let item_metadata = ItemMetadata::Path {
            metadata,
            children_opt: None,
        };
        let thumb = ItemThumbnail::new(
            &path,
            item_metadata,
            Mime::TEXT_PLAIN,
            128,
            100 * 1024 * 1024,
            1,
            8,
        );
        assert!(
            matches!(thumb, ItemThumbnail::Text(_)),
            "small text file should produce Text thumbnail"
        );
        Ok(())
    }

    #[test]
    fn item_thumbnail_text_preview_empty_file_returns_not_image() -> io::Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().join("empty.txt");
        fs::File::create(&path)?;
        let metadata = fs::metadata(&path)?;
        let item_metadata = ItemMetadata::Path {
            metadata,
            children_opt: None,
        };
        let thumb = ItemThumbnail::new(
            &path,
            item_metadata,
            Mime::TEXT_PLAIN,
            128,
            100 * 1024 * 1024,
            1,
            8,
        );
        assert!(
            matches!(thumb, ItemThumbnail::NotImage),
            "empty text file should produce NotImage (no read)"
        );
        Ok(())
    }

    #[test]
    fn item_thumbnail_text_preview_invalid_utf8_uses_valid_prefix() -> io::Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().join("invalid_utf8.txt");
        // Valid UTF-8 "ab" then invalid byte sequence then "c"
        fs::write(&path, b"ab\xff\xfe\xfdc")?;
        let metadata = fs::metadata(&path)?;
        let item_metadata = ItemMetadata::Path {
            metadata,
            children_opt: None,
        };
        let thumb = ItemThumbnail::new(
            &path,
            item_metadata,
            Mime::TEXT_PLAIN,
            128,
            100 * 1024 * 1024,
            1,
            8,
        );
        match &thumb {
            ItemThumbnail::Text(content) => {
                // Text editor content may add a trailing newline
                assert_eq!(content.text().trim_end(), "ab");
            }
            _ => panic!(
                "expected Text thumbnail with valid prefix only, got {:?}",
                thumb
            ),
        }
        Ok(())
    }

    #[test]
    fn a_narrow_row_puts_its_right_column_first_on_its_second_line() {
        use super::{HeadingOptions, LIST_NAME_MIN, line_with_right, right_column_fits};

        let wide = 2000.0;
        assert!(right_column_fits(wide, 32, HeadingOptions::Name));
        assert!(!right_column_fits(LIST_NAME_MIN, 32, HeadingOptions::Name));
        // A bigger icon takes the room sooner; a size column is narrower
        // than a date column, so it stays longer
        let least = |icon, sort| {
            (0..2000)
                .map(|w| w as f32)
                .find(|&w| right_column_fits(w, icon, sort))
                .unwrap()
        };
        assert_eq!(
            least(64, HeadingOptions::Name),
            least(32, HeadingOptions::Name) + 32.0
        );
        assert!(least(32, HeadingOptions::Size) < least(32, HeadingOptions::Name));

        assert_eq!(
            line_with_right("Today 11:03", "Spreadsheet · 48 KB"),
            "Today 11:03 · Spreadsheet · 48 KB"
        );
        assert_eq!(line_with_right("", "Folder"), "Folder");
        assert_eq!(line_with_right("Today", ""), "Today");
    }

    /// A file that would be hidden, a dotfile or one named in `.hidden`, is
    /// drawn faded when hidden files are shown, and looks like any other once
    /// selected.
    #[test]
    fn a_shown_hidden_file_is_faded_until_selected() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in [".bashrc", "snap", "notes.md"] {
            fs::write(dir.path().join(name), b"").expect("write");
        }
        fs::write(dir.path().join(".hidden"), b"snap\n").expect("write");
        let items = scan_path(&dir.path().to_path_buf(), IconSizes::default());
        let faded = |name: &str, selected: bool| {
            items
                .iter()
                .find(|item| item.name == name)
                .expect("listed")
                .faded(selected)
        };
        assert!(faded(".bashrc", false));
        assert!(faded("snap", false));
        assert!(!faded("notes.md", false));

        assert!(!faded(".bashrc", true), "selected");
        assert!(faded("snap", false));
    }

    #[test]
    fn the_sort_bar_keeps_its_labels_only_where_they_fit() {
        use super::SORT_LABELS_MIN;

        let tab = Tab::new(
            Location::Path("/".into()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        let wanted = tab.sort_bar_labelled_width();
        assert!(wanted > 0.0, "the labels are measured");
        let least = wanted.max(SORT_LABELS_MIN);
        assert!(tab.sort_labels_fit(least));
        assert!(!tab.sort_labels_fit(least - 1.0));
        assert!(!tab.sort_labels_fit(SORT_LABELS_MIN - 1.0));
    }

    #[test]
    fn a_trash_search_result_says_where_it_was_trashed_from() {
        use crate::tab::{SearchLocation, SearchOptions};
        use std::time::Instant;

        let trashed = |original_parent: &str| {
            super::item_from_trash_entry(
                crate::trashing::TrashItem {
                    id: crate::trashing::TrashId::of_record("/t/info/a.txt.trashinfo"),
                    name: "a.txt".into(),
                    original_parent: (!original_parent.is_empty()).then(|| original_parent.into()),
                    time_deleted: 0,
                },
                crate::trashing::TrashItemMetadata {
                    size: crate::trashing::TrashItemSize::Bytes(1),
                },
                IconSizes::default(),
            )
        };
        let tab = Tab::new(
            Location::Search(
                SearchLocation::Trash,
                "a".to_owned(),
                SearchOptions {
                    show_hidden: false,
                    recursive: false,
                },
                Instant::now(),
            ),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        assert_eq!(
            tab.search_folder_text(&trashed("/home/u/Documents")),
            "/home/u/Documents"
        );
        assert_eq!(
            tab.search_folder_text(&trashed("")),
            crate::fl!("trash-original-unknown")
        );
    }

    #[test]
    fn the_right_column_follows_a_slide_only_if_the_sort_bar_keeps_its_shape() -> io::Result<()> {
        use super::{SORT_LABELS_MIN, View};
        use crate::ui::iced::Size;

        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("a.txt"), b"hi")?;
        let location = Location::Path(dir.path().into());
        let (_, items) = location.scan(IconSizes::default());
        let mut tab = Tab::new(
            location,
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        tab.set_items(items);
        let width = |tab: &Tab, w: f32| tab.size_opt.set(Some(Size::new(w, 800.0)));

        width(&tab, 1400.0);
        assert!(
            tab.column_slide_fits(400.0, true),
            "1400 → 1000: labels both sides"
        );
        width(&tab, 1000.0);
        assert!(tab.column_slide_fits(400.0, false), "1000 → 1400: closing");

        // Where the labels start to fit: never under SORT_LABELS_MIN, and later
        // when the fonts at hand measure them wider
        let labels_min = tab.sort_bar_labelled_width().max(SORT_LABELS_MIN);
        width(&tab, labels_min + 200.0);
        assert!(
            !tab.column_slide_fits(400.0, true),
            "the sort bar drops its labels on the way"
        );
        // The narrowest view whose rows keep their right column
        let right_min = (0..2000)
            .map(|w| w as f32)
            .find(|&w| tab.shows_right_column(w))
            .expect("some width has room for the right column");
        assert!(right_min < SORT_LABELS_MIN - 10.0, "room for the next case");
        width(&tab, SORT_LABELS_MIN - 1.0);
        assert!(
            tab.column_slide_fits(SORT_LABELS_MIN - 1.0 - right_min, true),
            "icons alone on both sides, right column on both"
        );
        width(&tab, right_min + 50.0);
        assert!(
            !tab.column_slide_fits(100.0, true),
            "the right column goes on the way"
        );
        width(&tab, right_min - 50.0);
        assert!(
            !tab.column_slide_fits(10.0, true),
            "no right column to slide"
        );

        // The exact boundary keeps the labels.
        width(&tab, labels_min + 400.0);
        assert!(
            tab.column_slide_fits(400.0, true),
            "down to exactly where the labels fit"
        );

        width(&tab, 1400.0);
        tab.config.view = View::Grid;
        assert!(!tab.column_slide_fits(400.0, true), "grid view reflows");
        tab.config.view = View::List;

        // A search's rows are not slid.
        {
            use crate::tab::{SearchLocation, SearchOptions};
            use std::time::Instant;

            tab.location = Location::Search(
                SearchLocation::Path(dir.path().into()),
                "term".to_owned(),
                SearchOptions {
                    show_hidden: false,
                    recursive: false,
                },
                Instant::now(),
            );
            assert!(
                !tab.column_slide_fits(400.0, true),
                "a search never follows"
            );
            tab.location = Location::Path(dir.path().into());
        }

        tab.size_opt.set(None);
        assert!(
            !tab.column_slide_fits(400.0, true),
            "no size: nothing to compare against"
        );

        // No rows shown: the empty-state message would re-centre with a
        // jump when the slide settled instead.
        tab.set_items(Vec::new());
        width(&tab, 1400.0);
        assert!(
            !tab.column_slide_fits(400.0, true),
            "no items, nothing to keep in place"
        );

        Ok(())
    }

    /// The right column holds what the list is sorted by, the date when
    /// sorted by name; the second line the other two, skipping unknown ones.
    #[test]
    fn a_row_shows_the_sorted_detail_on_the_right() {
        use super::{HeadingOptions as H, list_details};

        let row = |sort| list_details(sort, "Document", "48.2 KB", "Today 11:03");
        assert_eq!(
            row(H::Name),
            ("Document · 48.2 KB".into(), "Today 11:03".into())
        );
        assert_eq!(
            row(H::Modified),
            ("Document · 48.2 KB".into(), "Today 11:03".into())
        );
        assert_eq!(
            row(H::TrashedOn),
            ("Document · 48.2 KB".into(), "Today 11:03".into())
        );
        assert_eq!(
            row(H::Size),
            ("Document · Today 11:03".into(), "48.2 KB".into())
        );
        assert_eq!(
            row(H::Type),
            ("48.2 KB · Today 11:03".into(), "Document".into())
        );

        // A folder whose items are not counted yet has no size to show
        assert_eq!(
            list_details(H::Name, "Folder", "", "Yesterday"),
            ("Folder".into(), "Yesterday".into())
        );
    }

    /// The order button says the order as the sort menu does.
    #[test]
    fn the_order_is_said_in_the_sort_menu_s_words() {
        use super::{HeadingOptions as H, sort_order_words};

        assert_eq!(sort_order_words(H::Name, true), crate::fl!("sort-a-z"));
        assert_eq!(sort_order_words(H::Type, false), crate::fl!("sort-z-a"));
        assert_eq!(
            sort_order_words(H::Modified, false),
            crate::fl!("sort-newest-first")
        );
        assert_eq!(
            sort_order_words(H::TrashedOn, true),
            crate::fl!("sort-oldest-first")
        );
        assert_eq!(
            sort_order_words(H::Size, false),
            crate::fl!("sort-largest-to-smallest")
        );
    }
}
