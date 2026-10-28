use crate::spawn_detached::spawn_detached;
use crate::ui::iced::futures::{self, StreamExt, stream};
use crate::{FxOrderMap, archive, fl};
use std::borrow::Cow;
use std::fmt::{self, Formatter};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use zip::AesMode::Aes256;

pub use self::controller::{CancelOnDrop, Controller, ControllerState};
pub mod controller;

pub use notifiers::*;
mod notifiers;

pub use self::reader::OpReader;
pub(crate) use crate::fs_at::{cannot_write, rename_no_replace};
pub mod reader;

use self::recursive::{Context, Method};

/// Source and destination paths, paired
type PathPairs = Vec<(PathBuf, PathBuf)>;
pub mod recursive;
pub mod root;

/// What an operation asks the user, and where the answer goes: the app
/// shows it as it sees fit. A question dropped unanswered answers Cancel.
#[derive(Debug)]
pub enum Prompt {
    /// The name `to` is taken: replace it, keep both, skip, or merge two
    /// folders. `totals`: two folders' files and bytes, to compare them by.
    Replace {
        from: PathBuf,
        to: PathBuf,
        totals: Option<((u64, u64), (u64, u64))>,
        /// Whether "Apply to all" is worth offering.
        multiple: bool,
        conflict_count: usize,
        answer: mpsc::Sender<ReplaceResult>,
    },
    /// A path the operation `controller` controls may not touch.
    Blocked {
        controller: Controller,
        blocked: Blocked,
        ask: Ask,
        answer: mpsc::Sender<BlockedAnswer>,
    },
}

/// The kind of archive a compress makes.
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

/// Where an operation's prompts go.
pub type Prompts = mpsc::UnboundedSender<Prompt>;

async fn handle_replace(
    prompts: Prompts,
    file_from: PathBuf,
    file_to: PathBuf,
    multiple: bool,
    conflict_count: usize,
) -> ReplaceResult {
    // Two folders are compared by what they hold
    let totals = compio::runtime::spawn_blocking({
        let (file_from, file_to) = (file_from.clone(), file_to.clone());
        move || {
            (file_from.is_dir() && file_to.is_dir())
                .then(|| (folder_totals(&file_from), folder_totals(&file_to)))
        }
    })
    .await
    .ok()
    .flatten();

    let (tx, mut rx) = mpsc::channel(1);
    let _ = prompts.send(Prompt::Replace {
        from: file_from,
        to: file_to,
        totals,
        multiple,
        conflict_count,
        answer: tx,
    });
    rx.recv().await.unwrap_or(ReplaceResult::Cancel)
}

/// Ask what to do about a path the operation may not touch, and wait for
/// the answer. A dropped question counts as Cancel.
async fn handle_blocked(
    prompts: Prompts,
    controller: Controller,
    blocked: Blocked,
    ask: Ask,
) -> BlockedAnswer {
    let (tx, mut rx) = mpsc::channel(1);
    let _ = prompts.send(Prompt::Blocked {
        controller,
        blocked,
        ask,
        answer: tx,
    });
    rx.recv().await.unwrap_or(BlockedAnswer::Cancel)
}

fn get_directory_name(file_name: &str) -> &str {
    for ext in crate::archive::SUPPORTED_EXTENSIONS {
        if let Some(stripped) = file_name.strip_suffix(ext) {
            return stripped;
        }
    }
    file_name
}

/// Extract one archive on the blocking pool and flush what it wrote to disk.
async fn extract_archive(
    path: PathBuf,
    dir: PathBuf,
    password: &Option<String>,
    controller: &Controller,
) -> Result<(), OperationError> {
    let password = password.clone();
    let controller_clone = controller.clone();
    let (written_files, target_dirs) = compio::runtime::spawn_blocking(move || {
        crate::archive::extract(&path, &dir, &password, &controller_clone)
    })
    .await
    .map_err(wrap_compio_spawn_error)??;
    if !written_files.is_empty() || !target_dirs.is_empty() {
        sync_to_disk(written_files, target_dirs)
            .await
            .map_err(|(path, err)| OperationError::at(&path, &err))?;
    }
    Ok(())
}

/// Create a unique hidden directory inside `to` to stage an extraction in.
/// Whether nothing is at `path`: not found, rather than unreadable, which
/// may be put right before a retry.
fn surely_gone(path: &Path) -> bool {
    path.symlink_metadata()
        .is_err_and(|err| err.kind() == io::ErrorKind::NotFound)
}

/// The turn of one permission change to a file, held until it is dropped.
struct PermissionsTurn(PathBuf);

/// The files whose permissions are being changed now.
static PERMISSIONS_CHANGING: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
/// Wakes the changes waiting for their turn.
static PERMISSIONS_FREED: std::sync::Condvar = std::sync::Condvar::new();

impl PermissionsTurn {
    /// Waits until no other change to `path` runs, and takes the turn.
    fn take(path: &Path) -> Self {
        let mut changing = PERMISSIONS_CHANGING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while changing.iter().any(|other| other == path) {
            changing = PERMISSIONS_FREED
                .wait(changing)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        changing.push(path.to_path_buf());
        Self(path.to_path_buf())
    }
}

impl Drop for PermissionsTurn {
    fn drop(&mut self) {
        let mut changing = PERMISSIONS_CHANGING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        changing.retain(|other| *other != self.0);
        PERMISSIONS_FREED.notify_all();
    }
}

/// A new folder in `to` to extract an archive into, named `dir_name`, or
/// numbered when that is taken. Made here, so it is the extraction's own
/// and nothing else's, to take away again should the extraction fail. A
/// name that is not one of its own, such as `..` from `...zip`, gives way
/// to `fallback`, the archive's name.
fn extract_dir(to: &Path, dir_name: &str, fallback: &str) -> Result<PathBuf, OperationError> {
    let dir_name = if matches!(dir_name, "" | "." | "..") {
        fallback
    } else {
        dir_name
    };
    let copy = fl!("copy_noun");
    for n in 0..=MAX_UNIQUE_ATTEMPTS {
        let name = if n == 0 {
            dir_name.to_string()
        } else {
            format!("{dir_name} ({copy} {n})")
        };
        let path = to.join(name);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(OperationError::from_err(err)),
        }
    }
    Err(OperationError::from_err(io::Error::from(
        io::ErrorKind::AlreadyExists,
    )))
}

fn staging_dir(to: &Path, dir_name: &str) -> Result<PathBuf, OperationError> {
    for n in 0.. {
        let name = if n == 0 {
            format!(".{dir_name}.extracting")
        } else {
            format!(".{dir_name}.extracting.{n}")
        };
        let path = to.join(name);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(OperationError::from_err(err)),
        }
    }
    unreachable!()
}

/// What a copy or move could not do to a path, for lack of permission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Blocked {
    /// A file could not be read, so it could not be copied or moved.
    Read(PathBuf),
    /// A folder could not be listed, so none of it could be copied or moved.
    List(PathBuf),
    /// An original could not be removed after it was copied (move only).
    Remove(PathBuf),
    /// A link could not be made, because the destination's filesystem
    /// cannot hold links at all: FAT32 and exFAT, as on most USB sticks.
    /// `fs` names that filesystem when it is known.
    Link {
        path: PathBuf,
        fs: Option<&'static str>,
    },
    /// The folder everything goes into cannot be written to by this user.
    Destination(PathBuf),
    /// A step failed while the operation ran, for a reason in `reason`:
    /// the drive filled up, went away, or could not be read or written.
    Failed { path: PathBuf, reason: String },
    /// A file is larger than the filesystem it goes to can hold: FAT keeps
    /// files under 4 GB.
    TooBig { path: PathBuf, fs: &'static str },
    /// A name the filesystem it goes to cannot hold: FAT, exFAT and NTFS
    /// refuse some characters.
    BadName { path: PathBuf, fs: &'static str },
    /// A selected item cannot be moved, found before anything of it is
    /// touched: it could be copied, but its originals could not be removed
    /// from `folder`, for lack of permission or, when `read_only`, because
    /// that filesystem is read-only.
    Move {
        path: PathBuf,
        folder: PathBuf,
        read_only: bool,
    },
    /// An item to delete, or to remove from the trash, that this user may
    /// not remove from its folder. Root deletes it for good.
    Delete(PathBuf),
    /// An item to trash on a drive with no trash of its own that can be used
    /// or made: nothing is ever copied into another drive's trash.
    NoTrash(PathBuf),
}

impl Blocked {
    /// The path the question is about.
    pub fn path(&self) -> &Path {
        match self {
            Self::Read(path)
            | Self::List(path)
            | Self::Remove(path)
            | Self::Destination(path)
            | Self::Delete(path)
            | Self::NoTrash(path) => path,
            Self::Link { path, .. }
            | Self::Failed { path, .. }
            | Self::TooBig { path, .. }
            | Self::BadName { path, .. }
            | Self::Move { path, .. } => path,
        }
    }

    /// Whether root could do what this user may not: only a lack of
    /// permission. Root cannot make a drive writable, or FAT hold a link.
    pub fn root_can_help(&self) -> bool {
        match self {
            Self::Read(_)
            | Self::List(_)
            | Self::Remove(_)
            | Self::Destination(_)
            | Self::Delete(_) => true,
            Self::Move { read_only, .. } => !read_only,
            Self::Link { .. }
            | Self::TooBig { .. }
            | Self::BadName { .. }
            | Self::Failed { .. }
            | Self::NoTrash(_) => false,
        }
    }

    /// Whether trying the same step again could succeed. A filesystem that
    /// cannot hold links never will.
    pub fn can_retry(&self) -> bool {
        !matches!(
            self,
            Self::Link { .. }
                | Self::TooBig { .. }
                | Self::BadName { .. }
                | Self::Move { .. }
                | Self::Destination(_)
                | Self::Delete(_)
                | Self::NoTrash(_)
        )
    }
}

/// The name of the filesystem `dir` is on, for the ones a question or an
/// error names: those that cannot hold links or large files.
pub(crate) fn filesystem_name(dir: &Path) -> Option<&'static str> {
    // From `linux/magic.h`, and `fs/ntfs3` for NTFS
    const MSDOS_SUPER_MAGIC: i64 = 0x4d44;
    const EXFAT_SUPER_MAGIC: i64 = 0x2011_bab0;
    const NTFS3_SUPER_MAGIC: i64 = 0x7366_746e;
    const FUSE_SUPER_MAGIC: i64 = 0x6573_5546;

    match filesystem_magic(dir)? {
        MSDOS_SUPER_MAGIC => Some("FAT"),
        EXFAT_SUPER_MAGIC => Some("exFAT"),
        NTFS3_SUPER_MAGIC => Some("NTFS"),
        // A drive mounted through FUSE, as exfat-fuse and ntfs-3g do: the
        // magic only says FUSE, but udev knows what is on the drive
        FUSE_SUPER_MAGIC => match fuse_drive_type(dir)?.as_str() {
            "vfat" => Some("FAT"),
            "exfat" => Some("exFAT"),
            "ntfs" => Some("NTFS"),
            _ => None,
        },
        _ => None,
    }
}

/// The filesystem udev found on the drive a FUSE mount at `dir` reads,
/// such as `exfat`. Only a block device's FUSE mount has one: its device
/// number is the drive's.
fn fuse_drive_type(dir: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    let dev = fs::metadata(dir).ok()?.dev();
    let data = fs::read_to_string(format!(
        "/run/udev/data/b{}:{}",
        libc::major(dev),
        libc::minor(dev)
    ))
    .ok()?;
    data.lines()
        .find_map(|line| line.strip_prefix("E:ID_FS_TYPE="))
        .map(str::to_owned)
}

/// The magic number `statfs` gives for the filesystem `dir` is on.
pub(crate) fn filesystem_magic(dir: &Path) -> Option<i64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir_c = CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the pointer comes from a CString that outlives the call, and
    // `stat` is only read after the call filled it
    if unsafe { libc::statfs(dir_c.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statfs` returned 0, so it filled `stat`
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::useless_conversion)]
    Some(i64::from(stat.f_type))
}

/// Why a step failed, in words, for the error the user sees: `"name": the
/// reason`. The I/O error inside `err` decides the reason; one this does not
/// know gives its own message.
pub(crate) fn failure_text(path: &Path, err: &(dyn std::error::Error + 'static)) -> String {
    Cause::of(err).text(path)
}

/// What an error at a path came down to: the I/O error inside it, if there
/// is one, and its own message. Kept apart from the words the user reads,
/// which are made when it is shown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cause {
    pub io: Option<io::ErrorKind>,
    pub message: String,
}

impl Cause {
    pub fn of(err: &(dyn std::error::Error + 'static)) -> Self {
        let mut source = Some(err);
        let io = std::iter::from_fn(|| {
            let err = source?;
            source = err.source();
            Some(err)
        })
        .find_map(|err| err.downcast_ref::<io::Error>());
        match io {
            Some(io) => Self {
                io: Some(io.kind()),
                message: io.to_string(),
            },
            None => Self {
                io: None,
                message: err.to_string(),
            },
        }
    }

    /// `"name": the reason`, for `path`.
    fn text(&self, path: &Path) -> String {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let reason = match self.io {
            Some(io::ErrorKind::PermissionDenied) => fl!("reason-no-permission"),
            Some(io::ErrorKind::StorageFull) => fl!("reason-drive-full"),
            Some(io::ErrorKind::ReadOnlyFilesystem) => fl!("reason-read-only"),
            Some(io::ErrorKind::NotFound) => fl!("reason-gone"),
            Some(io::ErrorKind::FileTooLarge) => fl!("reason-too-big"),
            _ => self.message.clone(),
        };
        fl!("failed-path", name = name, reason = reason)
    }
}

/// How a [`Blocked`] question is asked: what it offers besides Skip and
/// Cancel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Ask {
    /// How many questions of its kind are left in the operation, this one
    /// included: "Same for the rest" is offered above one. A failure cannot
    /// be foreseen, so for one it is how many steps are left.
    pub count: usize,
    /// Whether Retry is offered: only while the operation runs, when a step
    /// can change.
    pub retry: bool,
    /// Whether root is offered, and if so whether it is already granted in
    /// this operation: "Retry as root" asks for the password, "Use root
    /// again" does not.
    pub root: Option<bool>,
    /// The last "Retry as root" was not granted: the prompt was dismissed or
    /// the password refused.
    pub not_granted: bool,
    /// Whether Skip is offered. A delete step that failed mid-way offers
    /// Cancel, Abort and Try again instead.
    pub skip: bool,
    /// Whether Abort is offered: stop here and keep what is done.
    pub abort: bool,
    /// Whether "Delete permanently" is offered: for an item the trash cannot
    /// take.
    pub permanently: bool,
}

/// The answer to a [`Blocked`] question.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockedAnswer {
    /// Try the same step again.
    Retry,
    /// Leave the path out; `true` also leaves out every later path of this
    /// operation that is blocked, without asking.
    Skip(bool),
    /// Stop the operation, keeping what it has done.
    Cancel,
    /// Do it as root; `true` also does every later path of this kind as root,
    /// without asking.
    RetryAsRoot(bool),
    /// Stop the operation here and keep what it has done.
    Abort,
    /// Delete it for good, as the trash cannot take it; `true` does the same
    /// for every later item of this kind, without asking.
    Permanently(bool),
}

/// Whether the folder `dir` can take new entries at all. A read-only
/// filesystem stops an operation before it starts: no one could write
/// there. Lack of permission is asked about with the other problems; any
/// other answer (the folder is gone, the filesystem does not say) leaves it
/// to the copy itself, as before.
fn check_writable(dir: &Path) -> Result<(), String> {
    if cannot_write(dir) != Some(true) {
        return Ok(());
    }
    let folder = dir.file_name().map_or_else(
        || dir.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    Err(fl!("destination-read-only", folder = folder))
}

/// How much room the filesystem `dir` is on has for this user, in bytes.
pub(crate) fn free_space(dir: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir_c = CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the pointer comes from a CString that outlives the call, and
    // `stat` is only read after the call filled it
    if unsafe { libc::statvfs(dir_c.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statvfs` returned 0, so it filled `stat`
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::useless_conversion)]
    Some(u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize)))
}

/// Whether this user clearly may not read `path`: only a refusal for lack
/// of permission counts.
pub(crate) fn cannot_read(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let Ok(path_c) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: the pointer comes from a CString that outlives the call
    let refused = unsafe { libc::access(path_c.as_ptr(), libc::R_OK) } != 0;
    refused && io::Error::last_os_error().kind() == io::ErrorKind::PermissionDenied
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReplaceResult {
    Replace(bool),
    KeepBoth,
    Skip(bool),
    Cancel,
    /// For a folder whose name is taken by a folder: put its contents into
    /// the one there. `true` merges every later such folder too.
    Merge(bool),
}

/// A folder's files and their size in all, walking all of it; for a dialog
/// comparing two folders. Blocking.
pub(crate) fn folder_totals(dir: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0, 0);
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                folders.push(entry.path());
            } else {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    (files, bytes)
}

/// `merge_folders`: a folder whose name is taken by a folder goes into it
/// without asking, as an extraction's do.
async fn copy_or_move(
    paths: Vec<PathBuf>,
    to: PathBuf,
    method: Method,
    merge_folders: bool,
    prompts: &Prompts,
    controller: Controller,
) -> Result<OperationSelection, OperationError> {
    let prompts = prompts.clone();
    let controller_c = controller.clone();

    compio::runtime::spawn(async move {
        let controller = controller_c;
        log::info!(
            "{} {:?} to {}",
            match method {
                Method::Copy => "Copy",
                Method::Move { .. } => "Move",
            },
            paths,
            to.display()
        );

        // A folder cannot go inside itself. The rename fails with EINVAL, and
        // the recursive fallback would then copy the tree into its own
        // subfolder and be unable to remove the source it had just filled.
        if let Some(from) = paths.iter().find(|from| to.starts_with(from)) {
            log::warn!("refusing to put {} inside itself", from.display());
            return Err(OperationError::from_err(fl!("into-itself")));
        }

        // A folder that cannot take new entries fails the whole operation
        // before anything is touched, rather than on its first file
        let writable = {
            let to = to.clone();
            compio::runtime::spawn_blocking(move || check_writable(&to))
                .await
                .map_err(wrap_compio_spawn_error)?
        };
        if let Err(reason) = writable {
            log::warn!("refusing to write into {}: {reason}", to.display());
            return Err(OperationError::from_err(reason));
        }

        // Handle duplicate file names by renaming paths
        let from_to_pairs_iter = paths
            .into_iter()
            .zip(std::iter::repeat(to.as_path()))
            .filter_map(|(from, to)| {
                if matches!(from.parent(), Some(parent) if parent == to)
                    && matches!(method, Method::Copy)
                {
                    // `from`'s parent is equal to `to` which means we're copying to the same
                    // directory (duplicating files)
                    let to = copy_unique_path(&from, to);
                    Some((from, to))
                } else if let Some(name) = from.file_name() {
                    let to = to.join(name);
                    Some((from, to))
                } else {
                    None
                }
            });

        // Attempt quick and simple renames

        // Pairs still to copy or move recursively, and pairs a plain rename
        // already handled
        let (from_to_pairs, renamed): (PathPairs, PathPairs) =
            if matches!(method, Method::Move { .. }) {
                from_to_pairs_iter
                    .map(|(from, to)| async move {
                        // `rename_no_replace`, not `exists` and then `rename`:
                        // `rename(2)` replaces whatever is at the destination,
                        // and `exists` follows symlinks, so a dangling one read
                        // as free and was overwritten with no conflict to
                        // answer. `renameat2` makes the test and the move one
                        // step, which also closes the gap between them where a
                        // second move of the same name could land.
                        let renamed = {
                            let (from, to) = (from.clone(), to.clone());
                            compio::runtime::spawn_blocking(move || rename_no_replace(&from, &to))
                                .await
                                .map_err(wrap_compio_spawn_error)
                        };
                        match renamed {
                            Ok(Ok(())) => {
                                log::info!("renamed {} to {}", from.display(), to.display());
                                Err((from, to))
                            }
                            Ok(Err(err)) => {
                                log::info!(
                                    "failed to rename {} to {}, fallback to recursive move: {}",
                                    from.display(),
                                    to.display(),
                                    err
                                );
                                Ok((from, to))
                            }
                            Err(err) => {
                                log::warn!(
                                    "failed to run the rename of {} to {}, fallback to recursive move: {}",
                                    from.display(),
                                    to.display(),
                                    err
                                );
                                Ok((from, to))
                            }
                        }
                    })
                    .collect::<crate::ui::iced::futures::stream::FuturesOrdered<_>>()
                    .fold(
                        (Vec::new(), Vec::new()),
                        |(mut pairs, mut renamed), pair| async move {
                            match pair {
                                Ok(pair) => pairs.push(pair),
                                Err(pair) => renamed.push(pair),
                            }
                            (pairs, renamed)
                        },
                    )
                    .await
            } else {
                (from_to_pairs_iter.collect(), Vec::new())
            };

        // Renamed entries count as moved for the selection and for undo. The
        // rename is only taken when the destination did not exist, so each of
        // these is a genuine creation.
        let mut op_sel = OperationSelection::default();
        for (from, to) in &renamed {
            op_sel.ignored.push(from.clone());
            op_sel.selected.push(to.clone());
        }

        recursive_pairs(
            from_to_pairs,
            method,
            renamed,
            op_sel,
            merge_folders,
            prompts,
            controller,
        )
        .await
    })
    .await
    .map_err(wrap_compio_spawn_error)?
}

/// Brings trashed items back to where they were: by the trash's own rename
/// where nothing is in the way, the folders they were in made again where
/// gone; the rest as a move out of the trash, where a name taken there is
/// asked about as for any move, and so is a folder this user may not write
/// to. An item all back leaves the trash, its `.trashinfo` with it.
async fn restore(
    items: Vec<crate::trashing::TrashItem>,
    prompts: &Prompts,
    controller: Controller,
) -> Result<OperationSelection, OperationError> {
    let prompts = prompts.clone();
    compio::runtime::spawn(async move {
        let mut op_sel = OperationSelection::default();
        // Brought back by a rename, to go back into the trash if cancelled
        let mut back = Vec::new();
        let mut pairs = Vec::new();
        let mut stopped = None;
        for item in &items {
            if let Err(state) = controller.check().await {
                stopped = Some(state);
                break;
            }
            let (from, _) = crate::trashing::in_trash(item);
            let Some(to) = item.original_path() else {
                // Nowhere known to go back to: deleting is all it allows
                return Err(OperationError {
                    partial: Box::new(op_sel),
                    ..OperationError::from_msg(fl!(
                        "failed-path",
                        name = item.name.to_string_lossy(),
                        reason = fl!("dimmed-origin-unknown")
                    ))
                });
            };
            let restored = compio::runtime::spawn_blocking({
                let item = item.clone();
                move || crate::trashing::restore(&item)
            })
            .await
            .map_err(wrap_compio_spawn_error)?;
            if let Ok(crate::trashing::Restored::Back) = restored {
                op_sel.moved.push((from.clone(), to.clone()));
                op_sel.ignored.push(from);
                op_sel.created.push(to.clone());
                op_sel.selected.push(to.clone());
                back.push((item.clone(), to));
            } else {
                if let Err(err) = &restored {
                    log::info!("{} goes back by a move: {err}", to.display());
                }
                pairs.push((item.clone(), from, to));
            }
        }
        // Cancelled while the last restore ran: as if before it
        if stopped.is_none() && controller.is_cancelled() {
            stopped = Some(ControllerState::Cancelled);
        }
        let result = if let Some(state) = stopped {
            // Cancelled: what came back goes into the trash again. Aborted:
            // it stays, and can be undone
            if state == ControllerState::Cancelled && !controller.is_aborted() {
                op_sel = put_back_restored(back).await?;
            }
            Err(OperationError {
                partial: Box::new(op_sel),
                ..OperationError::from_state(state)
            })
        } else if pairs.is_empty() {
            Ok(op_sel)
        } else {
            // The folders they were in, made again where gone
            let folders: Vec<PathBuf> = pairs
                .iter()
                .filter_map(|(_, _, to)| to.parent().map(Path::to_path_buf))
                .collect();
            compio::runtime::spawn_blocking(move || {
                for folder in folders {
                    if let Err(err) = fs::create_dir_all(&folder) {
                        log::warn!("failed to make {} again: {err}", folder.display());
                    }
                }
            })
            .await
            .map_err(wrap_compio_spawn_error)?;
            let moves = pairs
                .iter()
                .map(|(_, from, to)| (from.clone(), to.clone()))
                .collect();
            // Not handed what the trash brought back: cancelled, the move
            // renames back into `files/` what it was given as done, and
            // those have no record there any more
            let moved = recursive_pairs(
                moves,
                Method::Move {
                    cross_device_copy: false,
                },
                Vec::new(),
                OperationSelection::default(),
                false,
                prompts,
                controller.clone(),
            )
            .await;
            match moved {
                Ok(moved) => {
                    op_sel.absorb(moved);
                    Ok(op_sel)
                }
                // The move put back its own; what the trash brought back
                // goes back into it with a record
                // Asked, though putting back the move's own may have failed
                // since and turned the state to failed: what the trash
                // brought back can still go back
                Err(err) if controller.cancel_asked() && !controller.is_aborted() => {
                    let not_back = put_back_restored(back).await?;
                    Err(err.after(not_back))
                }
                // What the renames brought back is still back
                Err(err) => Err(err.after(op_sel)),
            }
        };

        // Whatever the move came to, an item no longer in the trash leaves
        // its listing
        let moved: Vec<_> = pairs.into_iter().map(|(item, ..)| item).collect();
        compio::runtime::spawn_blocking(move || {
            for item in moved {
                if let Err(err) = crate::trashing::forget(&item) {
                    log::warn!(
                        "failed to remove the record of {}: {err}",
                        item.named_path().display()
                    );
                }
            }
        })
        .await
        .map_err(wrap_compio_spawn_error)?;
        result
    })
    .await
    .map_err(wrap_compio_spawn_error)?
}

/// Sends what a restore brought back by the trash's own rename back into
/// that trash, as a cancel leaves it: with a record again, so it can be
/// restored. Returns what could not go back, as still restored.
async fn put_back_restored(
    back: Vec<(crate::trashing::TrashItem, PathBuf)>,
) -> Result<OperationSelection, OperationError> {
    let not_back = compio::runtime::spawn_blocking(move || {
        back.into_iter()
            .filter(|(item, to)| crate::trashing::put_back(item, to).is_err())
            .collect::<Vec<_>>()
    })
    .await
    .map_err(wrap_compio_spawn_error)?;
    let mut op_sel = OperationSelection::default();
    for (item, to) in not_back {
        log::warn!("failed to put {} back in the trash", to.display());
        op_sel
            .moved
            .push((crate::trashing::in_trash(&item).0, to.clone()));
        op_sel.created.push(to.clone());
        op_sel.selected.push(to);
    }
    Ok(op_sel)
}

/// Empties `bins`: every item for good, as [`purge`] does. A record left
/// without its item stays: it holds nothing, and another program may be
/// trashing that item right now.
async fn empty(
    bins: Vec<crate::trashing::Bin>,
    prompts: &Prompts,
    controller: Controller,
) -> Result<OperationSelection, OperationError> {
    let listed = compio::runtime::spawn_blocking(move || {
        let mut items = Vec::new();
        for bin in &bins {
            match crate::trashing::list_bin(bin) {
                Ok(listed) => items.extend(listed.into_iter().map(|(item, _)| item)),
                Err(err) => return Err((bin.path().join("files"), err)),
            }
        }
        Ok(items)
    })
    .await
    .map_err(wrap_compio_spawn_error)?
    .map_err(|(path, err)| OperationError::at(&path, &err))?;
    purge(listed, prompts, controller).await
}

/// Removes items from the trash for good, asking about what this user may
/// not remove: see [`Context::purge`].
async fn purge(
    items: Vec<crate::trashing::TrashItem>,
    prompts: &Prompts,
    controller: Controller,
) -> Result<OperationSelection, OperationError> {
    let prompts = prompts.clone();
    compio::runtime::spawn(async move {
        let blocked_controller = controller.clone();
        let mut context = Context::new(controller).on_blocked(move |blocked, ask| {
            Box::pin(handle_blocked(
                prompts.clone(),
                blocked_controller.clone(),
                blocked,
                ask,
            ))
        });
        let items = items
            .into_iter()
            .map(|item| {
                let (files, info) = crate::trashing::in_trash(&item);
                (item, files, info)
            })
            .collect();
        context.purge(items).await?;
        Ok(OperationSelection::default())
    })
    .await
    .map_err(wrap_compio_spawn_error)?
}

/// Deletes `paths`, to the trash or for good when `permanently`, asking
/// the app what it cannot do: see [`Context::delete`].
async fn delete(
    paths: Vec<PathBuf>,
    permanently: bool,
    prompts: &Prompts,
    controller: Controller,
) -> Result<OperationSelection, OperationError> {
    let prompts = prompts.clone();
    compio::runtime::spawn(async move {
        let mut context = Context::new(controller.clone());
        let blocked_controller = controller.clone();
        context = context.on_blocked(move |blocked, ask| {
            Box::pin(handle_blocked(
                prompts.clone(),
                blocked_controller.clone(),
                blocked,
                ask,
            ))
        });
        context.delete(paths, permanently).await?;
        Ok(context.take_selection())
    })
    .await
    .map_err(wrap_compio_spawn_error)?
}

/// Copy or move exactly these `(from, to)` pairs, each `to` a whole
/// destination path rather than a folder to go into.
///
/// `renamed` is what a move already renamed into place whole, and `op_sel`
/// what the caller has already selected; the run adds to them.
async fn recursive_pairs(
    pairs: PathPairs,
    method: Method,
    renamed: PathPairs,
    op_sel: OperationSelection,
    merge_folders: bool,
    prompts: Prompts,
    controller: Controller,
) -> Result<OperationSelection, OperationError> {
    let mut context = Context::new(controller.clone());
    context.op_sel = op_sel;
    context.renamed_before(renamed);
    context.merge_folders = merge_folders;

    context = context.on_progress(move |_op, progress| {
        let item_progress = match progress.total_bytes {
            Some(total_bytes) => {
                if total_bytes == 0 {
                    1.0
                } else {
                    progress.current_bytes as f32 / total_bytes as f32
                }
            }
            None => 0.0,
        };
        let total_progress =
            (item_progress + progress.current_ops as f32) / progress.total_ops as f32;
        controller.set_progress(total_progress);
    });

    let blocked_tx = prompts.clone();
    context = context.on_replace(move |op, conflict_count| {
        let prompts = prompts.clone();
        Box::pin(handle_replace(
            prompts,
            op.from.clone(),
            op.to.clone(),
            true,
            conflict_count,
        ))
    });

    let blocked_controller = context.controller();
    context = context.on_blocked(move |blocked, ask| {
        Box::pin(handle_blocked(
            blocked_tx.clone(),
            blocked_controller.clone(),
            blocked,
            ask,
        ))
    });

    context.recursive_copy_or_move(pairs, method).await?;

    Ok(context.take_selection())
}

/// Flushes what was written to disk: the files, then the folders that
/// gained entries. The first failure is returned with its path: what it
/// wrote may not be on the drive. A filesystem that cannot flush, and a
/// path that is gone, are not failures.
pub async fn sync_to_disk(
    written_files: Vec<PathBuf>,
    target_dirs: std::collections::HashSet<PathBuf>,
) -> Result<(), (PathBuf, io::Error)> {
    /// A written file that is a link is not followed: opening it opens what
    /// it points to, which can be a loop or a pipe, and the link itself is
    /// kept by its folder. A folder is reached however its path goes,
    /// through links too.
    async fn sync(path: PathBuf, follow: bool) -> Result<(), (PathBuf, io::Error)> {
        let meta = if follow {
            compio::fs::metadata(&path).await
        } else {
            compio::fs::symlink_metadata(&path).await
        };
        let result = match meta {
            // Only files and folders
            Ok(meta) if !(meta.is_file() || meta.is_dir()) => return Ok(()),
            // Read-only: a copy can be read-only, and fsync needs no more
            Ok(_) => match compio::fs::OpenOptions::new().read(true).open(&path).await {
                Ok(file) => file.sync_all().await,
                Err(err) => Err(err),
            },
            Err(err) => Err(err),
        };
        match result {
            Err(err) if !cannot_sync(&err) => Err((path, err)),
            _ => Ok(()),
        }
    }

    stream::iter(written_files.into_iter().map(|path| sync(path, false)))
        .buffer_unordered(32)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<(), _>>()?;
    stream::iter(target_dirs.into_iter().map(|path| sync(path, true)))
        .buffer_unordered(16)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect()
}

/// Whether `err` from opening or flushing a path only says the flush cannot
/// happen, not that written data was lost: the path is gone or not ours to
/// open, or the filesystem does not flush.
pub(crate) fn cannot_sync(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
    ) || err.raw_os_error() == Some(libc::EINVAL)
}

/// The created paths of `result`, with any path that sits inside another one
/// dropped: removing the outermost entry takes its contents with it.
fn created_only(result: &OperationSelection) -> Vec<PathBuf> {
    let mut created: Vec<PathBuf> = result.created.clone();
    created.sort();
    let mut roots: Vec<PathBuf> = Vec::with_capacity(created.len());
    for path in created {
        if roots.last().is_some_and(|root| path.starts_with(root)) {
            continue;
        }
        roots.push(path);
    }
    roots
}

impl Operation {
    /// The operations that undo this one after it completed with `result`,
    /// or none when it cannot be undone. Several are returned when the
    /// originals came from different folders.
    pub fn undo(&self, result: &OperationSelection) -> Vec<Operation> {
        match self {
            Self::Rename { from, to } => vec![Self::Rename {
                from: to.clone(),
                to: from.clone(),
            }],
            Self::BatchRename { renames } => {
                // Only the renames that went through are reversed, last first
                let done =
                    |to: &PathBuf| result.selected.is_empty() || result.selected.contains(to);
                let renames: Vec<_> = renames
                    .iter()
                    .rev()
                    .filter(|(_, to)| done(to))
                    .map(|(from, to)| (to.clone(), from.clone()))
                    .collect();
                if renames.is_empty() {
                    Vec::new()
                } else {
                    vec![Self::BatchRename { renames }]
                }
            }
            Self::Move { .. } => {
                // Each moved item goes back to the folder it came from, taken
                // from the pairs the move recorded. Not by matching basenames
                // against the sources: Keep Both renames the destination, so
                // the name that matches is the file that was already there, and
                // undoing would carry that one off instead.
                //
                // Outermost destination first, with anything inside one of
                // those left to ride along with it. A merge into a folder that
                // already existed records only the children it transferred, so
                // what the folder held before stays where it is.
                let mut moved = result.moved.clone();
                moved.sort_by(|(_, a), (_, b)| a.cmp(b));
                let mut roots: Vec<PathBuf> = Vec::with_capacity(moved.len());
                let mut by_parent: FxOrderMap<PathBuf, Vec<PathBuf>> = FxOrderMap::default();
                let mut renamed: Vec<Self> = Vec::new();
                for (from, to) in moved {
                    if roots.last().is_some_and(|root| to.starts_with(root)) {
                        continue;
                    }
                    if from.file_name() == to.file_name() {
                        if let Some(parent) = from.parent() {
                            by_parent
                                .entry(parent.to_path_buf())
                                .or_default()
                                .push(to.clone());
                        }
                    } else {
                        // Keep Both gave the destination a name of its own, and
                        // a move into a folder cannot take that back, so this
                        // one goes home as a rename to the whole path it came
                        // from. On its own, because the parts of an undo all
                        // run at once: none of them may wait on another.
                        renamed.push(Self::Rename {
                            from: to.clone(),
                            to: from,
                        });
                    }
                    roots.push(to);
                }
                // Then what the move replaced comes back from the trash
                let restore = (!result.trash_items.is_empty()).then(|| Self::Restore {
                    items: result.trash_items.clone(),
                });
                by_parent
                    .into_iter()
                    .map(|(to, paths)| Self::Move {
                        paths,
                        to,
                        cross_device_copy: false,
                    })
                    .chain(renamed)
                    .chain(restore)
                    .collect()
            }
            // Only the paths the copy created are removed, and they go to the
            // trash rather than being destroyed, so an undo of a copy that
            // replaced or merged into existing files is still recoverable
            // The copies go to the trash, those that replaced files too; then
            // the replaced files come back from it, in their place
            Self::Copy { .. } | Self::Extract { .. }
                if !created_only(result).is_empty() || !result.trash_items.is_empty() =>
            {
                let mut paths = created_only(result);
                // Only a replaced file's place that something holds: one whose
                // replacement was left out, and the original could not come
                // back, is empty, and only the original is to restore
                for path in result
                    .trash_items
                    .iter()
                    .filter_map(crate::trashing::TrashItem::original_path)
                    .filter(|path| path.symlink_metadata().is_ok())
                {
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                }
                let mut undo = Vec::new();
                if !paths.is_empty() {
                    undo.push(Self::Delete { paths });
                }
                if !result.trash_items.is_empty() {
                    undo.push(Self::Restore {
                        items: result.trash_items.clone(),
                    });
                }
                undo
            }
            // A new folder may have been filled since it was created, so an
            // undo trashes it rather than destroying it and its contents
            Self::NewFile { path } | Self::NewFolder { path } => vec![Self::Delete {
                paths: vec![path.clone()],
            }],
            // Pasted content is the only copy of what was on the clipboard, so
            // undoing a paste puts it in the trash rather than destroying it
            Self::WriteFile { .. } if !created_only(result).is_empty() => vec![Self::Delete {
                paths: created_only(result),
            }],
            Self::Delete { .. } if !result.trash_items.is_empty() => vec![Self::Restore {
                items: result.trash_items.clone(),
            }],
            // What came back goes to the trash again, and what it replaced
            // comes back
            Self::Restore { .. } if !created_only(result).is_empty() => {
                let mut paths = created_only(result);
                for path in result
                    .trash_items
                    .iter()
                    .filter_map(crate::trashing::TrashItem::original_path)
                {
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                }
                let mut undo = vec![Self::Delete { paths }];
                if !result.trash_items.is_empty() {
                    undo.push(Self::Restore {
                        items: result.trash_items.clone(),
                    });
                }
                undo
            }
            Self::Compress { to, .. } => vec![Self::Delete {
                paths: vec![to.clone()],
            }],
            _ => Vec::new(),
        }
    }

    /// This operation with what `done` already covers taken out, for
    /// retrying it after a failure part-way: trashing again what is already
    /// in the trash would fail as not found.
    /// The undo for an operation that failed part-way: only what `partial`
    /// says was done. An operation whose failure reports nothing done has
    /// nothing to undo, even where its normal undo does not consult the
    /// result (a refused compress must not schedule the archive that was
    /// already there for the trash).
    pub fn undo_after_failure(&self, partial: &OperationSelection) -> Vec<Operation> {
        if partial.is_empty() {
            Vec::new()
        } else {
            self.undo(partial)
        }
    }

    pub fn remaining(&self, done: &OperationSelection) -> Operation {
        match self {
            Self::Delete { paths } if !done.trash_items.is_empty() => Self::Delete {
                paths: paths
                    .iter()
                    .filter(|path| {
                        !done
                            .trash_items
                            .iter()
                            .any(|item| item.original_path().as_ref() == Some(*path))
                    })
                    .cloned()
                    .collect(),
            },
            // An item whose original is gone was moved whole. Not every
            // item in `done.moved`: a folder is noted there as soon as it is
            // made at the destination, before what it holds follows
            Self::Move {
                paths,
                to,
                cross_device_copy,
            } if !done.moved.is_empty() => Self::Move {
                paths: paths
                    .iter()
                    .filter(|path| !surely_gone(path))
                    .cloned()
                    .collect(),
                to: to.clone(),
                cross_device_copy: *cross_device_copy,
            },
            // The renames that went through are done: their sources are gone
            Self::BatchRename { renames } => Self::BatchRename {
                renames: renames
                    .iter()
                    .filter(|(_, to)| !done.selected.contains(to))
                    .cloned()
                    .collect(),
            },
            // What is gone already needs no deleting, and would fail the
            // check before anything else is tried
            Self::PermanentlyDelete { paths } => Self::PermanentlyDelete {
                paths: paths
                    .iter()
                    .filter(|path| !surely_gone(path))
                    .cloned()
                    .collect(),
            },
            // The archives all extracted are left out
            Self::Extract {
                paths,
                to,
                password,
                as_folder,
            } => Self::Extract {
                paths: paths
                    .iter()
                    .filter(|path| !done.ignored.contains(path))
                    .cloned()
                    .collect(),
                to: to.clone(),
                password: password.clone(),
                as_folder: *as_folder,
            },
            _ => self.clone(),
        }
    }

    /// Release anything large this operation carries once it has finished.
    ///
    /// Completed and failed operations are kept for the history dialog and
    /// for their description, neither of which needs the bytes a paste was
    /// carrying. Without this a pasted video stays in memory for the rest of
    /// the session. Undo does not need them either: it works from the paths
    /// the operation created.
    pub fn release_payload(&mut self) {
        if let Self::WriteFile { data, .. } = self {
            *data = Payload::from(&[][..]);
        }
    }

    /// Whether everything this operation acts on is still where it expects
    /// it, so an undo made from it can run.
    pub fn is_applicable(&self) -> bool {
        // The entry itself, not what a link at it points to: a dangling
        // link is there to rename, move or delete
        let there = |path: &PathBuf| path.symlink_metadata().is_ok();
        match self {
            Self::Rename { from, .. } => there(from),
            Self::BatchRename { renames } => renames.iter().all(|(from, _)| there(from)),
            // Not `to.is_dir()`: a move that emptied a folder removes it, and
            // undoing that move has to put the folder back. Only a destination
            // taken by something else than a folder is out of reach.
            Self::Move { paths, to, .. } => !(there(to) && !to.is_dir()) && paths.iter().all(there),
            Self::PermanentlyDelete { paths } => paths.iter().all(there),
            Self::Delete { paths } => paths.iter().all(there),
            Self::Restore { items } => !items.is_empty(),
            _ => true,
        }
    }
}

/// Bytes an operation carries, such as the contents of a clipboard paste.
///
/// It prints as its length. Operations are logged and shown in the failed
/// operation dialog with `{:#?}`, and the derived formatting of the bytes
/// themselves would be one line per byte: a modest paste becomes millions of
/// lines of text, which is enough to hang the window it is meant to explain.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct Payload(Arc<[u8]>);

impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} bytes", self.0.len())
    }
}

impl std::ops::Deref for Payload {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Into<Arc<[u8]>>> From<T> for Payload {
    fn from(bytes: T) -> Self {
        Self(bytes.into())
    }
}

/// How many names to try before giving up on finding a free one
const MAX_UNIQUE_ATTEMPTS: usize = 10_000;

/// `base` with ` (copy n)` inserted before its extension, matching the names
/// [`copy_unique_path`] produces. Unlike that function this asks the
/// filesystem nothing, so each call with a larger `n` is a different name.
fn numbered_path(base: &Path, n: usize) -> PathBuf {
    let parent = base.parent().unwrap_or(Path::new(""));
    let stem = base
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let copy = fl!("copy_noun");
    match base.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => parent.join(format!("{stem} ({copy} {n}).{ext}")),
        None => parent.join(format!("{stem} ({copy} {n})")),
    }
}

pub fn copy_unique_path(from: &Path, to: &Path) -> PathBuf {
    // List of compound extensions to check
    const COMPOUND_EXTENSIONS: &[&str] = &[
        ".tar.gz",
        ".tar.bz2",
        ".tar.xz",
        ".tar.zst",
        ".tar.lz",
        ".tar.lzma",
        ".tar.sz",
        ".tar.lzo",
        ".tar.br",
        ".tar.Z",
        ".tar.pz",
    ];

    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStrExt;

    let mut to = to.to_owned();
    // In bytes, not as text: a name that is not UTF-8 still gets a name of
    // its own, rather than `to` itself standing for the copy
    if let Some(file_name) = from.file_name() {
        let (stem, ext): (&OsStr, Option<&OsStr>) = if from.is_dir() {
            (file_name, None)
        } else {
            let bytes = file_name.as_bytes();
            COMPOUND_EXTENSIONS
                .iter()
                .find(|ext| bytes.len() > ext.len() && bytes.ends_with(ext.as_bytes()))
                .map(|ext| {
                    (
                        OsStr::from_bytes(&bytes[..bytes.len() - ext.len()]),
                        Some(OsStr::from_bytes(&ext.as_bytes()[1..])),
                    )
                })
                .unwrap_or_else(|| (from.file_stem().unwrap_or(file_name), from.extension()))
        };

        for n in 0.. {
            let new_name = if n == 0 {
                file_name.to_owned()
            } else {
                let mut new_name = OsString::from(stem);
                new_name.push(format!(" ({} {})", fl!("copy_noun"), n));
                if let Some(ext) = ext {
                    new_name.push(".");
                    new_name.push(ext);
                }
                new_name
            };

            to.push(&new_name);

            if !matches!(to.try_exists(), Ok(true)) {
                break;
            }
            // Continue if a copy with index exists
            to.pop();
        }
    }
    to
}

fn file_name(path: &Path) -> Cow<'_, str> {
    path.file_name()
        .map_or_else(|| fl!("unknown-folder").into(), |x| x.to_string_lossy())
}

fn parent_name(path: &Path) -> Cow<'_, str> {
    let Some(parent) = path.parent() else {
        return fl!("unknown-folder").into();
    };

    file_name(parent)
}

fn paths_parent_name(paths: &[PathBuf]) -> Cow<'_, str> {
    let Some(first_path) = paths.first() else {
        return fl!("unknown-folder").into();
    };

    let Some(parent) = first_path.parent() else {
        return fl!("unknown-folder").into();
    };

    for path in paths {
        if path.parent() != Some(parent) {
            return fl!("unknown-folder").into();
        }
    }

    file_name(parent)
}

#[derive(Clone, Debug, Default)]
pub struct OperationSelection {
    // Paths to ignore if they are already selected
    pub ignored: Vec<PathBuf>,
    // Paths to select
    pub selected: Vec<PathBuf>,
    /// Paths this operation brought into existence, as opposed to ones it
    /// overwrote or merely merged into. Undo may remove these and nothing
    /// else: a destination that already held the user's data is not ours to
    /// take away.
    pub created: Vec<PathBuf>,
    /// What this operation moved, as `(source, destination)` pairs, for the
    /// paths it brought into existence. Undo puts each destination back beside
    /// its source. Recorded as pairs because the two cannot be matched up
    /// afterwards: a Keep Both conflict gives the destination a different name.
    pub moved: Vec<(PathBuf, PathBuf)>,
    /// The trash entries a [`Operation::Delete`] created, for restoring them
    pub trash_items: Vec<crate::trashing::TrashItem>,
    /// What a copy or move left out because the user chose to skip it: files
    /// it could not read, folders it could not list, and originals of a move
    /// it could not remove.
    pub skipped: Vec<PathBuf>,
}

impl OperationSelection {
    /// Nothing was selected, created, moved or trashed
    pub fn is_empty(&self) -> bool {
        self.ignored.is_empty()
            && self.selected.is_empty()
            && self.created.is_empty()
            && self.moved.is_empty()
            && self.trash_items.is_empty()
    }

    /// `other` added to this: what an earlier step did, and then a later.
    pub fn absorb(&mut self, other: OperationSelection) {
        self.ignored.extend(other.ignored);
        self.selected.extend(other.selected);
        self.created.extend(other.created);
        self.moved.extend(other.moved);
        self.trash_items.extend(other.trash_items);
        self.skipped.extend(other.skipped);
    }
}

impl OperationError {
    /// This error, with `done` before what it says was done: a failure in a
    /// later step still leaves the earlier ones to undo, and not to redo.
    fn after(mut self, mut done: OperationSelection) -> Self {
        done.absorb(std::mem::take(&mut self.partial));
        self.partial = Box::new(done);
        self
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Operation {
    /// Compress files
    Compress {
        paths: Vec<PathBuf>,
        to: PathBuf,
        archive_type: ArchiveType,
        password: Option<String>,
    },
    /// Copy items
    Copy {
        paths: Vec<PathBuf>,
        to: PathBuf,
    },
    /// Move items to the trash
    Delete {
        paths: Vec<PathBuf>,
    },
    /// Delete a path from the trash
    DeleteTrash {
        items: Vec<crate::trashing::TrashItem>,
    },
    /// Empty the trash
    EmptyTrash,
    /// Empty the trash on the drive mounted at `root`, before it is ejected
    EmptyDriveTrash {
        root: PathBuf,
    },
    /// Uncompress files
    Extract {
        paths: Box<[PathBuf]>,
        to: PathBuf,
        password: Option<String>,
        /// Extract each archive into a new folder named after it instead of
        /// directly into `to`
        as_folder: bool,
    },
    /// Move items
    Move {
        paths: Vec<PathBuf>,
        to: PathBuf,
        cross_device_copy: bool,
    },
    NewFile {
        path: PathBuf,
    },
    NewFolder {
        path: PathBuf,
    },
    /// Permanently delete items, skipping the trash
    PermanentlyDelete {
        paths: Box<[PathBuf]>,
    },
    RemoveFromRecents {
        paths: Box<[PathBuf]>,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    /// Rename several items as one step, in order
    BatchRename {
        renames: Vec<(PathBuf, PathBuf)>,
    },
    /// Write bytes that came from the clipboard into a new file.
    ///
    /// The data is shared rather than cloned: the operation is kept on the
    /// undo stack, and a pasted video can be hundreds of megabytes.
    WriteFile {
        /// The name to try first; a free one beside it is used if it is taken
        path: PathBuf,
        data: Payload,
    },
    /// Restore a path from the trash
    Restore {
        items: Vec<crate::trashing::TrashItem>,
    },
    /// Set executable and launch
    SetExecutableAndLaunch {
        path: PathBuf,
    },
    /// Set permissions: the bits of `mode` in `mask`, the rest left as the
    /// file has them when this runs. `mode` is read from a listing that
    /// may be out of date, and a second change made before it is read
    /// again must not take back the first.
    SetPermissions {
        path: PathBuf,
        mode: u32,
        mask: u32,
    },
}

/// How an operation ended without completing. Making one changes nothing:
/// [`Operation::perform`] takes the controller to its end state from it.
#[derive(Clone, Debug)]
pub enum OperationErrorType {
    /// Stopped at the user's word: cancelled, or aborted, which
    /// [`Controller::is_aborted`] tells apart.
    Cancelled,
    /// Stopped as the operation had already failed.
    Failed,
    /// What went wrong at a path.
    Path {
        path: PathBuf,
        cause: Cause,
    },
    /// Anything else, in the words the user reads.
    Generic(String),
    PasswordRequired,
}
#[derive(Clone, Debug)]
pub struct OperationError {
    pub kind: OperationErrorType,
    /// What the operation got done before it failed. It is undone like a
    /// completed operation, and a retry starts after it. Boxed to keep the
    /// error small enough for every `Result` that carries it.
    pub partial: Box<OperationSelection>,
}

impl OperationError {
    fn of(kind: OperationErrorType) -> Self {
        Self {
            kind,
            partial: Box::default(),
        }
    }

    /// Stopped as the controller says: failed already, or cancelled.
    pub fn from_state(state: ControllerState) -> Self {
        Self::of(if state == ControllerState::Failed {
            OperationErrorType::Failed
        } else {
            OperationErrorType::Cancelled
        })
    }

    pub fn from_err<T: ToString>(err: T) -> Self {
        Self::of(OperationErrorType::Generic(err.to_string()))
    }

    /// `err` happened at `path`.
    pub fn at(path: &Path, err: &(dyn std::error::Error + 'static)) -> Self {
        Self::of(OperationErrorType::Path {
            path: path.to_path_buf(),
            cause: Cause::of(err),
        })
    }

    pub fn from_kind(kind: OperationErrorType) -> Self {
        Self::of(kind)
    }

    pub fn from_msg(m: impl Into<String>) -> Self {
        Self::of(OperationErrorType::Generic(m.into()))
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self.kind, OperationErrorType::Cancelled)
    }
}

impl std::error::Error for OperationError {}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            OperationErrorType::Cancelled => f.write_str(&fl!("cancelled")),
            OperationErrorType::Failed => f.write_str(&fl!("failed")),
            OperationErrorType::Path { path, cause } => f.write_str(&cause.text(path)),
            OperationErrorType::Generic(s) => s.fmt(f),
            OperationErrorType::PasswordRequired => f.write_str("Password required"),
        }
    }
}

impl Operation {
    /// This operation less what would change nothing: moving something into
    /// the folder it is already in, as when a drag is dropped back where it
    /// started. `None` when nothing is left to do. A copy into its own folder
    /// is kept, as that makes a duplicate.
    #[must_use]
    pub fn without_no_op_moves(self) -> Option<Self> {
        match self {
            Self::Move {
                mut paths,
                to,
                cross_device_copy,
            } => {
                paths.retain(|path| path.parent() != Some(to.as_path()));
                (!paths.is_empty()).then_some(Self::Move {
                    paths,
                    to,
                    cross_device_copy,
                })
            }
            other => Some(other),
        }
    }

    pub fn pending_text(&self, ratio: f32, state: ControllerState) -> String {
        let percent = (ratio * 100.0) as i32;
        self.pending_text_with(match state {
            ControllerState::Running => fl!("progress", percent = percent),
            ControllerState::Paused => fl!("progress-paused", percent = percent),
            ControllerState::Cancelled => fl!("progress-cancelled", percent = percent),
            ControllerState::Failed => fl!("progress-failed", percent = percent),
        })
    }

    /// What the operation is doing while it waits on the user about a path it
    /// may not touch: no percentage, since how far it got says little then
    /// (a move whose one original cannot be removed has copied everything).
    pub fn pending_text_asking(&self) -> String {
        self.pending_text_with(fl!("progress-asking"))
    }

    fn pending_text_with(&self, progress: String) -> String {
        let progress = || progress.clone();
        match self {
            Self::Compress { paths, to, .. } => fl!(
                "compressing",
                items = paths.len(),
                from = paths_parent_name(paths),
                to = file_name(to),
                progress = progress()
            ),
            Self::Copy { paths, to } => fl!(
                "copying",
                items = paths.len(),
                from = paths_parent_name(paths),
                to = file_name(to),
                progress = progress()
            ),
            Self::Delete { paths } => fl!(
                "moving",
                items = paths.len(),
                from = paths_parent_name(paths),
                to = fl!("trash"),
                progress = progress()
            ),
            Self::DeleteTrash { items } => {
                fl!("deleting", items = items.len(), progress = progress())
            }
            Self::EmptyTrash | Self::EmptyDriveTrash { .. } => {
                fl!("emptying-trash", progress = progress())
            }
            Self::Extract { paths, to, .. } => fl!(
                "extracting",
                items = paths.len(),
                from = paths_parent_name(paths),
                to = file_name(to),
                progress = progress()
            ),
            Self::Move { paths, to, .. } => fl!(
                "moving",
                items = paths.len(),
                from = paths_parent_name(paths),
                to = file_name(to),
                progress = progress()
            ),
            Self::NewFile { path } => fl!(
                "creating",
                name = file_name(path),
                parent = parent_name(path)
            ),
            Self::NewFolder { path } => fl!(
                "creating",
                name = file_name(path),
                parent = parent_name(path)
            ),
            Self::PermanentlyDelete { paths } => fl!("permanently-deleting", items = paths.len()),
            Self::Rename { from, to } => {
                fl!("renaming", from = file_name(from), to = file_name(to))
            }
            Self::BatchRename { renames } => fl!("renaming-many", count = renames.len()),
            Self::WriteFile { path, .. } => fl!(
                "creating",
                name = file_name(path),
                parent = parent_name(path)
            ),
            Self::RemoveFromRecents { paths } => fl!("removing-from-recents", items = paths.len()),
            Self::Restore { items } => fl!("restoring", items = items.len(), progress = progress()),
            Self::SetExecutableAndLaunch { path } => {
                fl!("setting-executable-and-launching", name = file_name(path))
            }
            Self::SetPermissions { path, mode, .. } => {
                fl!(
                    "setting-permissions",
                    name = file_name(path),
                    mode = format!("{:#03o}", mode)
                )
            }
        }
    }

    pub fn completed_text(&self) -> String {
        match self {
            Self::Compress { paths, to, .. } => fl!(
                "compressed",
                items = paths.len(),
                from = paths_parent_name(paths),
                to = file_name(to)
            ),
            Self::Copy { paths, to } => fl!(
                "copied",
                items = paths.len(),
                from = paths_parent_name(paths),
                to = file_name(to)
            ),
            Self::Delete { paths } => fl!(
                "moved",
                items = paths.len(),
                from = paths_parent_name(paths),
                to = fl!("trash")
            ),
            Self::DeleteTrash { items } => fl!("deleted", items = items.len()),
            Self::EmptyTrash | Self::EmptyDriveTrash { .. } => fl!("emptied-trash"),
            Self::Extract { paths, to, .. } => fl!(
                "extracted",
                items = paths.len(),
                from = paths_parent_name(paths),
                to = file_name(to)
            ),
            Self::Move { paths, to, .. } => fl!(
                "moved",
                items = paths.len(),
                from = paths_parent_name(paths),
                to = file_name(to)
            ),
            Self::NewFile { path } => fl!(
                "created",
                name = file_name(path),
                parent = parent_name(path)
            ),
            Self::NewFolder { path } => fl!(
                "created",
                name = file_name(path),
                parent = parent_name(path)
            ),
            Self::PermanentlyDelete { paths } => fl!("permanently-deleted", items = paths.len()),
            Self::RemoveFromRecents { paths } => fl!("removed-from-recents", items = paths.len()),
            Self::Rename { from, to } => fl!("renamed", from = file_name(from), to = file_name(to)),
            Self::BatchRename { renames } => fl!("renamed-many", count = renames.len()),
            Self::WriteFile { path, .. } => fl!(
                "created",
                name = file_name(path),
                parent = parent_name(path)
            ),
            Self::Restore { items } => fl!("restored", items = items.len()),
            Self::SetExecutableAndLaunch { path } => {
                fl!("set-executable-and-launched", name = file_name(path))
            }
            Self::SetPermissions { path, mode, .. } => {
                fl!(
                    "set-permissions",
                    name = file_name(path),
                    mode = format!("{:#03o}", mode)
                )
            }
        }
    }

    pub const fn show_progress_notification(&self) -> bool {
        // Long running operations show a progress notification
        match self {
            Self::Compress { .. }
            | Self::Copy { .. }
            | Self::Delete { .. }
            | Self::DeleteTrash { .. }
            | Self::EmptyTrash
            | Self::EmptyDriveTrash { .. }
            | Self::Extract { .. }
            | Self::Move { .. }
            | Self::PermanentlyDelete { .. }
            | Self::BatchRename { .. }
            | Self::Restore { .. } => true,
            Self::WriteFile { .. }
            | Self::NewFile { .. }
            | Self::NewFolder { .. }
            | Self::RemoveFromRecents { .. }
            | Self::Rename { .. }
            | Self::SetExecutableAndLaunch { .. }
            | Self::SetPermissions { .. } => false,
        }
    }

    pub fn toast(&self) -> Option<String> {
        match self {
            Self::Compress { .. } => Some(self.completed_text()),
            Self::Delete { .. } => Some(self.completed_text()),
            Self::Extract { .. } => Some(self.completed_text()),
            _ => None,
        }
    }

    /// Perform the operation, and take its controller to the state it ended
    /// in: an error says how it ended, and making one changed nothing.
    pub async fn perform(
        self,
        prompts: &Prompts,
        controller: Controller,
    ) -> Result<OperationSelection, OperationError> {
        // Dropped before it ends, its blocking jobs are told to stop
        let running = CancelOnDrop::new(controller.clone());
        let result = self.run(prompts, controller.clone()).await;
        if let Err(err) = &result {
            if err.is_cancelled() {
                controller.cancel();
            } else {
                controller.fail();
            }
        }
        running.disarm();
        result
    }

    async fn run(
        self,
        prompts: &Prompts,
        controller: Controller,
    ) -> Result<OperationSelection, OperationError> {
        let controller_clone = controller.clone();

        let paths: Result<OperationSelection, OperationError> = match self {
            Self::Compress {
                paths,
                to,
                archive_type,
                password,
            } => {
                let controller_c = controller.clone();
                compio::runtime::spawn_blocking(
                    move || -> Result<OperationSelection, OperationError> {
                        let controller = controller_c;
                        // Entry names are relative to the deepest folder that
                        // holds every selected item: a selection from search
                        // results or Recents spans folders, so the archive's
                        // own parent is not a prefix of all of them.
                        let mut parents = paths.iter().filter_map(|path| path.parent());
                        let Some(relative_root) = parents.next().map(|first| {
                            parents.fold(first, |root, parent| {
                                root.ancestors()
                                    .find(|ancestor| parent.starts_with(ancestor))
                                    .unwrap_or(root)
                            })
                        }) else {
                            return Err(OperationError::from_err("nothing to compress".to_string()));
                        };
                        let relative_root = relative_root.to_path_buf();

                        let op_sel = OperationSelection {
                            ignored: paths.clone(),
                            selected: vec![to.clone()],
                            created: vec![to.clone()],
                            moved: Vec::new(),
                            trash_items: Vec::new(),
                            skipped: Vec::new(),
                        };

                        let mut paths = paths;
                        for path in &paths.clone() {
                            // A link to a folder is stored as a link: what it
                            // points at is not what was selected
                            if path.symlink_metadata().is_ok_and(|meta| meta.is_dir()) {
                                let new_paths_it = ignore::WalkBuilder::new(path)
                                    .standard_filters(false)
                                    .build();
                                for entry in new_paths_it.skip(1) {
                                    let entry = entry
                                        .map_err(OperationError::from_err)?;
                                    paths.push(entry.into_path());
                                }
                            }
                        }

                        // Zip entry names must be text. Check them all before
                        // the archive file is created, so a name we cannot
                        // store fails outright instead of leaving a partial
                        // archive that silently lacks the file.
                        if matches!(archive_type, ArchiveType::Zip) {
                            for path in &paths {
                                let relative = path
                                    .strip_prefix(&relative_root)
                                    .map_err(OperationError::from_err)?;
                                if relative.to_str().is_none() {
                                    return Err(OperationError::from_err(format!(
                                            "cannot store {} in a zip archive: the name is not valid UTF-8",
                                            path.display()
                                        )));
                                }
                            }
                        }

                        // `create_new`, not `create`: the dialog's existence
                        // check ran a frame ago, so an archive of this name
                        // may have appeared since and must not be truncated.
                        let file = fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&to)
                            .map_err(OperationError::from_err)?;
                        let written = (|| -> Result<(), OperationError> {
                        match archive_type {
                            ArchiveType::Tgz => {
                                let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
                                    io::BufWriter::new(file),
                                    flate2::Compression::default(),
                                ));
                                // Store links as links: following them would
                                // copy whatever they point at into the archive
                                archive.follow_symlinks(false);

                                let total_paths = paths.len();
                                for (i, path) in paths.iter().enumerate() {
                                    futures::executor::block_on(async {
                                        controller
                                            .check()
                                            .await
                                            .map_err(OperationError::from_state)
                                    })?;

                                    controller.set_progress((i as f32) / total_paths as f32);

                                    // tar stores raw bytes, so the name needs
                                    // no conversion and nothing is dropped
                                    let relative_path = path
                                        .strip_prefix(&relative_root)
                                        .map_err(OperationError::from_err)?;
                                    let meta = fs::symlink_metadata(path)
                                        .map_err(OperationError::from_err)?;
                                    if !meta.is_file() {
                                        archive
                                            .append_path_with_name(path, relative_path)
                                            .map_err(OperationError::from_err)?;
                                        continue;
                                    }
                                    // A file read through the controller, so a
                                    // large one still pauses, stops and shows
                                    // how far it is
                                    let mut header = tar::Header::new_gnu();
                                    header.set_metadata(&meta);
                                    let mut reader = OpReader::part(
                                        fs::File::open(path).map_err(|e| {
                                            OperationError::from_err(e)
                                        })?,
                                        meta.len(),
                                        controller.clone(),
                                        i,
                                        total_paths,
                                    );
                                    let appended =
                                        archive.append_data(&mut header, relative_path, &mut reader);
                                    if appended.is_err() && controller.is_cancelled() {
                                        return Err(OperationError::from_state(ControllerState::Cancelled));
                                    }
                                    appended
                                        .map_err(OperationError::from_err)?;
                                }

                                // Every layer finished and flushed by hand, its
                                // errors kept: dropped, each would lose them
                                // and leave a cut-off archive taken for whole
                                archive
                                    .into_inner()
                                    .and_then(flate2::write::GzEncoder::finish)
                                    .and_then(|out| out.into_inner().map_err(io::IntoInnerError::into_error))
                                    .and_then(|file| file.sync_all())
                                    .map_err(OperationError::from_err)?;
                            }
                            ArchiveType::Zip => {
                                let mut archive = zip::ZipWriter::new(io::BufWriter::new(file));

                                let total_paths = paths.len();
                                let mut buffer = vec![0; 4 * 1024 * 1024];
                                for (i, path) in paths.iter().enumerate() {
                                    futures::executor::block_on(async {
                                        controller
                                            .check()
                                            .await
                                            .map_err(OperationError::from_state)
                                    })?;

                                    controller.set_progress((i as f32) / total_paths as f32);

                                    let mut zip_options = zip::write::SimpleFileOptions::default();
                                    if password.is_some() {
                                        zip_options = zip_options.with_aes_encryption(
                                            Aes256,
                                            password.as_deref().unwrap(),
                                        );
                                    }
                                    {
                                        let relative_path = path
                                            .strip_prefix(&relative_root)
                                            .map_err(OperationError::from_err)?
                                            .to_str()
                                            .ok_or_else(|| {
                                                OperationError::from_err(format!(
                                                        "cannot store {} in a zip archive",
                                                        path.display()
                                                    ))
                                            })?;
                                        // Not followed: a link is stored as a
                                        // link, never as what it points at
                                        let metadata = fs::symlink_metadata(path).map_err(|e| {
                                            OperationError::from_err(e)
                                        })?;

                                        if let Ok(modified) = metadata.modified()
                                            && let Some(last_modified) =
                                                archive::system_time_to_zip_date_time(modified)
                                        {
                                            zip_options =
                                                zip_options.last_modified_time(last_modified);
                                        }

                                        use std::os::unix::fs::MetadataExt;
                                        let mode = metadata.mode();
                                        zip_options = zip_options.unix_permissions(mode);

                                        if metadata.is_symlink() {
                                            let target = fs::read_link(path).map_err(|e| {
                                                OperationError::from_err(e)
                                            })?;
                                            // `add_symlink`, not `add_symlink_from_path`:
                                            // the latter normalises the target like an
                                            // entry name, turning `../x` into `x`
                                            let target = target.to_str().ok_or_else(|| {
                                                OperationError::from_err(format!(
                                                        "cannot store the link {} in a zip archive: its target is not valid UTF-8",
                                                        path.display()
                                                    ))
                                            })?;
                                            archive
                                                .add_symlink(relative_path, target, zip_options)
                                                .map_err(|e| {
                                                    OperationError::from_err(e)
                                                })?;
                                        } else if metadata.is_file() {
                                            let mut file = fs::File::open(path).map_err(|e| {
                                                OperationError::from_err(e)
                                            })?;
                                            let total = metadata.len();
                                            if total >= 4 * 1024 * 1024 * 1024 {
                                                // The large file option must be enabled for files above 4 GiB
                                                zip_options = zip_options.large_file(true);
                                            }
                                            archive
                                                .start_file(relative_path, zip_options)
                                                .map_err(|e| {
                                                    OperationError::from_err(e)
                                                })?;
                                            let mut current = 0;
                                            loop {
                                                futures::executor::block_on(async {
                                                    controller.check().await.map_err(|s| {
                                                        OperationError::from_state(s)
                                                    })
                                                })?;

                                                let count =
                                                    file.read(&mut buffer).map_err(|e| {
                                                        OperationError::from_err(e)
                                                    })?;
                                                if count == 0 {
                                                    break;
                                                }
                                                archive.write_all(&buffer[..count]).map_err(
                                                    OperationError::from_err,
                                                )?;
                                                current += count;

                                                let file_progress = current as f32 / total as f32;
                                                let total_progress =
                                                    (i as f32 + file_progress) / total_paths as f32;
                                                controller.set_progress(total_progress);
                                            }
                                        } else {
                                            archive
                                                .add_directory(relative_path, zip_options)
                                                .map_err(|e| {
                                                    OperationError::from_err(e)
                                                })?;
                                        }
                                    }
                                }

                                archive
                                    .finish()
                                    .map_err(io::Error::from)
                                    .and_then(|out| out.into_inner().map_err(io::IntoInnerError::into_error))
                                    .and_then(|file| file.sync_all())
                                    .map_err(OperationError::from_err)?;
                            }
                        }
                        Ok(())
                        })();
                        // A cancelled or failed compress leaves no partial
                        // archive behind; the undo entry only exists once
                        // the operation completes.
                        if written.is_err() {
                            let _ = fs::remove_file(&to);
                        }
                        written?;

                        Ok(op_sel)
                    },
                )
                .await
                .map_err(wrap_compio_spawn_error)?
            }
            Self::Copy { paths, to } => {
                copy_or_move(paths, to, Method::Copy, false, prompts, controller).await
            }
            Self::Delete { paths } => delete(paths, false, prompts, controller.clone()).await,
            Self::DeleteTrash { items } => purge(items, prompts, controller.clone()).await,
            Self::EmptyTrash => {
                let bins = compio::runtime::spawn_blocking(crate::trashing::bins_to_empty)
                    .await
                    .map_err(wrap_compio_spawn_error)?
                    .map_err(|err| OperationError::from_err(err.to_string()))?;
                empty(bins, prompts, controller.clone()).await
            }
            Self::EmptyDriveTrash { root } => {
                let bins =
                    compio::runtime::spawn_blocking(move || crate::trashing::drive_bins(&root))
                        .await
                        .map_err(wrap_compio_spawn_error)?
                        .map_err(|err| OperationError::from_err(err.to_string()))?;
                empty(bins, prompts, controller.clone()).await
            }
            Self::Extract {
                paths,
                to,
                password,
                as_folder,
            } => {
                let controller_clone = controller.clone();
                let prompts = prompts.clone();
                compio::runtime::spawn(async move {
                    let controller = controller_clone;
                    let total_paths = paths.len();
                    let mut op_sel = OperationSelection::default();
                    for (i, path) in paths.iter().enumerate() {
                        // What the archives before got done stays done, to
                        // undo, and to leave out of a retry
                        let done = || op_sel.clone();
                        if let Err(state) = controller.check().await {
                            return Err(OperationError::from_state(state).after(done()));
                        }

                        controller.set_progress((i as f32) / total_paths as f32);

                        let Some(file_name) = path.file_name().and_then(|f| f.to_str()) else {
                            continue;
                        };
                        let dir_name = get_directory_name(file_name).to_string();

                        if as_folder {
                            let new_dir = extract_dir(&to, &dir_name, file_name)
                                .map_err(|err| err.after(done()))?;
                            if let Err(err) = extract_archive(
                                path.clone(),
                                new_dir.clone(),
                                &password,
                                &controller,
                            )
                            .await
                            {
                                // The folder was made for it, so it is ours:
                                // half an archive goes with it. Aborted,
                                // what was done stays, to undo
                                let mut done = done();
                                if controller.is_aborted() {
                                    done.created.push(new_dir.clone());
                                } else {
                                    let _ = compio::runtime::spawn_blocking(move || {
                                        fs::remove_dir_all(new_dir)
                                    })
                                    .await;
                                }
                                return Err(err.after(done));
                            }
                            op_sel.created.push(new_dir.clone());
                            op_sel.selected.push(new_dir);
                        } else {
                            // Extract into a hidden staging directory beside the destination,
                            // then move the entries into place. The move goes through the
                            // same replace dialog as copy/move, so existing files are never
                            // overwritten silently.
                            let staging =
                                staging_dir(&to, &dir_name).map_err(|err| err.after(done()))?;
                            let result = async {
                                extract_archive(
                                    path.clone(),
                                    staging.clone(),
                                    &password,
                                    &controller,
                                )
                                .await?;
                                // Enumerated off the runtime thread: a staging
                                // directory holds every entry of the archive,
                                // and on a slow or remote destination reading
                                // them would stall every other operation.
                                let to_list = staging.clone();
                                let entries: Vec<PathBuf> =
                                    compio::runtime::spawn_blocking(move || {
                                        fs::read_dir(&to_list)?
                                            .map(|entry| entry.map(|e| e.path()))
                                            .collect::<io::Result<Vec<_>>>()
                                    })
                                    .await
                                    .map_err(wrap_compio_spawn_error)?
                                    .map_err(OperationError::from_err)?;
                                let mut moved = copy_or_move(
                                    entries.clone(),
                                    to.clone(),
                                    Method::Move {
                                        cross_device_copy: false,
                                    },
                                    true,
                                    &prompts,
                                    controller.clone(),
                                )
                                .await?;
                                sync_to_disk(Vec::new(), [to.clone()].into_iter().collect())
                                    .await
                                    .map_err(|(path, err)| {
                                        OperationError::at(&path, &err).after(moved.clone())
                                    })?;
                                // Entries moved by a plain rename are not in the selection
                                for entry in entries {
                                    if let Some(name) = entry.file_name() {
                                        let dest = to.join(name);
                                        if dest.exists() && !moved.selected.contains(&dest) {
                                            moved.selected.push(dest);
                                        }
                                    }
                                }
                                Ok::<_, OperationError>(moved)
                            }
                            .await;
                            // Skipped or cancelled entries stay in the staging
                            // directory, so this can be the whole archive.
                            // Unlinking it entry by entry takes about as long
                            // as writing it did, which is far too long to hold
                            // the one thread every operation runs on. The
                            // staging directory stays ours until it is gone.
                            let to_clean = staging.clone();
                            let _ = compio::runtime::spawn_blocking(move || {
                                fs::remove_dir_all(to_clean)
                            })
                            .await;
                            // Only what the move actually created may be undone:
                            // entries the user skipped, and directories that were
                            // merged into, were already the user's. What it
                            // replaced is in the trash, to come back on undo
                            let moved = result.map_err(|err| err.after(done()))?;
                            op_sel.selected.extend(moved.selected);
                            op_sel.created.extend(moved.created);
                            op_sel.trash_items.extend(moved.trash_items);
                        }
                        // The archive done: a retry leaves it out
                        op_sel.ignored.push(path.clone());
                    }

                    Ok::<_, OperationError>(op_sel)
                })
                .await
                .map_err(wrap_compio_spawn_error)?
            }
            Self::Move {
                paths,
                to,
                cross_device_copy,
            } => {
                copy_or_move(
                    paths,
                    to,
                    Method::Move { cross_device_copy },
                    false,
                    prompts,
                    controller,
                )
                .await
            }
            Self::NewFolder { path } => {
                let controller_clone = controller.clone();
                compio::runtime::spawn(async move {
                    let controller = controller_clone;
                    controller
                        .check()
                        .await
                        .map_err(OperationError::from_state)?;
                    compio::fs::create_dir(&path)
                        .await
                        .map_err(OperationError::from_err)?;
                    Result::<_, OperationError>::Ok(OperationSelection {
                        ignored: Vec::new(),
                        selected: vec![path.clone()],
                        created: vec![path],
                        moved: Vec::new(),
                        trash_items: Vec::new(),
                        skipped: Vec::new(),
                    })
                })
            }
            .await
            .map_err(wrap_compio_spawn_error)?,
            Self::NewFile { path } => {
                let controller_clone = controller.clone();
                compio::runtime::spawn(async move {
                    let controller = controller_clone;
                    controller
                        .check()
                        .await
                        .map_err(OperationError::from_state)?;
                    // `create_new`, not `create`: creating a new file must
                    // never truncate one that is already there. The dialog
                    // warns about a name that is taken, but that warning is a
                    // snapshot of a directory anything else may write to, so
                    // the refusal has to be here, where the file is made.
                    compio::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .await
                        .map_err(OperationError::from_err)?;
                    Result::<_, OperationError>::Ok(OperationSelection {
                        ignored: Vec::new(),
                        selected: vec![path.clone()],
                        created: vec![path],
                        moved: Vec::new(),
                        trash_items: Vec::new(),
                        skipped: Vec::new(),
                    })
                })
            }
            .await
            .map_err(wrap_compio_spawn_error)?,
            Self::PermanentlyDelete { paths } => {
                delete(paths.into_vec(), true, prompts, controller.clone()).await
            }
            Self::RemoveFromRecents { paths } => {
                // Through the same worker that records and clears recents.
                // The file is read, edited and written back whole, so a
                // removal running beside a recording would undo one of them.
                tokio::task::spawn_blocking(move || crate::recents::remove(paths))
                    .await
                    .map_err(OperationError::from_err)?
                    .map_err(OperationError::from_err)?;

                Ok(OperationSelection::default())
            }
            Self::WriteFile { path, data } => {
                let controller_clone = controller.clone();
                compio::runtime::spawn(async move {
                    let controller = controller_clone;
                    controller
                        .check()
                        .await
                        .map_err(OperationError::from_state)?;
                    // Create exclusively and step aside if the name is taken,
                    // so two pastes racing for the same name cannot have one
                    // overwrite the other
                    if path.parent().is_none() {
                        return Err(OperationError::from_msg(format!(
                            "path {} has no parent directory",
                            path.display()
                        )));
                    }
                    // The candidate is built from the attempt number rather
                    // than by asking whether a name is free. `copy_unique_path`
                    // answers that with `try_exists`, which follows symlinks
                    // and so calls a dangling link free; exclusive creation
                    // then fails on the very same name, and the retry would
                    // never move on.
                    let mut target = path.clone();
                    let mut attempt = 0_usize;
                    let file = loop {
                        controller
                            .check()
                            .await
                            .map_err(OperationError::from_state)?;
                        match compio::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&target)
                            .await
                        {
                            Ok(file) => break file,
                            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                                attempt += 1;
                                if attempt > MAX_UNIQUE_ATTEMPTS {
                                    return Err(OperationError::from_err(err));
                                }
                                target = numbered_path(&path, attempt);
                            }
                            Err(err) => return Err(OperationError::from_err(err)),
                        }
                    };
                    // In pieces, so a large paste still pauses, stops and
                    // shows how far it is
                    const PIECE: usize = 4 * 1024 * 1024;
                    let mut file = file;
                    let written = async {
                        let mut at = 0;
                        while at < data.len() {
                            controller
                                .check()
                                .await
                                .map_err(OperationError::from_state)?;
                            let piece = data[at..].len().min(PIECE);
                            let compio::BufResult(result, _) =
                                compio::io::AsyncWriteAtExt::write_all_at(
                                    &mut file,
                                    data[at..at + piece].to_vec(),
                                    at as u64,
                                )
                                .await;
                            result.map_err(OperationError::from_err)?;
                            at += piece;
                            controller.set_progress(at as f32 / data.len() as f32);
                        }
                        file.sync_all().await.map_err(OperationError::from_err)
                    }
                    .await;
                    if let Err(err) = written {
                        // Made by this paste, under a name no one had: cut
                        // short, it is no copy of the clipboard, and goes
                        drop(file);
                        let _ = compio::fs::remove_file(&target).await;
                        return Err(err);
                    }
                    Result::<_, OperationError>::Ok(OperationSelection {
                        ignored: Vec::new(),
                        selected: vec![target.clone()],
                        created: vec![target],
                        moved: Vec::new(),
                        trash_items: Vec::new(),
                        skipped: Vec::new(),
                    })
                })
            }
            .await
            .map_err(wrap_compio_spawn_error)?,
            Self::Rename { from, to } => {
                let controller_clone = controller.clone();
                let prompts = prompts.clone();

                compio::runtime::spawn(async move {
                    let controller = controller_clone;
                    controller
                        .check()
                        .await
                        .map_err(OperationError::from_state)?;
                    // The folder the file belongs in may be gone: undoing a
                    // move that emptied a folder and removed it puts the file
                    // back by its whole original path. For a rename the user
                    // typed, this is the folder the file is already in.
                    if let Some(parent) = to.parent()
                        && let Err(err) = compio::fs::create_dir_all(parent).await
                    {
                        log::warn!("failed to create {}: {err}", parent.display());
                    }
                    // `rename_no_replace`, not `rename`: `rename(2)` replaces
                    // the destination without a word, and a rename is a move
                    // the user asked for by name, not permission to destroy
                    // whatever already has that name. The dialog warns about a
                    // taken name, but that warning is a snapshot of a folder
                    // anything else may write to, so the refusal belongs here.
                    // Restore and batch rename already work this way.
                    let renamed = compio::runtime::spawn_blocking({
                        let (from, to) = (from.clone(), to.clone());
                        move || rename_no_replace(&from, &to)
                    })
                    .await
                    .map_err(wrap_compio_spawn_error)?;
                    match renamed {
                        Ok(()) => {}
                        // `renameat2` cannot cross a filesystem. Undoing a move
                        // that did cross one has to come back the way it went,
                        // by copying and then removing what was copied.
                        Err(err) if err.raw_os_error() == Some(libc::EXDEV) => {
                            return recursive_pairs(
                                vec![(from.clone(), to)],
                                Method::Move {
                                    cross_device_copy: false,
                                },
                                Vec::new(),
                                OperationSelection {
                                    ignored: vec![from],
                                    ..Default::default()
                                },
                                false,
                                prompts,
                                controller,
                            )
                            .await;
                        }
                        Err(err) => return Err(OperationError::from_err(err)),
                    }
                    Result::<_, OperationError>::Ok(OperationSelection {
                        ignored: vec![from],
                        selected: vec![to],
                        created: Vec::new(),
                        moved: Vec::new(),
                        trash_items: Vec::new(),
                        skipped: Vec::new(),
                    })
                })
            }
            .await
            .map_err(wrap_compio_spawn_error)?,
            Self::BatchRename { renames } => {
                let controller_clone = controller.clone();

                compio::runtime::spawn(async move {
                    let controller = controller_clone;
                    let total = renames.len();
                    let mut op_sel = OperationSelection::default();
                    for (i, (from, to)) in renames.into_iter().enumerate() {
                        // The renames before stay done, to undo, and to
                        // leave out of a retry
                        if let Err(state) = controller.check().await {
                            return Err(OperationError::from_state(state).after(op_sel));
                        }
                        controller.set_progress((i as f32) / (total as f32));
                        // Never replace: the preview that produced this list
                        // is a snapshot, the folder may have changed since,
                        // and on a remote folder the preview does not check
                        // destinations at all
                        let renamed = compio::runtime::spawn_blocking({
                            let (from, to) = (from.clone(), to.clone());
                            move || rename_no_replace(&from, &to)
                        })
                        .await
                        .map_err(wrap_compio_spawn_error)?;
                        if let Err(err) = renamed {
                            return Err(OperationError::from_err(err).after(op_sel));
                        }
                        op_sel.ignored.push(from);
                        op_sel.selected.push(to);
                    }
                    Result::<_, OperationError>::Ok(op_sel)
                })
            }
            .await
            .map_err(wrap_compio_spawn_error)?,
            Self::Restore { items } => restore(items, prompts, controller.clone()).await,
            Self::SetExecutableAndLaunch { path } => {
                controller
                    .check()
                    .await
                    .map_err(OperationError::from_state)?;

                compio::runtime::spawn_blocking(move || -> Result<(), OperationError> {
                    use std::os::unix::fs::PermissionsExt;

                    let at = |err| OperationError::at(&path, &err);
                    let mut perms = fs::metadata(&path).map_err(at)?.permissions();
                    let current_mode = perms.mode();
                    let new_mode = current_mode | 0o111;
                    perms.set_mode(new_mode);
                    fs::set_permissions(&path, perms).map_err(at)?;

                    let mut command = std::process::Command::new(&path);
                    spawn_detached(&mut command).map_err(at)?;

                    Ok(())
                })
                .await
                .map_err(wrap_compio_spawn_error)??;
                Ok(OperationSelection::default())
            }
            Self::SetPermissions { path, mode, mask } => {
                controller
                    .check()
                    .await
                    .map_err(OperationError::from_state)?;

                let controller_clone = controller.clone();
                let path_clone = path.clone();
                compio::runtime::spawn_blocking(move || -> Result<(), OperationError> {
                    let controller = controller_clone;
                    let path = path_clone;
                    use std::os::unix::fs::PermissionsExt;
                    // One change at a time to a file: each reads what the
                    // one before left, so changes made in quick succession
                    // all hold. Other files do not wait on it
                    let _one = PermissionsTurn::take(&path);
                    // Cancelled while it waited its turn: nothing changes
                    if controller.is_cancelled() || controller.is_failed() {
                        return Err(OperationError::from_state(controller.state()));
                    }
                    let at = |err| OperationError::at(&path, &err);
                    let now = fs::metadata(&path).map_err(at)?.permissions().mode();
                    let perms = fs::Permissions::from_mode((now & !mask & 0o7777) | (mode & mask));
                    fs::set_permissions(&path, perms).map_err(at)?;

                    Ok(())
                })
                .await
                .map_err(wrap_compio_spawn_error)??;
                Ok(OperationSelection {
                    ignored: Vec::new(),
                    selected: vec![path],
                    created: Vec::new(),
                    moved: Vec::new(),
                    trash_items: Vec::new(),
                    skipped: Vec::new(),
                })
            }
        };

        controller_clone.set_progress(1.0);

        paths
    }
}

#[track_caller]
fn wrap_compio_spawn_error(err: compio::runtime::JoinError) -> OperationError {
    log::error!(
        "compio runtime spawn failed: {}",
        std::backtrace::Backtrace::capture()
    );

    // compio 0.19 replaces `Box<dyn Any + Send>` with `JoinError`, distinguishing
    // cancellation from panic. Both previously returned an opaque payload. The panic
    // payload is still the same box, so an `OperationError` from a panicking task can
    // be recovered as before.
    match err {
        compio::runtime::JoinError::Cancelled => {
            OperationError::from_msg("compio runtime task was cancelled")
        }
        compio::runtime::JoinError::Panicked(payload) => match payload.downcast() {
            Ok(err) => *err,
            Err(_) => OperationError::from_msg("compio runtime spawn failed"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io;
    use std::path::PathBuf;

    use crate::ui::iced::futures::future;
    use log::debug;
    use test_log::test;

    use super::{
        ArchiveType, Blocked, BlockedAnswer, Controller, Operation, OperationError,
        OperationSelection, ReplaceResult,
    };
    use crate::app::test_utils::{
        NAME_LEN, NUM_DIRS, NUM_FILES, NUM_HIDDEN, NUM_NESTED, empty_fs, filter_dirs, filter_files,
        simple_fs,
    };
    use crate::fl;

    #[test]
    fn a_move_into_the_folder_things_are_in_leaves_them_out() {
        let here = PathBuf::from("/files/here");
        let moving = |paths: &[&str]| Operation::Move {
            paths: paths.iter().map(PathBuf::from).collect(),
            to: here.clone(),
            cross_device_copy: false,
        };

        // Dropped back where they came from: nothing to do
        assert!(
            moving(&["/files/here/a", "/files/here/b"])
                .without_no_op_moves()
                .is_none()
        );

        // Only what really moves is kept
        match moving(&["/files/here/a", "/files/there/b"]).without_no_op_moves() {
            Some(Operation::Move { paths, to, .. }) => {
                assert_eq!(paths, [PathBuf::from("/files/there/b")]);
                assert_eq!(to, here);
            }
            other => panic!("expected the move of b, got {other:?}"),
        }

        // A copy into its own folder makes a duplicate, which is wanted
        let copy = Operation::Copy {
            paths: vec![PathBuf::from("/files/here/a")],
            to: here.clone(),
        };
        assert!(copy.without_no_op_moves().is_some());
    }

    /// A name that is not UTF-8 still gets a copy's name of its own, never
    /// the folder it is copied into
    #[test]
    fn a_copy_of_a_name_not_utf8_gets_its_own_name() -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let fs = empty_fs()?;
        let path = fs.path();
        let name = std::ffi::OsStr::from_bytes(b"bad\xff");
        fs::create_dir(path.join(name))?;
        let file = std::ffi::OsStr::from_bytes(b"f\xff.tar.gz");
        fs::write(path.join(file), b"")?;

        let copy = super::copy_unique_path(&path.join(name), path);
        let mut want = name.to_owned();
        want.push(format!(" ({} 1)", fl!("copy_noun")));
        assert_eq!(copy, path.join(want));

        let copy = super::copy_unique_path(&path.join(file), path);
        let mut want = std::ffi::OsStr::from_bytes(b"f\xff").to_owned();
        want.push(format!(" ({} 1).tar.gz", fl!("copy_noun")));
        assert_eq!(copy, path.join(want));
        Ok(())
    }

    /// Simple wrapper around `[Operation::Copy]`
    pub async fn operation_copy(
        paths: Vec<PathBuf>,
        to: PathBuf,
    ) -> Result<OperationSelection, OperationError> {
        let id = fastrand::u64(0..u64::MAX);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let paths_clone = paths.clone();
        let to_clone = to.clone();

        // Wrap this into its own future so that it may be polled concurerntly with the message handler.
        let handle_copy = async move {
            Operation::Copy {
                paths: paths_clone,
                to: to_clone,
            }
            .perform(&tx, Controller::default())
            .await
        };

        // Concurrently handling messages will prevent the mpsc channel from blocking when full.
        let handle_messages = async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    super::Prompt::Replace { answer: tx, .. } => {
                        debug!("[{id}] Replace request");
                        tx.send(ReplaceResult::Cancel)
                            .await
                            .expect("Sending a response to a replace request should succeed");
                    }
                    super::Prompt::Blocked { answer: tx, .. } => {
                        tx.send(BlockedAnswer::Cancel)
                            .await
                            .expect("Sending an answer to a blocked question should succeed");
                    }
                }
            }
        };

        future::join(handle_messages, handle_copy).await.1
    }

    /// Run any [`Operation`], answering every replace dialog with `reply`.
    async fn perform(
        operation: Operation,
        reply: ReplaceResult,
    ) -> Result<OperationSelection, OperationError> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle_operation = async move { operation.perform(&tx, Controller::default()).await };
        let handle_messages = async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    super::Prompt::Replace { answer: tx, .. } => {
                        tx.send(reply)
                            .await
                            .expect("Sending a response to a replace request should succeed");
                    }
                    super::Prompt::Blocked { answer: tx, .. } => {
                        tx.send(BlockedAnswer::Cancel)
                            .await
                            .expect("Sending an answer to a blocked question should succeed");
                    }
                }
            }
        };
        future::join(handle_messages, handle_operation).await.1
    }

    /// Run any [`Operation`], answering every question about a path it may
    /// not touch with `answer`, which also learns whether anything after that
    /// path could be asked about, and every replace dialog with Cancel. Returns
    /// the questions asked, in order, beside the result.
    async fn perform_blocked(
        operation: Operation,
        mut answer: impl FnMut(&Blocked, usize, bool) -> BlockedAnswer,
    ) -> (Result<OperationSelection, OperationError>, Vec<Blocked>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle_operation = async move { operation.perform(&tx, Controller::default()).await };
        let handle_messages = async move {
            let mut asked = Vec::new();
            while let Some(msg) = rx.recv().await {
                match msg {
                    super::Prompt::Replace { answer: tx, .. } => {
                        tx.send(ReplaceResult::Cancel)
                            .await
                            .expect("Sending a response to a replace request should succeed");
                    }
                    super::Prompt::Blocked {
                        blocked,
                        ask,
                        answer: tx,
                        ..
                    } => {
                        let reply = answer(&blocked, ask.count, ask.retry);
                        asked.push(blocked);
                        tx.send(reply)
                            .await
                            .expect("Sending an answer to a blocked question should succeed");
                    }
                }
            }
            asked
        };
        let (asked, result) = future::join(handle_messages, handle_operation).await;
        (result, asked)
    }

    /// Takes every permission from `path`, and whether that stops this user:
    /// running with privileges that ignore the mode, nothing is denied and
    /// there is nothing to test.
    fn deny(path: &std::path::Path, mode: u32) -> io::Result<bool> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        let denied = if path.is_dir() {
            if mode & 0o400 == 0 {
                fs::read_dir(path).is_err()
            } else {
                // Not writable: a probe file cannot be made
                let probe = path.join(".probe");
                let made = File::create(&probe).is_ok();
                let _ = fs::remove_file(&probe);
                !made
            }
        } else {
            File::open(path).is_err()
        };
        Ok(denied)
    }

    /// Gives `path` back its permissions, so the test's folder can be removed.
    fn allow(path: &std::path::Path) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o755 } else { 0o644 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }

    fn copy(paths: Vec<PathBuf>, to: PathBuf) -> Operation {
        Operation::Copy { paths, to }
    }

    #[test(compio::test)]
    async fn an_unreadable_file_is_asked_about_and_skipped() -> io::Result<()> {
        let fs = empty_fs()?;
        let from = fs.path().join("source");
        fs::create_dir(&from)?;
        fs::write(from.join("a.txt"), b"A")?;
        fs::write(from.join("z.txt"), b"Z")?;
        let locked = from.join("locked.txt");
        fs::write(&locked, b"L")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        if !deny(&locked, 0o000)? {
            return Ok(());
        }

        let (result, asked) = perform_blocked(copy(vec![from.clone()], to.clone()), |_, _, _| {
            BlockedAnswer::Skip(false)
        })
        .await;
        allow(&locked)?;

        let sel = result.expect("a skipped file does not fail the copy");
        assert_eq!(asked, [Blocked::Read(locked.clone())]);
        assert_eq!(sel.skipped, [locked]);
        assert_eq!(fs::read(to.join("source/a.txt"))?, b"A");
        assert_eq!(fs::read(to.join("source/z.txt"))?, b"Z");
        assert!(
            !to.join("source/locked.txt").exists(),
            "a skipped file leaves nothing behind"
        );
        Ok(())
    }

    #[test(compio::test)]
    async fn same_for_the_rest_asks_once() -> io::Result<()> {
        let fs = empty_fs()?;
        let from = fs.path().join("source");
        fs::create_dir(&from)?;
        let (one, two) = (from.join("one.txt"), from.join("two.txt"));
        fs::write(&one, b"1")?;
        fs::write(&two, b"2")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        if !deny(&one, 0o000)? || !deny(&two, 0o000)? {
            return Ok(());
        }

        let (result, asked) =
            perform_blocked(copy(vec![from], to), |_, _, _| BlockedAnswer::Skip(true)).await;
        allow(&one)?;
        allow(&two)?;

        let mut skipped = result.expect("skipping does not fail the copy").skipped;
        skipped.sort();
        assert_eq!(asked.len(), 1, "asked once for both: {asked:?}");
        assert_eq!(skipped, [one, two]);
        Ok(())
    }

    #[test(compio::test)]
    async fn cancel_stops_the_copy_as_cancelled() -> io::Result<()> {
        let fs = empty_fs()?;
        let locked = fs.path().join("locked.txt");
        fs::write(&locked, b"L")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        if !deny(&locked, 0o000)? {
            return Ok(());
        }

        let (result, asked) = perform_blocked(copy(vec![locked.clone()], to.clone()), |_, _, _| {
            BlockedAnswer::Cancel
        })
        .await;
        allow(&locked)?;

        let err = result.expect_err("a cancelled copy is an error");
        assert_eq!(err.to_string(), fl!("cancelled"));
        assert_eq!(asked.len(), 1);
        assert!(!to.join("locked.txt").exists());
        Ok(())
    }

    #[test(compio::test)]
    async fn an_unlistable_folder_is_asked_about_and_left_out() -> io::Result<()> {
        let fs = empty_fs()?;
        let from = fs.path().join("source");
        let locked = from.join("locked");
        fs::create_dir_all(&locked)?;
        fs::write(locked.join("inside.txt"), b"I")?;
        fs::write(from.join("a.txt"), b"A")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        if !deny(&locked, 0o000)? {
            return Ok(());
        }

        let (result, asked) = perform_blocked(copy(vec![from.clone()], to.clone()), |_, _, _| {
            BlockedAnswer::Skip(false)
        })
        .await;
        allow(&locked)?;

        let sel = result.expect("a skipped folder does not fail the copy");
        assert_eq!(asked, [Blocked::List(locked.clone())]);
        assert_eq!(sel.skipped, [locked]);
        assert_eq!(fs::read(to.join("source/a.txt"))?, b"A");
        assert!(
            !to.join("source/locked").exists(),
            "not even an empty folder is made for it"
        );
        Ok(())
    }

    #[test(compio::test)]
    async fn a_move_that_could_not_remove_its_originals_asks_first() -> io::Result<()> {
        let fs = empty_fs()?;
        let from = fs.path().join("source");
        fs::create_dir(&from)?;
        let file = from.join("f.txt");
        fs::write(&file, b"F")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        // Not writable: its entries cannot be removed, and as a folder with
        // a new parent it cannot be renamed either, so the move copies
        if !deny(&from, 0o555)? {
            return Ok(());
        }

        let (result, asked) = perform_blocked(
            Operation::Move {
                paths: vec![from.clone()],
                to: to.clone(),
                cross_device_copy: false,
            },
            |_, _, _| BlockedAnswer::Skip(false),
        )
        .await;
        allow(&from)?;

        // Asked before anything is copied, and Skip leaves it all alone
        let sel = result.expect("skipping an item does not fail the move");
        assert_eq!(
            asked,
            [Blocked::Move {
                path: from.clone(),
                folder: from.clone(),
                read_only: false,
            }]
        );
        assert_eq!(sel.skipped, std::slice::from_ref(&from));
        assert!(!to.join("source").exists(), "nothing is copied");
        assert_eq!(fs::read(&file)?, b"F", "the original stays");
        Ok(())
    }

    #[test(compio::test)]
    async fn a_folder_this_user_cannot_write_to_is_asked_about_first() -> io::Result<()> {
        let fs = empty_fs()?;
        let file = fs.path().join("a.txt");
        fs::write(&file, b"A")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        if !deny(&to, 0o555)? {
            return Ok(());
        }

        let (result, asked) = perform_blocked(copy(vec![file], to.clone()), |_, _, _| {
            BlockedAnswer::Skip(false)
        })
        .await;
        allow(&to)?;

        // Skipping the folder leaves out everything going there
        result.expect("skipping is not a failure");
        assert_eq!(asked, [Blocked::Destination(to.clone())]);
        assert!(!to.join("a.txt").exists());
        Ok(())
    }

    #[test(compio::test)]
    async fn questions_before_the_operation_offer_no_retry() -> io::Result<()> {
        let fs = empty_fs()?;
        let locked = fs.path().join("locked.txt");
        fs::write(&locked, b"L")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        if !deny(&locked, 0o000)? {
            return Ok(());
        }

        let mut retries = Vec::new();
        let (result, _) = perform_blocked(copy(vec![locked.clone()], to), |_, _, retry| {
            retries.push(retry);
            BlockedAnswer::Skip(false)
        })
        .await;
        allow(&locked)?;

        result.expect("skipping does not fail the copy");
        assert_eq!(retries, [false], "nothing has changed to try again for");
        Ok(())
    }

    #[test(compio::test)]
    async fn a_taken_name_is_asked_about_before_anything_is_copied() -> io::Result<()> {
        let fs = empty_fs()?;
        let (a, b) = (fs.path().join("a.txt"), fs.path().join("b.txt"));
        fs::write(&a, b"A")?;
        fs::write(&b, b"new")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        fs::write(to.join("b.txt"), b"old")?;

        // The helper answers the replace question with Cancel: asked during
        // the checks, it stops the copy before a.txt, listed first, is copied
        let (result, asked) = perform_blocked(copy(vec![a, b], to.clone()), |_, _, _| {
            BlockedAnswer::Skip(false)
        })
        .await;

        result.expect_err("cancelled");
        assert!(asked.is_empty(), "{asked:?}");
        assert!(
            !to.join("a.txt").exists(),
            "nothing was copied before asking"
        );
        assert_eq!(fs::read(to.join("b.txt"))?, b"old");
        Ok(())
    }

    #[test(compio::test)]
    async fn a_skip_settles_a_later_problem_with_the_same_file() -> io::Result<()> {
        let fs = empty_fs()?;
        let locked = fs.path().join("locked.txt");
        fs::write(&locked, b"L")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        // Taken at the destination too: a second problem, after the first
        fs::write(to.join("locked.txt"), b"old")?;
        if !deny(&locked, 0o000)? {
            return Ok(());
        }

        let (result, asked) = perform_blocked(copy(vec![locked.clone()], to.clone()), |_, _, _| {
            BlockedAnswer::Skip(false)
        })
        .await;
        allow(&locked)?;

        // Had the replace question been asked, the helper's Cancel would
        // have failed the copy
        result.expect("the clash was never asked about");
        assert_eq!(asked, [Blocked::Read(locked)]);
        assert_eq!(fs::read(to.join("locked.txt"))?, b"old");
        Ok(())
    }

    #[test(compio::test)]
    async fn same_for_the_rest_covers_its_own_kind_only() -> io::Result<()> {
        let fs = empty_fs()?;
        let from = fs.path().join("source");
        let locked = from.join("locked");
        fs::create_dir_all(&locked)?;
        let secret = from.join("secret.txt");
        fs::write(&secret, b"S")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        if !deny(&locked, 0o000)? || !deny(&secret, 0o000)? {
            return Ok(());
        }

        // Ticked on the first question, of one kind: the other kind is
        // still asked
        let (result, asked) =
            perform_blocked(copy(vec![from], to), |_, _, _| BlockedAnswer::Skip(true)).await;
        allow(&locked)?;
        allow(&secret)?;

        result.expect("skipping does not fail the copy");
        assert_eq!(asked.len(), 2, "{asked:?}");
        Ok(())
    }

    #[test(compio::test)]
    async fn same_for_the_rest_is_offered_only_with_more_to_come() -> io::Result<()> {
        let fs = empty_fs()?;
        let from = fs.path().join("source");
        fs::create_dir(&from)?;
        let (one, two) = (from.join("one.txt"), from.join("two.txt"));
        fs::write(&one, b"1")?;
        fs::write(&two, b"2")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        if !deny(&one, 0o000)? || !deny(&two, 0o000)? {
            return Ok(());
        }

        // Two unreadable files: the first is asked with both left, the
        // second with itself only
        let mut counts = Vec::new();
        let (result, _) = perform_blocked(copy(vec![from], to.clone()), |_, count, _| {
            counts.push(count);
            BlockedAnswer::Skip(false)
        })
        .await;
        result.expect("skipping does not fail the copy");
        assert_eq!(counts, [2, 1], "the count of those left, each included");

        allow(&one)?;
        allow(&two)?;

        // One file on its own, moved out of a folder that cannot lose it:
        // asked before it is moved, with nothing after it
        let lone = fs.path().join("lone");
        fs::create_dir(&lone)?;
        let file = lone.join("f.txt");
        fs::write(&file, b"F")?;
        if !deny(&lone, 0o555)? {
            return Ok(());
        }
        let mut counts = Vec::new();
        let (result, asked) = perform_blocked(
            Operation::Move {
                paths: vec![file.clone()],
                to: to.clone(),
                cross_device_copy: false,
            },
            |_, count, _| {
                counts.push(count);
                BlockedAnswer::Skip(false)
            },
        )
        .await;
        allow(&lone)?;
        result.expect("skipping does not fail the move");
        assert_eq!(
            asked,
            [Blocked::Move {
                path: file,
                folder: lone,
                read_only: false,
            }]
        );
        assert_eq!(counts, [1]);
        Ok(())
    }

    #[test]
    fn a_failure_says_why_in_words() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(
            super::failure_text(std::path::Path::new("/a/secret.env"), &denied),
            fl!(
                "failed-path",
                name = "secret.env",
                reason = fl!("reason-no-permission")
            )
        );
        // Found inside the context an open adds around it
        let wrapped: Box<dyn std::error::Error> =
            anyhow::Error::from(io::Error::from(io::ErrorKind::StorageFull))
                .context("failed to write")
                .into();
        assert_eq!(
            super::failure_text(std::path::Path::new("/a/big.iso"), &*wrapped),
            fl!(
                "failed-path",
                name = "big.iso",
                reason = fl!("reason-drive-full")
            )
        );
        // One it does not know keeps its own message
        let other = io::Error::other("the cable came out");
        assert_eq!(
            super::failure_text(std::path::Path::new("/a/f"), &other),
            fl!("failed-path", name = "f", reason = "the cable came out")
        );
    }

    #[test]
    fn a_link_cannot_be_retried_and_the_rest_can() {
        let link = Blocked::Link {
            path: PathBuf::from("/p/result"),
            fs: Some("exFAT"),
        };
        assert!(!link.can_retry());
        assert!(Blocked::Read(PathBuf::from("/p/f")).can_retry());
        assert_eq!(link.path(), std::path::Path::new("/p/result"));
    }

    #[test(compio::test)]
    async fn a_cancelled_copy_takes_away_what_it_copied() -> io::Result<()> {
        let fs = empty_fs()?;
        let (a, locked) = (fs.path().join("a.txt"), fs.path().join("locked.txt"));
        fs::write(&a, b"A")?;
        fs::write(&locked, b"L")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        if !deny(&locked, 0o000)? {
            return Ok(());
        }

        let (result, asked) = perform_blocked(
            copy(vec![a.clone(), locked.clone()], to.clone()),
            |_, _, _| BlockedAnswer::Cancel,
        )
        .await;
        allow(&locked)?;

        let err = result.expect_err("cancelled");
        assert_eq!(err.to_string(), fl!("cancelled"));
        assert_eq!(asked, [Blocked::Read(locked)]);
        assert!(!to.join("a.txt").exists(), "the copy made before is gone");
        assert!(err.partial.is_empty(), "nothing is left to undo");
        assert_eq!(fs::read(&a)?, b"A");
        Ok(())
    }

    #[test(compio::test)]
    async fn a_cancelled_move_puts_back_what_it_moved() -> io::Result<()> {
        let fs = empty_fs()?;
        let src = fs.path().join("src");
        fs::create_dir(&src)?;
        // Moved whole by a rename before anything is asked
        let a = src.join("a.txt");
        fs::write(&a, b"A")?;
        // Then asked about: its original could not be removed
        let locked = fs.path().join("locked");
        fs::create_dir(&locked)?;
        fs::write(locked.join("f.txt"), b"F")?;
        let to = fs.path().join("dest");
        fs::create_dir(&to)?;
        if !deny(&locked, 0o555)? {
            return Ok(());
        }

        let (result, asked) = perform_blocked(
            Operation::Move {
                paths: vec![a.clone(), locked.clone()],
                to: to.clone(),
                cross_device_copy: false,
            },
            |_, _, _| BlockedAnswer::Cancel,
        )
        .await;
        allow(&locked)?;

        result.expect_err("cancelled");
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert_eq!(fs::read(&a)?, b"A", "back where it was");
        assert_eq!(
            fs::read_dir(&to)?.count(),
            0,
            "nothing left at the destination"
        );
        Ok(())
    }

    #[test(compio::test)]
    async fn a_cancelled_move_across_filesystems_leaves_nothing_behind() -> io::Result<()> {
        let Ok(shm) = tempfile::tempdir_in("/dev/shm") else {
            return Ok(());
        };
        let fs = empty_fs()?;
        let from = fs.path().join("folder");
        fs::create_dir(&from)?;
        fs::write(from.join("a.txt"), b"A")?;
        let locked = from.join("z.txt");
        fs::write(&locked, b"Z")?;
        if !deny(&locked, 0o000)? {
            return Ok(());
        }

        let (result, asked) = perform_blocked(
            Operation::Move {
                paths: vec![from.clone()],
                to: shm.path().to_path_buf(),
                cross_device_copy: false,
            },
            |_, _, _| BlockedAnswer::Cancel,
        )
        .await;
        allow(&locked)?;

        result.expect_err("cancelled");
        assert_eq!(asked, [Blocked::Read(locked)]);
        assert!(!shm.path().join("folder").exists(), "nothing left behind");
        assert_eq!(fs::read(from.join("a.txt"))?, b"A");
        Ok(())
    }

    /// Runs `operation`, which asks nothing, and gives back its controller
    /// as the operation left it.
    async fn perform_watched(
        operation: Operation,
    ) -> (Result<OperationSelection, OperationError>, Controller) {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let controller = Controller::default();
        let watched = controller.clone();
        let result = operation.perform(&tx, controller).await;
        (result, watched)
    }

    /// An operation dropped part-way, as when the runtime lets its task go,
    /// is cancelled, so the blocking jobs it started stop too
    #[test(compio::test)]
    async fn an_operation_dropped_part_way_is_cancelled() -> io::Result<()> {
        let fs = empty_fs()?;
        let (from, to) = (fs.path().join("from"), fs.path().join("to"));
        fs::create_dir_all(&from)?;
        fs::create_dir_all(&to)?;
        fs::write(from.join("a"), b"new")?;
        fs::write(to.join("a"), b"old")?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let prompts = tx;
        let controller = Controller::default();
        let copy = Operation::Copy {
            paths: vec![from.join("a")],
            to,
        }
        .perform(&prompts, controller.clone());
        let mut copy = Box::pin(copy);
        // Run until it asks about the name taken, then let it go unanswered
        let asked = future::select(copy.as_mut(), Box::pin(rx.recv())).await;
        assert!(matches!(asked, future::Either::Right((Some(_), _))));
        assert!(!controller.is_cancelled());
        drop(copy);
        assert!(controller.is_cancelled());
        Ok(())
    }

    /// An item whose origin is not recorded has nowhere to go back to: its
    /// restore fails, naming it, and it stays in the trash
    #[test(compio::test)]
    async fn an_item_of_unknown_origin_is_not_restored() -> io::Result<()> {
        let fs = empty_fs()?;
        let kept = fs.path().join("trash/files/lone");
        fs::create_dir_all(kept.parent().expect("parent"))?;
        fs::write(&kept, b"L")?;
        let item = crate::trashing::TrashItem {
            id: crate::trashing::TrashId::Orphan(kept.clone()),
            name: "lone".into(),
            original_parent: None,
            time_deleted: 1,
        };
        let (result, _) = perform_watched(Operation::Restore { items: vec![item] }).await;
        let err = result.expect_err("nowhere to go");
        assert!(err.to_string().contains("lone"), "{err}");
        assert_eq!(fs::read(&kept)?, b"L");
        Ok(())
    }

    /// A failure at a path keeps the path and the I/O error, and the
    /// operation ends failed, not cancelled
    #[test(compio::test)]
    async fn a_failure_at_a_path_says_where_and_ends_failed() -> io::Result<()> {
        let fs = empty_fs()?;
        let missing = fs.path().join("missing");
        let (result, controller) = perform_watched(Operation::SetPermissions {
            path: missing.clone(),
            mode: 0o600,
            mask: 0o777,
        })
        .await;
        let err = result.expect_err("nothing to change");
        match &err.kind {
            super::OperationErrorType::Path { path, cause } => {
                assert_eq!(path, &missing);
                assert_eq!(cause.io, Some(io::ErrorKind::NotFound));
            }
            other => panic!("not a failure at a path: {other:?}"),
        }
        assert!(!err.is_cancelled());
        assert!(controller.is_failed());
        Ok(())
    }

    /// An error made from a message alone ends the operation failed: it once
    /// left the controller running, and dropping the operation's own
    /// controller then cancelled it, so the failure showed as a cancel
    #[test(compio::test)]
    async fn a_failure_without_a_path_ends_failed_not_cancelled() {
        let (result, controller) = perform_watched(Operation::WriteFile {
            path: PathBuf::from("/"),
            data: super::Payload::from(&b"data"[..]),
        })
        .await;
        let err = result.expect_err("no folder to write in");
        assert!(matches!(err.kind, super::OperationErrorType::Generic(_)));
        assert!(controller.is_failed() && !controller.is_cancelled());
    }

    /// A dangling link renamed is still there to rename back
    #[test(compio::test)]
    async fn a_renamed_dangling_link_can_be_undone() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        std::os::unix::fs::symlink(path.join("nowhere"), path.join("a"))?;
        let rename = Operation::Rename {
            from: path.join("a"),
            to: path.join("b"),
        };
        let result = perform(rename.clone(), ReplaceResult::Cancel)
            .await
            .expect("renamed");
        run_undo(rename.undo(&result)).await.expect("undone");
        assert!(path.join("a").symlink_metadata()?.is_symlink());
        Ok(())
    }

    /// A copied folder keeps its mode: a private one stays private, a
    /// read-only one is read-only once filled
    #[test(compio::test)]
    async fn a_copied_folder_keeps_its_mode() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let fs = empty_fs()?;
        let path = fs.path();
        fs::create_dir_all(path.join("private/locked"))?;
        fs::write(path.join("private/locked/a"), b"A")?;
        fs::set_permissions(
            path.join("private/locked"),
            fs::Permissions::from_mode(0o555),
        )?;
        fs::set_permissions(path.join("private"), fs::Permissions::from_mode(0o700))?;
        fs::create_dir(path.join("to"))?;

        operation_copy(vec![path.join("private")], path.join("to"))
            .await
            .expect("copied");
        let mode = |p: &str| -> io::Result<u32> {
            Ok(fs::metadata(path.join(p))?.permissions().mode() & 0o7777)
        };
        assert_eq!(mode("to/private")?, 0o700);
        assert_eq!(mode("to/private/locked")?, 0o555);
        assert_eq!(fs::read(path.join("to/private/locked/a"))?, b"A");
        // So the temporary folder can go
        fs::set_permissions(
            path.join("private/locked"),
            fs::Permissions::from_mode(0o755),
        )?;
        fs::set_permissions(
            path.join("to/private/locked"),
            fs::Permissions::from_mode(0o755),
        )?;
        Ok(())
    }

    /// Permission changes made from one out-of-date mode each keep the
    /// other's, and one made through a link from the link's own mode
    /// changes only the part asked for
    #[test(compio::test)]
    async fn permission_changes_take_only_the_bits_they_change() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let fs = empty_fs()?;
        let path = fs.path();
        let file = path.join("f");
        fs::write(&file, b"f")?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        let set = |path: PathBuf, mode, mask| Operation::SetPermissions { path, mode, mask };
        // Both from the mode listed before either ran
        perform(set(file.clone(), 0o100640, 0o070), ReplaceResult::Cancel)
            .await
            .expect("group set");
        perform(set(file.clone(), 0o100604, 0o007), ReplaceResult::Cancel)
            .await
            .expect("other set");
        assert_eq!(fs::metadata(&file)?.permissions().mode() & 0o7777, 0o644);

        // A link's own mode is 0777; taking away Other's from it leaves the
        // target's group as it was
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        std::os::unix::fs::symlink(&file, path.join("link"))?;
        perform(
            set(path.join("link"), 0o120770, 0o007),
            ReplaceResult::Cancel,
        )
        .await
        .expect("other set through the link");
        assert_eq!(fs::metadata(&file)?.permissions().mode() & 0o7777, 0o600);
        Ok(())
    }

    /// Run `[Operation::Move]`, answering every replace dialog with `reply`.
    async fn operation_move(
        paths: Vec<PathBuf>,
        to: PathBuf,
        reply: ReplaceResult,
    ) -> Result<OperationSelection, OperationError> {
        perform(
            Operation::Move {
                paths,
                to,
                cross_device_copy: false,
            },
            reply,
        )
        .await
    }

    /// Run everything a completed operation's undo produced, the way the app
    /// does: it refuses the whole entry unless every part of it still applies.
    async fn run_undo(undo: Vec<Operation>) -> Result<(), OperationError> {
        assert!(
            !undo.is_empty(),
            "there was nothing to undo in the first place"
        );
        assert!(
            undo.iter().all(Operation::is_applicable),
            "the app skips an undo entry unless all of it applies: {undo:?}"
        );
        for operation in undo {
            // As the app does before launching an undo: a move that emptied a
            // folder removed it, and putting the files back needs it there
            if let Operation::Move { to, .. } = &operation {
                fs::create_dir_all(to).expect("the folder can be put back");
            }
            perform(operation, ReplaceResult::Cancel).await?;
        }
        Ok(())
    }

    /// Run `[Operation::Extract]`, answering every replace dialog with `reply`.
    async fn operation_extract(
        paths: Vec<PathBuf>,
        to: PathBuf,
        as_folder: bool,
        reply: ReplaceResult,
    ) -> Result<OperationSelection, OperationError> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle_extract = async move {
            Operation::Extract {
                paths: paths.into_boxed_slice(),
                to,
                password: None,
                as_folder,
            }
            .perform(&tx, Controller::default())
            .await
        };
        let handle_messages = async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    super::Prompt::Replace { answer: tx, .. } => {
                        tx.send(reply)
                            .await
                            .expect("Sending a response to a replace request should succeed");
                    }
                    _ => unreachable!("unexpected message from extract operation"),
                }
            }
        };
        future::join(handle_messages, handle_extract).await.1
    }

    /// Write a zip archive containing `a.txt` and `sub/b.txt`.
    fn write_test_zip(path: &std::path::Path) -> io::Result<()> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(File::create(path)?);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("a.txt", options)?;
        zip.write_all(b"archive a")?;
        zip.add_directory("sub", options)?;
        zip.start_file("sub/b.txt", options)?;
        zip.write_all(b"archive b")?;
        zip.finish()?;
        Ok(())
    }

    /// Write a tar.gz archive containing `a.txt` and `sub/b.txt`.
    fn write_test_tar_gz(path: &std::path::Path) -> io::Result<()> {
        let encoder = flate2::write::GzEncoder::new(File::create(path)?, Default::default());
        let mut tar = tar::Builder::new(encoder);
        for (name, data) in [
            ("a.txt", &b"archive a"[..]),
            ("sub/b.txt", &b"archive b"[..]),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            tar.append_data(&mut header, name, data)?;
        }
        tar.into_inner()?.finish()?;
        Ok(())
    }

    fn assert_no_staging_dir(dir: &std::path::Path) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let name = entry?.file_name();
            let name = name.to_string_lossy();
            assert!(
                !name.contains(".extracting"),
                "staging directory {name} should have been removed"
            );
        }
        Ok(())
    }

    /// The failed operation dialog prints the operation, so a paste must not
    /// expand its bytes into the message
    #[test]
    fn a_payload_prints_its_size_not_its_bytes() {
        let op = Operation::WriteFile {
            path: "/tmp/pasted.bin".into(),
            data: vec![0_u8; 1024 * 1024].into(),
        };
        let shown = format!("{op:#?}");
        assert!(shown.contains("1048576 bytes"), "{shown}");
        assert!(
            shown.lines().count() < 20,
            "the dialog text must stay readable, got {} lines",
            shown.lines().count()
        );
    }

    /// Pasting where a dangling symlink already holds the name must pick a
    /// different name rather than retrying one that can never be created
    #[test(compio::test)]
    async fn write_file_steps_past_a_dangling_symlink() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let target = path.join("pasted.txt");
        std::os::unix::fs::symlink(path.join("missing"), &target)?;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = Operation::WriteFile {
            path: target.clone(),
            data: (&b"pasted"[..]).into(),
        }
        .perform(&tx, Controller::default())
        .await
        .expect("the paste should find a free name");

        let written = result.created.first().expect("a path was created");
        assert_ne!(written, &target, "must not write through the dangling link");
        assert_eq!(fs::read(written)?, b"pasted");
        assert!(
            fs::symlink_metadata(&target)?.file_type().is_symlink(),
            "the existing link is left alone"
        );
        Ok(())
    }

    /// A copy that cannot create its destination must leave whatever holds
    /// that name alone. `create_new` refuses to write through a dangling
    /// symlink, and error cleanup used to unlink it without replacing it and
    /// without ever asking about replacement.
    #[test(compio::test)]
    async fn a_failed_copy_leaves_an_occupied_destination_alone() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let from = path.join("source");
        fs::create_dir(&from)?;
        fs::write(from.join("file.txt"), b"SOURCE")?;
        let to = path.join("dest");
        fs::create_dir(&to)?;
        let occupied = to.join("file.txt");
        std::os::unix::fs::symlink(path.join("missing"), &occupied)?;

        let result = operation_copy(vec![from.join("file.txt")], to).await;
        assert!(
            result.is_err(),
            "the copy cannot create a destination that is already taken"
        );
        assert!(
            fs::symlink_metadata(&occupied)?.file_type().is_symlink(),
            "a destination this copy did not create is not its to remove"
        );
        Ok(())
    }

    /// The fast move path must not replace anything. `rename(2)` overwrites
    /// its destination, and `exists` follows symlinks, so a dangling one read
    /// as a free name and was silently replaced.
    #[test(compio::test)]
    async fn a_move_does_not_overwrite_a_dangling_symlink() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let from = path.join("source");
        fs::create_dir(&from)?;
        let source = from.join("file.txt");
        fs::write(&source, b"SOURCE")?;
        let to = path.join("dest");
        fs::create_dir(&to)?;
        let occupied = to.join("file.txt");
        std::os::unix::fs::symlink(path.join("missing"), &occupied)?;

        let result = operation_move(vec![source.clone()], to, ReplaceResult::Cancel).await;
        assert!(result.is_err(), "there is no free name to move to");
        assert!(
            fs::symlink_metadata(&occupied)?.file_type().is_symlink(),
            "the link that was already there is left alone"
        );
        assert_eq!(fs::read(&source)?, b"SOURCE", "the source is still there");
        Ok(())
    }

    /// Keeping both must record the file that was moved, not the one that was
    /// already at the destination: the cleanup op still carries the name the
    /// move was planned with, and undo used to carry that file off instead.
    #[test(compio::test)]
    async fn keeping_both_records_the_file_that_moved() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let from = path.join("source");
        fs::create_dir(&from)?;
        let source = from.join("file.txt");
        fs::write(&source, b"SOURCE")?;
        let to = path.join("dest");
        fs::create_dir(&to)?;
        let existing = to.join("file.txt");
        fs::write(&existing, b"DESTINATION")?;

        let result = operation_move(vec![source.clone()], to.clone(), ReplaceResult::KeepBoth)
            .await
            .expect("keeping both always has a free name to take");

        assert_eq!(
            fs::read(&existing)?,
            b"DESTINATION",
            "the file that was already there is untouched"
        );
        assert!(!source.exists(), "the source was moved away");
        assert!(
            !result.selected.contains(&existing),
            "the selection names what moved, not what was already there: {:?}",
            result.selected
        );

        let undo = Operation::Move {
            paths: vec![source.clone()],
            to,
            cross_device_copy: false,
        }
        .undo(&result);
        run_undo(undo).await.expect("the undo runs");

        assert_eq!(
            fs::read(&source)?,
            b"SOURCE",
            "the file comes back under the name it had, not the one the \
             conflict gave it"
        );
        assert_eq!(
            fs::read(&existing)?,
            b"DESTINATION",
            "and the file that was already there is still not touched"
        );
        Ok(())
    }

    /// Merging a folder into one that was already there transfers only its
    /// children, so undoing must bring only those back: reversing the whole
    /// destination folder used to carry off files that were never moved.
    #[test(compio::test)]
    async fn undoing_a_merge_leaves_the_destination_folder_alone() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let from = path.join("source");
        fs::create_dir_all(from.join("folder"))?;
        fs::write(from.join("folder/moved.txt"), b"MOVED")?;
        let to = path.join("dest");
        fs::create_dir_all(to.join("folder"))?;
        let untouched = to.join("folder/existing.txt");
        fs::write(&untouched, b"EXISTING")?;

        // The folder is asked about, and merged; nothing inside it clashes
        let result = operation_move(
            vec![from.join("folder")],
            to.clone(),
            ReplaceResult::Merge(false),
        )
        .await
        .expect("merged, the names inside the folders differ");

        assert_eq!(fs::read(to.join("folder/moved.txt"))?, b"MOVED");
        assert_eq!(fs::read(&untouched)?, b"EXISTING");

        let undo = Operation::Move {
            paths: vec![from.join("folder")],
            to,
            cross_device_copy: false,
        }
        .undo(&result);
        assert_eq!(
            undo,
            vec![Operation::Move {
                paths: vec![path.join("dest/folder/moved.txt")],
                to: from.join("folder"),
                cross_device_copy: false,
            }],
            "only the child that moved comes back"
        );

        // The forward move emptied the source folder and removed it, so the
        // undo has to put it back before it can put anything into it
        run_undo(undo).await.expect("the undo runs");
        assert_eq!(fs::read(from.join("folder/moved.txt"))?, b"MOVED");
        assert_eq!(
            fs::read(&untouched)?,
            b"EXISTING",
            "never moved, never back"
        );
        assert!(
            !path.join("dest/folder/moved.txt").exists(),
            "the child is no longer at the destination"
        );
        Ok(())
    }

    /// Keeping both inside a folder that is merged into another leaves the
    /// undo with a source folder the move itself removed, so putting the file
    /// back has to put the folder back first.
    #[test(compio::test)]
    async fn keeping_both_inside_a_merge_can_still_be_undone() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let from = path.join("source");
        fs::create_dir_all(from.join("folder"))?;
        let source = from.join("folder/child.txt");
        fs::write(&source, b"MOVED")?;
        let to = path.join("dest");
        fs::create_dir_all(to.join("folder"))?;
        let existing = to.join("folder/child.txt");
        fs::write(&existing, b"EXISTING")?;

        let result = operation_move(
            vec![from.join("folder")],
            to.clone(),
            ReplaceResult::KeepBoth,
        )
        .await
        .expect("keeping both always has a free name to take");

        assert_eq!(fs::read(&existing)?, b"EXISTING", "left where it was");
        assert!(
            !from.join("folder").exists(),
            "the move emptied the source folder and removed it"
        );

        let undo = Operation::Move {
            paths: vec![from.join("folder")],
            to,
            cross_device_copy: false,
        }
        .undo(&result);
        run_undo(undo).await.expect("the undo runs");

        assert_eq!(
            fs::read(&source)?,
            b"MOVED",
            "the file is back in the folder it came from, under its own name"
        );
        assert_eq!(fs::read(&existing)?, b"EXISTING", "and still left alone");
        Ok(())
    }

    /// Keeping both across a filesystem boundary has to come back across it
    /// too: the rename that puts a file back by its whole original path cannot
    /// cross one, so it falls back to the way the move went out.
    #[test(compio::test)]
    async fn keeping_both_across_filesystems_can_still_be_undone() -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;

        let fs = empty_fs()?;
        let from = fs.path().join("source");
        fs::create_dir(&from)?;

        // A second filesystem to move onto, or there is nothing to test here
        let Ok(other) = tempfile::TempDir::new_in("/dev/shm") else {
            return Ok(());
        };
        if fs::metadata(fs.path())?.dev() == fs::metadata(other.path())?.dev() {
            return Ok(());
        }
        let to = other.path().to_path_buf();

        let source = from.join("file.txt");
        fs::write(&source, b"SOURCE")?;
        let existing = to.join("file.txt");
        fs::write(&existing, b"DESTINATION")?;

        let result = operation_move(vec![source.clone()], to.clone(), ReplaceResult::KeepBoth)
            .await
            .expect("keeping both always has a free name to take");
        assert!(!source.exists(), "the move crossed the boundary");

        let undo = Operation::Move {
            paths: vec![source.clone()],
            to,
            cross_device_copy: false,
        }
        .undo(&result);
        run_undo(undo).await.expect("the undo runs");

        assert_eq!(
            fs::read(&source)?,
            b"SOURCE",
            "the file came back over the boundary, under the name it had"
        );
        assert_eq!(fs::read(&existing)?, b"DESTINATION", "still left alone");
        Ok(())
    }

    /// A copy whose source cannot be read must not leave the destination it
    /// opened behind. Both opens run at once, so the destination can already
    /// exist by the time the source error comes back.
    #[test(compio::test)]
    async fn a_copy_that_cannot_read_its_source_leaves_nothing_behind() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let fs = empty_fs()?;
        let path = fs.path();
        let from = path.join("source");
        fs::create_dir(&from)?;
        let unreadable = from.join("file.txt");
        fs::write(&unreadable, b"SOURCE")?;
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000))?;
        let to = path.join("dest");
        fs::create_dir(&to)?;

        if File::open(&unreadable).is_ok() {
            // Running with privileges that ignore the mode, so there is no
            // unreadable source here to test with
            return Ok(());
        }

        let result = operation_copy(vec![unreadable], to.clone()).await;
        assert!(result.is_err(), "the source cannot be read");
        assert!(
            !to.join("file.txt").exists(),
            "the empty destination this copy opened is its own to clear away"
        );
        Ok(())
    }

    /// A batch rename must never replace a file, whatever the preview said:
    /// it is a snapshot, and on a remote folder it does not check at all
    #[test(compio::test)]
    async fn batch_rename_refuses_to_replace_an_existing_file() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        fs::write(path.join("a.txt"), b"SOURCE")?;
        fs::write(path.join("b.txt"), b"VICTIM")?;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = Operation::BatchRename {
            renames: vec![(path.join("a.txt"), path.join("b.txt"))],
        }
        .perform(&tx, Controller::default())
        .await;

        assert!(result.is_err(), "renaming onto an existing file must fail");
        assert_eq!(fs::read(path.join("b.txt"))?, b"VICTIM");
        assert_eq!(fs::read(path.join("a.txt"))?, b"SOURCE");
        Ok(())
    }

    /// A selection from search results or Recents spans folders; the archive
    /// must hold every file under names relative to their common ancestor
    #[test(compio::test)]
    async fn compress_accepts_paths_from_different_folders() -> io::Result<()> {
        let fs_ = empty_fs()?;
        let path = fs_.path();
        fs::create_dir(path.join("a"))?;
        fs::create_dir(path.join("b"))?;
        fs::write(path.join("a/one.txt"), b"one")?;
        fs::write(path.join("b/two.txt"), b"two")?;

        // The app picks the destination from the first selected path's parent
        let to = path.join("a/one.tar.gz");
        perform(
            Operation::Compress {
                paths: vec![path.join("a/one.txt"), path.join("b/two.txt")],
                to: to.clone(),
                archive_type: ArchiveType::Tgz,
                password: None,
            },
            ReplaceResult::Cancel,
        )
        .await
        .expect("compressing files from two folders must succeed");

        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(File::open(&to)?));
        let mut names: Vec<PathBuf> = archive
            .entries()?
            .map(|entry| entry.and_then(|e| e.path().map(|p| p.into_owned())))
            .collect::<io::Result<_>>()?;
        names.sort();
        assert_eq!(
            names,
            vec![PathBuf::from("a/one.txt"), PathBuf::from("b/two.txt")]
        );
        Ok(())
    }

    #[test(compio::test)]
    async fn creating_a_file_never_truncates_an_existing_one() -> io::Result<()> {
        let fs_ = empty_fs()?;
        let path = fs_.path().join("notes.txt");
        fs::write(&path, b"work worth keeping")?;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = Operation::NewFile { path: path.clone() }
            .perform(&tx, Controller::default())
            .await;

        assert!(
            result.is_err(),
            "creating a file over an existing one must fail"
        );
        assert_eq!(
            fs::read(&path)?,
            b"work worth keeping",
            "the existing file was truncated"
        );
        Ok(())
    }

    #[test(compio::test)]
    async fn renaming_onto_an_existing_file_leaves_it_alone() -> io::Result<()> {
        let fs_ = empty_fs()?;
        let path = fs_.path();
        fs::write(path.join("a.txt"), b"SOURCE")?;
        fs::write(path.join("b.txt"), b"VICTIM")?;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = Operation::Rename {
            from: path.join("a.txt"),
            to: path.join("b.txt"),
        }
        .perform(&tx, Controller::default())
        .await;

        assert!(result.is_err(), "renaming onto an existing file must fail");
        assert_eq!(fs::read(path.join("b.txt"))?, b"VICTIM");
        assert_eq!(fs::read(path.join("a.txt"))?, b"SOURCE");
        Ok(())
    }

    #[test(compio::test)]
    async fn renaming_to_a_free_name_still_works() -> io::Result<()> {
        let fs_ = empty_fs()?;
        let path = fs_.path();
        fs::write(path.join("a.txt"), b"SOURCE")?;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Operation::Rename {
            from: path.join("a.txt"),
            to: path.join("b.txt"),
        }
        .perform(&tx, Controller::default())
        .await
        .expect("renaming to a free name should work");

        assert_eq!(fs::read(path.join("b.txt"))?, b"SOURCE");
        assert!(!path.join("a.txt").exists());
        Ok(())
    }

    /// Copying a file onto itself through a symlinked parent must leave it
    /// alone rather than unlink it behind the replace dialog
    #[test(compio::test)]
    async fn copy_onto_itself_through_a_symlinked_parent_keeps_the_file() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let real = path.join("docs");
        fs::create_dir(&real)?;
        fs::write(real.join("a.txt"), b"precious")?;
        let link = path.join("link-to-docs");
        std::os::unix::fs::symlink(&real, &link)?;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let run = async move {
            Operation::Copy {
                paths: vec![link.join("a.txt")],
                to: real.clone(),
            }
            .perform(&tx, Controller::default())
            .await
        };
        let replies = async move {
            let mut asked = 0;
            while let Some(msg) = rx.recv().await {
                if let super::Prompt::Replace { answer: tx, .. } = msg {
                    asked += 1;
                    let _ = tx.send(ReplaceResult::Replace(false)).await;
                }
            }
            asked
        };
        let (asked, result) = future::join(replies, run).await;

        result.expect("copying a file onto itself should succeed as a no-op");
        assert_eq!(asked, 0, "the user should not be asked about a self copy");
        assert_eq!(fs::read(path.join("docs/a.txt"))?, b"precious");
        Ok(())
    }

    /// A paste into a folder that was deleted underneath the tab must fail,
    /// not quietly bring the folder back.
    #[test(compio::test)]
    async fn copying_into_a_missing_folder_fails_without_creating_it() -> io::Result<()> {
        let fs = empty_fs()?;
        let source = fs.path().join("file.txt");
        fs::write(&source, b"SOURCE")?;
        let to = fs.path().join("gone");

        operation_copy(vec![source], to.clone())
            .await
            .expect_err("there is nowhere to copy to");
        assert!(!to.exists(), "the missing destination is not recreated");
        Ok(())
    }

    #[test]
    fn rename_no_replace_refuses_an_occupied_destination() {
        use super::rename_no_replace;
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");

        // A plain file at the destination is refused
        fs::write(&from, b"new").unwrap();
        fs::write(&to, b"mine").unwrap();
        let err = rename_no_replace(&from, &to).expect_err("must refuse an existing file");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&to).unwrap(), b"mine");

        // So is a dangling symlink, which `exists()` would not have seen
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path().join("missing"), &link).unwrap();
        let err = rename_no_replace(&from, &link).expect_err("must refuse a dangling symlink");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // A free destination still works
        let free = dir.path().join("free");
        rename_no_replace(&from, &free).unwrap();
        assert_eq!(fs::read(&free).unwrap(), b"new");
    }

    /// Moving a folder into one of its own subfolders can never complete:
    /// it must be refused up front, leaving the tree exactly as it was.
    #[test(compio::test)]
    async fn moving_a_folder_into_its_own_subfolder_is_refused() -> io::Result<()> {
        let fs = empty_fs()?;
        let b = fs.path().join("a/b");
        let c = b.join("c");
        fs::create_dir_all(&c)?;
        fs::write(b.join("file.txt"), b"KEEP")?;

        let result = operation_move(vec![b.clone()], c.clone(), ReplaceResult::Cancel).await;
        assert!(
            result.is_err(),
            "a move into its own subfolder must be refused: {result:?}"
        );

        assert_eq!(fs::read(b.join("file.txt"))?, b"KEEP");
        let entries: Vec<_> = fs::read_dir(&c)?.collect::<io::Result<_>>()?;
        assert!(
            entries.is_empty(),
            "nothing may land inside the subfolder: {entries:?}"
        );
        Ok(())
    }

    #[test]
    fn a_failure_that_did_nothing_records_no_undo() {
        let compress = Operation::Compress {
            paths: vec![PathBuf::from("/a/one")],
            to: PathBuf::from("/a/existing.zip"),
            archive_type: ArchiveType::Zip,
            password: None,
        };
        assert!(
            compress
                .undo_after_failure(&OperationSelection::default())
                .is_empty(),
            "a refused compress must not schedule the archive already there for the trash"
        );
        for op in [
            Operation::Rename {
                from: PathBuf::from("/a/x"),
                to: PathBuf::from("/a/y"),
            },
            Operation::NewFolder {
                path: PathBuf::from("/a/new"),
            },
        ] {
            assert!(
                op.undo_after_failure(&OperationSelection::default())
                    .is_empty()
            );
        }
    }

    #[test]
    fn undo_maps_operations_to_their_reverse() {
        use super::Operation;
        let sel = |paths: &[&str]| OperationSelection {
            selected: paths.iter().map(PathBuf::from).collect(),
            ..Default::default()
        };

        // Rename swaps
        let rename = Operation::Rename {
            from: "/a/x".into(),
            to: "/a/y".into(),
        };
        assert_eq!(
            rename.undo(&OperationSelection::default()),
            vec![Operation::Rename {
                from: "/a/y".into(),
                to: "/a/x".into()
            }]
        );

        // A batch reverses only the renames that went through, last first
        let batch = Operation::BatchRename {
            renames: vec![
                ("/a/1".into(), "/a/one".into()),
                ("/a/2".into(), "/a/two".into()),
                ("/a/3".into(), "/a/three".into()),
            ],
        };
        assert_eq!(
            batch.undo(&sel(&["/a/one", "/a/two"])),
            vec![Operation::BatchRename {
                renames: vec![
                    ("/a/two".into(), "/a/2".into()),
                    ("/a/one".into(), "/a/1".into()),
                ],
            }]
        );
        assert_eq!(batch.undo(&OperationSelection::default()).len(), 1);

        // Move sends each item back to its own source folder
        let moved = |pairs: &[(&str, &str)]| OperationSelection {
            moved: pairs
                .iter()
                .map(|(from, to)| (PathBuf::from(from), PathBuf::from(to)))
                .collect(),
            ..Default::default()
        };
        let mv = Operation::Move {
            paths: vec!["/a/one".into(), "/b/two".into()],
            to: "/dest".into(),
            cross_device_copy: true,
        };
        let undo = mv.undo(&moved(&[("/a/one", "/dest/one"), ("/b/two", "/dest/two")]));
        assert_eq!(undo.len(), 2);
        assert!(undo.contains(&Operation::Move {
            paths: vec!["/dest/one".into()],
            to: "/a".into(),
            cross_device_copy: false,
        }));
        assert!(undo.contains(&Operation::Move {
            paths: vec!["/dest/two".into()],
            to: "/b".into(),
            cross_device_copy: false,
        }));

        // Keep Both renamed the destination: the file that came back is the one
        // that was moved, not the one that was already sitting there
        let keep_both = Operation::Move {
            paths: vec!["/a/file.txt".into()],
            to: "/dest".into(),
            cross_device_copy: false,
        };
        assert_eq!(
            keep_both.undo(&moved(&[("/a/file.txt", "/dest/file (copy).txt")])),
            vec![Operation::Rename {
                from: "/dest/file (copy).txt".into(),
                to: "/a/file.txt".into(),
            }],
            "the renamed destination goes back under the name it had, and the \
             file it was kept alongside is left where it is"
        );

        // A merge into a folder that was already there records only the
        // children it transferred, so undo leaves that folder's own files alone
        let merge = Operation::Move {
            paths: vec!["/a/folder".into()],
            to: "/dest".into(),
            cross_device_copy: false,
        };
        assert_eq!(
            merge.undo(&moved(&[("/a/folder/moved.txt", "/dest/folder/moved.txt")])),
            vec![Operation::Move {
                paths: vec!["/dest/folder/moved.txt".into()],
                to: "/a/folder".into(),
                cross_device_copy: false,
            }],
            "only the merged child comes back, not the whole destination folder"
        );

        // A folder that the move created covers everything inside it
        assert_eq!(
            merge.undo(&moved(&[
                ("/a/folder/moved.txt", "/dest/folder/moved.txt"),
                ("/a/folder", "/dest/folder"),
            ])),
            vec![Operation::Move {
                paths: vec!["/dest/folder".into()],
                to: "/a".into(),
                cross_device_copy: false,
            }],
            "a destination inside another destination rides along with it"
        );

        // New items are deleted for good; copied, extracted and compressed
        // results go to the trash, and a copy reverses only what it created
        let copy = Operation::Copy {
            paths: vec!["/a/one".into()],
            to: "/dest".into(),
        };
        let created = |paths: &[&str]| OperationSelection {
            created: paths.iter().map(PathBuf::from).collect(),
            ..Default::default()
        };
        assert_eq!(
            copy.undo(&created(&["/dest/one"])),
            vec![Operation::Delete {
                paths: vec![PathBuf::from("/dest/one")]
            }]
        );
        // What a copy replaced went to the trash: its copy goes there too,
        // then it comes back
        let replaced = crate::trashing::TrashItem {
            id: crate::trashing::TrashId::of_record("/trash/info/one.trashinfo"),
            name: "one".into(),
            original_parent: Some("/dest".into()),
            time_deleted: 1,
        };
        assert_eq!(
            copy.undo(&OperationSelection {
                created: vec!["/dest/one".into()],
                trash_items: vec![replaced.clone()],
                ..Default::default()
            }),
            vec![
                Operation::Delete {
                    paths: vec![PathBuf::from("/dest/one")]
                },
                Operation::Restore {
                    items: vec![replaced.clone()]
                }
            ]
        );
        assert!(
            copy.undo(&sel(&["/dest/one"])).is_empty(),
            "a copy that created nothing, because every destination was \
             replaced or merged into, has nothing to undo"
        );
        assert_eq!(
            copy.undo(&created(&["/dest/dir", "/dest/dir/inner"])),
            vec![Operation::Delete {
                paths: vec![PathBuf::from("/dest/dir")]
            }],
            "a created path inside another created path is covered by it"
        );
        // A new file or folder goes to the trash, not straight to oblivion
        for op in [
            Operation::NewFile {
                path: "/a/new".into(),
            },
            Operation::NewFolder {
                path: "/a/new".into(),
            },
        ] {
            assert_eq!(
                op.undo(&OperationSelection::default()),
                vec![Operation::Delete {
                    paths: vec!["/a/new".into()]
                }]
            );
        }
        let compress = Operation::Compress {
            paths: vec!["/a/one".into()],
            to: "/a/one.zip".into(),
            archive_type: ArchiveType::Zip,
            password: None,
        };
        assert_eq!(
            compress.undo(&OperationSelection::default()),
            vec![Operation::Delete {
                paths: vec!["/a/one.zip".into()]
            }]
        );

        // Not undoable
        assert!(
            Operation::EmptyTrash
                .undo(&OperationSelection::default())
                .is_empty()
        );
        assert!(
            Operation::Delete {
                paths: vec!["/a/one".into()]
            }
            .undo(&OperationSelection::default())
            .is_empty(),
            "a trash operation with no recorded entries cannot be undone"
        );
        assert!(
            Operation::Restore {
                items: vec![crate::trashing::TrashItem {
                    id: crate::trashing::TrashId::of_record("/trash/info/one.trashinfo"),
                    name: "one".into(),
                    original_parent: Some("/a".into()),
                    time_deleted: 1,
                }]
            }
            .undo(&OperationSelection::default())
            .is_empty(),
            "a restore that brought nothing back, every item skipped, has \
             nothing to undo: what holds its place is someone else's"
        );
    }

    /// A file a copy replaced is in the trash, and the restore its undo
    /// ends with puts it back once the copy is gone
    #[test(compio::test)]
    async fn a_replaced_file_comes_back_on_undo() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        fs::create_dir(path.join("from"))?;
        fs::write(path.join("from/b"), b"new")?;
        fs::create_dir(path.join("to"))?;
        fs::write(path.join("to/b"), b"old")?;
        let copy = Operation::Copy {
            paths: vec![path.join("from/b")],
            to: path.join("to"),
        };
        let result = perform(copy.clone(), ReplaceResult::Replace(false))
            .await
            .expect("copied");
        assert_eq!(fs::read(path.join("to/b"))?, b"new");

        let undo = copy.undo(&result);
        let [
            Operation::Delete { paths },
            restore @ Operation::Restore { .. },
        ] = undo.as_slice()
        else {
            panic!("a delete, then a restore: {undo:?}");
        };
        assert_eq!(paths, &vec![path.join("to/b")]);
        // The delete would send the copy to the user's trash; here it is
        // only removed
        fs::remove_file(path.join("to/b"))?;
        perform(restore.clone(), ReplaceResult::Cancel)
            .await
            .expect("restored");
        assert_eq!(fs::read(path.join("to/b"))?, b"old");
        Ok(())
    }

    /// A restore into a folder whose name is taken asks about it, and
    /// merges; one whose folder is gone makes it again
    #[test(compio::test)]
    async fn a_restore_merges_or_makes_its_folder_again() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        // Kept by a trash at its id, as entries inside a trashed folder are
        let kept = path.join("trash");
        fs::create_dir_all(kept.join("d"))?;
        fs::write(kept.join("d/a"), b"A")?;
        fs::write(kept.join("lone"), b"L")?;
        fs::create_dir_all(path.join("to/d"))?;
        fs::write(path.join("to/d/c"), b"C")?;
        let item = |id: PathBuf, parent: PathBuf| crate::trashing::TrashItem {
            name: id.file_name().expect("name").to_owned(),
            id: crate::trashing::TrashId::Orphan(id),
            original_parent: Some(parent),
            time_deleted: 1,
        };
        let items = vec![
            item(kept.join("d"), path.join("to")),
            item(kept.join("lone"), path.join("gone/deeper")),
        ];
        let result = perform(Operation::Restore { items }, ReplaceResult::Merge(false))
            .await
            .expect("restored");
        assert_eq!(fs::read(path.join("to/d/a"))?, b"A");
        assert_eq!(fs::read(path.join("to/d/c"))?, b"C");
        assert_eq!(fs::read(path.join("gone/deeper/lone"))?, b"L");
        assert!(!kept.join("d").exists() && !kept.join("lone").exists());
        // Undone, only what came back goes: not the folder it merged into
        let undo = Operation::Restore { items: Vec::new() }.undo(&result);
        let [Operation::Delete { paths }] = undo.as_slice() else {
            panic!("a delete: {undo:?}");
        };
        assert!(paths.contains(&path.join("to/d/a")));
        assert!(paths.contains(&path.join("gone/deeper/lone")));
        assert!(!paths.contains(&path.join("to/d")));
        Ok(())
    }

    /// A restore cancelled at a name taken for one item puts the item the
    /// trash already brought back into the trash again, with its record,
    /// so it can still be restored
    #[test(compio::test)]
    async fn a_cancelled_restore_keeps_what_came_back_restorable() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let fs = empty_fs()?;
        let top = fs.path();
        // SAFETY: `getuid` has no preconditions and cannot fail
        let bin = top.join(format!(".Trash-{}", unsafe { libc::getuid() }));
        fs::create_dir_all(bin.join("files"))?;
        fs::create_dir_all(bin.join("info"))?;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
        let trashed = |name: &str| -> io::Result<crate::trashing::TrashItem> {
            fs::write(bin.join("files").join(name), name)?;
            let info = bin.join("info").join(format!("{name}.trashinfo"));
            fs::write(
                &info,
                format!("[Trash Info]\nPath={name}\nDeletionDate=2026-10-28T10:00:00\n"),
            )?;
            Ok(crate::trashing::TrashItem {
                id: crate::trashing::TrashId::of_record(info),
                name: name.into(),
                original_parent: Some(top.to_path_buf()),
                time_deleted: 1,
            })
        };
        let items = vec![trashed("a")?, trashed("b")?];
        // B's name is taken
        fs::write(top.join("b"), b"new")?;

        perform(Operation::Restore { items }, ReplaceResult::Cancel)
            .await
            .expect_err("cancelled");
        assert!(!top.join("a").exists(), "a went back");
        assert_eq!(fs::read(top.join("b"))?, b"new");
        let dir = crate::trashing::drive_bins(top)?.pop().expect("the trash");
        let listed = crate::trashing::list_bin(&dir)?;
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert!(
            listed
                .iter()
                .all(|(item, _)| !crate::trashing::origin_unknown(item)),
            "each with its record: {listed:?}"
        );
        Ok(())
    }

    /// A trash operation that failed part-way is retried without the
    /// paths that did reach the trash
    #[test]
    fn a_retried_trash_operation_skips_what_was_already_trashed() {
        let op = Operation::Delete {
            paths: vec!["/a/one".into(), "/a/two".into(), "/a/three".into()],
        };
        let trashed = |name: &str| crate::trashing::TrashItem {
            id: crate::trashing::TrashId::of_record(format!("/trash/info/{name}.trashinfo")),
            name: name.into(),
            original_parent: Some("/a".into()),
            time_deleted: 1,
        };
        let done = OperationSelection {
            trash_items: vec![trashed("one"), trashed("two")],
            ..Default::default()
        };
        assert_eq!(
            op.remaining(&done),
            Operation::Delete {
                paths: vec!["/a/three".into()]
            }
        );
        assert_eq!(
            op.remaining(&OperationSelection::default()),
            op,
            "a failure before anything was trashed retries everything"
        );
    }

    /// A retry after a failure part-way leaves out what was done: renames
    /// that went through, archives extracted, items moved, and what is
    /// already gone for good
    #[test]
    fn a_retry_leaves_out_what_was_done() {
        let done = OperationSelection {
            selected: vec!["/a/x2".into()],
            ignored: vec!["/a/one.zip".into()],
            moved: vec![("/a/one".into(), "/b/one".into())],
            ..Default::default()
        };
        let batch = Operation::BatchRename {
            renames: vec![
                ("/a/x1".into(), "/a/x2".into()),
                ("/a/y1".into(), "/a/y2".into()),
            ],
        };
        assert_eq!(
            batch.remaining(&done),
            Operation::BatchRename {
                renames: vec![("/a/y1".into(), "/a/y2".into())]
            }
        );
        let extract = Operation::Extract {
            paths: vec!["/a/one.zip".into(), "/a/two.zip".into()].into(),
            to: "/a".into(),
            password: None,
            as_folder: true,
        };
        let Operation::Extract { paths, .. } = extract.remaining(&done) else {
            panic!("an extract");
        };
        assert_eq!(&*paths, [PathBuf::from("/a/two.zip")]);
        // Moved whole, an original is gone. A folder made at the
        // destination is noted as moved before what it holds follows, and
        // with its original still there, it is not done
        let fs = empty_fs().expect("fs");
        let path = fs.path();
        fs::create_dir(path.join("half")).expect("mkdir");
        let moved = OperationSelection {
            moved: vec![
                (path.join("whole"), path.join("to/whole")),
                (path.join("half"), path.join("to/half")),
            ],
            ..Default::default()
        };
        let moving = Operation::Move {
            paths: vec![path.join("whole"), path.join("half")],
            to: path.join("to"),
            cross_device_copy: false,
        };
        let Operation::Move { paths, .. } = moving.remaining(&moved) else {
            panic!("a move");
        };
        assert_eq!(paths, [path.join("half")]);

        // Gone is left out; out of reach for now is not
        use std::os::unix::fs::PermissionsExt;
        let kept = path.join("kept");
        fs::write(&kept, b"k").expect("write");
        fs::create_dir(path.join("shut")).expect("mkdir");
        fs::write(path.join("shut/in"), b"i").expect("write");
        fs::set_permissions(path.join("shut"), fs::Permissions::from_mode(0o000)).expect("chmod");
        let delete = Operation::PermanentlyDelete {
            paths: vec![path.join("gone"), kept.clone(), path.join("shut/in")].into(),
        };
        let left = delete.remaining(&OperationSelection::default());
        fs::set_permissions(path.join("shut"), fs::Permissions::from_mode(0o755)).expect("chmod");
        // Kept either way: read or not (root reads it), it is there
        assert_eq!(
            left,
            Operation::PermanentlyDelete {
                paths: vec![kept, path.join("shut/in")].into()
            }
        );
    }

    #[test(compio::test)]
    async fn compress_refuses_existing_archive_and_leaves_no_partial() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        fs::write(path.join("one"), b"one")?;
        let to = path.join("one.zip");
        fs::write(&to, b"precious")?;

        let compress = |paths: Vec<PathBuf>| {
            perform(
                Operation::Compress {
                    paths,
                    to: to.clone(),
                    archive_type: ArchiveType::Zip,
                    password: None,
                },
                ReplaceResult::Cancel,
            )
        };

        assert!(
            compress(vec![path.join("one")]).await.is_err(),
            "an archive that already exists must not be overwritten"
        );
        assert_eq!(
            fs::read(&to)?,
            b"precious",
            "the existing archive is untouched"
        );

        fs::remove_file(&to)?;
        assert!(
            compress(vec![path.join("one"), path.join("missing")])
                .await
                .is_err(),
            "a source that cannot be read fails the compress"
        );
        assert!(!to.exists(), "a failed compress leaves no partial archive");
        Ok(())
    }

    #[test(compio::test)]
    async fn zip_keeps_a_relative_link_target_verbatim() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let project = path.join("project");
        fs::create_dir_all(project.join("sub"))?;
        fs::write(project.join("target"), b"t")?;
        std::os::unix::fs::symlink("../target", project.join("sub").join("link"))?;
        let to = path.join("project.zip");
        Operation::Compress {
            paths: vec![project.clone()],
            to: to.clone(),
            archive_type: ArchiveType::Zip,
            password: None,
        }
        .perform(
            &tokio::sync::mpsc::unbounded_channel().0,
            Controller::default(),
        )
        .await
        .expect("Compress operation should have succeeded");

        let mut zip = zip::ZipArchive::new(File::open(&to)?)?;
        let mut file = zip.by_name("project/sub/link")?;
        assert!(file.is_symlink());
        let mut target = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut target)?;
        assert_eq!(target, b"../target", "the link target is stored as written");
        Ok(())
    }

    #[test(compio::test)]
    async fn compress_stores_symlinks_as_symlinks() -> io::Result<()> {
        // A link inside the compressed folder points outside it. The archive
        // must hold the link itself, not a copy of the secret it points at.
        let fs = empty_fs()?;
        let path = fs.path();
        let secret = path.join("secret.env");
        fs::write(&secret, b"TOKEN=hunter2")?;
        let project = path.join("project");
        fs::create_dir(&project)?;
        std::os::unix::fs::symlink(&secret, project.join("config"))?;

        for archive_type in [ArchiveType::Tgz, ArchiveType::Zip] {
            let to = path.join(format!("project{}", archive_type.extension()));
            Operation::Compress {
                paths: vec![project.clone()],
                to: to.clone(),
                archive_type,
                password: None,
            }
            .perform(
                &tokio::sync::mpsc::unbounded_channel().0,
                Controller::default(),
            )
            .await
            .expect("Compress operation should have succeeded");

            let bytes = fs::read(&to)?;
            assert!(
                !bytes.windows(7).any(|w| w == b"hunter2"),
                "{} contains the symlink target's bytes",
                to.display()
            );
            match archive_type {
                ArchiveType::Tgz => {
                    let decoder = flate2::read::GzDecoder::new(File::open(&to)?);
                    let mut found = false;
                    for entry in tar::Archive::new(decoder).entries()? {
                        let entry = entry?;
                        if entry.path()?.ends_with("config") {
                            assert!(entry.header().entry_type().is_symlink());
                            assert_eq!(entry.link_name()?.as_deref(), Some(secret.as_path()));
                            found = true;
                        }
                    }
                    assert!(found, "tar has no config entry");
                }
                ArchiveType::Zip => {
                    let mut zip = zip::ZipArchive::new(File::open(&to)?)?;
                    let file = zip.by_name("project/config")?;
                    assert!(file.is_symlink(), "zip config entry is not a symlink");
                }
            }
        }
        Ok(())
    }

    /// A selected link to a folder is stored as that link, not as the
    /// folder it points at with all it holds
    #[test(compio::test)]
    async fn compress_stores_a_selected_folder_link_as_a_link() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let fs = empty_fs()?;
        let path = fs.path();
        let secret = path.join("private");
        fs::create_dir(&secret)?;
        fs::write(secret.join("key"), b"TOKEN=hunter2")?;
        std::os::unix::fs::symlink(&secret, path.join("link"))?;
        fs::write(path.join("plain"), b"kept whole")?;
        fs::set_permissions(path.join("plain"), fs::Permissions::from_mode(0o640))?;

        for archive_type in [ArchiveType::Tgz, ArchiveType::Zip] {
            let to = path.join(format!("out{}", archive_type.extension()));
            Operation::Compress {
                paths: vec![path.join("link"), path.join("plain")],
                to: to.clone(),
                archive_type,
                password: None,
            }
            .perform(
                &tokio::sync::mpsc::unbounded_channel().0,
                Controller::default(),
            )
            .await
            .expect("compressed");

            let mut names = Vec::new();
            match archive_type {
                ArchiveType::Tgz => {
                    let decoder = flate2::read::GzDecoder::new(File::open(&to)?);
                    for entry in tar::Archive::new(decoder).entries()? {
                        let mut entry = entry?;
                        let name = entry.path()?.into_owned();
                        if name.ends_with("plain") {
                            assert_eq!(entry.header().mode()? & 0o7777, 0o640);
                            let mut bytes = Vec::new();
                            io::Read::read_to_end(&mut entry, &mut bytes)?;
                            assert_eq!(bytes, b"kept whole");
                        }
                        names.push(name);
                    }
                }
                ArchiveType::Zip => {
                    let zip = zip::ZipArchive::new(File::open(&to)?)?;
                    for name in zip.file_names() {
                        names.push(PathBuf::from(name.map_err(io::Error::other)?.as_ref()));
                    }
                }
            }
            names.sort();
            assert_eq!(
                names,
                [PathBuf::from("link"), PathBuf::from("plain")],
                "{}",
                to.display()
            );
        }
        Ok(())
    }

    /// An archive that fails part-way into a folder of its own takes the
    /// folder with it; one that went before stays done, to undo, and is
    /// left out of a retry
    #[test(compio::test)]
    async fn a_failed_extract_into_a_folder_leaves_nothing_of_it() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let good = path.join("good.zip");
        write_test_zip(&good)?;
        let cut = path.join("cut.tar.gz");
        write_test_tar_gz(&cut)?;
        let len = fs::metadata(&cut)?.len();
        File::options().write(true).open(&cut)?.set_len(len - 30)?;

        let err = operation_extract(
            vec![good.clone(), cut.clone()],
            path.to_owned(),
            true,
            ReplaceResult::Cancel,
        )
        .await
        .expect_err("the cut archive fails");
        assert!(!path.join("cut").exists(), "no half of it is left");
        assert_eq!(fs::read(path.join("good/a.txt"))?, b"archive a");
        assert_eq!(err.partial.created, [path.join("good")]);
        let extract = Operation::Extract {
            paths: vec![good, cut.clone()].into(),
            to: path.to_owned(),
            password: None,
            as_folder: true,
        };
        let Operation::Extract { paths, .. } = extract.remaining(&err.partial) else {
            panic!("an extract");
        };
        assert_eq!(&*paths, [cut]);
        Ok(())
    }

    /// An archive whose name leaves no folder name of its own, `..zip`,
    /// gets a folder of its own in the destination, made for it: not one
    /// named after the destination, beside it
    #[test(compio::test)]
    async fn an_archive_named_dot_dot_zip_extracts_into_a_folder_of_its_own() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path().join("dest");
        fs::create_dir(&path)?;
        let archive = path.join("..zip");
        write_test_zip(&archive)?;

        let result =
            operation_extract(vec![archive], path.clone(), true, ReplaceResult::Cancel).await;
        let outside: Vec<_> = fs::read_dir(fs.path())?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<_>>()?;
        result.expect("extracted");
        assert_eq!(outside, ["dest"], "nothing beside the destination");
        let folder = path.join(format!("..zip ({} 1)", fl!("copy_noun")));
        assert_eq!(fs::read(folder.join("a.txt"))?, b"archive a");
        Ok(())
    }

    /// What an extract replaced comes back when it is undone
    #[test(compio::test)]
    async fn undoing_an_extract_brings_back_what_it_replaced() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let archive = path.join("test.zip");
        write_test_zip(&archive)?;
        fs::write(path.join("a.txt"), b"mine")?;

        let extract = Operation::Extract {
            paths: vec![archive.clone()].into(),
            to: path.to_owned(),
            password: None,
            as_folder: false,
        };
        let result = operation_extract(
            vec![archive],
            path.to_owned(),
            false,
            ReplaceResult::Replace(true),
        )
        .await
        .expect("extracted");
        assert_eq!(fs::read(path.join("a.txt"))?, b"archive a");
        let undo = extract.undo(&result);
        assert!(
            undo.iter()
                .any(|op| matches!(op, Operation::Restore { items }
                if items.iter().any(|item| item.original_path() == Some(path.join("a.txt"))))),
            "{undo:?}"
        );
        Ok(())
    }

    /// A zip whose folder lets no one in still extracts whole: its
    /// contents get their times before the folder its mode
    #[test(compio::test)]
    async fn a_zip_folder_closed_to_search_still_extracts() -> io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let fs = empty_fs()?;
        let path = fs.path();
        let archive = path.join("closed.zip");
        let mut zip = zip::ZipWriter::new(File::create(&archive)?);
        let options = zip::write::SimpleFileOptions::default();
        zip.add_directory("shut", options.unix_permissions(0o600))?;
        zip.start_file("shut/f.txt", options.unix_permissions(0o644))?;
        zip.write_all(b"inside")?;
        zip.finish()?;

        let result =
            operation_extract(vec![archive], path.to_owned(), true, ReplaceResult::Cancel).await;
        let shut = path.join("closed/shut");
        let mode = fs::metadata(&shut).map(|meta| meta.permissions().mode() & 0o777);
        let _ = fs::set_permissions(&shut, fs::Permissions::from_mode(0o755));
        result.expect("extracted");
        assert_eq!(mode?, 0o600);
        assert_eq!(fs::read(shut.join("f.txt"))?, b"inside");
        Ok(())
    }

    #[test(compio::test)]
    async fn extract_zip_directly() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let archive = path.join("test.zip");
        write_test_zip(&archive)?;

        let op_sel = operation_extract(
            vec![archive.clone()],
            path.to_owned(),
            false,
            ReplaceResult::Cancel,
        )
        .await
        .expect("Extract operation should have succeeded");

        assert_eq!(fs::read(path.join("a.txt"))?, b"archive a");
        assert_eq!(fs::read(path.join("sub/b.txt"))?, b"archive b");
        assert!(
            !path.join("test").exists(),
            "No folder named after the archive"
        );
        assert_no_staging_dir(path)?;
        assert_eq!(op_sel.ignored, vec![archive]);
        let mut selected = op_sel.selected;
        selected.sort();
        assert_eq!(selected, vec![path.join("a.txt"), path.join("sub")]);
        Ok(())
    }

    #[test(compio::test)]
    async fn extract_tar_gz_directly() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let archive = path.join("test.tar.gz");
        write_test_tar_gz(&archive)?;

        operation_extract(
            vec![archive.clone()],
            path.to_owned(),
            false,
            ReplaceResult::Cancel,
        )
        .await
        .expect("Extract operation should have succeeded");

        assert_eq!(fs::read(path.join("a.txt"))?, b"archive a");
        assert_eq!(fs::read(path.join("sub/b.txt"))?, b"archive b");
        assert!(
            !path.join("test").exists(),
            "No folder named after the archive"
        );
        assert_no_staging_dir(path)?;
        Ok(())
    }

    #[test(compio::test)]
    async fn extract_zip_as_folder() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let archive = path.join("test.zip");
        write_test_zip(&archive)?;

        let op_sel = operation_extract(
            vec![archive.clone()],
            path.to_owned(),
            true,
            ReplaceResult::Cancel,
        )
        .await
        .expect("Extract operation should have succeeded");

        assert_eq!(fs::read(path.join("test/a.txt"))?, b"archive a");
        assert_eq!(fs::read(path.join("test/sub/b.txt"))?, b"archive b");
        assert!(!path.join("a.txt").exists());
        assert_eq!(op_sel.selected, vec![path.join("test")]);

        // A second extraction gets a unique folder name
        operation_extract(vec![archive], path.to_owned(), true, ReplaceResult::Cancel)
            .await
            .expect("Extract operation should have succeeded");
        assert!(
            path.join(format!("test ({} 1)", fl!("copy_noun")))
                .join("a.txt")
                .exists()
        );
        Ok(())
    }

    #[test(compio::test)]
    async fn extract_zip_directly_replace_conflict() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let archive = path.join("test.zip");
        write_test_zip(&archive)?;
        fs::write(path.join("a.txt"), b"existing a")?;
        fs::create_dir(path.join("sub"))?;
        fs::write(path.join("sub/b.txt"), b"existing b")?;
        fs::write(path.join("sub/c.txt"), b"existing c")?;

        operation_extract(
            vec![archive],
            path.to_owned(),
            false,
            ReplaceResult::Replace(false),
        )
        .await
        .expect("Extract operation should have succeeded");

        assert_eq!(fs::read(path.join("a.txt"))?, b"archive a");
        assert_eq!(fs::read(path.join("sub/b.txt"))?, b"archive b");
        assert_eq!(
            fs::read(path.join("sub/c.txt"))?,
            b"existing c",
            "Unrelated files in a merged directory are kept"
        );
        assert_no_staging_dir(path)?;
        Ok(())
    }

    #[test(compio::test)]
    async fn extract_zip_directly_skip_conflict() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let archive = path.join("test.zip");
        write_test_zip(&archive)?;
        fs::write(path.join("a.txt"), b"existing a")?;

        operation_extract(
            vec![archive],
            path.to_owned(),
            false,
            ReplaceResult::Skip(false),
        )
        .await
        .expect("Extract operation should have succeeded");

        assert_eq!(fs::read(path.join("a.txt"))?, b"existing a");
        assert_eq!(fs::read(path.join("sub/b.txt"))?, b"archive b");
        assert_no_staging_dir(path)?;
        Ok(())
    }

    /// Undo of an extraction must remove only what the extraction created,
    /// never a file the user chose to keep or a directory merged into
    #[test(compio::test)]
    async fn extract_undo_leaves_skipped_and_merged_paths_alone() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let archive = path.join("test.zip");
        write_test_zip(&archive)?;
        // `a.txt` is kept by the user, `sub` already exists and is merged into
        fs::write(path.join("a.txt"), b"my notes")?;
        fs::create_dir(path.join("sub"))?;
        fs::write(path.join("sub/c.txt"), b"unrelated")?;

        let op = Operation::Extract {
            paths: vec![archive.clone()].into_boxed_slice(),
            to: path.to_owned(),
            password: None,
            as_folder: false,
        };
        let result = operation_extract(
            vec![archive],
            path.to_owned(),
            false,
            ReplaceResult::Skip(false),
        )
        .await
        .expect("Extract operation should have succeeded");

        let undone: Vec<PathBuf> = op
            .undo(&result)
            .into_iter()
            .flat_map(|op| match op {
                Operation::Delete { paths } => paths,
                other => panic!("unexpected undo operation {other:?}"),
            })
            .collect();

        assert!(
            !undone.contains(&path.join("a.txt")),
            "undo must not remove the file the user kept: {undone:?}"
        );
        assert!(
            !undone.contains(&path.join("sub")),
            "undo must not remove a pre-existing directory: {undone:?}"
        );
        assert!(
            undone.contains(&path.join("sub/b.txt")),
            "undo must remove what the extraction created: {undone:?}"
        );
        Ok(())
    }

    #[test(compio::test)]
    async fn extract_zip_directly_skip_nested_conflict() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let archive = path.join("test.zip");
        write_test_zip(&archive)?;
        fs::create_dir(path.join("sub"))?;
        fs::write(path.join("sub/b.txt"), b"existing b")?;

        operation_extract(
            vec![archive],
            path.to_owned(),
            false,
            ReplaceResult::Skip(false),
        )
        .await
        .expect("Extract operation should have succeeded");

        assert_eq!(fs::read(path.join("a.txt"))?, b"archive a");
        assert_eq!(fs::read(path.join("sub/b.txt"))?, b"existing b");
        assert_no_staging_dir(path)?;
        Ok(())
    }

    #[test(compio::test)]
    async fn extract_zip_directly_keep_both() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();
        let archive = path.join("test.zip");
        write_test_zip(&archive)?;
        fs::write(path.join("a.txt"), b"existing a")?;

        operation_extract(
            vec![archive],
            path.to_owned(),
            false,
            ReplaceResult::KeepBoth,
        )
        .await
        .expect("Extract operation should have succeeded");

        assert_eq!(fs::read(path.join("a.txt"))?, b"existing a");
        assert_eq!(
            fs::read(path.join(format!("a ({} 1).txt", fl!("copy_noun"))))?,
            b"archive a"
        );
        assert_no_staging_dir(path)?;
        Ok(())
    }

    #[test(compio::test)]
    async fn copy_file_to_same_location() -> io::Result<()> {
        let fs = simple_fs(NUM_FILES, 0, 1, 0, NAME_LEN)?;
        let path = fs.path();

        // Get the first file from the first directory
        let first_dir = filter_dirs(path)?
            .next()
            .expect("Should have at least one directory");
        let first_file = filter_files(&first_dir)?
            .next()
            .expect("Should have at least one file");

        // Duplicate that file
        let base_name = first_file
            .file_name()
            .and_then(|name| name.to_str())
            .expect("File name exists and is valid");
        debug!(
            "Duplicating {} in {}",
            first_file.display(),
            first_dir.display()
        );
        operation_copy(vec![first_file.clone()], first_dir.clone())
            .await
            .expect("Copy operation should have succeeded");

        assert!(first_file.exists(), "Original file should still exist");
        let expected = first_dir.join(format!("{base_name} ({} 1)", fl!("copy_noun")));
        assert!(expected.exists(), "File should have been duplicated");

        Ok(())
    }

    #[test(compio::test)]
    async fn copy_file_with_extension_to_same_loc() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();

        let base_name = "foo.txt";
        let base_path = path.join(base_name);
        File::create(&base_path)?;
        debug!("Duplicating {}", base_path.display());
        operation_copy(vec![base_path.clone()], path.to_owned())
            .await
            .expect("Copy operation should have succeeded");

        assert!(base_path.exists(), "Original file should still exist");
        let expected = path.join(format!("foo ({} 1).txt", fl!("copy_noun")));
        assert!(expected.exists(), "File should have been duplicated");

        Ok(())
    }

    #[test(compio::test)]
    async fn copy_dir_to_same_location() -> io::Result<()> {
        let fs = simple_fs(NUM_FILES, 0, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();

        // First directory path
        let first_dir = filter_dirs(path)?
            .next()
            .expect("Should have at least one directory");
        let base_name = first_dir
            .file_name()
            .and_then(|name| name.to_str())
            .expect("First directory exists and has a valid name");
        debug!("Duplicating directory {}", first_dir.display());
        operation_copy(vec![first_dir.clone()], path.to_owned())
            .await
            .expect("Copy operation should have succeeded");

        assert!(first_dir.exists(), "Original directory should still exist");
        let expected = path.join(format!("{base_name} ({} 1)", fl!("copy_noun")));
        assert!(expected.exists(), "Directory should have been duplicated");

        Ok(())
    }

    #[test(compio::test)]
    async fn copying_file_multiple_times_to_same_location() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();

        let base_name = "cosmic";
        let base_path = path.join(base_name);
        File::create(&base_path)?;

        for i in 1..5 {
            debug!("Duplicating {}", base_path.display());
            operation_copy(vec![base_path.clone()], path.to_owned())
                .await
                .expect("Copy operation should have succeeded");
            assert!(base_path.exists(), "Original file should still exist");
            assert!(
                path.join(format!("{base_name} ({} {i})", fl!("copy_noun")))
                    .exists(),
                "File should have been duplicated (copy #{i})"
            );
        }

        Ok(())
    }

    #[test(compio::test)]
    async fn copy_to_diff_dir_doesnt_dupe_files() -> io::Result<()> {
        let fs = simple_fs(NUM_FILES, NUM_HIDDEN, NUM_DIRS, NUM_NESTED, NAME_LEN)?;
        let path = fs.path();

        let (first_dir, second_dir) = {
            let mut dirs = filter_dirs(path)?;
            (
                dirs.next().expect("Should have at least two dirs"),
                dirs.next().expect("Should have at least two dirs"),
            )
        };
        let first_file = filter_files(&first_dir)?
            .next()
            .expect("Should have at least one file");
        // Both directories have a file with the same name.
        let base_name = first_file
            .file_name()
            .and_then(|name| name.to_str())
            .expect("File name exists and is valid");

        debug!(
            "Copying {} to {}",
            first_file.display(),
            second_dir.display()
        );
        // Cancel in the replace dialog cancels the copy, which puts back
        // whatever it did: here nothing, and both files stay as they were
        let err = operation_copy(vec![first_file.clone()], second_dir.clone())
            .await
            .expect_err(concat!(
                "Copy operation should have been cancelled ",
                "because we're copying to different directories ",
                "without replacement"
            ));
        assert_eq!(err.to_string(), fl!("cancelled"));
        assert!(
            first_dir.join(base_name).exists(),
            "First file should still exist"
        );
        assert!(
            second_dir.join(base_name).exists(),
            "Second file should still exist"
        );

        Ok(())
    }

    #[test(compio::test)]
    async fn copy_file_with_diff_name_to_diff_dir() -> io::Result<()> {
        let fs = empty_fs()?;
        let path = fs.path();

        let dir_path = path.join("cosmic");
        fs::create_dir(&dir_path)?;
        let file_path = path.join("ferris");
        File::create(&file_path)?;
        let expected = dir_path.join("ferris");

        debug!("Copying {} to {}", file_path.display(), expected.display());
        operation_copy(vec![file_path.clone()], dir_path.clone())
            .await
            .expect("Copy operation should have succeeded");

        assert!(file_path.exists(), "Original file should still exist");
        assert!(expected.exists(), "File should have been copied");

        Ok(())
    }
}
