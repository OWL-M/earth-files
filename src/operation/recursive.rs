// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use super::{Controller, OperationSelection, ReplaceResult, copy_unique_path};
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

/// Work out every step of copying or moving `from_parent` to `to_parent`.
///
/// Blocking by nature: it reads the whole tree and stats every entry. Run it
/// on a worker, never on the thread the operations themselves run on.
fn plan_tree(
    from_parent: &Path,
    to_parent: &Path,
    method: Method,
    controller: &Controller,
) -> Result<Vec<PlannedOp>, OperationError> {
    let mut planned = Vec::new();
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
        return Ok(planned);
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

        let entry = entry.map_err(|err| {
            OperationError::from_err(
                format!(
                    "failed to walk directory {}: {}",
                    from_parent.display(),
                    err
                ),
                controller,
            )
        })?;
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
    Ok(planned)
}

/// How long a plan that finds its controller paused waits before looking
/// again, mirroring what `Controller::check` does where it can be awaited.
const PLAN_PAUSE_POLL: std::time::Duration = std::time::Duration::from_millis(50);

pub struct Context {
    buf: Vec<u8>,
    controller: Controller,
    on_progress: Box<dyn OnProgress>,
    on_replace: Pin<Box<dyn OnReplace>>,
    pub(crate) op_sel: OperationSelection,
    replace_result_opt: Option<ReplaceResult>,
    remaining_conflicts: usize,
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

impl Context {
    pub fn new(controller: Controller) -> Self {
        Self {
            // 128K is the optimal upper size of a buffer.
            buf: vec![0u8; 128 * 1024],
            controller,
            on_progress: Box::new(|_op, _progress| {}),
            on_replace: Box::pin(|_op, _count| Box::pin(async { ReplaceResult::Cancel })),
            op_sel: OperationSelection::default(),
            replace_result_opt: None,
            remaining_conflicts: 0,
        }
    }

    pub async fn recursive_copy_or_move(
        &mut self,
        from_to_pairs: impl IntoIterator<Item = (PathBuf, PathBuf)>,
        method: Method,
    ) -> Result<bool, OperationError> {
        let mut ops = Vec::new();
        let mut cleanup_ops = Vec::new();
        let mut written_files = Vec::new();
        let mut target_dirs = std::collections::HashSet::new();
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
            let planned = {
                let from_parent = from_parent.clone();
                let to_parent = to_parent.clone();
                let controller = self.controller.clone();
                compio::runtime::spawn_blocking(move || {
                    plan_tree(&from_parent, &to_parent, method, &controller)
                })
                .await
                .map_err(super::wrap_compio_spawn_error)??
            };

            for PlannedOp { kind, from, to } in planned {
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

            self.op_sel.ignored.push(from_parent);
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
        for (current_ops, mut op) in ops.into_iter().enumerate() {
            self.controller
                .check()
                .await
                .map_err(|s| OperationError::from_state(s, &self.controller))?;

            let progress = Progress {
                current_ops,
                total_ops,
                current_bytes: 0,
                total_bytes: None,
            };
            (self.on_progress)(&op, &progress);
            // Noted before the op runs, because a Keep Both conflict rewrites
            // `op.to` to a fresh name and a Replace leaves an existing path in
            // place. Only a destination that was not already there counts as
            // created, and only created paths may be undone.
            let to_before = op.to.clone();
            let to_existed = compio::fs::symlink_metadata(&to_before).await.is_ok();
            if op.run(self, progress).await.map_err(|err| {
                OperationError::from_err(
                    format!(
                        "failed to {:?} {} to {}: {}",
                        op.kind,
                        op.from.display(),
                        op.to.display(),
                        err
                    ),
                    &self.controller,
                )
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
                // Cancelled
                return Ok(false);
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
            OpKind::Remove => {
                compio::fs::remove_file(&self.from).await?;
            }
            OpKind::Rmdir => {
                match compio::fs::remove_dir(&self.from).await {
                    Ok(()) => {}
                    // A skipped entry legitimately leaves the source directory behind
                    Err(err) if err.raw_os_error() == Some(libc::ENOTEMPTY) => {}
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
                compio::runtime::spawn_blocking(move || std::os::unix::fs::symlink(target, to))
                    .await
                    .map_err(super::wrap_compio_spawn_error)??;
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

        let from_file = from_file_open_result?;

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
    use super::{Method, OpKind, PlannedOp, plan_tree};
    use crate::operation::{Controller, ControllerState};
    use std::fs;

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
        .expect("planning");

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

        let planned =
            plan_tree(&from, &to, Method::Copy, &Controller::default()).expect("planning");

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
