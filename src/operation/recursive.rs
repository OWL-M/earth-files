// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    Blocked, BlockedAnswer, Controller, OperationSelection, ReplaceResult, copy_unique_path,
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
    match err {
        ignore::Error::WithPath { path, err } if err.io_error().is_some_and(is_denied) => {
            Some(path.clone())
        }
        _ => None,
    }
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

/// What a walk planned, and the folders in it that it could not list.
type Plan = (Vec<PlannedOp>, Vec<PathBuf>);

/// Work out every step of copying or moving `from_parent` to `to_parent`.
///
/// A folder that cannot be listed for lack of permission is left out, its
/// own step included, and returned beside the plan so the user can be asked
/// about it.
///
/// Blocking by nature: it reads the whole tree and stats every entry. Run it
/// on a worker, never on the thread the operations themselves run on.
fn plan_tree(
    from_parent: &Path,
    to_parent: &Path,
    method: Method,
    controller: &Controller,
) -> Result<Plan, OperationError> {
    let mut planned = Vec::new();
    let mut unlisted = Vec::new();
    // A root that is itself a link is one step, never a walk: the walker
    // follows a root link that points at a regular file, and would plan a
    // copy of the target's bytes where the user selected the link.
    let root = fs::symlink_metadata(from_parent).map_err(|err| {
        OperationError::from_err(
            format!("failed to stat {}: {}", from_parent.display(), err),
            controller,
        )
    })?;
    if root.file_type().is_symlink() {
        let target = fs::read_link(from_parent).map_err(|err| {
            OperationError::from_err(
                format!("failed to read link {}: {}", from_parent.display(), err),
                controller,
            )
        })?;
        planned.push(PlannedOp {
            kind: OpKind::Symlink { target },
            from: from_parent.to_path_buf(),
            to: to_parent.to_path_buf(),
        });
        return Ok((planned, unlisted));
    }
    for entry in ignore::WalkBuilder::new(from_parent)
        .standard_filters(false)
        .build()
    {
        // Checked here, inside the work: nothing outside can interrupt a
        // blocking job, so a walk that is not watched from within would run to
        // the end of the tree after the user cancelled it.
        loop {
            match controller.state() {
                super::ControllerState::Running => break,
                super::ControllerState::Paused => std::thread::sleep(PLAN_PAUSE_POLL),
                state => return Err(OperationError::from_state(state, controller)),
            }
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                if let Some(dir) = unlisted_dir(&err) {
                    // Its own step came before the error: without it,
                    // nothing of the folder is copied, not even empty
                    planned.retain(|op: &PlannedOp| op.from != dir);
                    unlisted.push(dir);
                    continue;
                }
                return Err(OperationError::from_err(
                    format!(
                        "failed to walk directory {}: {}",
                        from_parent.display(),
                        err
                    ),
                    controller,
                ));
            }
        };
        let file_type = entry.file_type();
        let from = entry.into_path();
        let kind = if file_type.is_some_and(|t| t.is_dir()) {
            OpKind::Mkdir
        } else if file_type.is_some_and(|t| t.is_file()) {
            match method {
                Method::Copy => OpKind::Copy,
                Method::Move { cross_device_copy } => OpKind::Move { cross_device_copy },
            }
        } else if file_type.is_some_and(|t| t.is_symlink()) {
            let target = fs::read_link(&from).map_err(|err| {
                OperationError::from_err(
                    format!("failed to read link {}: {}", from_parent.display(), err),
                    controller,
                )
            })?;
            OpKind::Symlink { target }
        } else {
            // Sockets, FIFOs and device nodes cannot be copied meaningfully
            log::warn!(
                "skipping {}: not a regular file, directory or symlink",
                from.display()
            );
            continue;
        };
        let to = if from == from_parent {
            // When copying a file, from matches from_parent, and to_parent must be used
            to_parent.to_path_buf()
        } else {
            let relative = from.strip_prefix(from_parent).map_err(|err| {
                OperationError::from_err(
                    format!(
                        "failed to remove prefix {} from {}: {}",
                        from_parent.display(),
                        from.display(),
                        err
                    ),
                    controller,
                )
            })?;
            to_parent.join(relative)
        };
        planned.push(PlannedOp { kind, from, to });
    }
    Ok((planned, unlisted))
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
    /// The user chose to copy every item that cannot be moved from here on.
    copy_instead: bool,
    /// The steps that ran, oldest first, so a cancelled operation can put
    /// back what they did.
    done: Vec<Done>,
    /// Where each moved or copied path landed, which Keep Both may have
    /// renamed: the cleanup step that removes its original does not know.
    landed: std::collections::HashMap<PathBuf, PathBuf>,
}

/// One selected item, planned: its steps, the folders in it the walk could
/// not list yet, and what the user decided about it.
struct Item {
    from_parent: PathBuf,
    to_parent: PathBuf,
    planned: Vec<PlannedOp>,
    unlisted: std::collections::VecDeque<PathBuf>,
    /// Copied instead of moved: its originals could not be removed.
    keeps_original: bool,
    /// Left out whole.
    skipped: bool,
}

/// A later step as a question would see it: what of it could be blocked.
enum Ahead {
    /// It reads `from`.
    Read(PathBuf),
    /// It makes a link at `to`.
    Link(PathBuf),
    /// It removes `from` from its folder.
    Remove(PathBuf),
}

impl Ahead {
    /// Whether this step would be blocked too, as far as can be told before
    /// it runs. Blocking: it asks the filesystem.
    fn would_block(&self) -> bool {
        match self {
            Self::Read(from) => super::cannot_read(from),
            Self::Link(to) => to.parent().and_then(super::filesystem_name).is_some(),
            Self::Remove(from) => from.parent().and_then(super::cannot_write).is_some(),
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

/// Asked about a blocked path, with whether anything after it could be asked
/// about too.
pub trait OnBlocked:
    Fn(Blocked, bool) -> Pin<Box<dyn Future<Output = BlockedAnswer>>> + 'static
{
}
impl<F> OnBlocked for F where
    F: Fn(Blocked, bool) -> Pin<Box<dyn Future<Output = BlockedAnswer>>> + 'static
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
            on_blocked: Box::new(|_blocked, _more| Box::pin(async { BlockedAnswer::Cancel })),
            op_sel: OperationSelection::default(),
            replace_result_opt: None,
            remaining_conflicts: 0,
            skip_blocked: false,
            copy_instead: false,
            done: Vec::new(),
            landed: std::collections::HashMap::new(),
        }
    }

    /// The controller this context runs under.
    pub fn controller(&self) -> Controller {
        self.controller.clone()
    }

    /// Plan `from_parent` to `to_parent` on a blocking worker; see
    /// [`plan_tree`] for why.
    async fn plan(
        &self,
        from_parent: &Path,
        to_parent: &Path,
        method: Method,
    ) -> Result<Plan, OperationError> {
        let from_parent = from_parent.to_path_buf();
        let to_parent = to_parent.to_path_buf();
        let controller = self.controller.clone();
        compio::runtime::spawn_blocking(move || {
            plan_tree(&from_parent, &to_parent, method, &controller)
        })
        .await
        .map_err(super::wrap_compio_spawn_error)?
    }

    /// Ask what to do about `blocked`, unless the user already chose to skip
    /// every blocked path. `more`: whether anything after it could be asked
    /// about too.
    async fn blocked(&mut self, blocked: Blocked, more: bool) -> BlockedAnswer {
        if self.skip_blocked {
            return BlockedAnswer::Skip(false);
        }
        let answer = (self.on_blocked)(blocked, more).await;
        if answer == BlockedAnswer::Skip(true) {
            self.skip_blocked = true;
        }
        answer
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
            let result = if step.is_cleanup {
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
                keeps_original: true,
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
        // Every item is planned before anything is asked, so each question
        // knows whether another like it is still to come: "Same for the
        // rest" is offered only then.
        let mut items = Vec::new();
        for (from_parent, to_parent) in from_to_pairs {
            self.controller
                .check()
                .await
                .map_err(|s| OperationError::from_state(s, &self.controller))?;

            if from_parent == to_parent {
                // Skip matching source and destination
                continue;
            }

            // Walked on a blocking worker. Reading a whole tree and stat-ing
            // every entry is blocking work, and this runs on the one thread
            // every file operation shares: done here, a large or slow tree
            // stops every other copy, move and delete until it is finished.
            //
            // What comes back is plain data. The `Rc<Skip>` each op shares
            // with its cleanup op belongs to this thread and cannot be made on
            // another, so it is put on afterwards, below.
            let (planned, unlisted) = self.plan(&from_parent, &to_parent, method).await?;
            items.push(Item {
                from_parent,
                to_parent,
                planned,
                unlisted: unlisted.into(),
                keeps_original: false,
                skipped: false,
            });
        }

        // Folders the walk could not list, asked about one at a time. A
        // retry walks that folder alone again, and what it could still not
        // list is asked about next.
        for index in 0..items.len() {
            while let Some(dir) = items[index].unlisted.pop_front() {
                let more = items[index..].iter().any(|item| !item.unlisted.is_empty());
                match self.blocked(Blocked::List(dir.clone()), more).await {
                    BlockedAnswer::Retry => {
                        let item = &items[index];
                        let to_dir = match dir.strip_prefix(&item.from_parent) {
                            Ok(relative) if !relative.as_os_str().is_empty() => {
                                item.to_parent.join(relative)
                            }
                            _ => item.to_parent.clone(),
                        };
                        let (more, still) = self.plan(&dir, &to_dir, method).await?;
                        let item = &mut items[index];
                        item.planned.extend(more);
                        for dir in still.into_iter().rev() {
                            item.unlisted.push_front(dir);
                        }
                    }
                    // Copying instead answers only a move's question
                    BlockedAnswer::Skip(_) | BlockedAnswer::CopyInstead(_) => {
                        self.op_sel.skipped.push(dir);
                    }
                    BlockedAnswer::Cancel => return Err(self.cancelled()),
                }
            }
        }

        // Before anything of a moved item is touched: could its originals be
        // removed once they are copied? If not, it is not moved by default;
        // the user decides whether to copy it instead. Every item is checked
        // first, so the question knows whether another one follows.
        if matches!(
            method,
            Method::Move {
                cross_device_copy: false
            }
        ) {
            let froms: Vec<Vec<PathBuf>> = items
                .iter()
                .map(|item| item.planned.iter().map(|op| op.from.clone()).collect())
                .collect();
            let mut blocks: Vec<Option<(PathBuf, bool)>> =
                compio::runtime::spawn_blocking(move || {
                    froms.iter().map(|froms| removal_block(froms)).collect()
                })
                .await
                .map_err(super::wrap_compio_spawn_error)?;
            for index in 0..items.len() {
                while let Some((folder, read_only)) = blocks[index].take() {
                    let answer = if self.copy_instead {
                        BlockedAnswer::CopyInstead(false)
                    } else {
                        let blocked = Blocked::Move {
                            path: items[index].from_parent.clone(),
                            folder,
                            read_only,
                        };
                        let more = blocks[index + 1..].iter().any(Option::is_some);
                        self.blocked(blocked, more).await
                    };
                    match answer {
                        // Not offered; asking again finds out afresh
                        BlockedAnswer::Retry => {
                            let froms: Vec<PathBuf> = items[index]
                                .planned
                                .iter()
                                .map(|op| op.from.clone())
                                .collect();
                            blocks[index] =
                                compio::runtime::spawn_blocking(move || removal_block(&froms))
                                    .await
                                    .map_err(super::wrap_compio_spawn_error)?;
                        }
                        BlockedAnswer::CopyInstead(all) => {
                            self.copy_instead |= all;
                            items[index].keeps_original = true;
                        }
                        BlockedAnswer::Skip(_) => {
                            items[index].skipped = true;
                            self.op_sel.skipped.push(items[index].from_parent.clone());
                        }
                        BlockedAnswer::Cancel => return Err(self.cancelled()),
                    }
                }
            }
        }

        for item in items {
            if item.skipped {
                continue;
            }
            let keeps_original = item.keeps_original;
            for PlannedOp { kind, from, to } in item.planned {
                // Copied instead of moved: a plain copy, never a hard link
                // that would leave the two sharing their contents
                let kind = match kind {
                    OpKind::Move { .. } if keeps_original => OpKind::Copy,
                    kind => kind,
                };
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
                    keeps_original,
                };
                if matches!(method, Method::Move { .. })
                    && !keeps_original
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

        // Count potential conflicts (files that would need replacement).
        // Another stat per op, and for the same reason as the walk it does not
        // belong on this thread.
        self.remaining_conflicts = {
            let conflict_candidates: Vec<PathBuf> = ops
                .iter()
                .filter(|op| {
                    matches!(
                        op.kind,
                        OpKind::Copy | OpKind::Move { .. } | OpKind::Symlink { .. }
                    )
                })
                .map(|op| op.to.clone())
                .collect();
            compio::runtime::spawn_blocking(move || {
                conflict_candidates
                    .into_iter()
                    .filter(|to| to.is_file())
                    .count()
            })
            .await
            .map_err(super::wrap_compio_spawn_error)?
        };

        let total_ops = ops.len();
        // What each step could be blocked on, for a question to look ahead
        // at whether another like it is still to come
        let ahead: Vec<(Option<Ahead>, Rc<Skip>, bool)> = ops
            .iter()
            .map(|op| {
                let ahead = match op.kind {
                    OpKind::Copy | OpKind::Move { .. } => Some(Ahead::Read(op.from.clone())),
                    OpKind::Symlink { .. } => Some(Ahead::Link(op.to.clone())),
                    OpKind::Remove | OpKind::Rmdir => Some(Ahead::Remove(op.from.clone())),
                    OpKind::Mkdir => None,
                };
                (ahead, op.skipped.clone(), op.is_cleanup)
            })
            .collect();
        let ahead = Rc::new(ahead);
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
                // Whether a later step would be blocked too: not this one's
                // own cleanup, nor any step of something already left out
                let later: Vec<&Ahead> = ahead[current_ops + 1..]
                    .iter()
                    .filter(|(_, skipped, is_cleanup)| {
                        !Rc::ptr_eq(skipped, &op.skipped)
                            && !skipped.normal.get()
                            && !(*is_cleanup && skipped.cleanup.get())
                    })
                    .filter_map(|(ahead, ..)| ahead.as_ref())
                    .collect();
                let later: Vec<Ahead> = later
                    .into_iter()
                    .map(|ahead| match ahead {
                        Ahead::Read(path) => Ahead::Read(path.clone()),
                        Ahead::Link(path) => Ahead::Link(path.clone()),
                        Ahead::Remove(path) => Ahead::Remove(path.clone()),
                    })
                    .collect();
                let more =
                    compio::runtime::spawn_blocking(move || later.iter().any(Ahead::would_block))
                        .await
                        .map_err(super::wrap_compio_spawn_error)
                        .unwrap_or(true);
                match self.blocked(blocked.clone(), more).await {
                    BlockedAnswer::Retry => {}
                    BlockedAnswer::Skip(_) | BlockedAnswer::CopyInstead(_) => {
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
                    });
                }
                // What an undo has to put back, paired with where it came from.
                // The pairing cannot be recovered afterwards: a Keep Both
                // conflict gives the destination a different name. A directory
                // that was already there is left out, because a merge into it
                // transfers only its children and carrying the whole folder
                // back would take what it already held with it.
                if !op.is_cleanup
                    && !op.keeps_original
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

        let replace_result = match self.replace_result_opt {
            Some(result) => result,
            None => (self.on_replace)(op, self.remaining_conflicts).await,
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
    /// Part of an item copied instead of moved, because its originals could
    /// not be removed: what it makes is a copy, not a move.
    pub keeps_original: bool,
}

impl Op {
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
            keeps_original: self.keeps_original,
        })
    }

    async fn run(&mut self, ctx: &mut Context, progress: Progress) -> Result<bool, Box<dyn Error>> {
        if self.skipped.normal.get() || (self.is_cleanup && self.skipped.cleanup.get()) {
            return Ok(true);
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
                                keeps_original: self.keeps_original,
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
    use super::{Context, Done, Method, OpKind, PlannedOp, plan_tree};
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

    #[test]
    fn a_selected_link_to_a_file_is_planned_as_a_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("target.txt"), b"t").expect("write");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink("target.txt", &link).expect("symlink");

        let planned = plan_tree(
            &link,
            &dir.path().join("out"),
            Method::Copy,
            &Controller::default(),
        )
        .expect("planning")
        .0;

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

        let (planned, unlisted) =
            plan_tree(&from, &to, Method::Copy, &Controller::default()).expect("planning");
        assert!(unlisted.is_empty());

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

        let result = plan_tree(&from, &dir.path().join("to"), Method::Copy, &controller);

        assert!(
            result.is_err(),
            "a cancelled plan returned a list of work to do"
        );
    }
}
