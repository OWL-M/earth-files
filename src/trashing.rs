//! The trash, as `xdg_kit::trash` keeps it to the freedesktop.org Trash
//! specification: the home trash, and on any other drive `.Trash/$uid` (in
//! a shared, sticky `.Trash`) or `.Trash-$uid` at the drive's root.
//!
//! This module fits it to what the app shows and does. An item is named by
//! where its `.trashinfo` is, or, without one, by where the item is; an
//! entry of `files/` without a `.trashinfo` is listed with its origin
//! unknown, as the spec has such an entry shown, not hidden. Network
//! drives are kept out: nothing is trashed there and their trashes are not
//! looked for, as asking them can hang; the drive the home trash is on is
//! the one exception, as its files go to the home trash.

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use xdg_kit::trash::{
    AttemptFailure, Context, DiscardError, EraseFailure, EraseOptions, EraseProblemKind,
    Exclusions, Fallbacks, FileKind, Instants, Limits, ListError, Metadata, Mount, OpenErrorKind,
    Origin, Payload, Policy, Quiescent, RestoreFailure, RestoreOptions, Trash, TrashDir,
    TrashError, TrashKind,
};

/// Which trashed thing an item is, and where it is: unique among the items
/// listed.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum TrashId {
    /// An entry of a trash with its `.trashinfo`, readable or not: where
    /// that is, and where the entry is, in the trash's `files/`.
    Recorded { info: PathBuf, payload: PathBuf },
    /// An entry of a trash's `files/` with no `.trashinfo`: where it is.
    Orphan(PathBuf),
    /// Something inside a trashed folder: where it is.
    Child(PathBuf),
}

impl TrashId {
    /// Where the trashed thing is.
    pub fn payload(&self) -> &Path {
        match self {
            Self::Recorded { payload, .. } => payload,
            Self::Orphan(path) | Self::Child(path) => path,
        }
    }

    /// Where its `.trashinfo` is, for an entry that has one.
    pub fn record(&self) -> Option<&Path> {
        match self {
            Self::Recorded { info, .. } => Some(info),
            Self::Orphan(_) | Self::Child(_) => None,
        }
    }
}

/// One item in a trash.
#[derive(Clone, Debug)]
pub struct TrashItem {
    pub id: TrashId,
    /// Its name where it came from.
    pub name: OsString,
    /// The folder it came from; `None` when unknown, as for an entry
    /// without a readable `.trashinfo`, which cannot be put back.
    pub original_parent: Option<PathBuf>,
    /// When it was trashed, in seconds since the Unix epoch.
    pub time_deleted: i64,
}

impl TrashItem {
    /// Where it came from, and goes back to when restored; `None` when that
    /// is unknown.
    pub fn original_path(&self) -> Option<PathBuf> {
        self.original_parent
            .as_ref()
            .map(|parent| parent.join(&self.name))
    }

    /// The path to name it by: where it came from, or where it is when that
    /// is unknown.
    pub fn named_path(&self) -> PathBuf {
        self.original_path()
            .unwrap_or_else(|| self.id.payload().to_path_buf())
    }
}

#[cfg(test)]
impl TrashId {
    /// The entry whose `.trashinfo` is at `info`, in its trash's `files/`.
    pub fn of_record(info: impl Into<PathBuf>) -> Self {
        let info = info.into();
        let payload = info
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new(""))
            .join("files")
            .join(info.file_stem().unwrap_or_default());
        Self::Recorded { info, payload }
    }
}

/// Where the trash keeps `item`, and its `.trashinfo` when it has one.
pub(crate) fn in_trash(item: &TrashItem) -> (PathBuf, Option<PathBuf>) {
    (
        item.id.payload().to_path_buf(),
        item.id.record().map(Path::to_path_buf),
    )
}

/// Items are the same item when their ids are.
impl PartialEq for TrashItem {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for TrashItem {}

impl std::hash::Hash for TrashItem {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// How big a trashed item is: a file's bytes, or a folder's entries.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrashItemSize {
    Bytes(u64),
    Entries(usize),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TrashItemMetadata {
    pub size: TrashItemSize,
}

/// A trash folder, holding `files/` and `info/`.
pub type Bin = TrashDir;

/// The mounts there are now, or none when they cannot be read.
fn mounts() -> Vec<Mount> {
    xdg_kit::trash::mounts(&Limits::default()).unwrap_or_else(|err| {
        log::warn!("failed to read the mounts: {err}");
        Vec::new()
    })
}

/// Whether a drive of this filesystem type takes a trash: a local one.
/// Network drives do not: nothing is trashed there, so their trashes are
/// never looked for, and asking them can hang.
fn takes_a_trash(fs_type: &str) -> bool {
    crate::tab::fs_type_kind(fs_type) == crate::tab::FsKind::Local
}

/// The mount among `mounts` that `path` is under, by its name: the one
/// with the longest mount point above it.
fn mount_of<'a>(path: &Path, mounts: &'a [Mount]) -> Option<&'a Mount> {
    mounts
        .iter()
        .filter(|mount| path.starts_with(&mount.mount_point))
        .max_by_key(|mount| mount.mount_point.as_os_str().len())
}

/// `base` with the app's policy, for `mounts`:
/// - network drives are excluded from trashing, found by their mount point
///   alone, but for the one the home trash is on: its files go to the home
///   trash by a rename, as any other's on its drive;
/// - where a drive refuses the rename that never replaces, a file goes by a
///   hard link and a folder by a placeholder folder renamed over, rather
///   than not at all.
fn with_policy(base: Context, mounts: &[Mount]) -> Context {
    // Where the home trash really is: its folder, or one above it, may be a
    // link onto another drive, and that drive is the one its files are on
    let home_trash = base.home_trash().path();
    let resolved = home_trash
        .ancestors()
        .find_map(|at| fs::canonicalize(at).ok())
        .unwrap_or(home_trash);
    let home = mount_of(&resolved, mounts).map(|mount| mount.mount_point.clone());
    let mut policy = Policy::default();
    policy.fallbacks = Fallbacks::LinkAndPlaceholder;
    policy.exclusions = Exclusions::from_mounts(mounts, |mount| {
        !takes_a_trash(&mount.fs_type) && Some(&mount.mount_point) != home.as_ref()
    });
    base.with_policy(policy)
}

/// The trash as this user has it now, with the mounts it was made for.
fn context() -> io::Result<(Context, Vec<Mount>)> {
    let base = Context::from_env().map_err(io::Error::other)?;
    let mounts = mounts();
    Ok((with_policy(base, &mounts), mounts))
}

/// The trash folders found on drives, opened, with those there that could
/// not be.
struct Found {
    trashes: Vec<Trash>,
    /// Trashes there that could not be opened or read, and why. One that
    /// fails the spec's checks is not this user's to use, and is left out
    /// of these, said only in the log.
    problems: Vec<io::Error>,
}

impl Found {
    /// Its trashes, or what kept one from being opened: what an emptying
    /// must not take for an empty trash.
    fn strict(mut self) -> io::Result<Vec<Trash>> {
        match self.problems.drain(..).next() {
            Some(err) => Err(err),
            None => Ok(self.trashes),
        }
    }
}

/// Opens the trash folders `dirs`, as far as they are there. One without
/// its `files/` or `info/` gets it made, as trashing into it would: without
/// it the trash cannot be opened at all, and what its other folder holds
/// would neither be shown nor emptied.
fn open_all(context: &Context, dirs: impl IntoIterator<Item = TrashDir>) -> Found {
    let mut found = Found {
        trashes: Vec::new(),
        problems: Vec::new(),
    };
    for dir in dirs {
        let root = dir.path();
        let mut opened = context.open(&dir);
        if let Err(err) = &opened
            && matches!(err.kind, OpenErrorKind::Absent)
            && err.path.parent() == Some(root.as_path())
            && ["files", "info"]
                .map(OsStr::new)
                .contains(&err.path.file_name().unwrap_or_default())
        {
            let made = [root.join("files"), root.join("info")]
                .iter()
                .try_for_each(|sub| make_dir(sub));
            match made {
                Ok(()) => {
                    log::info!("made the missing folders of the trash {}", root.display());
                    opened = context.open(&dir);
                }
                Err(err) => {
                    found.problems.push(err);
                    continue;
                }
            }
        }
        match opened {
            Ok(trash) => found.trashes.push(trash),
            // No trash there, or no drive
            Err(err)
                if matches!(err.kind, OpenErrorKind::Absent)
                    && (err.path == root || !err.path.starts_with(&root)) => {}
            // One that fails its checks is not this user's to use; a home
            // trash linked to nothing holds nothing
            Err(err)
                if matches!(
                    err.kind,
                    OpenErrorKind::Unsafe(_) | OpenErrorKind::BrokenLink { .. }
                ) =>
            {
                log::warn!("a trash is not used: {err}");
            }
            // A drive's folder that cannot be looked into, such as another
            // user's: whether it holds a trash of this user's cannot be
            // told, and a trash there could not be used anyway. The home
            // trash is this user's own, and its failures count
            Err(err) if dir.kind() != TrashKind::Home && !err.path.starts_with(&root) => {
                log::debug!("no trash looked for: {err}");
            }
            Err(err) => {
                let kind = match &err.kind {
                    OpenErrorKind::Io(io) => io.kind(),
                    _ => io::ErrorKind::Other,
                };
                found.problems.push(io::Error::new(kind, err));
            }
        }
    }
    found
}

/// Makes `dir`, mode 0700, unless it is there.
fn make_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

/// The trash folders on the drives that take one, opened: the home trash
/// and each local drive's.
fn discover(context: &Context, mounts: &[Mount]) -> Found {
    let mut dirs = vec![context.home_trash()];
    for mount in mounts.iter().filter(|mount| takes_a_trash(&mount.fs_type)) {
        dirs.push(context.shared_trash(&mount.mount_point));
        dirs.push(context.private_trash(&mount.mount_point));
    }
    open_all(context, dirs)
}

/// [`discover`], what could not be opened only said in the log: for
/// showing what there is.
fn discover_lenient(context: &Context, mounts: &[Mount]) -> Vec<Trash> {
    let found = discover(context, mounts);
    for problem in &found.problems {
        log::warn!("a trash cannot be read: {problem}");
    }
    found.trashes
}

/// Every trash there is now: the home trash, and the drive trashes on
/// mounted drives that take one (see [`takes_a_trash`]).
pub fn bins() -> Vec<Bin> {
    match context() {
        Ok((context, mounts)) => discover_lenient(&context, &mounts)
            .iter()
            .map(|trash| trash.dir().clone())
            .collect(),
        Err(err) => {
            log::warn!("failed to look for the trash: {err}");
            Vec::new()
        }
    }
}

/// [`bins`], for emptying them: one there that cannot be opened fails it,
/// rather than be left out of an emptying that says it is done.
pub fn bins_to_empty() -> io::Result<Vec<Bin>> {
    let (context, mounts) = context()?;
    Ok(discover(&context, &mounts)
        .strict()?
        .iter()
        .map(|trash| trash.dir().clone())
        .collect())
}

/// The trashes there are on the drive mounted at `top`, for emptying them:
/// one there that cannot be opened fails it.
pub fn drive_bins(top: &Path) -> io::Result<Vec<Bin>> {
    let (context, _) = context()?;
    Ok(drive_trashes(&context, top)
        .strict()?
        .iter()
        .map(|trash| trash.dir().clone())
        .collect())
}

fn drive_trashes(context: &Context, top: &Path) -> Found {
    open_all(
        context,
        [context.shared_trash(top), context.private_trash(top)],
    )
}

/// Whether the drive mounted at `top` has anything in its trash. A record
/// in `info/` without its item does not count: it holds nothing, and
/// emptying the trash leaves it.
pub fn drive_has_items(top: &Path) -> bool {
    context().is_ok_and(|(context, _)| {
        drive_trashes(&context, top)
            .trashes
            .iter()
            .any(|trash| holds_items(trash, true))
    })
}

/// Whether `trash` holds an item; when it cannot be read, `unread`.
fn holds_items(trash: &Trash, unread: bool) -> bool {
    trash
        .entries()
        .any(|entry| entry.map_or(unread, |entry| !matches!(entry.payload(), Payload::Missing)))
}

/// Whether no trash holds anything.
pub fn is_empty() -> bool {
    context().map_or(true, |(context, mounts)| {
        !discover_lenient(&context, &mounts)
            .iter()
            .any(|trash| holds_items(trash, false))
    })
}

/// What one trash holds: each item, with what its `.trashinfo` says, or
/// with its origin unknown when there is none to read. A trash or an item
/// that cannot be read fails it: taken for empty, it would be left behind
/// by an emptying that says it is done.
pub fn list_bin(bin: &Bin) -> io::Result<Vec<(TrashItem, TrashItemMetadata)>> {
    let (context, _) = context()?;
    list_dir(&context, bin)
}

fn list_dir(context: &Context, dir: &TrashDir) -> io::Result<Vec<(TrashItem, TrashItemMetadata)>> {
    let Some(trash) = open_all(context, [dir.clone()]).strict()?.pop() else {
        // No trash holds nothing
        return Ok(Vec::new());
    };
    let mut failed = None;
    let listed = read_trash(context, &trash, &mut failed);
    failed.map_or(Ok(listed), Err)
}

/// Everything in every trash.
/// What cannot be read is left out, to show the rest.
pub fn list() -> Vec<(TrashItem, TrashItemMetadata)> {
    let Ok((context, mounts)) = context() else {
        return Vec::new();
    };
    discover_lenient(&context, &mounts)
        .iter()
        .flat_map(|trash| {
            let mut failed = None;
            let listed = read_trash(&context, trash, &mut failed);
            if let Some(err) = failed {
                log::warn!(
                    "failed to read all of {}: {err}",
                    trash.dir().path().display()
                );
            }
            listed
        })
        .collect()
}

/// [`list_bin`] of an open trash, leaving out what cannot be read, the
/// first failure kept in `failed`.
fn read_trash(
    context: &Context,
    trash: &Trash,
    failed: &mut Option<io::Error>,
) -> Vec<(TrashItem, TrashItemMetadata)> {
    let mut listed = Vec::new();
    for entry in trash.entries() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                failed.get_or_insert(list_error(err));
                break;
            }
        };
        match item_of(context, &entry) {
            Ok(Some(item)) => listed.push(item),
            Ok(None) => {}
            Err(err) => {
                failed.get_or_insert(err);
            }
        }
    }
    listed
}

fn list_error(err: ListError) -> io::Error {
    match err {
        ListError::Io { error, .. } => error,
        other => io::Error::other(other),
    }
}

/// The item `entry` is, as the app shows it; `None` for a record without
/// its item, which holds nothing.
fn item_of(
    context: &Context,
    entry: &xdg_kit::trash::Entry,
) -> io::Result<Option<(TrashItem, TrashItemMetadata)>> {
    let kind = match entry.payload() {
        Payload::Present(kind) => *kind,
        Payload::Unknown(err) => return Err(io::Error::new(err.kind(), err.to_string())),
        _ => return Ok(None),
    };
    let payload = entry.payload_path();
    let size = if kind == FileKind::Directory {
        TrashItemSize::Entries(fs::read_dir(&payload).map_or(0, Iterator::count))
    } else {
        match fs::symlink_metadata(&payload) {
            Ok(meta) => TrashItemSize::Bytes(meta.len()),
            // Gone since it was listed
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        }
    };
    // A `.trashinfo` that cannot be read still goes with it
    let id = if matches!(entry.metadata(), Metadata::Missing) {
        TrashId::Orphan(payload)
    } else {
        TrashId::Recorded {
            info: entry.metadata_path(),
            payload,
        }
    };
    let (name, original_parent) = match entry.origin() {
        Origin::Known(path) => (
            path.file_name()
                .map_or_else(|| entry.name().to_owned(), OsStr::to_owned),
            path.parent().map(Path::to_path_buf),
        ),
        Origin::Unknown(_) => (entry.name().to_owned(), None),
    };
    let time_deleted = entry
        .deletion_date()
        .map_or(0, |date| seconds(date.instants(context.zone())));
    Ok(Some((
        TrashItem {
            id,
            name,
            original_parent,
            time_deleted,
        },
        TrashItemMetadata { size },
    )))
}

/// The Unix seconds of a deletion date in this time zone: the earlier of
/// two when clocks went back, the one before when they went forward.
fn seconds(instants: Instants) -> i64 {
    let at = match instants {
        Instants::Unique(at)
        | Instants::Ambiguous { earlier: at, .. }
        | Instants::Skipped { before: at, .. } => at,
        Instants::OutOfRange => return 0,
    };
    unix_seconds(at)
}

fn unix_seconds(at: SystemTime) -> i64 {
    match at.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// Whether the place `item` came from is unknown: an entry of `files/`
/// without a `.trashinfo`, or with one that cannot be read. It cannot be
/// put back, only deleted.
pub fn origin_unknown(item: &TrashItem) -> bool {
    item.original_parent.is_none()
}

/// Whether `path` has somewhere in a trash to go: a trash on its drive that
/// can be used or made. One that is not there counts as going: there is
/// nothing to ask about.
///
/// Blocking: it asks the filesystem, but never a drive kept out.
pub fn can_trash(path: &Path) -> bool {
    context().is_ok_and(|(context, _)| match context.plan(path) {
        Ok(plan) => plan.destination().is_some(),
        Err(TrashError::SourceMissing { .. }) => true,
        Err(_) => false,
    })
}

/// Sends `path` to the trash on its own drive, made there when missing, and
/// returns its entry. Fails when its drive has no trash that can be used.
pub fn put(path: &Path) -> io::Result<TrashItem> {
    let (context, _) = context()?;
    let trashed = context.trash(path).map_err(trash_error)?;
    Ok(trashed_item(trashed))
}

fn trashed_item(trashed: xdg_kit::trash::Trashed) -> TrashItem {
    if let xdg_kit::trash::Durability::NotSynced(err) = &trashed.durability {
        log::warn!(
            "{} is in the trash, but not yet safe from a crash: {err}",
            trashed.original.display()
        );
    }
    let mut info_name = trashed.name.clone();
    info_name.push(".trashinfo");
    TrashItem {
        id: TrashId::Recorded {
            info: trashed.trash.path().join("info").join(info_name),
            payload: trashed.trash.path().join("files").join(&trashed.name),
        },
        name: trashed
            .original
            .file_name()
            .map(OsStr::to_owned)
            .unwrap_or_default(),
        original_parent: trashed.original.parent().map(Path::to_path_buf),
        time_deleted: unix_seconds(SystemTime::now()),
    }
}

/// A failed trashing as an `io::Error` whose kind says why, as the delete
/// tells the user: a drive without a trash it can use is `Unsupported`,
/// unless a trash was there and could not be written to.
fn trash_error(err: TrashError) -> io::Error {
    let attempted = |attempts: &[xdg_kit::trash::Attempt]| {
        attempts.iter().find_map(|attempt| match &attempt.failure {
            AttemptFailure::Metadata(err) | AttemptFailure::Move(err) => Some(err.kind()),
            AttemptFailure::Open(open) => match &open.kind {
                OpenErrorKind::Io(err) => Some(err.kind()),
                _ => None,
            },
            _ => None,
        })
    };
    let kind = match &err {
        TrashError::Source { error, .. } => error.kind(),
        TrashError::SourceMissing { .. } => io::ErrorKind::NotFound,
        TrashError::NoTrash { attempts, .. } | TrashError::Unsupported { attempts, .. } => {
            attempted(attempts).unwrap_or(io::ErrorKind::Unsupported)
        }
        TrashError::Excluded(_) => io::ErrorKind::Unsupported,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, err)
}

/// The trash folder at `root`: the home trash, or a drive's
/// `$top/.Trash-$uid` or `$top/.Trash/$uid`.
fn dir_at(context: &Context, root: &Path) -> TrashDir {
    let home = context.home_trash();
    if home.path() == root {
        return home;
    }
    let parent = root.parent().unwrap_or(root);
    if root
        .file_name()
        .is_some_and(|name| name.as_bytes().starts_with(b".Trash-"))
    {
        context.private_trash(parent)
    } else {
        context.shared_trash(parent.parent().unwrap_or(parent))
    }
}

/// The trash `item` is at the top of, opened, with its name there: `None`
/// for an item inside a trashed folder, which the trash knows nothing of,
/// and for one in a folder that is no trash of this user's layout.
fn top_entry(context: &Context, item: &TrashItem) -> io::Result<Option<(Trash, OsString)>> {
    if let TrashId::Child(_) = item.id {
        return Ok(None);
    }
    let files = item.id.payload();
    let (Some(folder), Some(name)) = (files.parent(), files.file_name()) else {
        return Ok(None);
    };
    let Some(root) = folder.parent() else {
        return Ok(None);
    };
    let dir = dir_at(context, root);
    if dir.path() != root {
        return Ok(None);
    }
    let trash = context.open(&dir).map_err(io::Error::other)?;
    Ok(Some((trash, name.to_owned())))
}

/// Removes `item`'s record by its path, once its item is gone: for an item
/// whose trash cannot be opened as one.
fn remove_record(item: &TrashItem) -> io::Result<()> {
    let (files, info) = in_trash(item);
    match (fs::symlink_metadata(&files), info) {
        (Err(err), Some(info)) if err.kind() == io::ErrorKind::NotFound => {
            match fs::remove_file(info) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

/// Where the entry `name` of the trash folder `root` came from, as its
/// `.trashinfo` says.
pub(crate) fn origin_at(root: &Path, name: &OsStr) -> Option<PathBuf> {
    let (context, _) = context().ok()?;
    origin_in(&context, root, name)
}

fn origin_in(context: &Context, root: &Path, name: &OsStr) -> Option<PathBuf> {
    let trash = context.open(&dir_at(context, root)).ok()?;
    match trash.entry(name).ok()??.origin() {
        Origin::Known(path) => Some(path.clone()),
        Origin::Unknown(_) => None,
    }
}

/// Removes `item` from the trash for good: the item, then its `.trashinfo`
/// once the item is gone. Failing part-way, the `.trashinfo` stays, and so
/// does the entry in the trash. `go_on` is asked before each entry.
pub fn purge(item: &TrashItem, go_on: &dyn Fn() -> io::Result<()>) -> io::Result<()> {
    let (context, _) = context()?;
    purge_in(&context, item, go_on)
}

fn purge_in(
    context: &Context,
    item: &TrashItem,
    go_on: &dyn Fn() -> io::Result<()>,
) -> io::Result<()> {
    let Some((trash, name)) = top_entry(context, item)? else {
        // Inside a trashed folder: only the path itself
        let (files, _) = in_trash(item);
        return match crate::fs_at::remove_tree_checked(&files, go_on) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => remove_record(item),
        };
    };
    let Some(entry) = trash.entry(&name).map_err(io::Error::other)? else {
        // Gone already
        return Ok(());
    };
    if matches!(entry.payload(), Payload::Missing) {
        // The item went, its record did not: it is this delete's to take
        return match trash.discard_metadata(&entry, Quiescent::asserted()) {
            Ok(()) | Err(DiscardError::NoMetadata) => Ok(()),
            Err(err) => Err(io::Error::other(err)),
        };
    }
    let stopped = RefCell::new(None);
    let cancel = || match go_on() {
        Ok(()) => false,
        Err(err) => {
            stopped.replace(Some(err));
            true
        }
    };
    match trash.erase(&entry, &EraseOptions::default(), &cancel) {
        Ok(erased) => match erased.metadata {
            xdg_kit::trash::MetadataRemoval::Kept(err) => Err(err),
            _ => Ok(()),
        },
        Err(err) => Err(match err.failure {
            EraseFailure::Cancelled => stopped
                .into_inner()
                .unwrap_or_else(|| io::Error::from(io::ErrorKind::Interrupted)),
            EraseFailure::Unconfirmed(err) => err,
            // What was in the way, as the delete asks about it: one this
            // user may not remove is for root
            EraseFailure::Incomplete => {
                let kind = err
                    .problems
                    .iter()
                    .find_map(|problem| match &problem.kind {
                        EraseProblemKind::Io(err) => Some(err.kind()),
                        _ => None,
                    })
                    .unwrap_or(io::ErrorKind::Other);
                let first = err
                    .problems
                    .first()
                    .map(|problem| problem.path.display().to_string())
                    .unwrap_or_default();
                io::Error::new(kind, format!("{first}: {err}"))
            }
            _ => io::Error::other(err),
        }),
    }
}

/// What came of putting an item back by the trash's own means.
#[derive(Debug, Eq, PartialEq)]
pub enum Restored {
    /// It is back where it was, and gone from the trash with its record.
    Back,
    /// It could not be renamed back: something has its name there, or that
    /// place is on another drive. A move brings it back, and
    /// [`forget`] then removes its record.
    NeedsMove,
}

/// Puts `item` back where it was by a rename, making the folders it was
/// in where they are gone. An item inside a trashed folder, or one the
/// trash cannot rename back, is left for a move.
pub fn restore(item: &TrashItem) -> io::Result<Restored> {
    let (context, _) = context()?;
    restore_in(&context, item)
}

fn restore_in(context: &Context, item: &TrashItem) -> io::Result<Restored> {
    let Some((trash, name)) = top_entry(context, item)? else {
        return Ok(Restored::NeedsMove);
    };
    let Some(entry) = trash.entry(&name).map_err(io::Error::other)? else {
        return Err(io::Error::from(io::ErrorKind::NotFound));
    };
    let options = RestoreOptions {
        create_parents: true,
        ..RestoreOptions::default()
    };
    match trash.restore(&entry, &options) {
        Ok(restored) => {
            if let xdg_kit::trash::Durability::NotSynced(err) = &restored.durability {
                log::warn!(
                    "{} is back, but not yet safe from a crash: {err}",
                    restored.destination.display()
                );
            }
            if let xdg_kit::trash::MetadataRemoval::Kept(err) = restored.metadata {
                log::warn!(
                    "failed to remove the record of {}: {err}",
                    item.named_path().display()
                );
            }
            Ok(Restored::Back)
        }
        Err(err) => match err.failure {
            RestoreFailure::Conflict(_) | RestoreFailure::CrossDevice(_) => Ok(Restored::NeedsMove),
            RestoreFailure::Parent { error, .. } | RestoreFailure::Move(error) => Err(error),
            _ => Err(io::Error::other(err)),
        },
    }
}

/// Removes the record of `item`, now that a move took it out of the
/// trash: only once its item is gone from there.
pub fn forget(item: &TrashItem) -> io::Result<()> {
    let (context, _) = context()?;
    forget_in(&context, item)
}

fn forget_in(context: &Context, item: &TrashItem) -> io::Result<()> {
    let Some((trash, name)) = top_entry(context, item)? else {
        return remove_record(item);
    };
    let Some(entry) = trash.entry(&name).map_err(io::Error::other)? else {
        return Ok(());
    };
    if !matches!(entry.payload(), Payload::Missing) {
        return Ok(());
    }
    // This process moved it out: no trashing of that name is under way
    match trash.discard_metadata(&entry, Quiescent::asserted()) {
        Ok(()) | Err(DiscardError::NoMetadata) => Ok(()),
        Err(err) => Err(io::Error::other(err)),
    }
}

/// Sends `path`, just brought back from `item`'s trash, back into that
/// trash, as a cancelled restore leaves it.
pub fn put_back(item: &TrashItem, path: &Path) -> io::Result<()> {
    let (context, _) = context()?;
    let (files, _) = in_trash(item);
    let root = files
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    context
        .trash_into(&dir_at(&context, root), path)
        .map(|_| ())
        .map_err(trash_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A context whose home trash is in `data`, so tests leave the user's
    /// own alone.
    fn context_in(data: &Path) -> Context {
        let zone = xdg_kit::trash::LocalZone::system().expect("zone");
        let base = Context::new(
            data,
            // SAFETY: `getuid` has no preconditions and cannot fail
            unsafe { libc::getuid() },
            std::env::current_dir().expect("cwd"),
            zone,
        )
        .expect("context");
        with_policy(base, &[])
    }

    fn drive() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// A drive at `top` with its own trash, known as a trash folder.
    fn drive_trash(context: &Context, top: &Path) -> TrashDir {
        let dir = context.private_trash(top);
        for sub in ["files", "info"] {
            fs::create_dir_all(dir.path().join(sub)).expect("mkdir");
        }
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod");
        crate::trash::remember_folder(dir.path());
        dir
    }

    fn trash_one(context: &Context, dir: &TrashDir, path: &Path) -> TrashItem {
        trashed_item(context.trash_into(dir, path).expect("trashed"))
    }

    #[test]
    fn trashing_onto_a_drive_records_where_it_came_from() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        fs::create_dir(top.path().join("dir")).expect("mkdir");
        let item = top.path().join("dir/café.txt");
        fs::write(&item, "x").expect("write");
        let trashed = trash_one(&context, &dir, &item);
        assert!(!item.exists());
        assert_eq!(
            trashed.id.record(),
            Some(dir.path().join("info/café.txt.trashinfo").as_path())
        );
        assert_eq!(trashed.id.payload(), dir.path().join("files/café.txt"));
        let listed = list_dir(&context, &dir).expect("listed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0.original_path(), trashed.original_path());
        assert!(listed[0].0.time_deleted > 0);
        assert!(matches!(listed[0].1.size, TrashItemSize::Bytes(1)));
    }

    #[test]
    fn listing_shows_orphans_and_leaves_out_records_alone() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        fs::create_dir(dir.path().join("files/orphan")).expect("mkdir");
        fs::write(dir.path().join("files/orphan/a"), "a").expect("write");
        fs::write(
            dir.path().join("info/record.trashinfo"),
            "[Trash Info]\nPath=record\nDeletionDate=2026-10-06T12:00:00\n",
        )
        .expect("write");
        let listed = list_dir(&context, &dir).expect("listed");
        assert_eq!(listed.len(), 1, "{listed:?}");
        let (orphan, meta) = &listed[0];
        assert!(origin_unknown(orphan));
        assert_eq!(orphan.id, TrashId::Orphan(dir.path().join("files/orphan")));
        assert!(matches!(meta.size, TrashItemSize::Entries(1)));
    }

    #[test]
    fn a_trashinfo_that_cannot_be_read_gives_an_unknown_origin() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        fs::write(dir.path().join("files/x"), "x").expect("write");
        fs::write(dir.path().join("info/x.trashinfo"), "junk").expect("write");
        let listed = list_dir(&context, &dir).expect("listed");
        assert!(origin_unknown(&listed[0].0));
        assert_eq!(
            listed[0].0.id,
            TrashId::Recorded {
                info: dir.path().join("info/x.trashinfo"),
                payload: dir.path().join("files/x"),
            }
        );
    }

    #[test]
    fn dates_are_read_in_local_time() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        fs::write(dir.path().join("files/x"), "x").expect("write");
        fs::write(
            dir.path().join("info/x.trashinfo"),
            "[Trash Info]\nPath=x\nDeletionDate=2026-10-06T12:30:00\n",
        )
        .expect("write");
        let at = list_dir(&context, &dir).expect("listed")[0].0.time_deleted;
        let back = jiff::Timestamp::from_second(at)
            .expect("timestamp")
            .to_zoned(jiff::tz::TimeZone::system());
        assert_eq!(
            back.strftime("%Y-%m-%dT%H:%M:%S").to_string(),
            "2026-10-06T12:30:00"
        );
    }

    #[test]
    fn purging_removes_the_item_then_its_trashinfo() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        let item = top.path().join("f");
        fs::write(&item, "x").expect("write");
        let trashed = trash_one(&context, &dir, &item);
        purge_in(&context, &trashed, &|| Ok(())).expect("purged");
        assert!(!dir.path().join("files/f").exists());
        assert!(!dir.path().join("info/f.trashinfo").exists());
    }

    #[test]
    fn purging_an_orphan_removes_it() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        fs::create_dir_all(dir.path().join("files/o/deep")).expect("mkdir");
        let (orphan, _) = list_dir(&context, &dir)
            .expect("listed")
            .pop()
            .expect("listed");
        purge_in(&context, &orphan, &|| Ok(())).expect("purged");
        assert_eq!(
            fs::read_dir(dir.path().join("files")).expect("ls").count(),
            0
        );
    }

    #[test]
    fn a_purge_that_fails_keeps_the_trashinfo_and_says_why() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        let item = top.path().join("d");
        fs::create_dir(&item).expect("mkdir");
        fs::write(item.join("inner"), "x").expect("write");
        let trashed = trash_one(&context, &dir, &item);
        // The folder's contents cannot be removed
        let kept = dir.path().join("files/d");
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o555)).expect("chmod");
        let purged = purge_in(&context, &trashed, &|| Ok(()));
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o755)).expect("chmod");
        if crate::fs_at::cannot_write(&kept).is_some() || purged.is_err() {
            let err = purged.expect_err("could not remove");
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
            assert!(dir.path().join("info/d.trashinfo").exists());
        }
    }

    #[test]
    fn a_cancelled_purge_says_so() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        let item = top.path().join("f");
        fs::write(&item, "x").expect("write");
        let trashed = trash_one(&context, &dir, &item);
        let err = purge_in(&context, &trashed, &|| {
            Err(io::Error::from(io::ErrorKind::Interrupted))
        })
        .expect_err("cancelled");
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert!(dir.path().join("files/f").exists());
    }

    #[test]
    fn a_drive_trash_counts_its_items_not_records_alone() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let has = |context: &Context| {
            drive_trashes(context, top.path())
                .trashes
                .iter()
                .any(|trash| holds_items(trash, true))
        };
        assert!(!has(&context));
        let dir = drive_trash(&context, top.path());
        assert!(!has(&context));
        fs::write(dir.path().join("info/x.trashinfo"), "").expect("write");
        assert!(!has(&context), "a record without its item holds nothing");
        fs::write(dir.path().join("files/x"), "").expect("write");
        assert!(has(&context));
    }

    #[test]
    fn restoring_renames_back_and_takes_the_record() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        fs::create_dir(top.path().join("gone")).expect("mkdir");
        let item = top.path().join("gone/f");
        fs::write(&item, "x").expect("write");
        let trashed = trash_one(&context, &dir, &item);
        fs::remove_dir(top.path().join("gone")).expect("rmdir");
        assert_eq!(
            restore_in(&context, &trashed).expect("restored"),
            Restored::Back
        );
        assert!(item.is_file(), "its folder made again");
        assert!(!dir.path().join("info/f.trashinfo").exists());
    }

    #[test]
    fn a_taken_name_is_left_for_a_move_and_forgotten_after() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        let item = top.path().join("f");
        fs::write(&item, "old").expect("write");
        let trashed = trash_one(&context, &dir, &item);
        fs::write(&item, "new").expect("write");
        assert_eq!(
            restore_in(&context, &trashed).expect("asked"),
            Restored::NeedsMove
        );
        assert_eq!(fs::read_to_string(&item).expect("read"), "new");
        forget_in(&context, &trashed).expect("kept while the item is there");
        assert!(dir.path().join("info/f.trashinfo").exists());
        // The move took it
        fs::remove_file(dir.path().join("files/f")).expect("rm");
        forget_in(&context, &trashed).expect("forgotten");
        assert!(!dir.path().join("info/f.trashinfo").exists());
    }

    #[test]
    fn a_relative_origin_is_read_from_the_drive_root() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        fs::write(dir.path().join("files/x"), "x").expect("write");
        fs::write(
            dir.path().join("info/x.trashinfo"),
            "[Trash Info]\nPath=a/x\nDeletionDate=2026-10-06T12:00:00\n",
        )
        .expect("write");
        assert_eq!(
            origin_in(&context, &dir.path(), OsStr::new("x")),
            Some(top.path().join("a/x"))
        );
    }

    /// An orphan named like a `.trashinfo` is an item, not the record of
    /// the item whose name it carries: deleting it leaves that one alone
    #[test]
    fn purging_an_orphan_named_like_a_trashinfo_takes_only_it() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        let item = top.path().join("report");
        fs::write(&item, "kept").expect("write");
        trash_one(&context, &dir, &item);
        fs::write(dir.path().join("files/report.trashinfo"), "orphan").expect("write");
        let (orphan, _) = list_dir(&context, &dir)
            .expect("listed")
            .into_iter()
            .find(|(item, _)| origin_unknown(item))
            .expect("listed");
        assert_eq!(
            in_trash(&orphan),
            (dir.path().join("files/report.trashinfo"), None)
        );
        purge_in(&context, &orphan, &|| Ok(())).expect("purged");
        assert!(!dir.path().join("files/report.trashinfo").exists());
        assert!(
            dir.path().join("files/report").exists(),
            "the other item stays"
        );
        assert!(dir.path().join("info/report.trashinfo").exists());
    }

    /// Inside a trashed folder, a `*.trashinfo` under a folder named `info`
    /// is still an item of that folder
    #[test]
    fn a_trashinfo_inside_a_trashed_folder_is_an_item() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        // As another test running in parallel may do: look for the trash
        // folders again, which must not forget this one
        crate::trash::refresh_folders();
        let inner = dir.path().join("files/d/info/x.trashinfo");
        fs::create_dir_all(inner.parent().expect("parent")).expect("mkdir");
        fs::write(&inner, "x").expect("write");
        let item = TrashItem {
            id: TrashId::Child(inner.clone()),
            name: "x.trashinfo".into(),
            original_parent: None,
            time_deleted: 0,
        };
        assert_eq!(in_trash(&item), (inner.clone(), None));
        // Deleting it takes only it, as a path inside the trashed folder
        purge_in(&context, &item, &|| Ok(())).expect("purged");
        assert!(!inner.exists());
        assert!(dir.path().join("files/d/info").is_dir());
    }

    /// A trash without its `info/` still shows what its `files/` holds, the
    /// missing folder made as trashing would
    #[test]
    fn a_trash_without_its_info_still_lists_its_items() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        fs::remove_dir(dir.path().join("info")).expect("rmdir");
        fs::write(dir.path().join("files/x"), "x").expect("write");
        let listed = list_dir(&context, &dir).expect("listed");
        assert_eq!(listed.len(), 1);
        assert!(origin_unknown(&listed[0].0));
        assert!(dir.path().join("info").is_dir());
    }

    /// A trash there that cannot be opened fails what empties it, rather
    /// than be left out of an emptying that says it is done
    #[test]
    fn a_trash_that_cannot_be_opened_fails_emptying() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        let dir = drive_trash(&context, top.path());
        fs::write(dir.path().join("files/x"), "x").expect("write");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o000)).expect("chmod");
        let unreadable = fs::read_dir(dir.path()).is_err();
        let found = drive_trashes(&context, top.path());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod");
        if unreadable {
            assert!(found.strict().is_err());
        }
    }

    /// A drive whose folder this user cannot look into holds no trash of
    /// theirs to empty: it does not stop an emptying
    #[test]
    fn a_drive_that_cannot_be_looked_into_does_not_block_emptying() {
        let (data, top) = (drive(), drive());
        let context = context_in(data.path());
        fs::set_permissions(top.path(), fs::Permissions::from_mode(0o000)).expect("chmod");
        let unreadable = fs::read_dir(top.path()).is_err();
        let found = drive_trashes(&context, top.path());
        fs::set_permissions(top.path(), fs::Permissions::from_mode(0o700)).expect("chmod");
        if unreadable {
            assert!(found.strict().expect("not a failure").is_empty());
        }
    }

    /// A home trash that is a link to a folder of any name is followed
    #[test]
    fn a_linked_home_trash_is_followed() {
        let (data, elsewhere) = (drive(), drive());
        let real = elsewhere.path().join("my-trash");
        for sub in ["files", "info"] {
            fs::create_dir_all(real.join(sub)).expect("mkdir");
        }
        fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).expect("chmod");
        fs::write(real.join("files/x"), "x").expect("write");
        std::os::unix::fs::symlink(&real, data.path().join("Trash")).expect("link");
        let context = context_in(data.path());
        let listed = list_dir(&context, &context.home_trash()).expect("listed");
        assert_eq!(listed.len(), 1);
    }

    /// A home trash linked to nothing is no trash: it does not stop an
    /// emptying of the others
    #[test]
    fn a_broken_home_trash_link_does_not_block_emptying() {
        let data = drive();
        std::os::unix::fs::symlink(data.path().join("gone"), data.path().join("Trash"))
            .expect("link");
        let context = context_in(data.path());
        let found = open_all(&context, [context.home_trash()]);
        assert!(found.strict().expect("not a failure").is_empty());
    }

    /// The home trash's drive is found where the home trash really is: a
    /// data folder linked onto a network drive keeps that drive trashable
    #[test]
    fn the_home_exception_follows_links() {
        let (real, link_dir) = (drive(), drive());
        let data = link_dir.path().join("data");
        std::os::unix::fs::symlink(real.path(), &data).expect("link");
        let mountinfo = format!(
            "1 0 8:1 / / rw - ext4 /dev/sda1 rw\n2 1 0:40 / {} rw - nfs4 server:/x rw\n",
            real.path().display()
        );
        let mounts = xdg_kit::trash::parse_mountinfo(mountinfo.as_bytes()).expect("parsed");
        let zone = xdg_kit::trash::LocalZone::system().expect("zone");
        // SAFETY: `getuid` has no preconditions and cannot fail
        let base = Context::new(&data, unsafe { libc::getuid() }, "/", zone).expect("context");
        let context = with_policy(base, &mounts);
        let inside = real.path().join("f");
        assert!(
            !matches!(context.plan(&inside), Err(TrashError::Excluded(_))),
            "the drive the home trash is really on"
        );
    }

    /// What may be trashed onto a drive is what the trash listing looks at:
    /// local drives, never network ones
    #[test]
    fn only_local_drives_take_a_trash() {
        assert!(takes_a_trash("ext4"));
        assert!(takes_a_trash("vfat"));
        assert!(takes_a_trash("fuseblk"));
        for remote in ["nfs4", "cifs", "fuse.sshfs", "fuse.gvfsd-fuse"] {
            assert!(!takes_a_trash(remote), "{remote}");
        }
    }

    /// Network drives are kept out of trashing by their mount point, but
    /// for the one the home trash is on, whose files go there
    #[test]
    fn network_drives_are_excluded_but_the_homes() {
        let mountinfo = b"1 0 8:1 / / rw - ext4 /dev/sda1 rw\n\
            2 1 0:40 / /home rw - nfs4 server:/home rw\n\
            3 1 0:41 / /mnt/share rw - cifs //server/share rw\n";
        let mounts = xdg_kit::trash::parse_mountinfo(mountinfo).expect("parsed");
        let zone = xdg_kit::trash::LocalZone::system().expect("zone");
        let base = Context::new("/home/u/.local/share", 1000, "/", zone).expect("context");
        let context = with_policy(base, &mounts);
        let excluded = |path: &str| matches!(context.plan(path), Err(TrashError::Excluded(_)));
        assert!(excluded("/mnt/share/a"), "a network drive");
        assert!(!excluded("/home/u/a"), "the home trash's drive");
    }

    /// A bind mount is a mount of its own: the innermost mount names a path
    #[test]
    fn a_path_belongs_to_the_innermost_mount() {
        let mountinfo = b"1 0 8:1 / / rw - ext4 /dev/sda1 rw\n\
            2 1 8:2 / /home rw - ext4 /dev/sda2 rw\n\
            3 1 8:1 /x /mnt/bind rw - ext4 /dev/sda1 rw\n";
        let mounts = xdg_kit::trash::parse_mountinfo(mountinfo).expect("parsed");
        let of = |path: &str| mount_of(Path::new(path), &mounts).map(|m| m.mount_point.clone());
        assert_eq!(of("/mnt/bind/a/b"), Some(PathBuf::from("/mnt/bind")));
        assert_eq!(
            of("/home/u/.local/share/Trash"),
            Some(PathBuf::from("/home"))
        );
        assert_eq!(of("/mnt/other"), Some(PathBuf::from("/")));
    }

    /// A trash inside a folder named `files` that has an `info` beside it
    /// is still a trash: its `.trashinfo` is the record, not the item
    #[test]
    fn a_trash_under_a_folder_named_files_keeps_its_records() {
        let (data, outer) = (drive(), drive());
        let context = context_in(data.path());
        fs::create_dir(outer.path().join("info")).expect("mkdir");
        let top = outer.path().join("files/drive");
        fs::create_dir_all(&top).expect("mkdir");
        let dir = drive_trash(&context, &top);
        let item = top.join("x");
        fs::write(&item, "contents").expect("write");
        let trashed = trash_one(&context, &dir, &item);
        assert_eq!(
            in_trash(&trashed),
            (
                dir.path().join("files/x"),
                Some(dir.path().join("info/x.trashinfo"))
            )
        );
    }
}
