use crate::app::{ArchiveType, DialogPage, Message, REPLACE_BUTTON_ID};
use crate::config::IconSizes;
use crate::spawn_detached::spawn_detached;
use crate::ui::iced::futures::channel::mpsc::Sender;
use crate::ui::iced::futures::{self, SinkExt, StreamExt, stream};
use crate::{FxOrderMap, archive, fl, tab};
use std::borrow::Cow;
use std::fmt::{self, Formatter};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex as TokioMutex, mpsc};
use walkdir::WalkDir;
use zip::AesMode::Aes256;

pub use self::controller::{Controller, ControllerState};
pub mod controller;

pub use notifiers::*;
mod notifiers;

pub use self::reader::OpReader;
pub mod reader;

use self::recursive::{Context, Method};

/// Source and destination paths, paired
type PathPairs = Vec<(PathBuf, PathBuf)>;
pub mod recursive;

async fn handle_replace(
    msg_tx: Arc<TokioMutex<Sender<Message>>>,
    file_from: PathBuf,
    file_to: PathBuf,
    multiple: bool,
    conflict_count: usize,
) -> ReplaceResult {
    let item_from = match tab::item_from_path(file_from, IconSizes::default()) {
        Ok(ok) => Box::new(ok),
        Err(err) => {
            log::warn!("{err}");
            return ReplaceResult::Cancel;
        }
    };

    let item_to = match tab::item_from_path(file_to, IconSizes::default()) {
        Ok(ok) => Box::new(ok),
        Err(err) => {
            log::warn!("{err}");
            return ReplaceResult::Cancel;
        }
    };

    let (tx, mut rx) = mpsc::channel(1);
    let _ = msg_tx
        .lock()
        .await
        .send(Message::DialogPush(
            DialogPage::Replace {
                from: item_from,
                to: item_to,
                multiple,
                apply_to_all: false,
                conflict_count,
                tx,
            },
            Some(REPLACE_BUTTON_ID.clone()),
        ))
        .await;
    rx.recv().await.unwrap_or(ReplaceResult::Cancel)
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
        sync_to_disk(written_files, target_dirs).await;
    }
    Ok(())
}

/// Create a unique hidden directory inside `to` to stage an extraction in.
fn staging_dir(
    to: &Path,
    dir_name: &str,
    controller: &Controller,
) -> Result<PathBuf, OperationError> {
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
            Err(err) => return Err(OperationError::from_err(err, controller)),
        }
    }
    unreachable!()
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReplaceResult {
    Replace(bool),
    KeepBoth,
    Skip(bool),
    Cancel,
}

async fn copy_or_move(
    paths: Vec<PathBuf>,
    to: PathBuf,
    method: Method,
    msg_tx: &Arc<TokioMutex<Sender<Message>>>,
    controller: Controller,
) -> Result<OperationSelection, OperationError> {
    let msg_tx = msg_tx.clone();
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
            return Err(OperationError::from_err(fl!("into-itself"), &controller));
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
        for (from, to) in renamed {
            op_sel.moved.push((from.clone(), to.clone()));
            op_sel.ignored.push(from);
            op_sel.created.push(to.clone());
            op_sel.selected.push(to);
        }

        recursive_pairs(from_to_pairs, method, op_sel, msg_tx, controller).await
    })
    .await
    .map_err(wrap_compio_spawn_error)?
}

/// Copy or move exactly these `(from, to)` pairs, each `to` a whole
/// destination path rather than a folder to go into.
///
/// `op_sel` is what the caller has already accounted for; the run adds to it.
async fn recursive_pairs(
    pairs: PathPairs,
    method: Method,
    op_sel: OperationSelection,
    msg_tx: Arc<TokioMutex<Sender<Message>>>,
    controller: Controller,
) -> Result<OperationSelection, OperationError> {
    let mut context = Context::new(controller.clone());
    context.op_sel = op_sel;

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

    context = context.on_replace(move |op, conflict_count| {
        let msg_tx = msg_tx.clone();
        Box::pin(handle_replace(
            msg_tx,
            op.from.clone(),
            op.to.clone(),
            true,
            conflict_count,
        ))
    });

    context.recursive_copy_or_move(pairs, method).await?;

    Ok(context.op_sel)
}

pub async fn sync_to_disk(
    written_files: Vec<PathBuf>,
    target_dirs: std::collections::HashSet<PathBuf>,
) {
    // Sync files to disk
    stream::iter(written_files.into_iter().map(|path| async move {
        if let Ok(file) = compio::fs::OpenOptions::new().write(true).open(&path).await {
            let _ = file.sync_all().await;
        }
    }))
    .buffer_unordered(32)
    .collect::<()>()
    .await;

    // Sync directories to disk
    stream::iter(target_dirs.into_iter().map(|path| async move {
        if let Ok(dir) = compio::fs::OpenOptions::new().read(true).open(&path).await {
            let _ = dir.sync_all().await;
        }
    }))
    .buffer_unordered(16)
    .collect::<()>()
    .await;
}

/// The newest trash entry for each of `paths`, deleted no earlier than
/// `since` (Unix seconds).
fn trashed_entries(paths: &[PathBuf], since: i64) -> Vec<trash::TrashItem> {
    let entries = match trash::os_limited::list() {
        Ok(entries) => entries,
        Err(err) => {
            log::warn!("failed to list trash after deleting: {err}");
            return Vec::new();
        }
    };
    paths
        .iter()
        .filter_map(|path| {
            entries
                .iter()
                .filter(|entry| entry.time_deleted >= since && entry.original_path() == *path)
                .max_by_key(|entry| entry.time_deleted)
                .cloned()
        })
        .collect()
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
                by_parent
                    .into_iter()
                    .map(|(to, paths)| Self::Move {
                        paths,
                        to,
                        cross_device_copy: false,
                    })
                    .chain(renamed)
                    .collect()
            }
            // Only the paths the copy created are removed, and they go to the
            // trash rather than being destroyed, so an undo of a copy that
            // replaced or merged into existing files is still recoverable
            Self::Copy { .. } if !created_only(result).is_empty() => vec![Self::Delete {
                paths: created_only(result),
            }],
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
            Self::Restore { items } => vec![Self::Delete {
                paths: items.iter().map(trash::TrashItem::original_path).collect(),
            }],
            Self::Extract { .. } if !created_only(result).is_empty() => vec![Self::Delete {
                paths: created_only(result),
            }],
            Self::Compress { to, .. } => vec![Self::Delete {
                paths: vec![to.clone()],
            }],
            _ => Vec::new(),
        }
    }

    /// This operation with what `done` already covers taken out, for
    /// retrying it after a failure part-way: trashing again what is already
    /// in the trash would fail as not found.
    pub fn remaining(&self, done: &OperationSelection) -> Operation {
        match self {
            Self::Delete { paths } if !done.trash_items.is_empty() => Self::Delete {
                paths: paths
                    .iter()
                    .filter(|path| {
                        !done
                            .trash_items
                            .iter()
                            .any(|item| item.original_path() == **path)
                    })
                    .cloned()
                    .collect(),
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
        match self {
            Self::Rename { from, .. } => from.exists(),
            Self::BatchRename { renames } => renames.iter().all(|(from, _)| from.exists()),
            // Not `to.is_dir()`: a move that emptied a folder removes it, and
            // undoing that move has to put the folder back. Only a destination
            // taken by a file is out of reach.
            Self::Move { paths, to, .. } => !to.is_file() && paths.iter().all(|p| p.exists()),
            Self::PermanentlyDelete { paths } => paths.iter().all(|p| p.exists()),
            Self::Delete { paths } => paths.iter().all(|p| p.exists()),
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

/// Rename `from` to `to`, refusing to replace anything already at `to`.
///
/// `renameat2` makes the check and the rename one step, so nothing can appear
/// at the destination in between. Kernels and filesystems that do not support
/// the flag answer `EINVAL`, `ENOSYS` or `EOPNOTSUPP`; there we fall back to
/// looking first, which leaves a small window but still never overwrites the
/// file we can see. The check is on the link itself, so a dangling symlink at
/// the destination counts as occupied.
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from_c = CString::new(from.as_os_str().as_bytes())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let to_c = CString::new(to.as_os_str().as_bytes())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

    // SAFETY: both pointers come from CStrings that outlive the call
    let status = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if status == 0 {
        return Ok(());
    }

    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP) => {
            if fs::symlink_metadata(to).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} already exists", to.display()),
                ));
            }
            fs::rename(from, to)
        }
        _ => Err(err),
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

    let mut to = to.to_owned();
    if let Some(file_name) = from.file_name().and_then(|name| name.to_str()) {
        let (stem, ext) = if from.is_dir() {
            (file_name.to_string(), None)
        } else {
            let file_name = file_name.to_string();
            COMPOUND_EXTENSIONS
                .iter()
                .copied()
                .find(|&ext| file_name.ends_with(ext))
                .map(|ext| {
                    (
                        file_name.strip_suffix(ext).unwrap().to_string(),
                        Some(ext[1..].to_string()),
                    )
                })
                .unwrap_or_else(|| {
                    from.file_stem()
                        .and_then(|s| s.to_str())
                        .map_or((file_name, None), |stem| {
                            (
                                stem.to_string(),
                                from.extension()
                                    .and_then(|e| e.to_str())
                                    .map(str::to_string),
                            )
                        })
                })
        };

        for n in 0.. {
            let new_name = if n == 0 {
                file_name.to_string()
            } else {
                match ext {
                    Some(ref ext) => format!("{} ({} {}).{}", stem, fl!("copy_noun"), n, ext),
                    None => format!("{} ({} {})", stem, fl!("copy_noun"), n),
                }
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
    pub trash_items: Vec<trash::TrashItem>,
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
        items: Vec<trash::TrashItem>,
    },
    /// Empty the trash
    EmptyTrash,
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
        items: Vec<trash::TrashItem>,
    },
    /// Set executable and launch
    SetExecutableAndLaunch {
        path: PathBuf,
    },
    /// Set permissions
    SetPermissions {
        path: PathBuf,
        mode: u32,
    },
}

#[derive(Clone, Debug)]
pub enum OperationErrorType {
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
    pub fn from_state(state: ControllerState, controller: &Controller) -> Self {
        let message = if state == ControllerState::Failed {
            controller.set_state(ControllerState::Failed);
            fl!("failed")
        } else {
            controller.cancel();
            fl!("cancelled")
        };

        Self {
            kind: OperationErrorType::Generic(message),
            partial: Box::default(),
        }
    }

    pub fn from_err<T: ToString>(err: T, controller: &Controller) -> Self {
        controller.set_state(ControllerState::Failed);

        Self {
            kind: OperationErrorType::Generic(err.to_string()),
            partial: Box::default(),
        }
    }

    pub fn from_kind(kind: OperationErrorType, controller: &Controller) -> Self {
        controller.set_state(ControllerState::Failed);
        Self {
            kind,
            partial: Box::default(),
        }
    }

    pub fn from_msg(m: impl Into<String>) -> Self {
        Self {
            kind: OperationErrorType::Generic(m.into()),
            partial: Box::default(),
        }
    }
}

impl std::error::Error for OperationError {}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            OperationErrorType::Generic(s) => s.fmt(f),
            OperationErrorType::PasswordRequired => f.write_str("Password required"),
        }
    }
}

impl Operation {
    pub fn pending_text(&self, ratio: f32, state: ControllerState) -> String {
        let percent = (ratio * 100.0) as i32;
        let progress = || match state {
            ControllerState::Running => fl!("progress", percent = percent),
            ControllerState::Paused => fl!("progress-paused", percent = percent),
            ControllerState::Cancelled => fl!("progress-cancelled", percent = percent),
            ControllerState::Failed => fl!("progress-failed", percent = percent),
        };
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
            Self::EmptyTrash => fl!("emptying-trash", progress = progress()),
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
            Self::SetPermissions { path, mode } => {
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
            Self::EmptyTrash => fl!("emptied-trash"),
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
            Self::SetPermissions { path, mode } => {
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

    /// Perform the operation
    pub async fn perform(
        self,
        msg_tx: &Arc<TokioMutex<Sender<Message>>>,
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
                            return Err(OperationError::from_err(
                                "nothing to compress".to_string(),
                                &controller,
                            ));
                        };
                        let relative_root = relative_root.to_path_buf();

                        let op_sel = OperationSelection {
                            ignored: paths.clone(),
                            selected: vec![to.clone()],
                            created: vec![to.clone()],
                            moved: Vec::new(),
                            trash_items: Vec::new(),
                        };

                        let mut paths = paths;
                        for path in &paths.clone() {
                            if path.is_dir() {
                                let new_paths_it = WalkDir::new(path).into_iter();
                                for entry in new_paths_it.skip(1) {
                                    let entry = entry
                                        .map_err(|e| OperationError::from_err(e, &controller))?;
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
                                    .map_err(|e| OperationError::from_err(e, &controller))?;
                                if relative.to_str().is_none() {
                                    return Err(OperationError::from_err(
                                        format!(
                                            "cannot store {} in a zip archive: the name is not valid UTF-8",
                                            path.display()
                                        ),
                                        &controller,
                                    ));
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
                            .map_err(|e| OperationError::from_err(e, &controller))?;
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
                                            .map_err(|e| OperationError::from_state(e, &controller))
                                    })?;

                                    controller.set_progress((i as f32) / total_paths as f32);

                                    // tar stores raw bytes, so the name needs
                                    // no conversion and nothing is dropped
                                    let relative_path = path
                                        .strip_prefix(&relative_root)
                                        .map_err(|e| OperationError::from_err(e, &controller))?;
                                    archive
                                        .append_path_with_name(path, relative_path)
                                        .map_err(|e| OperationError::from_err(e, &controller))?;
                                }

                                archive
                                    .finish()
                                    .map_err(|e| OperationError::from_err(e, &controller))?;
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
                                            .map_err(|s| OperationError::from_state(s, &controller))
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
                                            .map_err(|e| OperationError::from_err(e, &controller))?
                                            .to_str()
                                            .ok_or_else(|| {
                                                OperationError::from_err(
                                                    format!(
                                                        "cannot store {} in a zip archive",
                                                        path.display()
                                                    ),
                                                    &controller,
                                                )
                                            })?;
                                        // Not followed: a link is stored as a
                                        // link, never as what it points at
                                        let metadata = fs::symlink_metadata(path).map_err(|e| {
                                            OperationError::from_err(e, &controller)
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
                                                OperationError::from_err(e, &controller)
                                            })?;
                                            archive
                                                .add_symlink_from_path(
                                                    relative_path,
                                                    target,
                                                    zip_options,
                                                )
                                                .map_err(|e| {
                                                    OperationError::from_err(e, &controller)
                                                })?;
                                        } else if metadata.is_file() {
                                            let mut file = fs::File::open(path).map_err(|e| {
                                                OperationError::from_err(e, &controller)
                                            })?;
                                            let total = metadata.len();
                                            if total >= 4 * 1024 * 1024 * 1024 {
                                                // The large file option must be enabled for files above 4 GiB
                                                zip_options = zip_options.large_file(true);
                                            }
                                            archive
                                                .start_file(relative_path, zip_options)
                                                .map_err(|e| {
                                                    OperationError::from_err(e, &controller)
                                                })?;
                                            let mut current = 0;
                                            loop {
                                                futures::executor::block_on(async {
                                                    controller.check().await.map_err(|s| {
                                                        OperationError::from_state(s, &controller)
                                                    })
                                                })?;

                                                let count =
                                                    file.read(&mut buffer).map_err(|e| {
                                                        OperationError::from_err(e, &controller)
                                                    })?;
                                                if count == 0 {
                                                    break;
                                                }
                                                archive.write_all(&buffer[..count]).map_err(
                                                    |e| OperationError::from_err(e, &controller),
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
                                                    OperationError::from_err(e, &controller)
                                                })?;
                                        }
                                    }
                                }

                                archive
                                    .finish()
                                    .map_err(|e| OperationError::from_err(e, &controller))?;
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
                copy_or_move(paths, to, Method::Copy, msg_tx, controller).await
            }
            Self::Delete { paths } => {
                let total = paths.len();
                let started = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                let mut paths = paths;
                let mut failure = None;
                let mut trashed = 0;
                for (i, path) in paths.iter().enumerate() {
                    let result = async {
                        // Awaited, not blocked on. Every operation shares one
                        // compio runtime thread, so blocking here while the user
                        // has this one paused would stop every other copy, move
                        // and delete along with it.
                        controller
                            .check()
                            .await
                            .map_err(|s| OperationError::from_state(s, &controller))?;

                        controller.set_progress((i as f32) / (total as f32));

                        let path = path.clone();
                        compio::runtime::spawn_blocking(move || trash::delete(path))
                            .await
                            .map_err(wrap_compio_spawn_error)?
                            .map_err(|e| OperationError::from_err(e, &controller))
                    }
                    .await;
                    if let Err(err) = result {
                        failure = Some(err);
                        break;
                    }
                    trashed = i + 1;
                }
                // Trashing returns nothing, so find the entries just created:
                // the newest entry per path deleted since the start. A failure
                // part-way still records them, so what did reach the trash can
                // be undone and a retry has only the rest to do
                paths.truncate(trashed);
                let trash_items =
                    compio::runtime::spawn_blocking(move || trashed_entries(&paths, started))
                        .await
                        .map_err(wrap_compio_spawn_error)?;
                let op_sel = OperationSelection {
                    trash_items,
                    ..Default::default()
                };
                match failure {
                    Some(err) => Err(OperationError {
                        partial: Box::new(op_sel),
                        ..err
                    }),
                    None => Ok(op_sel),
                }
            }
            Self::DeleteTrash { items } => {
                let controller_clone = controller.clone();
                compio::runtime::spawn_blocking(move || -> Result<(), OperationError> {
                    let controller = controller_clone;
                    let count = items.len();
                    for (i, item) in items.into_iter().enumerate() {
                        futures::executor::block_on(async {
                            controller
                                .check()
                                .await
                                .map_err(|s| OperationError::from_state(s, &controller))
                        })?;

                        controller.set_progress(i as f32 / count as f32);

                        trash::os_limited::purge_all([item])
                            .map_err(|e| OperationError::from_err(e, &controller))?;
                    }
                    Ok(())
                })
                .await
                .map_err(wrap_compio_spawn_error)?
                .map_err(|e| OperationError::from_err(e, &controller))?;
                Ok(OperationSelection::default())
            }
            Self::EmptyTrash => {
                let controller_clone = controller.clone();
                compio::runtime::spawn_blocking(move || -> Result<(), OperationError> {
                    let controller = controller_clone;
                    let items = trash::os_limited::list()
                        .map_err(|e| OperationError::from_err(e, &controller))?;
                    let count = items.len();
                    let mut errors: Vec<trash::Error> = Vec::new();

                    for (i, item) in items.into_iter().enumerate() {
                        futures::executor::block_on(async {
                            controller
                                .check()
                                .await
                                .map_err(|s| OperationError::from_state(s, &controller))
                        })?;

                        if let Err(e) = trash::os_limited::purge_all([item]) {
                            errors.push(e);
                        }

                        controller.set_progress(i as f32 / count as f32);
                    }

                    // Report errors at the end
                    if !errors.is_empty() {
                        log::warn!("Failed to purge {} items:", errors.len());
                        for e in &errors {
                            log::warn!("  - {e}");
                        }

                        // Return an error to signal partial failure
                        return Err(OperationError::from_err(
                            format!(
                                "Failed to delete {} of {} items. Check log for details.",
                                errors.len(),
                                count
                            ),
                            &controller,
                        ));
                    }

                    Ok(())
                })
                .await
                .map_err(wrap_compio_spawn_error)?
                .map_err(|e| OperationError::from_err(e, &controller))?;
                Ok(OperationSelection::default())
            }
            Self::Extract {
                paths,
                to,
                password,
                as_folder,
            } => {
                let controller_clone = controller.clone();
                let msg_tx = msg_tx.clone();
                compio::runtime::spawn(async move {
                    let controller = controller_clone;
                    let total_paths = paths.len();
                    let mut op_sel = OperationSelection::default();
                    for (i, path) in paths.iter().enumerate() {
                        controller
                            .check()
                            .await
                            .map_err(|s| OperationError::from_state(s, &controller))?;

                        controller.set_progress((i as f32) / total_paths as f32);

                        let Some(file_name) = path.file_name().and_then(|f| f.to_str()) else {
                            continue;
                        };
                        let dir_name = get_directory_name(file_name).to_string();
                        op_sel.ignored.push(path.clone());

                        if as_folder {
                            let mut new_dir = to.join(&dir_name);
                            if new_dir.exists()
                                && let Some(new_dir_parent) = new_dir.parent()
                            {
                                new_dir = copy_unique_path(&new_dir, new_dir_parent);
                            }
                            extract_archive(path.clone(), new_dir.clone(), &password, &controller)
                                .await?;
                            op_sel.created.push(new_dir.clone());
                            op_sel.selected.push(new_dir);
                        } else {
                            // Extract into a hidden staging directory beside the destination,
                            // then move the entries into place. The move goes through the
                            // same replace dialog as copy/move, so existing files are never
                            // overwritten silently.
                            let staging = staging_dir(&to, &dir_name, &controller)?;
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
                                    .map_err(|e| OperationError::from_err(e, &controller))?;
                                let mut moved = copy_or_move(
                                    entries.clone(),
                                    to.clone(),
                                    Method::Move {
                                        cross_device_copy: false,
                                    },
                                    &msg_tx,
                                    controller.clone(),
                                )
                                .await?;
                                sync_to_disk(Vec::new(), [to.clone()].into_iter().collect()).await;
                                // Entries moved by a plain rename are not in the selection
                                for entry in entries {
                                    if let Some(name) = entry.file_name() {
                                        let dest = to.join(name);
                                        if dest.exists() && !moved.selected.contains(&dest) {
                                            moved.selected.push(dest);
                                        }
                                    }
                                }
                                Ok::<_, OperationError>((moved.selected, moved.created))
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
                            // merged into, were already the user's
                            let (selected, created) = result?;
                            op_sel.selected.extend(selected);
                            op_sel.created.extend(created);
                        }
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
                    msg_tx,
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
                        .map_err(|s| OperationError::from_state(s, &controller))?;
                    compio::fs::create_dir(&path)
                        .await
                        .map_err(|e| OperationError::from_err(e, &controller))?;
                    Result::<_, OperationError>::Ok(OperationSelection {
                        ignored: Vec::new(),
                        selected: vec![path.clone()],
                        created: vec![path],
                        moved: Vec::new(),
                        trash_items: Vec::new(),
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
                        .map_err(|s| OperationError::from_state(s, &controller))?;
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
                        .map_err(|e| OperationError::from_err(e, &controller))?;
                    Result::<_, OperationError>::Ok(OperationSelection {
                        ignored: Vec::new(),
                        selected: vec![path.clone()],
                        created: vec![path],
                        moved: Vec::new(),
                        trash_items: Vec::new(),
                    })
                })
            }
            .await
            .map_err(wrap_compio_spawn_error)?,
            Self::PermanentlyDelete { paths } => {
                let total = paths.len();
                for (idx, path) in paths.into_iter().enumerate() {
                    controller
                        .check()
                        .await
                        .map_err(|s| OperationError::from_state(s, &controller))?;

                    controller.set_progress((idx as f32) / (total as f32));

                    tokio::task::spawn_blocking(|| {
                        if path.is_symlink() || path.is_file() {
                            fs::remove_file(path)
                        } else if path.is_dir() {
                            fs::remove_dir_all(path)
                        } else {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "File to delete is not symlink, file or directory",
                            ))
                        }
                    })
                    .await
                    .map_err(|e| OperationError::from_err(e, &controller))?
                    .map_err(|e| OperationError::from_err(e, &controller))?;
                }

                Ok(OperationSelection::default())
            }
            Self::RemoveFromRecents { paths } => {
                // Through the same worker that records and clears recents.
                // The file is read, edited and written back whole, so a
                // removal running beside a recording would undo one of them.
                tokio::task::spawn_blocking(move || crate::recents::remove(paths))
                    .await
                    .map_err(|e| OperationError::from_err(e, &controller))?
                    .map_err(|e| OperationError::from_err(e, &controller))?;

                Ok(OperationSelection::default())
            }
            Self::WriteFile { path, data } => {
                let controller_clone = controller.clone();
                compio::runtime::spawn(async move {
                    let controller = controller_clone;
                    controller
                        .check()
                        .await
                        .map_err(|s| OperationError::from_state(s, &controller))?;
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
                            .map_err(|s| OperationError::from_state(s, &controller))?;
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
                                    return Err(OperationError::from_err(err, &controller));
                                }
                                target = numbered_path(&path, attempt);
                            }
                            Err(err) => return Err(OperationError::from_err(err, &controller)),
                        }
                    };
                    let mut file = file;
                    let compio::BufResult(result, _) =
                        compio::io::AsyncWriteAtExt::write_all_at(&mut file, data.to_vec(), 0)
                            .await;
                    result.map_err(|e| OperationError::from_err(e, &controller))?;
                    file.sync_all()
                        .await
                        .map_err(|e| OperationError::from_err(e, &controller))?;
                    Result::<_, OperationError>::Ok(OperationSelection {
                        ignored: Vec::new(),
                        selected: vec![target.clone()],
                        created: vec![target],
                        moved: Vec::new(),
                        trash_items: Vec::new(),
                    })
                })
            }
            .await
            .map_err(wrap_compio_spawn_error)?,
            Self::Rename { from, to } => {
                let controller_clone = controller.clone();
                let msg_tx = msg_tx.clone();

                compio::runtime::spawn(async move {
                    let controller = controller_clone;
                    controller
                        .check()
                        .await
                        .map_err(|s| OperationError::from_state(s, &controller))?;
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
                                OperationSelection {
                                    ignored: vec![from],
                                    ..Default::default()
                                },
                                msg_tx,
                                controller,
                            )
                            .await;
                        }
                        Err(err) => return Err(OperationError::from_err(err, &controller)),
                    }
                    Result::<_, OperationError>::Ok(OperationSelection {
                        ignored: vec![from],
                        selected: vec![to],
                        created: Vec::new(),
                        moved: Vec::new(),
                        trash_items: Vec::new(),
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
                        controller
                            .check()
                            .await
                            .map_err(|s| OperationError::from_state(s, &controller))?;
                        controller.set_progress((i as f32) / (total as f32));
                        // Never replace: the preview that produced this list
                        // is a snapshot, the folder may have changed since,
                        // and on a remote folder the preview does not check
                        // destinations at all
                        compio::runtime::spawn_blocking({
                            let (from, to) = (from.clone(), to.clone());
                            move || rename_no_replace(&from, &to)
                        })
                        .await
                        .map_err(wrap_compio_spawn_error)?
                        .map_err(|e| OperationError::from_err(e, &controller))?;
                        op_sel.ignored.push(from);
                        op_sel.selected.push(to);
                    }
                    Result::<_, OperationError>::Ok(op_sel)
                })
            }
            .await
            .map_err(wrap_compio_spawn_error)?,
            Self::Restore { items } => {
                let total = items.len();
                let mut paths = Vec::with_capacity(total);
                for (i, item) in items.into_iter().enumerate() {
                    controller
                        .check()
                        .await
                        .map_err(|s| OperationError::from_state(s, &controller))?;

                    controller.set_progress((i as f32) / (total as f32));

                    paths.push(item.original_path());

                    // Items with .trashinfo id use standard restore; sub-items use manual move
                    if item.id.to_str().is_some_and(|s| s.ends_with(".trashinfo")) {
                        compio::runtime::spawn_blocking(|| trash::os_limited::restore_all([item]))
                            .await
                            .map_err(wrap_compio_spawn_error)?
                            .map_err(|e| OperationError::from_err(e, &controller))?;
                    } else {
                        let from = PathBuf::from(&item.id);
                        let to = item.original_path();
                        // Restoring must never take the place of something the
                        // user has since put back at the original path
                        compio::runtime::spawn_blocking({
                            let (from, to) = (from.clone(), to.clone());
                            move || {
                                if let Some(parent) = to.parent() {
                                    fs::create_dir_all(parent)?;
                                }
                                rename_no_replace(&from, &to)
                            }
                        })
                        .await
                        .map_err(wrap_compio_spawn_error)?
                        .map_err(|e| OperationError::from_err(e, &controller))?;
                    }
                }
                Ok(OperationSelection {
                    ignored: Vec::new(),
                    selected: paths,
                    created: Vec::new(),
                    moved: Vec::new(),
                    trash_items: Vec::new(),
                })
            }
            Self::SetExecutableAndLaunch { path } => {
                controller
                    .check()
                    .await
                    .map_err(|s| OperationError::from_state(s, &controller))?;

                let controller_clone = controller.clone();
                compio::runtime::spawn_blocking(move || -> Result<(), OperationError> {
                    let controller = controller_clone;
                    use std::os::unix::fs::PermissionsExt;

                    let mut perms = fs::metadata(&path)
                        .map_err(|e| OperationError::from_err(e, &controller))?
                        .permissions();
                    let current_mode = perms.mode();
                    let new_mode = current_mode | 0o111;
                    perms.set_mode(new_mode);
                    fs::set_permissions(&path, perms)
                        .map_err(|e| OperationError::from_err(e, &controller))?;

                    let mut command = std::process::Command::new(path);
                    spawn_detached(&mut command)
                        .map_err(|e| OperationError::from_err(e, &controller))?;

                    Ok(())
                })
                .await
                .map_err(wrap_compio_spawn_error)?
                .map_err(|e| OperationError::from_err(e, &controller))?;
                Ok(OperationSelection::default())
            }
            Self::SetPermissions { path, mode } => {
                controller
                    .check()
                    .await
                    .map_err(|s| OperationError::from_state(s, &controller))?;

                let controller_clone = controller.clone();
                let path_clone = path.clone();
                compio::runtime::spawn_blocking(move || -> Result<(), OperationError> {
                    let controller = controller_clone;
                    let path = path_clone;
                    use std::os::unix::fs::PermissionsExt;
                    let perms = fs::Permissions::from_mode(mode);
                    fs::set_permissions(&path, perms)
                        .map_err(|e| OperationError::from_err(e, &controller))?;

                    Ok(())
                })
                .await
                .map_err(wrap_compio_spawn_error)?
                .map_err(|e| OperationError::from_err(e, &controller))?;
                Ok(OperationSelection {
                    ignored: Vec::new(),
                    selected: vec![path],
                    created: Vec::new(),
                    moved: Vec::new(),
                    trash_items: Vec::new(),
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

    use crate::ui::iced::futures::channel::mpsc;
    use crate::ui::iced::futures::{StreamExt, future};
    use log::debug;
    use test_log::test;
    use tokio::sync;

    use super::{Controller, Operation, OperationError, OperationSelection, ReplaceResult};
    use crate::app::test_utils::{
        NAME_LEN, NUM_DIRS, NUM_FILES, NUM_HIDDEN, NUM_NESTED, empty_fs, filter_dirs, filter_files,
        simple_fs,
    };
    use crate::app::{DialogPage, Message};
    use crate::fl;

    /// Simple wrapper around `[Operation::Copy]`
    pub async fn operation_copy(
        paths: Vec<PathBuf>,
        to: PathBuf,
    ) -> Result<OperationSelection, OperationError> {
        let id = fastrand::u64(0..u64::MAX);
        let (tx, mut rx) = mpsc::channel(1);
        let paths_clone = paths.clone();
        let to_clone = to.clone();

        // Wrap this into its own future so that it may be polled concurerntly with the message handler.
        let handle_copy = async move {
            Operation::Copy {
                paths: paths_clone,
                to: to_clone,
            }
            .perform(&sync::Mutex::new(tx).into(), Controller::default())
            .await
        };

        // Concurrently handling messages will prevent the mpsc channel from blocking when full.
        let handle_messages = async move {
            while let Some(msg) = rx.next().await {
                match msg {
                    Message::DialogPush(DialogPage::Replace { tx, .. }, _id_to_focus) => {
                        debug!("[{id}] Replace request");
                        tx.send(ReplaceResult::Cancel)
                            .await
                            .expect("Sending a response to a replace request should succeed");
                    }
                    _ => unreachable!(
                        "Only [ `Message::PendingProgress`, `Message::DialogPush(DialogPage::Replace)` ] are sent from operation"
                    ),
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
        let (tx, mut rx) = mpsc::channel(1);
        let handle_operation = async move {
            operation
                .perform(&sync::Mutex::new(tx).into(), Controller::default())
                .await
        };
        let handle_messages = async move {
            while let Some(msg) = rx.next().await {
                match msg {
                    Message::DialogPush(DialogPage::Replace { tx, .. }, _id_to_focus) => {
                        tx.send(reply)
                            .await
                            .expect("Sending a response to a replace request should succeed");
                    }
                    _ => unreachable!("unexpected message from operation"),
                }
            }
        };
        future::join(handle_messages, handle_operation).await.1
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
        let (tx, mut rx) = mpsc::channel(1);
        let handle_extract = async move {
            Operation::Extract {
                paths: paths.into_boxed_slice(),
                to,
                password: None,
                as_folder,
            }
            .perform(&sync::Mutex::new(tx).into(), Controller::default())
            .await
        };
        let handle_messages = async move {
            while let Some(msg) = rx.next().await {
                match msg {
                    Message::DialogPush(DialogPage::Replace { tx, .. }, _id_to_focus) => {
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

        let (tx, _rx) = mpsc::channel(1);
        let result = Operation::WriteFile {
            path: target.clone(),
            data: (&b"pasted"[..]).into(),
        }
        .perform(&sync::Mutex::new(tx).into(), Controller::default())
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

        let result = operation_move(vec![from.join("folder")], to.clone(), ReplaceResult::Cancel)
            .await
            .expect("nothing conflicts, the names inside the folders differ");

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

        let (tx, _rx) = mpsc::channel(1);
        let result = Operation::BatchRename {
            renames: vec![(path.join("a.txt"), path.join("b.txt"))],
        }
        .perform(&sync::Mutex::new(tx).into(), Controller::default())
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
                archive_type: crate::app::ArchiveType::Tgz,
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

        let (tx, _rx) = mpsc::channel(1);
        let result = Operation::NewFile { path: path.clone() }
            .perform(&sync::Mutex::new(tx).into(), Controller::default())
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

        let (tx, _rx) = mpsc::channel(1);
        let result = Operation::Rename {
            from: path.join("a.txt"),
            to: path.join("b.txt"),
        }
        .perform(&sync::Mutex::new(tx).into(), Controller::default())
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

        let (tx, _rx) = mpsc::channel(1);
        Operation::Rename {
            from: path.join("a.txt"),
            to: path.join("b.txt"),
        }
        .perform(&sync::Mutex::new(tx).into(), Controller::default())
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

        let (tx, mut rx) = mpsc::channel(1);
        let run = async move {
            Operation::Copy {
                paths: vec![link.join("a.txt")],
                to: real.clone(),
            }
            .perform(&sync::Mutex::new(tx).into(), Controller::default())
            .await
        };
        let replies = async move {
            let mut asked = 0;
            while let Some(msg) = rx.next().await {
                if let Message::DialogPush(DialogPage::Replace { tx, .. }, _) = msg {
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
            archive_type: crate::app::ArchiveType::Zip,
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
    }

    /// A trash operation that failed part-way is retried without the
    /// paths that did reach the trash
    #[test]
    fn a_retried_trash_operation_skips_what_was_already_trashed() {
        let op = Operation::Delete {
            paths: vec!["/a/one".into(), "/a/two".into(), "/a/three".into()],
        };
        let trashed = |name: &str| trash::TrashItem {
            id: format!("/trash/info/{name}.trashinfo").into(),
            name: name.into(),
            original_parent: "/a".into(),
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
                    archive_type: crate::app::ArchiveType::Zip,
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

        for archive_type in [crate::app::ArchiveType::Tgz, crate::app::ArchiveType::Zip] {
            let to = path.join(format!("project{}", archive_type.extension()));
            Operation::Compress {
                paths: vec![project.clone()],
                to: to.clone(),
                archive_type,
                password: None,
            }
            .perform(
                &sync::Mutex::new(mpsc::channel(1).0).into(),
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
                crate::app::ArchiveType::Tgz => {
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
                crate::app::ArchiveType::Zip => {
                    let mut zip = zip::ZipArchive::new(File::open(&to)?)?;
                    let file = zip.by_name("project/config")?;
                    assert!(file.is_symlink(), "zip config entry is not a symlink");
                }
            }
        }
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
        operation_copy(vec![first_file.clone()], second_dir.clone())
            .await
            .expect(concat!(
                "Copy operation should have been cancelled ",
                "because we're copying to different directories ",
                "without replacement"
            ));
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
