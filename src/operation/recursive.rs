// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use super::root::proto::Request;
use super::root::{self, Helper, StartError};
use super::{
    Ask, Blocked, BlockedAnswer, Controller, OperationSelection, ReplaceResult, copy_unique_path,
};
use crate::fl;
use crate::operation::{OperationError, sync_to_disk};
use crate::ui::iced::futures;
use anyhow::Context as AnyhowContext;
use compio::BufResult;
use compio::buf::{IntoInner, IoBuf};
use compio::driver::ToSharedFd;
use compio::driver::op::AsyncifyFd;
use compio::io::{AsyncReadAt, AsyncWriteAtExt};
use futures::{FutureExt, StreamExt};
use std::cell::Cell;
use std::error::Error;
use std::fs;
use std::future::Future;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[cfg(feature = "gvfs")]
use gio::prelude::FileExtManual;

#[derive(Debug)]
pub enum GioCopyError {
    Controller(OperationError),
    #[cfg(feature = "gvfs")]
    GLib(glib::Error),
}

impl std::fmt::Display for GioCopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Controller(_) => f.write_str("controller state"),
            #[cfg(feature = "gvfs")]
            Self::GLib(_) => f.write_str("gio copy failed"),
        }
    }
}

impl Error for GioCopyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Controller(_) => None,
            #[cfg(feature = "gvfs")]
            Self::GLib(err) => Some(err),
        }
    }
}

/// A step that stopped for lack of permission, carrying what to ask the
/// user. Found again by downcasting the step's error.
#[derive(Debug)]
struct Denied(Blocked);

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "permission denied: {}", self.0.path().display())
    }
}

impl Error for Denied {}

/// Whether an error is one the user may be asked about: the path is there,
/// but this user may not touch it. `EACCES` and `EPERM` both read as this.
fn is_denied(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied
}

/// The folder a walk error is about, when the walk could not list it for
/// lack of permission. The walk carries on past it.
fn unlisted_dir(err: &ignore::Error) -> Option<PathBuf> {
    if !err.io_error().is_some_and(is_denied) {
        return None;
    }
    // The walkers wrap the path and the depth around the I/O error in
    // either order: the one-thread walk puts the path outside, the parallel
    // one can put the depth there
    fn path_of(err: &ignore::Error) -> Option<PathBuf> {
        match err {
            ignore::Error::WithPath { path, .. } => Some(path.clone()),
            ignore::Error::WithDepth { err, .. } => path_of(err),
            _ => None,
        }
    }
    path_of(err)
}

/// The destinations among `planned` already taken: `(from, to)`. Following
/// links, as the copy does when it looks before writing. The same file
/// reached another way, as through a symlinked folder, is no clash: the copy
/// leaves it alone without asking.
fn clashes(planned: &[PlannedOp]) -> Vec<(PathBuf, PathBuf)> {
    use std::os::unix::fs::MetadataExt;

    planned
        .iter()
        .filter(|op| !matches!(op.kind, OpKind::Mkdir))
        .filter(|op| {
            fs::metadata(&op.to).is_ok_and(|to| {
                to.is_file()
                    && !fs::metadata(&op.from)
                        .is_ok_and(|from| from.dev() == to.dev() && from.ino() == to.ino())
            })
        })
        .map(|op| (op.from.clone(), op.to.clone()))
        .collect()
}

/// The first folder a move could not remove one of `froms` from, and
/// whether that is because its filesystem is read-only rather than for lack
/// of permission. Removing an entry takes writing to the folder it is in; in
/// a sticky folder someone else owns, such as `/tmp`, it also takes owning
/// the entry.
///
/// Blocking: it asks the filesystem about every folder once.
fn removal_block(froms: &[PathBuf]) -> Option<(PathBuf, bool)> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` has no preconditions and cannot fail
    let uid = unsafe { libc::geteuid() };
    // Per folder: `Err` for a folder nothing can be removed from, `Ok(true)`
    // for a sticky one where only owned entries can
    let mut folders: std::collections::HashMap<&Path, Result<bool, bool>> =
        std::collections::HashMap::new();
    for from in froms {
        let Some(folder) = from.parent() else {
            continue;
        };
        let check = *folders.entry(folder).or_insert_with(|| {
            if let Some(read_only) = super::cannot_write(folder) {
                return Err(read_only);
            }
            Ok(fs::metadata(folder)
                .is_ok_and(|meta| meta.mode() & libc::S_ISVTX != 0 && meta.uid() != uid))
        });
        match check {
            Err(read_only) => return Some((folder.to_path_buf(), read_only)),
            Ok(true) if fs::symlink_metadata(from).is_ok_and(|meta| meta.uid() != uid) => {
                return Some((folder.to_path_buf(), false));
            }
            Ok(_) => {}
        }
    }
    None
}

#[derive(Clone, Copy)]
pub enum Method {
    Copy,
    Move { cross_device_copy: bool },
}

mod check;

/// One step of a planned copy or move, with nothing thread-local in it.
///
/// [`Op`] cannot cross threads: its `Rc<Skip>` is shared with the cleanup op
/// beside it, and an `Rc` belongs to the thread that made it. The walk that
/// works out what to do runs on a worker, so it produces these instead, and
/// the operation thread turns them into `Op`s.
struct PlannedOp {
    kind: OpKind,
    from: PathBuf,
    to: PathBuf,
}

/// How long a plan that finds its controller paused waits before looking
/// again, mirroring what `Controller::check` does where it can be awaited.
const PLAN_PAUSE_POLL: std::time::Duration = std::time::Duration::from_millis(50);

pub struct Context {
    buf: Vec<u8>,
    controller: Controller,
    on_progress: Box<dyn OnProgress>,
    on_replace: Pin<Box<dyn OnReplace>>,
    on_blocked: Box<dyn OnBlocked>,
    pub(crate) op_sel: OperationSelection,
    replace_result_opt: Option<ReplaceResult>,
    remaining_conflicts: usize,
    /// The user chose to skip every blocked path from here on.
    skip_blocked: bool,
    /// What the user decided, before the operation ran, for each
    /// destination that was already taken.
    decided: std::collections::HashMap<PathBuf, ReplaceResult>,
    /// How much room a folder's filesystem has; replaced in tests, where no
    /// real drive can be made to run out.
    free_space: fn(&Path) -> Option<u64>,
    /// The steps that ran, oldest first, so a cancelled operation can put
    /// back what they did.
    done: Vec<Done>,
    /// The helper doing steps as root, once the user granted it in this
    /// operation. Dropped with the operation, which ends it.
    helper: Option<Arc<Mutex<Helper>>>,
    /// How the helper is started; replaced in tests, where there is no
    /// `pkexec` and no root.
    start_helper: root::Start,
    /// What the helper may touch: this operation's sources and the folders
    /// it puts things into.
    scope: root::Scope,
    /// Where each moved or copied path landed, which Keep Both may have
    /// renamed: the cleanup step that removes its original does not know.
    landed: std::collections::HashMap<PathBuf, PathBuf>,
}

/// One selected item, checked: its steps, and whether the user left it out
/// whole.
struct Item {
    from_parent: PathBuf,
    to_parent: PathBuf,
    planned: Vec<PlannedOp>,
    skipped: bool,
    /// Done as root: the user granted it for this item.
    root: bool,
    /// Copying it takes room at the destination.
    takes_room: bool,
}

/// What the check of one selected item came to.
struct ItemCheck {
    checked: check::Checked,
    /// For a move: the folder its originals could not be removed from, and
    /// whether because it is read-only.
    removal: Option<(PathBuf, bool)>,
    /// Destinations already taken: `(from, to)`.
    clashes: Vec<(PathBuf, PathBuf)>,
    /// Copying it takes room at the destination: anything but a move on the
    /// same filesystem, which needs none.
    takes_room: bool,
}

/// A problem found before the operation runs, to ask about.
#[derive(Clone)]
enum Problem {
    Blocked(Blocked),
    /// A destination already taken.
    Clash {
        from: PathBuf,
        to: PathBuf,
    },
}

impl Problem {
    fn path(&self) -> &Path {
        match self {
            Self::Blocked(blocked) => blocked.path(),
            Self::Clash { from, .. } => from,
        }
    }

    /// Which kind of question it makes, for "Same for the rest".
    fn kind(&self) -> Option<std::mem::Discriminant<Blocked>> {
        match self {
            Self::Blocked(blocked) => Some(std::mem::discriminant(blocked)),
            Self::Clash { .. } => None,
        }
    }
}

/// A step that ran, for putting back what it did.
struct Done {
    from: PathBuf,
    to: PathBuf,
    /// It removed an original, now only at `to`.
    is_cleanup: bool,
    is_dir: bool,
    /// It brought `to` into existence.
    created: bool,
    /// It ran as root, so putting it back does too.
    root: bool,
}

pub trait OnProgress: Fn(&Op, &Progress) + 'static {}
impl<F> OnProgress for F where F: Fn(&Op, &Progress) + 'static {}

pub trait OnReplace:
    for<'a> Fn(&'a Op, usize) -> Pin<Box<dyn Future<Output = ReplaceResult> + 'a>> + 'static
{
}
impl<F> OnReplace for F where
    F: for<'a> Fn(&'a Op, usize) -> Pin<Box<dyn Future<Output = ReplaceResult> + 'a>> + 'static
{
}

/// Asked about a blocked path, with what the question offers: see [`Ask`].
pub trait OnBlocked:
    Fn(Blocked, Ask) -> Pin<Box<dyn Future<Output = BlockedAnswer>>> + 'static
{
}
impl<F> OnBlocked for F where
    F: Fn(Blocked, Ask) -> Pin<Box<dyn Future<Output = BlockedAnswer>>> + 'static
{
}

impl Context {
    pub fn new(controller: Controller) -> Self {
        Self {
            // 128K is the optimal upper size of a buffer.
            buf: vec![0u8; 128 * 1024],
            controller,
            on_progress: Box::new(|_op, _progress| {}),
            on_replace: Box::pin(|_op, _count| Box::pin(async { ReplaceResult::Cancel })),
            on_blocked: Box::new(|_blocked, _ask| Box::pin(async { BlockedAnswer::Cancel })),
            op_sel: OperationSelection::default(),
            replace_result_opt: None,
            remaining_conflicts: 0,
            skip_blocked: false,
            decided: std::collections::HashMap::new(),
            free_space: super::free_space,
            done: Vec::new(),
            helper: None,
            start_helper: Helper::start,
            scope: root::Scope::default(),
            landed: std::collections::HashMap::new(),
        }
    }

    /// The controller this context runs under.
    pub fn controller(&self) -> Controller {
        self.controller.clone()
    }

    /// Checks one selected item on a blocking worker: walks it (see
    /// [`check::check_tree`]), and for a move, whether its originals could
    /// be removed; then which of its destinations are already taken.
    async fn check_item(
        &self,
        from_parent: &Path,
        to_parent: &Path,
        method: Method,
    ) -> Result<ItemCheck, OperationError> {
        use std::os::unix::fs::MetadataExt;

        let from_parent = from_parent.to_path_buf();
        let to_parent = to_parent.to_path_buf();
        let controller = self.controller.clone();
        compio::runtime::spawn_blocking(move || {
            let into = to_parent.parent().unwrap_or(&to_parent);
            let target = check::Target::of(into);
            let parallel = check::walks_in_parallel(&from_parent);
            let checked = check::check_tree(
                &from_parent,
                &to_parent,
                method,
                &controller,
                target,
                parallel,
            )?;
            let same_device = matches!(
                (fs::symlink_metadata(&from_parent), fs::metadata(into)),
                (Ok(from), Ok(into)) if from.dev() == into.dev()
            );
            let removal = match method {
                Method::Move {
                    cross_device_copy: false,
                } => {
                    let froms: Vec<PathBuf> =
                        checked.planned.iter().map(|op| op.from.clone()).collect();
                    removal_block(&froms)
                }
                _ => None,
            };
            let clashes = clashes(&checked.planned);
            Ok(ItemCheck {
                checked,
                removal,
                clashes,
                takes_room: matches!(method, Method::Copy) || !same_device,
            })
        })
        .await
        .map_err(super::wrap_compio_spawn_error)?
    }

    /// Ask what to do about `blocked`, unless the user already chose to skip
    /// every blocked path.
    async fn blocked(&mut self, blocked: Blocked, ask: Ask) -> BlockedAnswer {
        if self.skip_blocked {
            return BlockedAnswer::Skip(false);
        }
        let answer = (self.on_blocked)(blocked, ask).await;
        if answer == BlockedAnswer::Skip(true) {
            self.skip_blocked = true;
        }
        answer
    }

    /// Starts the helper if it is not running yet: the first grant of root
    /// in this operation, which asks for the password.
    async fn ensure_helper(&mut self) -> Result<(), StartError> {
        if self.helper.is_some() {
            return Ok(());
        }
        let (start, scope) = (self.start_helper, self.scope.clone());
        let helper = compio::runtime::spawn_blocking(move || start(&scope))
            .await
            .map_err(|_| {
                StartError::Failed(std::io::Error::other("the helper could not start"))
            })??;
        self.helper = Some(Arc::new(Mutex::new(helper)));
        Ok(())
    }

    /// Runs `f` with the helper on a blocking worker: every request waits on
    /// the helper's reply.
    async fn with_helper<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Helper) -> std::io::Result<T> + Send + 'static,
    ) -> std::io::Result<T> {
        let helper = self
            .helper
            .clone()
            .ok_or_else(|| std::io::Error::other("root was not granted"))?;
        compio::runtime::spawn_blocking(move || {
            let mut helper = helper
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            f(&mut helper)
        })
        .await
        .map_err(|_| std::io::Error::other("the helper's worker stopped"))?
    }

    /// Has the helper carry out one step.
    async fn root_call(&self, request: Request) -> std::io::Result<()> {
        self.with_helper(move |helper| helper.call(&request)).await
    }

    /// Has the helper copy `op`'s file, reporting progress as it goes.
    async fn root_copy(&mut self, op: &Op, mut progress: Progress) -> std::io::Result<u64> {
        let helper = self
            .helper
            .clone()
            .ok_or_else(|| std::io::Error::other("root was not granted"))?;
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<u64>();
        let (from, to) = (op.from.clone(), op.to.clone());
        let job = compio::runtime::spawn_blocking(move || {
            let mut helper = helper
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            helper.copy(&from, &to, |bytes| {
                let _ = tx.unbounded_send(bytes);
            })
        });
        let mut job = std::pin::pin!(job.fuse());
        loop {
            futures::select! {
                bytes = rx.next() => {
                    if let Some(bytes) = bytes {
                        progress.current_bytes = bytes;
                        (self.on_progress)(op, &progress);
                    }
                }
                result = job => {
                    return result.map_err(|_| std::io::Error::other("the helper's worker stopped"))?;
                }
            }
        }
    }

    /// Plans and checks, as root, a folder of `item` this user could not
    /// list: its steps, its problems, and the destinations in it already
    /// taken.
    async fn check_listed_as_root(
        &self,
        item: &Item,
        dir: &Path,
        method: Method,
    ) -> Result<(check::Checked, Vec<(PathBuf, PathBuf)>), OperationError> {
        let to_dir = match dir.strip_prefix(&item.from_parent) {
            Ok(relative) if !relative.as_os_str().is_empty() => item.to_parent.join(relative),
            _ => item.to_parent.clone(),
        };
        let into = item
            .to_parent
            .parent()
            .unwrap_or(&item.to_parent)
            .to_path_buf();
        let dir = dir.to_path_buf();
        let failed_dir = dir.clone();
        self.with_helper(move |helper| {
            let target = check::Target::of(&into);
            let checked = check::check_listed(&dir, &to_dir, method, target, &mut |folder| {
                helper.list(folder)
            })?;
            let clashes = clashes(&checked.planned);
            Ok((checked, clashes))
        })
        .await
        .map_err(|err| {
            OperationError::from_err(super::failure_text(&failed_dir, &err), &self.controller)
        })
    }

    /// Whether everything going into each folder of `room` fits there.
    async fn check_room(
        &self,
        room: &std::collections::HashMap<PathBuf, u64>,
    ) -> Result<(), OperationError> {
        for (into, needed) in room {
            let folder = into.clone();
            let free_space = self.free_space;
            let free = compio::runtime::spawn_blocking(move || free_space(&folder))
                .await
                .map_err(super::wrap_compio_spawn_error)?;
            if let Some(free) = free
                && *needed > free
            {
                return Err(OperationError::from_err(
                    fl!(
                        "not-enough-space",
                        needed = crate::tab::format_size(*needed),
                        free = crate::tab::format_size(free)
                    ),
                    &self.controller,
                ));
            }
        }
        Ok(())
    }

    /// The operation stops at the user's word, keeping what it has done so
    /// that can be undone.
    fn cancelled(&self) -> OperationError {
        OperationError {
            partial: Box::new(self.op_sel.clone()),
            ..OperationError::from_state(super::ControllerState::Cancelled, &self.controller)
        }
    }

    /// Copies or moves each pair. Cancelled, by the user or by a question's
    /// Cancel, it puts everything back as it was before it started: see
    /// [`Self::roll_back`].
    pub async fn recursive_copy_or_move(
        &mut self,
        from_to_pairs: impl IntoIterator<Item = (PathBuf, PathBuf)>,
        method: Method,
    ) -> Result<bool, OperationError> {
        // Whole items a move already renamed into place before this ran
        let renamed = match method {
            Method::Move { .. } => self.op_sel.moved.clone(),
            Method::Copy => Vec::new(),
        };
        let result = self.run_pairs(from_to_pairs, method).await;
        if result.is_err() && self.controller.is_cancelled() {
            let not_back = self.roll_back(&renamed).await;
            if not_back.is_empty() {
                return Err(OperationError::from_state(
                    super::ControllerState::Cancelled,
                    &self.controller,
                ));
            }
            // What could not be put back is the user's to see: as a
            // failure, with its names, not a quiet cancel
            let more = not_back.len() - 1;
            let name = not_back[0].file_name().map_or_else(
                || not_back[0].display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            return Err(OperationError::from_err(
                fl!("rollback-failed", name = name, more = more),
                &self.controller,
            ));
        }
        result
    }

    /// Puts back what the steps that ran did, newest first, then the whole
    /// items in `renamed`: originals a move had removed come back to where
    /// they were (renamed, or copied across filesystems), and what was made
    /// at the destination is removed. What was already at the destination
    /// and got replaced cannot come back and is left. Returns the paths that
    /// could not be put back; everything else still is.
    async fn roll_back(&mut self, renamed: &[(PathBuf, PathBuf)]) -> Vec<PathBuf> {
        let mut not_back = Vec::new();
        for step in std::mem::take(&mut self.done).into_iter().rev() {
            let result = if step.root {
                self.put_back_as_root(&step).await
            } else if step.is_cleanup {
                self.restore(&step).await
            } else if step.created {
                unmake(&step.to, step.is_dir).await
            } else {
                Ok(())
            };
            if let Err(err) = result {
                log::warn!(
                    "failed to put back {} from {}: {err}",
                    step.from.display(),
                    step.to.display()
                );
                not_back.push(step.from);
            }
        }
        for (from, to) in renamed.iter().rev() {
            if let Err(err) = compio::fs::rename(to, from).await {
                log::warn!(
                    "failed to put back {} from {}: {err}",
                    from.display(),
                    to.display()
                );
                not_back.push(from.clone());
            }
        }
        not_back
    }

    /// Puts back a step that ran as root, through the helper: an original
    /// comes back from where it landed, and what was made is removed.
    async fn put_back_as_root(&mut self, step: &Done) -> Result<(), Box<dyn Error>> {
        let gone = |result: std::io::Result<()>| match result {
            Err(err) if err.raw_os_error() == Some(libc::ENOENT) => Ok(()),
            result => result,
        };
        if step.is_cleanup {
            if step.is_dir {
                self.root_call(Request::Mkdir {
                    from: step.to.clone(),
                    to: step.from.clone(),
                })
                .await?;
            } else {
                let renamed = self
                    .root_call(Request::Rename {
                        from: step.to.clone(),
                        to: step.from.clone(),
                    })
                    .await;
                match renamed {
                    // Across filesystems: copied back, then removed
                    Err(err) if err.raw_os_error() == Some(libc::EXDEV) => {
                        let (to, from) = (step.to.clone(), step.from.clone());
                        self.with_helper(move |helper| {
                            helper.copy(&to, &from, |_| {})?;
                            helper.call(&Request::Remove(to))
                        })
                        .await?;
                    }
                    result => result?,
                }
            }
        } else if step.created {
            let request = if step.is_dir {
                Request::Rmdir(step.to.clone())
            } else {
                Request::Remove(step.to.clone())
            };
            gone(self.root_call(request).await)?;
        }
        Ok(())
    }

    /// Brings back an original that a cleanup step removed, from where it
    /// landed.
    async fn restore(&mut self, step: &Done) -> Result<(), Box<dyn Error>> {
        if step.is_dir {
            return match compio::fs::create_dir(&step.from).await {
                Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => Err(err.into()),
                _ => Ok(()),
            };
        }
        match compio::fs::rename(&step.to, &step.from).await {
            Err(err) if err.raw_os_error() == Some(libc::EXDEV) => {}
            result => return result.map_err(Into::into),
        }
        // Across filesystems: copied back, a link as a link, then removed
        let meta = compio::fs::symlink_metadata(&step.to).await?;
        if meta.is_symlink() {
            let (to, from) = (step.to.clone(), step.from.clone());
            compio::runtime::spawn_blocking(move || {
                std::os::unix::fs::symlink(fs::read_link(&to)?, &from)
            })
            .await
            .map_err(super::wrap_compio_spawn_error)??;
        } else {
            let mut back = Op {
                kind: OpKind::Copy,
                from: step.to.clone(),
                to: step.from.clone(),
                skipped: Rc::new(Skip {
                    normal: Cell::new(false),
                    cleanup: Cell::new(false),
                }),
                is_cleanup: false,
                created_to: Cell::new(false),
                root: false,
            };
            let progress = Progress {
                current_ops: 0,
                total_ops: 1,
                current_bytes: 0,
                total_bytes: None,
            };
            back.copy(self, progress).await?;
        }
        compio::fs::remove_file(&step.to).await?;
        Ok(())
    }

    async fn run_pairs(
        &mut self,
        from_to_pairs: impl IntoIterator<Item = (PathBuf, PathBuf)>,
        method: Method,
    ) -> Result<bool, OperationError> {
        let mut ops = Vec::new();
        let mut cleanup_ops = Vec::new();
        let mut written_files = Vec::new();
        let mut target_dirs = std::collections::HashSet::new();
        // Every item is checked before anything is touched or asked: the
        // whole size is known before the first question, and every question
        // knows how many more of its kind there are.
        let mut items = Vec::new();
        let mut problems: Vec<(Option<usize>, Problem)> = Vec::new();
        self.controller.set_checking(true);
        let mut room: std::collections::HashMap<PathBuf, u64> = std::collections::HashMap::new();
        for (from_parent, to_parent) in from_to_pairs {
            self.controller
                .check()
                .await
                .map_err(|s| OperationError::from_state(s, &self.controller))?;

            if from_parent == to_parent {
                // Skip matching source and destination
                continue;
            }

            let into = to_parent.parent().unwrap_or(&to_parent).to_path_buf();
            if !room.contains_key(&into) {
                // A folder this user may not write to is asked about first,
                // once; a read-only one was refused before anything ran
                let folder = into.clone();
                let denied = compio::runtime::spawn_blocking(move || super::cannot_write(&folder))
                    .await
                    .map_err(super::wrap_compio_spawn_error)?;
                if denied == Some(false) {
                    problems.push((None, Problem::Blocked(Blocked::Destination(into.clone()))));
                }
                room.insert(into.clone(), 0);
            }

            let index = items.len();
            let check = self.check_item(&from_parent, &to_parent, method).await?;
            if check.takes_room
                && let Some(needed) = room.get_mut(&into)
            {
                *needed += check.checked.size;
            }
            problems.extend(
                check
                    .checked
                    .problems
                    .into_iter()
                    .map(|blocked| (Some(index), Problem::Blocked(blocked))),
            );
            if let Some((folder, read_only)) = check.removal {
                let blocked = Blocked::Move {
                    path: from_parent.clone(),
                    folder,
                    read_only,
                };
                problems.push((Some(index), Problem::Blocked(blocked)));
            }
            problems.extend(
                check
                    .clashes
                    .into_iter()
                    .map(|(from, to)| (Some(index), Problem::Clash { from, to })),
            );
            items.push(Item {
                from_parent,
                to_parent,
                planned: check.checked.planned,
                skipped: false,
                root: false,
                takes_room: check.takes_room,
            });
        }

        // The size, before any question
        self.controller.set_checking(false);
        self.check_room(&room).await?;

        // What the helper may touch, should root be granted
        self.scope = root::Scope {
            sources: items.iter().map(|item| item.from_parent.clone()).collect(),
            destinations: room.keys().cloned().collect(),
        };

        // Then the questions, in the order their problems were found. One
        // that an earlier answer already settles is not asked.
        let mut skipped: Vec<PathBuf> = Vec::new();
        let mut rest: Vec<std::mem::Discriminant<Blocked>> = Vec::new();
        let mut rest_clash: Option<ReplaceResult> = None;
        let mut rest_root: Vec<std::mem::Discriminant<Blocked>> = Vec::new();
        // Folders listed as root can bring more to copy, and more problems
        let mut grew = false;
        let mut index = 0;
        while index < problems.len() {
            let settled = |item: Option<usize>, problem: &Problem, skipped: &[PathBuf]| {
                item.is_some_and(|item| items[item].skipped)
                    || skipped.iter().any(|path| problem.path().starts_with(path))
            };
            let (item, problem) = (problems[index].0, problems[index].1.clone());
            let problem = &problem;
            index += 1;
            if settled(item, problem, &skipped) {
                continue;
            }
            // How many of its kind are left, this one included
            let count = problems[index - 1..]
                .iter()
                .filter(|(other_item, other)| {
                    other.kind() == problem.kind() && !settled(*other_item, other, &skipped)
                })
                .count();
            match problem {
                Problem::Clash { from, to } => {
                    let answer = match rest_clash {
                        Some(answer) => answer,
                        None => {
                            // Asked as the copy itself would ask, about a
                            // step that has not run
                            let step = Op {
                                kind: OpKind::Copy,
                                from: from.clone(),
                                to: to.clone(),
                                skipped: Rc::new(Skip {
                                    normal: Cell::new(false),
                                    cleanup: Cell::new(false),
                                }),
                                is_cleanup: false,
                                created_to: Cell::new(false),
                                root: false,
                            };
                            (self.on_replace)(&step, count).await
                        }
                    };
                    match answer {
                        ReplaceResult::Replace(all) | ReplaceResult::Skip(all) if all => {
                            rest_clash = Some(answer);
                        }
                        _ => {}
                    }
                    match answer {
                        ReplaceResult::Replace(_) | ReplaceResult::KeepBoth => {
                            self.decided.insert(to.clone(), answer);
                        }
                        ReplaceResult::Skip(_) => {
                            skipped.push(from.clone());
                            self.op_sel.skipped.push(from.clone());
                        }
                        ReplaceResult::Cancel => return Err(self.cancelled()),
                    }
                }
                Problem::Blocked(blocked) => {
                    let kind = std::mem::discriminant(blocked);
                    let mut not_granted = false;
                    loop {
                        // Asked directly: "Same for the rest" here covers its
                        // own kind only, unlike a tick while the operation runs
                        let answer = if rest.contains(&kind) {
                            BlockedAnswer::Skip(false)
                        } else if rest_root.contains(&kind) {
                            BlockedAnswer::RetryAsRoot(false)
                        } else {
                            let ask = Ask {
                                count,
                                retry: false,
                                root: blocked.root_can_help().then_some(self.helper.is_some()),
                                not_granted,
                            };
                            (self.on_blocked)(blocked.clone(), ask).await
                        };
                        match answer {
                            BlockedAnswer::Skip(all) => {
                                if all {
                                    rest.push(kind);
                                }
                                match blocked {
                                    // Nothing can go there: everything going
                                    // there is left out
                                    Blocked::Destination(into) => {
                                        for item in &mut items {
                                            if item.to_parent.parent() == Some(into.as_path()) {
                                                item.skipped = true;
                                                self.op_sel.skipped.push(item.from_parent.clone());
                                            }
                                        }
                                    }
                                    // Not moved at all, rather than half moved
                                    Blocked::Move { .. } => {
                                        if let Some(item) = item {
                                            items[item].skipped = true;
                                        }
                                        self.op_sel.skipped.push(blocked.path().to_path_buf());
                                    }
                                    _ => {
                                        skipped.push(blocked.path().to_path_buf());
                                        self.op_sel.skipped.push(blocked.path().to_path_buf());
                                    }
                                }
                            }
                            BlockedAnswer::Cancel => return Err(self.cancelled()),
                            // Not offered before the operation runs: nothing
                            // has changed to try again for
                            BlockedAnswer::Retry => {}
                            BlockedAnswer::RetryAsRoot(all) => {
                                match self.ensure_helper().await {
                                    Ok(()) => {}
                                    // Asked again, saying so
                                    Err(StartError::NotGranted) => {
                                        not_granted = true;
                                        continue;
                                    }
                                    Err(StartError::Failed(err)) => {
                                        return Err(OperationError::from_err(
                                            super::failure_text(blocked.path(), &err),
                                            &self.controller,
                                        ));
                                    }
                                }
                                if all {
                                    rest_root.push(kind);
                                }
                                match blocked {
                                    // Everything going there goes as root
                                    Blocked::Destination(into) => {
                                        for item in &mut items {
                                            if item.to_parent.parent() == Some(into.as_path()) {
                                                item.root = true;
                                            }
                                        }
                                    }
                                    // Listed as root: what it holds joins the
                                    // plan, and is checked like the rest
                                    Blocked::List(dir) => {
                                        if let Some(item) = item {
                                            let (found, clashes) = self
                                                .check_listed_as_root(&items[item], dir, method)
                                                .await?;
                                            let into = items[item]
                                                .to_parent
                                                .parent()
                                                .unwrap_or(&items[item].to_parent)
                                                .to_path_buf();
                                            if items[item].takes_room
                                                && let Some(needed) = room.get_mut(&into)
                                            {
                                                *needed += found.size;
                                                grew = true;
                                            }
                                            items[item].planned.extend(found.planned);
                                            problems.extend(found.problems.into_iter().map(
                                                |blocked| (Some(item), Problem::Blocked(blocked)),
                                            ));
                                            problems.extend(clashes.into_iter().map(
                                                |(from, to)| {
                                                    (Some(item), Problem::Clash { from, to })
                                                },
                                            ));
                                            items[item].root = true;
                                        }
                                    }
                                    _ => {
                                        if let Some(item) = item {
                                            items[item].root = true;
                                        }
                                    }
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }

        // What root listed counts towards the room it takes
        if grew {
            self.check_room(&room).await?;
        }

        for item in items {
            if item.skipped {
                continue;
            }
            let root = item.root;
            for PlannedOp { kind, from, to } in item.planned {
                if skipped.iter().any(|path| from.starts_with(path)) {
                    continue;
                }
                let op = Op {
                    kind,
                    from,
                    to,
                    skipped: Rc::new(Skip {
                        normal: Cell::new(false),
                        cleanup: Cell::new(false),
                    }),
                    is_cleanup: false,
                    created_to: Cell::new(false),
                    root,
                };
                if matches!(method, Method::Move { .. })
                    && let Some(cleanup_op) = op.move_cleanup_op()
                {
                    cleanup_ops.push(cleanup_op);
                }
                if let Some(parent) = op.to.parent() {
                    target_dirs.insert(parent.to_path_buf());
                }
                ops.push(op);
            }

            self.op_sel.ignored.push(item.from_parent);
        }

        // Add cleanup ops after standard ops, in reverse
        cleanup_ops.reverse();
        ops.append(&mut cleanup_ops);

        // Taken destinations were asked about up front; one that appears
        // while the operation runs is asked about on its own
        self.remaining_conflicts = 1;

        let total_ops = ops.len();
        for (current_ops, mut op) in ops.into_iter().enumerate() {
            self.controller
                .check()
                .await
                .map_err(|s| OperationError::from_state(s, &self.controller))?;

            let progress = || Progress {
                current_ops,
                total_ops,
                current_bytes: 0,
                total_bytes: None,
            };
            (self.on_progress)(&op, &progress());
            // Noted before the op runs, because a Keep Both conflict rewrites
            // `op.to` to a fresh name and a Replace leaves an existing path in
            // place. Only a destination that was not already there counts as
            // created, and only created paths may be undone.
            let to_before = op.to.clone();
            let to_existed = compio::fs::symlink_metadata(&to_before).await.is_ok();
            // A step stopped for lack of permission asks the user, and runs
            // again for as long as they answer Retry.
            let mut left_alone = false;
            let ran = loop {
                let err = match op.run(self, progress()).await {
                    Err(err) => err,
                    ran => break ran,
                };
                let Some(Denied(blocked)) = err.downcast_ref::<Denied>() else {
                    break Err(err);
                };
                let blocked = blocked.clone();
                // Not foreseen by the checks: asked on its own
                let mut not_granted = false;
                let answer = loop {
                    let ask = Ask {
                        count: 1,
                        retry: blocked.can_retry(),
                        root: blocked.root_can_help().then_some(self.helper.is_some()),
                        not_granted,
                    };
                    let answer = self.blocked(blocked.clone(), ask).await;
                    if answer != BlockedAnswer::RetryAsRoot(false)
                        && answer != BlockedAnswer::RetryAsRoot(true)
                    {
                        break answer;
                    }
                    match self.ensure_helper().await {
                        Ok(()) => break answer,
                        Err(StartError::NotGranted) => not_granted = true,
                        Err(StartError::Failed(err)) => {
                            return Err(OperationError::from_err(
                                super::failure_text(blocked.path(), &err),
                                &self.controller,
                            ));
                        }
                    }
                };
                match answer {
                    BlockedAnswer::Retry => {}
                    // This step, and what is left of it, as root
                    BlockedAnswer::RetryAsRoot(_) => op.root = true,
                    BlockedAnswer::Skip(_) => {
                        // A file left unread is not written, and with it
                        // skipped its original stays; a cleanup that could
                        // not remove an original simply leaves it
                        if !op.is_cleanup {
                            op.skipped.normal.set(true);
                        }
                        left_alone = true;
                        self.op_sel.skipped.push(blocked.path().to_path_buf());
                        break Ok(true);
                    }
                    BlockedAnswer::Cancel => return Err(self.cancelled()),
                }
            };
            if ran.map_err(|err| {
                // Stopped by a cancel, it is a cancel, not a failure
                if self.controller.is_cancelled() {
                    return OperationError::from_state(
                        super::ControllerState::Cancelled,
                        &self.controller,
                    );
                }
                // The details for the log, the reason for the user
                log::warn!(
                    "failed to {:?} {} to {}: {}",
                    op.kind,
                    op.from.display(),
                    op.to.display(),
                    err
                );
                OperationError::from_err(super::failure_text(&op.from, &*err), &self.controller)
            })? {
                if matches!(
                    op.kind,
                    OpKind::Copy
                        | OpKind::Move {
                            cross_device_copy: true
                        }
                ) {
                    written_files.push(op.to.clone());
                }
                let creates = matches!(
                    op.kind,
                    OpKind::Copy | OpKind::Move { .. } | OpKind::Mkdir | OpKind::Symlink { .. }
                );
                let created =
                    creates && !op.skipped.normal.get() && (op.to != to_before || !to_existed);
                if created {
                    self.op_sel.created.push(op.to.clone());
                }
                // What ran, to put back if the operation is cancelled. A
                // step that was skipped, or asked about and left alone, did
                // nothing.
                let did_nothing = left_alone
                    || op.skipped.normal.get()
                    || (op.is_cleanup && op.skipped.cleanup.get());
                if !did_nothing {
                    let to = if op.is_cleanup {
                        self.landed.get(&op.from).cloned().unwrap_or(op.to.clone())
                    } else {
                        self.landed.insert(op.from.clone(), op.to.clone());
                        op.to.clone()
                    };
                    self.done.push(Done {
                        from: op.from.clone(),
                        to,
                        is_cleanup: op.is_cleanup,
                        is_dir: matches!(op.kind, OpKind::Mkdir | OpKind::Rmdir),
                        created,
                        root: op.root,
                    });
                }
                // What an undo has to put back, paired with where it came from.
                // The pairing cannot be recovered afterwards: a Keep Both
                // conflict gives the destination a different name. A directory
                // that was already there is left out, because a merge into it
                // transfers only its children and carrying the whole folder
                // back would take what it already held with it.
                if !op.is_cleanup
                    && !op.skipped.normal.get()
                    && (created
                        || matches!(
                            op.kind,
                            OpKind::Copy | OpKind::Move { .. } | OpKind::Symlink { .. }
                        ))
                {
                    self.op_sel.moved.push((op.from.clone(), op.to.clone()));
                }
                // The from path is ignored in the operation selection if it is a top level item.
                // Only from the op that did the work: a cleanup op still carries
                // the destination its main op was planned with, which Keep Both
                // has since renamed, so selecting it would name the file that
                // was already there rather than the one just moved.
                if !op.is_cleanup && self.op_sel.ignored.contains(&op.from) {
                    // So add the to path to the selection
                    self.op_sel.selected.push(op.to);
                }
            } else {
                // Cancelled in the replace dialog: put back like any cancel
                return Err(self.cancelled());
            }
        }

        // Flush files to disk
        sync_to_disk(written_files, target_dirs).await;

        Ok(true)
    }

    pub fn on_progress<F: OnProgress>(mut self, f: F) -> Self {
        self.on_progress = Box::new(f);
        self
    }

    pub fn on_replace(mut self, f: impl OnReplace + 'static) -> Self {
        self.on_replace = Box::pin(f);
        self
    }

    pub fn on_blocked(mut self, f: impl OnBlocked) -> Self {
        self.on_blocked = Box::new(f);
        self
    }

    async fn replace(&mut self, op: &Op) -> Result<ControlFlow<bool, PathBuf>, Box<dyn Error>> {
        // A source and destination that are the same file, which happens when
        // one of them is reached through a symlinked parent, cannot be copied
        // onto each other: replacing would unlink the only copy and then fail
        // to read it back. Nothing to do, so skip it without asking.
        if same_file(&op.from, &op.to).await {
            log::info!(
                "skipping {}: it is the same file as {}",
                op.from.display(),
                op.to.display()
            );
            op.skipped.normal.set(true);
            return Ok(ControlFlow::Break(true));
        }

        // Decided before the operation ran, or asked now: a destination
        // taken since the checks
        let replace_result = match (self.decided.get(&op.to), self.replace_result_opt) {
            (Some(decided), _) => *decided,
            (None, Some(result)) => result,
            (None, None) => (self.on_replace)(op, self.remaining_conflicts).await,
        };

        match replace_result {
            ReplaceResult::Replace(apply_to_all) => {
                if apply_to_all {
                    self.replace_result_opt = Some(replace_result);
                }
                compio::fs::remove_file(&op.to).await?;
                Ok(ControlFlow::Continue(op.to.clone()))
            }
            ReplaceResult::KeepBoth => match op.to.parent() {
                Some(to_parent) => Ok(ControlFlow::Continue(copy_unique_path(&op.from, to_parent))),
                None => Err(format!("failed to get parent of {}", op.to.display()).into()),
            },
            ReplaceResult::Skip(apply_to_all) => {
                if apply_to_all {
                    self.replace_result_opt = Some(replace_result);
                }
                op.skipped.normal.set(true);
                Ok(ControlFlow::Break(true))
            }
            ReplaceResult::Cancel => Ok(ControlFlow::Break(false)),
        }
    }
}

/// Removes what a step made at `to`: a folder only once it is empty, which
/// the steps inside it, put back first, have seen to. Already gone is fine.
async fn unmake(to: &Path, is_dir: bool) -> Result<(), Box<dyn Error>> {
    let result = if is_dir {
        compio::fs::remove_dir(to).await
    } else {
        compio::fs::remove_file(to).await
    };
    match result {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
        _ => Ok(()),
    }
}

/// Whether two paths name the same file on disk, following symlinks
async fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // Through the runtime rather than `std::fs`: this is asked once per
    // conflict, and on a slow or remote filesystem two stats on the operation
    // thread are two stalls nothing else can run through.
    match (compio::fs::metadata(a).await, compio::fs::metadata(b).await) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[derive(Debug)]
pub struct Progress {
    pub current_ops: usize,
    pub total_ops: usize,
    pub current_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Debug)]
pub enum OpKind {
    Copy,
    Move { cross_device_copy: bool },
    Mkdir,
    Remove,
    Rmdir,
    Symlink { target: PathBuf },
}

#[derive(Debug)]
pub struct Skip {
    /// Normal operation should be skipped
    pub normal: Cell<bool>,
    /// Cleanup operation should be skipped
    pub cleanup: Cell<bool>,
}

#[derive(Debug)]
pub struct Op {
    pub kind: OpKind,
    pub from: PathBuf,
    pub to: PathBuf,
    pub skipped: Rc<Skip>,
    pub is_cleanup: bool,
    /// Whether this op brought `to` into existence, so that cleaning up after
    /// a failure removes only what it put there.
    pub created_to: Cell<bool>,
    /// Done by the helper, as root.
    pub root: bool,
}

impl Op {
    /// Carries out this step through the helper, as root. A move is a rename
    /// there, falling back to a copy across filesystems; its cleanup step
    /// then finds the original already gone, which is fine.
    async fn run_as_root(
        &mut self,
        ctx: &mut Context,
        progress: Progress,
    ) -> Result<bool, Box<dyn Error>> {
        let gone = |result: std::io::Result<()>, also: i32| match result {
            Err(err)
                if err
                    .raw_os_error()
                    .is_some_and(|n| n == libc::ENOENT || n == also) =>
            {
                Ok(())
            }
            result => result,
        };
        if matches!(
            self.kind,
            OpKind::Copy | OpKind::Move { .. } | OpKind::Symlink { .. }
        ) && compio::fs::metadata(&self.to)
            .await
            .is_ok_and(|meta| meta.is_file())
        {
            match self.root_taken(ctx).await? {
                ControlFlow::Continue(()) => {}
                ControlFlow::Break(ret) => return Ok(ret),
            }
        }
        match &self.kind {
            OpKind::Copy => {
                ctx.root_copy(self, progress).await?;
                self.created_to.set(true);
            }
            OpKind::Move { cross_device_copy } => {
                let renamed = ctx
                    .root_call(Request::Rename {
                        from: self.from.clone(),
                        to: self.to.clone(),
                    })
                    .await;
                match renamed {
                    Err(err) if err.raw_os_error() == Some(libc::EXDEV) => {
                        if *cross_device_copy {
                            self.skipped.cleanup.set(true);
                        }
                        ctx.root_copy(self, progress).await?;
                    }
                    result => result?,
                }
                self.created_to.set(true);
            }
            OpKind::Mkdir => {
                ctx.root_call(Request::Mkdir {
                    from: self.from.clone(),
                    to: self.to.clone(),
                })
                .await?;
            }
            OpKind::Symlink { target } => {
                ctx.root_call(Request::Symlink {
                    target: target.clone(),
                    to: self.to.clone(),
                    from: self.from.clone(),
                })
                .await?;
            }
            OpKind::Remove => gone(ctx.root_call(Request::Remove(self.from.clone())).await, 0)?,
            OpKind::Rmdir => gone(
                ctx.root_call(Request::Rmdir(self.from.clone())).await,
                libc::ENOTEMPTY,
            )?,
        }
        Ok(true)
    }

    /// A destination already taken, for a step done as root: as decided
    /// before the operation ran, or asked now. Replacing removes it through
    /// the helper, since this user may not be able to.
    async fn root_taken(&mut self, ctx: &mut Context) -> Result<ControlFlow<bool>, Box<dyn Error>> {
        let answer = match (ctx.decided.get(&self.to), ctx.replace_result_opt) {
            (Some(decided), _) => *decided,
            (None, Some(result)) => result,
            (None, None) => (ctx.on_replace)(self, 1).await,
        };
        match answer {
            ReplaceResult::Replace(all) => {
                if all {
                    ctx.replace_result_opt = Some(answer);
                }
                ctx.root_call(Request::Remove(self.to.clone())).await?;
                Ok(ControlFlow::Continue(()))
            }
            ReplaceResult::KeepBoth => {
                if let Some(parent) = self.to.parent() {
                    self.to = copy_unique_path(&self.from, parent);
                }
                Ok(ControlFlow::Continue(()))
            }
            ReplaceResult::Skip(all) => {
                if all {
                    ctx.replace_result_opt = Some(answer);
                }
                self.skipped.normal.set(true);
                Ok(ControlFlow::Break(true))
            }
            ReplaceResult::Cancel => Ok(ControlFlow::Break(false)),
        }
    }

    fn move_cleanup_op(&self) -> Option<Self> {
        let kind = match self.kind {
            OpKind::Copy | OpKind::Move { .. } | OpKind::Symlink { .. } => OpKind::Remove,
            OpKind::Mkdir => OpKind::Rmdir,
            OpKind::Remove | OpKind::Rmdir => return None,
        };
        Some(Self {
            kind,
            from: self.from.clone(),
            to: self.to.clone(),
            skipped: self.skipped.clone(),
            is_cleanup: true,
            created_to: Cell::new(false),
            root: self.root,
        })
    }

    async fn run(&mut self, ctx: &mut Context, progress: Progress) -> Result<bool, Box<dyn Error>> {
        if self.skipped.normal.get() || (self.is_cleanup && self.skipped.cleanup.get()) {
            return Ok(true);
        }
        if self.root {
            return self.run_as_root(ctx, progress).await;
        }
        match self.kind {
            OpKind::Copy => {
                crate::operation::actively_writing_add(self.to.clone());
                let result = self.copy(ctx, progress).await;

                // Only a destination this copy created: an error also comes
                // from a path that was already taken, such as a dangling
                // symlink that `create_new` refuses to write through, and that
                // one belongs to whoever put it there. Removing it would
                // destroy it without ever asking about replacement.
                if result.is_err() && self.created_to.get() {
                    _ = compio::fs::remove_file(&self.to).await;
                }

                crate::operation::actively_writing_remove(&self.to);
                return result;
            }
            OpKind::Move { cross_device_copy } => {
                // Do not clean up if cross_device_copy is set
                if cross_device_copy {
                    self.skipped.cleanup.set(true);
                }

                // Remove `to` if overwriting and it is an existing file
                if compio::fs::metadata(&self.to)
                    .await
                    .is_ok_and(|metadata| metadata.is_file())
                {
                    match ctx.replace(self).await? {
                        ControlFlow::Continue(to) => {
                            self.to = to;
                        }
                        ControlFlow::Break(ret) => {
                            return Ok(ret);
                        }
                    }
                }
                // This is atomic and ensures `to` is not created by any other process
                match compio::fs::hard_link(&self.from, &self.to).await {
                    Ok(()) => {}
                    Err(err) => {
                        // Fall back to a plain copy on any failure but a
                        // name that appeared at `to` in the meantime, not
                        // only on a cross-device error: filesystems without
                        // hard links (vfat, exfat, many FUSE mounts) answer
                        // `EPERM` or `ENOTSUP`, and by now a replaced
                        // destination is already gone. Returning that error
                        // would leave nothing at `to`.
                        if err.raw_os_error() != Some(libc::EEXIST) {
                            if cross_device_copy {
                                // Do not clean up if cross_device_copy is set
                                self.skipped.cleanup.set(true);
                            }
                            let mut copy_op = Self {
                                kind: OpKind::Copy,
                                from: self.from.clone(),
                                to: self.to.clone(),
                                skipped: self.skipped.clone(),
                                is_cleanup: self.is_cleanup,
                                created_to: Cell::new(false),
                                root: self.root,
                            };
                            return Box::pin(copy_op.run(ctx, progress)).await;
                        }
                        return Err(err.into());
                    }
                }
            }
            OpKind::Mkdir => {
                compio::fs::create_dir_all(&self.to).await?;
            }
            OpKind::Remove => match compio::fs::remove_file(&self.from).await {
                Ok(()) => {}
                Err(err) if is_denied(&err) => {
                    return Err(Denied(Blocked::Remove(self.from.clone())).into());
                }
                Err(err) => return Err(err.into()),
            },
            OpKind::Rmdir => {
                match compio::fs::remove_dir(&self.from).await {
                    Ok(()) => {}
                    // A skipped entry legitimately leaves the source directory behind
                    Err(err) if err.raw_os_error() == Some(libc::ENOTEMPTY) => {}
                    Err(err) if is_denied(&err) => {
                        return Err(Denied(Blocked::Remove(self.from.clone())).into());
                    }
                    Err(err) => return Err(err.into()),
                }
            }
            OpKind::Symlink { ref target } => {
                // Remove `to` if overwriting and it is an existing file
                if compio::fs::metadata(&self.to)
                    .await
                    .is_ok_and(|metadata| metadata.is_file())
                {
                    match ctx.replace(self).await? {
                        ControlFlow::Continue(to) => {
                            self.to = to;
                        }
                        ControlFlow::Break(ret) => {
                            return Ok(ret);
                        }
                    }
                }
                // Off the operation thread: creating a link is a directory
                // write, and on a remote filesystem it is a round trip.
                let (target, to) = (target.clone(), self.to.clone());
                let made =
                    compio::runtime::spawn_blocking(move || std::os::unix::fs::symlink(target, to))
                        .await
                        .map_err(super::wrap_compio_spawn_error)?;
                match made {
                    Ok(()) => {}
                    // `EPERM`, unlike `EACCES`, is the filesystem saying it
                    // cannot hold links at all (symlink(2))
                    Err(err) if err.raw_os_error() == Some(libc::EPERM) => {
                        let fs = self.to.parent().and_then(super::filesystem_name);
                        return Err(Denied(Blocked::Link {
                            path: self.from.clone(),
                            fs,
                        })
                        .into());
                    }
                    Err(err) => return Err(err.into()),
                }
            }
        }
        Ok(true)
    }

    async fn copy(
        &mut self,
        ctx: &mut Context,
        mut progress: Progress,
    ) -> Result<bool, Box<dyn Error>> {
        // Remove `to` if overwriting and it is an existing file
        if compio::fs::metadata(&self.to)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            match ctx.replace(self).await? {
                ControlFlow::Continue(to) => {
                    self.to = to;
                }
                ControlFlow::Break(ret) => {
                    return Ok(ret);
                }
            }
        }

        let (from_file_open_result, metadata, to_file_open_result) = crate::ui::iced::futures::join!(
            async {
                compio::fs::OpenOptions::new()
                    .read(true)
                    .open(&self.from)
                    .await
                    .with_context(|| format!("failed to open {} for reading", self.from.display(),))
            },
            async { compio::fs::metadata(&self.from).await.ok() },
            // This is atomic and ensures `to` is not created by any other process
            async {
                compio::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&self.to)
                    .await
            }
        );

        // Noted before anything can return: the two opens run side by side, so
        // an exclusive create can have made the destination by the time the
        // source open comes back with an error, and that empty file is this
        // copy's to clear away.
        self.created_to.set(to_file_open_result.is_ok());

        let from_file = match from_file_open_result {
            Ok(file) => file,
            Err(err) if err.downcast_ref::<std::io::Error>().is_some_and(is_denied) => {
                return Err(Denied(Blocked::Read(self.from.clone())).into());
            }
            Err(err) => return Err(err.into()),
        };

        let mut to_file = match to_file_open_result {
            Ok(file) => file,
            #[cfg(not(feature = "gvfs"))]
            Err(why) => {
                _ = from_file.close().await;
                return Err(why)
                    .with_context(|| format!("failed to open {} for writing", self.to.display()))
                    .map_err(Into::into);
            }
            #[cfg(feature = "gvfs")]
            Err(why) => {
                _ = from_file.close().await;
                // A destination that is already taken is not something to push
                // past. This fallback exists for the unsupported-operation
                // errors that copying over MTP raises, but it removes whatever
                // is at the destination and copies with `OVERWRITE`, so letting
                // an `AlreadyExists` through would destroy a file — or a
                // dangling symlink — that nobody agreed to replace.
                if why.kind() == std::io::ErrorKind::AlreadyExists {
                    return Err(why)
                        .with_context(|| {
                            format!("failed to open {} for writing", self.to.display())
                        })
                        .map_err(Into::into);
                }
                return self
                    .gio_file_copy(ctx, progress)
                    .await
                    .map(|()| true)
                    .map_err(Into::into);
            }
        };
        progress.total_bytes = metadata.as_ref().map(|m| m.len());
        (ctx.on_progress)(self, &progress);

        if let Some(metadata) = metadata.as_ref()
            && let Err(why) = to_file.set_permissions(metadata.permissions()).await
        {
            // This error is not propagated upwards as some filesystems do not support setting permissions
            if !matches!(why.kind(), std::io::ErrorKind::Unsupported) {
                tracing::warn!(?why, "failed to set permissions for {}", self.to.display(),);
            }
        }

        // Prevent spamming the progress callbacks.
        let mut last_progress_update = Instant::now();
        // io_uring/IOCP requires transferring ownership of the buffer to the kernel.
        let mut buf_in = std::mem::take(&mut ctx.buf);
        // Track where the current read/write position is at.
        let mut pos = 0;

        loop {
            let BufResult(result, buf_out) = from_file.read_at(buf_in, pos).await;

            let count = match result {
                Ok(0) => {
                    buf_in = buf_out;
                    break;
                }
                Ok(count) => count,
                Err(why) => {
                    ctx.buf = buf_out;
                    tracing::error!("failed to read: {:?}", why);
                    _ = futures::future::join(from_file.close(), to_file.close()).await;
                    return Err(why).context("failed to read")?;
                }
            };

            // `write_all_at`, not `write_at`: a single write is allowed to
            // take fewer bytes than it was given, and advancing `pos` by the
            // whole read would leave a hole in the copy and report success. A
            // move would then delete the source of a file it had truncated.
            let BufResult(result, buf_out_slice) =
                to_file.write_all_at(buf_out.slice(..count), pos).await;
            let buf_out = buf_out_slice.into_inner();

            if let Err(why) = result {
                #[cfg(feature = "gvfs")]
                if let std::io::ErrorKind::Unsupported = why.kind() {
                    ctx.buf = buf_out;
                    _ = futures::future::join(from_file.close(), to_file.close()).await;
                    return self
                        .gio_file_copy(ctx, progress)
                        .await
                        .map(|_| true)
                        .map_err(Into::into);
                }

                tracing::error!("failed to write: {:?}", why);
                ctx.buf = buf_out;
                _ = futures::future::join(from_file.close(), to_file.close()).await;
                return Err(why).context("failed to write")?;
            }

            progress.current_bytes += count as u64;
            pos += count as u64;

            // Avoid spamming progress messages too early.
            let current = Instant::now();
            if current.duration_since(last_progress_update).as_millis() > 49 {
                last_progress_update = current;
                (ctx.on_progress)(self, &progress);

                // Also check if the progress was cancelled.
                if let Err(state) = ctx.controller.check().await {
                    ctx.buf = buf_out;
                    tracing::warn!(
                        "operation to copy from {:?} to {:?} cancelled",
                        self.from,
                        self.to
                    );
                    _ = futures::future::join(from_file.close(), to_file.close()).await;
                    return Err(OperationError::from_state(state, &ctx.controller).into());
                }
            }

            buf_in = buf_out;
        }

        ctx.buf = buf_in;

        if let Some(metadata) = metadata.as_ref() {
            let mut times = fs::FileTimes::new();
            if let Ok(time) = metadata.modified() {
                times = times.set_modified(time);
            }
            if let Ok(time) = metadata.accessed() {
                times = times.set_accessed(time);
            }
            let op = AsyncifyFd::new(to_file.to_shared_fd(), move |file: &std::fs::File| {
                BufResult(file.set_times(times).map(|_| 0), ())
            });
            match compio::runtime::submit(op).await.0.map(|_| ()) {
                Ok(()) => {
                    tracing::info!("set times for {} to {:?}", self.to.display(), times);
                }
                Err(why) => {
                    if !matches!(why.kind(), std::io::ErrorKind::Unsupported) {
                        tracing::error!(?why, "failed to set times for {}", self.to.display());
                    }
                }
            }
        }

        _ = to_file.close().await;

        Ok(true)
    }

    /// Fallback mechanism in the event that unsupported I/O error errors occur.
    /// Fixes unsupported errors when copying large files over MTP.
    #[cfg(feature = "gvfs")]
    async fn gio_file_copy(
        &self,
        ctx: &mut Context,
        mut progress: Progress,
    ) -> Result<(), GioCopyError> {
        _ = compio::fs::remove_file(&self.to).await;
        // Removed and written again here, so the destination is this copy's.
        self.created_to.set(true);

        let from = gio::File::for_path(&self.from);
        let to = gio::File::for_path(&self.to);
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (pause_tx, mut pause_rx) = tokio::sync::watch::channel(false);

        let task = compio::runtime::spawn_blocking(move || {
            let glib_context = glib::MainContext::new();
            let glib_loop = glib::MainLoop::new(Some(&glib_context), false);
            glib_context.with_thread_default(move || {
                let glib_loop2 = glib_loop.clone();
                glib::MainContext::ref_thread_default().spawn_local(async move {
                    // Create a future for copying the file with `gio::File`. This also creates a progress stream.
                    let (gio_copy_fut, mut progress_stream) = from.copy_future(
                        &to,
                        gio::FileCopyFlags::OVERWRITE | gio::FileCopyFlags::ALL_METADATA,
                        glib::Priority::LOW,
                    );

                    let mut copy_fut = gio_copy_fut
                        .map(|result| result.map_err(GioCopyError::GLib))
                        .fuse();

                    let progress_fut = std::pin::pin!(async {
                        while let Some((current_bytes, _)) = progress_stream.next().await {
                            _ = progress_tx.send(current_bytes);
                        }

                        drop(progress_tx);
                        futures::future::pending::<()>().await;
                    });

                    let mut progress_fut = progress_fut.fuse();
                    let mut pause_rx2 = pause_rx.clone();

                    loop {
                        let until_paused = std::pin::pin!(pause_rx.wait_for(|paused| *paused));
                        futures::select! {
                            _ = &mut progress_fut => {},

                            result = &mut copy_fut => {
                                _ = tx.send(result.map(|_| ()));
                                glib_loop2.quit();
                                return;
                            }

                            _ = until_paused.fuse() => {
                                _ = pause_rx2.wait_for(|paused| !*paused).await;
                            }
                        }
                    }
                });

                glib_loop.run();
            })
        });

        let mut last_progress_update = Instant::now();
        let mut task = task.fuse();
        let mut rx = rx.fuse();

        loop {
            let until_paused = std::pin::pin!(ctx.controller.until_paused());
            futures::select! {
                value = progress_rx.recv().fuse() => {
                    if let Some(current_bytes) = value {
                        progress.current_bytes = current_bytes as u64;
                        let current = Instant::now();
                        if current.duration_since(last_progress_update).as_millis() > 49 {
                            last_progress_update = current;
                            (ctx.on_progress)(self, &progress);
                            // Also check if the progress was cancelled.
                            if let Err(state) = ctx.controller.check().await {
                                tracing::warn!(
                                    "operation to copy from {:?} to {:?} cancelled",
                                    self.from,
                                    self.to
                                );
                                return Err::<(), GioCopyError>(GioCopyError::Controller(
                                    OperationError::from_state(state, &ctx.controller),
                                ));
                            }
                        }
                    }
                }

                result = rx => return result.unwrap(),

                _ = task => (),

                _ = until_paused.fuse() => {
                    // Pauses an active copy while the controller state is paused.
                    _ = pause_tx.send(true);
                    ctx.controller.until_unpaused().await;
                    _ = pause_tx.send(false);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::check::{Target, check_tree};
    use super::{Context, Done, Method, OpKind, PlannedOp};
    use crate::operation::{Controller, ControllerState};
    use std::fs;
    use std::path::Path;
    use test_log::test;

    /// A folder moved away whole, its originals removed: the steps that did
    /// it, as the step loop records them, oldest first.
    fn moved_folder(from: &Path, to: &Path) -> Vec<Done> {
        let step = |from: &Path, to: &Path, is_cleanup, is_dir, created| Done {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
            is_cleanup,
            is_dir,
            created,
            root: false,
        };
        let (from_file, to_file) = (from.join("f.txt"), to.join("f.txt"));
        vec![
            step(from, to, false, true, true),
            step(&from_file, &to_file, false, false, true),
            step(&from_file, &to_file, true, false, false),
            step(from, to, true, true, false),
        ]
    }

    /// Everything a cancelled move had removed comes back, and what it made
    /// at the destination goes: on the same filesystem by renaming, and
    /// across filesystems by copying back.
    #[test(compio::test)]
    async fn a_rolled_back_move_brings_its_originals_back() {
        let here = tempfile::tempdir().expect("tempdir");
        // `/dev/shm` is a filesystem of its own on Linux; without it, only
        // the same-filesystem case runs
        let there = tempfile::tempdir_in("/dev/shm").ok();
        for dest in [
            Some(here.path()),
            there.as_ref().map(tempfile::TempDir::path),
        ]
        .into_iter()
        .flatten()
        {
            let from = here.path().join("folder");
            let to = dest.join("moved");
            // The state a move leaves once its cleanups ran: all at `to`
            fs::create_dir(&to).expect("mkdir");
            fs::write(to.join("f.txt"), b"F").expect("write");

            let mut ctx = Context::new(Controller::default());
            ctx.done = moved_folder(&from, &to);
            let not_back = ctx.roll_back(&[]).await;

            assert!(not_back.is_empty(), "{not_back:?}");
            assert_eq!(fs::read(from.join("f.txt")).expect("back"), b"F");
            assert!(!to.exists(), "nothing is left at the destination");
            fs::remove_dir_all(&from).expect("clean up");
        }
    }

    /// A copy that does not fit is refused before anything is touched or
    /// asked; a move on the same filesystem needs no room and is not.
    #[test(compio::test)]
    async fn what_does_not_fit_is_refused_before_anything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("big.bin");
        fs::write(&from, vec![0u8; 4096]).expect("write");
        let to = dir.path().join("dest");
        fs::create_dir(&to).expect("mkdir");
        let full = |_: &Path| Some(1024);

        let mut ctx = Context::new(Controller::default());
        ctx.free_space = full;
        let err = ctx
            .recursive_copy_or_move([(from.clone(), to.join("big.bin"))], Method::Copy)
            .await
            .expect_err("it does not fit");
        assert_eq!(
            err.to_string(),
            crate::fl!(
                "not-enough-space",
                needed = crate::tab::format_size(4096),
                free = crate::tab::format_size(1024)
            )
        );
        assert!(!to.join("big.bin").exists(), "nothing was copied");

        let mut ctx = Context::new(Controller::default());
        ctx.free_space = full;
        let moved = ctx
            .recursive_copy_or_move(
                [(from.clone(), to.join("big.bin"))],
                Method::Move {
                    cross_device_copy: false,
                },
            )
            .await;
        assert!(moved.is_ok(), "{moved:?}");
        assert!(to.join("big.bin").exists());
    }

    /// A stand-in for root: before starting the helper in this process, it
    /// gives the paths in `ALLOW` their permissions back, as root would not
    /// need them, and counts how often it was started. Each test has its
    /// own, since tests run side by side.
    macro_rules! fake_root {
        ($name:ident, $granted:expr) => {
            mod $name {
                use crate::operation::root::{Helper, Scope, StartError};
                use std::path::PathBuf;
                use std::sync::Mutex;
                use std::sync::atomic::{AtomicUsize, Ordering};

                pub static ALLOW: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
                pub static STARTED: AtomicUsize = AtomicUsize::new(0);
                pub static REFUSE: AtomicUsize = AtomicUsize::new(0);

                pub fn start(scope: &Scope) -> Result<Helper, StartError> {
                    use std::os::unix::fs::PermissionsExt;
                    if REFUSE.load(Ordering::SeqCst) > 0 {
                        REFUSE.fetch_sub(1, Ordering::SeqCst);
                        return Err(StartError::NotGranted);
                    }
                    let _: bool = $granted;
                    STARTED.fetch_add(1, Ordering::SeqCst);
                    for path in ALLOW.lock().expect("allow").iter() {
                        let mode = if path.is_dir() { 0o755 } else { 0o644 };
                        let _ =
                            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
                    }
                    Helper::in_process(scope)
                }
            }
        };
    }

    /// Runs a copy through a context whose root is `start`, answering each
    /// question with `answer`; returns how each was asked.
    async fn copy_with_root(
        pairs: Vec<(std::path::PathBuf, std::path::PathBuf)>,
        start: crate::operation::root::Start,
        answer: fn(&crate::operation::Ask) -> crate::operation::BlockedAnswer,
    ) -> (
        Result<bool, crate::operation::OperationError>,
        Vec<crate::operation::Ask>,
    ) {
        use std::cell::RefCell;
        use std::rc::Rc;
        let asked: Rc<RefCell<Vec<crate::operation::Ask>>> = Rc::default();
        let seen = asked.clone();
        let mut ctx = Context::new(Controller::default()).on_blocked(move |_blocked, ask| {
            seen.borrow_mut().push(ask);
            let reply = answer(&ask);
            Box::pin(async move { reply })
        });
        ctx.start_helper = start;
        let result = ctx.recursive_copy_or_move(pairs, Method::Copy).await;
        let asked = asked.borrow().clone();
        (result, asked)
    }

    fn deny(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).expect("chmod");
        // Running with privileges that ignore the mode, nothing is denied
        fs::File::open(path).is_err() && fs::read_dir(path).is_err()
            || path.is_file() && fs::File::open(path).is_err()
    }

    fake_root!(root_for_files, true);

    #[test(compio::test)]
    async fn root_copies_what_this_user_cannot_and_asks_once() {
        use crate::operation::BlockedAnswer;
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().expect("tempdir");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, b"A").expect("write");
        fs::write(&b, b"B").expect("write");
        let to = dir.path().join("to");
        fs::create_dir(&to).expect("mkdir");
        if !deny(&a) || !deny(&b) {
            return;
        }
        *root_for_files::ALLOW.lock().expect("allow") = vec![a.clone(), b.clone()];

        let (result, asked) = copy_with_root(
            vec![(a.clone(), to.join("a")), (b.clone(), to.join("b"))],
            root_for_files::start,
            |_| BlockedAnswer::RetryAsRoot(false),
        )
        .await;

        result.expect("copied as root");
        assert_eq!(fs::read(to.join("a")).expect("a"), b"A");
        assert_eq!(fs::read(to.join("b")).expect("b"), b"B");
        // Asked for the password once; the second question offers root again
        assert_eq!(
            asked.iter().map(|ask| ask.root).collect::<Vec<_>>(),
            [Some(false), Some(true)]
        );
        assert_eq!(root_for_files::STARTED.load(Ordering::SeqCst), 1);
    }

    fake_root!(root_refused, true);

    #[test(compio::test)]
    async fn root_not_granted_asks_again_saying_so() {
        use crate::operation::BlockedAnswer;
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        fs::write(&a, b"A").expect("write");
        let to = dir.path().join("to");
        fs::create_dir(&to).expect("mkdir");
        if !deny(&a) {
            return;
        }
        root_refused::REFUSE.store(1, Ordering::SeqCst);

        let (result, asked) = copy_with_root(
            vec![(a.clone(), to.join("a"))],
            root_refused::start,
            |ask| {
                if ask.not_granted {
                    BlockedAnswer::Skip(false)
                } else {
                    BlockedAnswer::RetryAsRoot(false)
                }
            },
        )
        .await;

        result.expect("skipped in the end");
        assert_eq!(
            asked
                .iter()
                .map(|ask| (ask.root, ask.not_granted))
                .collect::<Vec<_>>(),
            [(Some(false), false), (Some(false), true)]
        );
        assert!(!to.join("a").exists());
        assert_eq!(root_refused::STARTED.load(Ordering::SeqCst), 0);
    }

    fake_root!(root_for_a_folder, true);

    #[test(compio::test)]
    async fn a_folder_listed_as_root_is_copied_whole() {
        use crate::operation::BlockedAnswer;
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("from");
        let locked = from.join("locked");
        fs::create_dir_all(locked.join("deeper")).expect("mkdir");
        fs::write(locked.join("deeper/f"), b"F").expect("write");
        fs::write(from.join("open"), b"O").expect("write");
        let to = dir.path().join("to");
        fs::create_dir(&to).expect("mkdir");
        if !deny(&locked) {
            return;
        }
        *root_for_a_folder::ALLOW.lock().expect("allow") = vec![locked.clone()];

        let (result, asked) = copy_with_root(
            vec![(from.clone(), to.join("from"))],
            root_for_a_folder::start,
            |_| BlockedAnswer::RetryAsRoot(false),
        )
        .await;

        result.expect("copied");
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert_eq!(fs::read(to.join("from/open")).expect("open"), b"O");
        assert_eq!(
            fs::read(to.join("from/locked/deeper/f")).expect("inside"),
            b"F"
        );
    }

    fake_root!(root_puts_back, true);

    /// A step done as root is put back as root when the operation is
    /// cancelled.
    #[test(compio::test)]
    async fn root_steps_are_put_back_as_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("folder");
        let to = dir.path().join("moved");
        fs::create_dir(&to).expect("mkdir");
        fs::write(to.join("f.txt"), b"F").expect("write");

        let mut ctx = Context::new(Controller::default());
        ctx.start_helper = root_puts_back::start;
        ctx.scope = crate::operation::root::Scope {
            sources: vec![from.clone()],
            destinations: vec![dir.path().to_path_buf()],
        };
        ctx.ensure_helper().await.expect("helper");
        ctx.done = moved_folder(&from, &to)
            .into_iter()
            .map(|step| Done { root: true, ..step })
            .collect();
        let not_back = ctx.roll_back(&[]).await;

        assert!(not_back.is_empty(), "{not_back:?}");
        assert_eq!(fs::read(from.join("f.txt")).expect("back"), b"F");
        assert!(!to.exists(), "nothing is left at the destination");
    }

    #[test]
    fn a_selected_link_to_a_file_is_planned_as_a_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("target.txt"), b"t").expect("write");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink("target.txt", &link).expect("symlink");

        let planned = check_tree(
            &link,
            &dir.path().join("out"),
            Method::Copy,
            &Controller::default(),
            Target::default(),
            false,
        )
        .expect("planning")
        .planned;

        assert_eq!(planned.len(), 1);
        assert!(
            matches!(&planned[0].kind, OpKind::Symlink { target } if target == std::path::Path::new("target.txt")),
            "a link is copied as a link, not as its target's bytes"
        );
    }

    #[test]
    fn a_plan_covers_the_whole_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::create_dir(&from).expect("mkdir");
        fs::create_dir(from.join("sub")).expect("mkdir");
        fs::write(from.join("a.txt"), b"a").expect("write");
        fs::write(from.join("sub").join("b.txt"), b"b").expect("write");
        std::os::unix::fs::symlink("a.txt", from.join("link")).expect("symlink");

        let checked = check_tree(
            &from,
            &to,
            Method::Copy,
            &Controller::default(),
            Target::default(),
            false,
        )
        .expect("planning");
        assert!(checked.problems.is_empty());
        let planned = checked.planned;

        let mut kinds: Vec<(String, &'static str)> = planned
            .iter()
            .map(|PlannedOp { kind, to, .. }| {
                let name = to
                    .strip_prefix(dir.path())
                    .expect("under the temp dir")
                    .to_string_lossy()
                    .into_owned();
                let kind = match kind {
                    OpKind::Mkdir => "mkdir",
                    OpKind::Copy => "copy",
                    OpKind::Symlink { .. } => "symlink",
                    _ => "other",
                };
                (name, kind)
            })
            .collect();
        kinds.sort();

        assert_eq!(
            kinds,
            vec![
                ("to".to_owned(), "mkdir"),
                ("to/a.txt".to_owned(), "copy"),
                ("to/link".to_owned(), "symlink"),
                ("to/sub".to_owned(), "mkdir"),
                ("to/sub/b.txt".to_owned(), "copy"),
            ],
            "every entry is planned, at its place under the destination"
        );
    }

    #[test]
    fn a_cancelled_plan_stops_rather_than_walking_the_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("from");
        fs::create_dir(&from).expect("mkdir");
        fs::write(from.join("a.txt"), b"a").expect("write");

        let controller = Controller::default();
        controller.set_state(ControllerState::Cancelled);

        let result = check_tree(
            &from,
            &dir.path().join("to"),
            Method::Copy,
            &controller,
            Target::default(),
            false,
        );

        assert!(
            result.is_err(),
            "a cancelled plan returned a list of work to do"
        );
    }
}
