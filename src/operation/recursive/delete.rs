//! Deleting: to the trash, or for good, and removing from the trash.
//!
//! Like a copy, a delete is checked whole before anything goes, and what it
//! cannot do is asked about one question at a time: see
//! [`Context::delete`].

use super::{Context, removal_block};
use crate::fl;
use crate::operation::root::proto::{
    Kind, Request, names_in, open_dir_at, open_path, stat_at, unlink_at,
};
use crate::operation::root::{self, Helper, StartError};
use crate::operation::{Ask, Blocked, BlockedAnswer, OperationError};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io;
use std::mem::Discriminant;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Where an item sent to the trash ends up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Place {
    /// Renamed into the trash on its own drive: the home trash, or the
    /// drive's own.
    Here,
    /// Nowhere: its drive has no trash that can be used or made.
    Nowhere,
}

/// Where `path` would go in the trash: see [`crate::trashing::bin_for`].
///
/// Blocking: it asks the filesystem.
pub(super) fn trash_place(path: &Path) -> Place {
    if fs::symlink_metadata(path).is_err() || crate::trashing::bin_for(path, false).is_some() {
        Place::Here
    } else {
        Place::Nowhere
    }
}

/// What a removal that may fail part-way got done: the first path it
/// removed, and how many in all.
#[derive(Debug, Default)]
struct Gone {
    first: Option<PathBuf>,
    count: usize,
}

impl Gone {
    fn note(&mut self, path: &Path) {
        if self.first.is_none() {
            self.first = Some(path.to_path_buf());
        }
        self.count += 1;
    }
}

/// Deletes `path` for good, a folder with everything in it. A link goes
/// itself, never what it points at.
pub(crate) fn remove_tree(path: &Path) -> io::Result<()> {
    remove_tree_noting(path, &mut Gone::default())
}

/// [`remove_tree`], noting in `gone` what went, so a failure part-way still
/// says what can no longer come back.
///
/// It goes through folder handles, as `std::fs::remove_dir_all` does: a
/// folder swapped for a link while it runs is never followed, so nothing
/// outside `path` can go.
fn remove_tree_noting(path: &Path, gone: &mut Gone) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    // Only to work on `name` in it: removing an entry takes no permission
    // to list its folder
    let parent = open_path(parent)?;
    remove_at(parent.as_fd(), name, path, gone)
}

/// Removes `name` from the folder open as `dir`, everything in it first
/// when it is a folder; `shown` is its path, for what is noted in `gone`.
fn remove_at(
    dir: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::OsStr,
    shown: &Path,
    gone: &mut Gone,
) -> io::Result<()> {
    let is_dir = stat_at(dir, name)?.st_mode & libc::S_IFMT == libc::S_IFDIR;
    if is_dir {
        // Opened without following: swapped for a link since, it fails
        let folder = open_dir_at(Some(dir), name, false)?;
        for child in names_in(folder.as_fd())? {
            remove_at(folder.as_fd(), &child, &shown.join(&child), gone)?;
        }
    }
    unlink_at(dir, name, is_dir)?;
    gone.note(shown);
    Ok(())
}

/// Brings a trash entry back to where it was: renamed, or across drives
/// copied and then removed from the trash. Never over something now in its
/// place, and nothing is made there first: the trash crate's own restore
/// makes an empty file, then cannot rename across drives.
pub(super) fn restore_entry(item: trash::TrashItem) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (kept, info) = crate::operation::in_trash(&item);
    let to = item.original_path();
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    match crate::operation::rename_no_replace(&kept, &to) {
        Ok(()) => {}
        Err(err) if err.raw_os_error() == Some(libc::EXDEV) => {
            if fs::symlink_metadata(&to).is_ok() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists).into());
            }
            if let Err(err) = copy_tree(&kept, &to) {
                // Half a copy is not the original: the trash still has it.
                // Taken meanwhile, `to` is someone else's, and stays
                if err.kind() != io::ErrorKind::AlreadyExists {
                    let _ = remove_tree(&to);
                }
                return Err(err.into());
            }
            remove_tree(&kept)?;
        }
        Err(err) => return Err(err.into()),
    }
    if let Some(info) = info {
        fs::remove_file(info)?;
    }
    Ok(())
}

/// Copies `from` to a new `to`, a folder with all it holds and a link as a
/// link, keeping modes and modification times. Refuses a `to` that exists.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let meta = fs::symlink_metadata(from)?;
    if meta.is_symlink() {
        return std::os::unix::fs::symlink(fs::read_link(from)?, to);
    }
    if meta.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        fs::set_permissions(to, meta.permissions())?;
        return Ok(());
    }
    let mut source = fs::File::open(from)?;
    let mut dest = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(to)?;
    io::copy(&mut source, &mut dest)?;
    dest.set_permissions(meta.permissions())?;
    if let Ok(modified) = meta.modified() {
        dest.set_times(fs::FileTimes::new().set_modified(modified))?;
    }
    dest.sync_all()
}

/// Deletes `path` for good through the helper, as root: a folder by
/// listing it and removing what it holds first.
fn remove_tree_as_root(helper: &mut Helper, path: &Path) -> io::Result<()> {
    remove_tree_as_root_noting(helper, path, &mut Gone::default())
}

/// [`remove_tree_as_root`], noting in `gone` what went.
fn remove_tree_as_root_noting(helper: &mut Helper, path: &Path, gone: &mut Gone) -> io::Result<()> {
    match helper.call(&Request::Remove(path.to_path_buf())) {
        Err(err) if err.raw_os_error() == Some(libc::EISDIR) => {
            for entry in helper.list(path)? {
                let child = path.join(&entry.name);
                if entry.kind == Kind::Dir {
                    remove_tree_as_root_noting(helper, &child, gone)?;
                } else {
                    helper.call(&Request::Remove(child.clone()))?;
                    gone.note(&child);
                }
            }
            helper.call(&Request::Rmdir(path.to_path_buf()))?;
        }
        result => result?,
    }
    gone.note(path);
    Ok(())
}

/// How one item is deleted, once its questions are answered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum How {
    Trash,
    /// For good, as this user.
    Permanently,
    /// For good, as root.
    Root,
    /// Left alone.
    Skip,
}

/// What the checks found about the items to delete: what to ask about each,
/// or why the delete cannot start.
fn check_deletes(
    paths: &[PathBuf],
    permanently: bool,
    place: fn(&Path) -> Place,
) -> Result<Vec<Option<Blocked>>, String> {
    let mut problems = vec![None; paths.len()];
    for (index, path) in paths.iter().enumerate() {
        if let Err(err) = fs::symlink_metadata(path)
            && err.kind() == io::ErrorKind::NotFound
        {
            return Err(crate::operation::failure_text(path, &err));
        }
        match removal_block(std::slice::from_ref(path)) {
            Some((_, true)) => {
                let err = io::Error::from(io::ErrorKind::ReadOnlyFilesystem);
                return Err(crate::operation::failure_text(path, &err));
            }
            Some((_, false)) => {
                problems[index] = Some(Blocked::Delete(path.clone()));
                continue;
            }
            None => {}
        }
        if permanently {
            continue;
        }
        if place(path) == Place::Nowhere {
            problems[index] = Some(Blocked::NoTrash(path.clone()));
        }
    }
    Ok(problems)
}

impl Context {
    /// Deletes `paths`: to the trash, or for good when `permanently`.
    ///
    /// Cancelled, what reached the trash comes back, and the error names
    /// what was deleted for good. Aborted, everything stays as it is, and
    /// what reached the trash can be undone.
    pub async fn delete(
        &mut self,
        paths: Vec<PathBuf>,
        permanently: bool,
    ) -> Result<(), OperationError> {
        let result = self.delete_items(paths, permanently).await;
        if self.controller.is_aborted() {
            return result.map_err(|_| self.cancelled());
        }
        // Cancelled, even as the last item was going: all of it comes back
        if self.controller.is_cancelled() {
            let not_back = self.untrash().await;
            let gone = std::mem::take(&mut self.deleted);
            let gone_more = std::mem::take(&mut self.deleted_more);
            let named = |paths: &[PathBuf]| {
                let name = paths[0].file_name().map_or_else(
                    || paths[0].display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                (name, paths.len() - 1)
            };
            if !not_back.is_empty() {
                let (name, more) = named(&not_back);
                return Err(OperationError::from_err(
                    fl!("rollback-failed", name = name, more = more),
                    &self.controller,
                ));
            }
            if !gone.is_empty() {
                let (name, more) = named(&gone);
                let more = more + gone_more;
                return Err(OperationError::from_err(
                    fl!("deleted-for-good", name = name, more = more),
                    &self.controller,
                ));
            }
            return Err(OperationError::from_state(
                crate::operation::ControllerState::Cancelled,
                &self.controller,
            ));
        }
        let Err(err) = result else {
            return Ok(());
        };
        // What reached the trash before the failure can be undone, and a
        // retry has only the rest to do
        Err(OperationError {
            partial: Box::new(self.op_sel.clone()),
            ..err
        })
    }

    async fn delete_items(
        &mut self,
        paths: Vec<PathBuf>,
        permanently: bool,
    ) -> Result<(), OperationError> {
        // What the helper may touch, should root be granted
        self.scope = root::Scope {
            sources: paths.clone(),
            destinations: Vec::new(),
        };
        let checked = {
            let (paths, place) = (paths.clone(), self.trash.place);
            compio::runtime::spawn_blocking(move || check_deletes(&paths, permanently, place))
                .await
                .map_err(crate::operation::wrap_compio_spawn_error)?
        };
        let problems =
            checked.map_err(|reason| OperationError::from_err(reason, &self.controller))?;

        // The questions, one at a time, in order
        let mut how = vec![
            if permanently {
                How::Permanently
            } else {
                How::Trash
            };
            paths.len()
        ];
        let mut rest: HashMap<Discriminant<Blocked>, BlockedAnswer> = HashMap::new();
        for (index, problem) in problems.iter().enumerate() {
            let Some(blocked) = problem else {
                continue;
            };
            let kind = std::mem::discriminant(blocked);
            let count = problems[index..]
                .iter()
                .flatten()
                .filter(|other| std::mem::discriminant(*other) == kind)
                .count();
            let mut not_granted = false;
            loop {
                let answer = match rest.get(&kind) {
                    Some(answer) => *answer,
                    None => {
                        let ask = Ask {
                            count,
                            root: blocked.root_can_help().then_some(self.helper.is_some()),
                            not_granted,
                            skip: true,
                            permanently: matches!(blocked, Blocked::NoTrash(_)),
                            ..Ask::default()
                        };
                        (self.on_blocked)(blocked.clone(), ask).await
                    }
                };
                match answer {
                    BlockedAnswer::Skip(all) => {
                        if all {
                            rest.insert(kind, BlockedAnswer::Skip(false));
                        }
                        how[index] = How::Skip;
                        self.op_sel.skipped.push(paths[index].clone());
                    }
                    BlockedAnswer::Permanently(all) => {
                        if all {
                            rest.insert(kind, BlockedAnswer::Permanently(false));
                        }
                        how[index] = How::Permanently;
                    }
                    BlockedAnswer::RetryAsRoot(all) => {
                        match self.ensure_helper().await {
                            Ok(()) => {}
                            Err(StartError::NotGranted) => {
                                not_granted = true;
                                continue;
                            }
                            Err(StartError::Failed(err)) => {
                                return Err(OperationError::from_err(
                                    crate::operation::failure_text(blocked.path(), &err),
                                    &self.controller,
                                ));
                            }
                        }
                        if all {
                            rest.insert(kind, BlockedAnswer::RetryAsRoot(false));
                        }
                        how[index] = How::Root;
                    }
                    // Nothing is done yet: stopping is cancelling
                    BlockedAnswer::Cancel | BlockedAnswer::Abort => {
                        return Err(self.cancelled());
                    }
                    // Not offered before anything runs
                    BlockedAnswer::Retry => {}
                }
                break;
            }
        }

        let total = paths.len();
        for (index, path) in paths.iter().enumerate() {
            self.controller
                .check()
                .await
                .map_err(|s| OperationError::from_state(s, &self.controller))?;
            self.controller.set_progress(index as f32 / total as f32);
            if how[index] == How::Skip {
                continue;
            }
            // A failure asks, and goes again for as long as the answer is
            // Try again
            loop {
                // For good, what went is noted even when the rest does not go
                let (done, gone): (Result<(), Box<dyn Error>>, Gone) = match how[index] {
                    How::Trash => (self.trash_one(path).await, Gone::default()),
                    How::Permanently => {
                        let path = path.clone();
                        compio::runtime::spawn_blocking(move || {
                            let mut gone = Gone::default();
                            let done = remove_tree_noting(&path, &mut gone);
                            (done, gone)
                        })
                        .await
                        .map(|(done, gone)| (done.map_err(Into::into), gone))
                        .unwrap_or_else(|_| {
                            let err = io::Error::other("the delete's worker stopped");
                            (Err(err.into()), Gone::default())
                        })
                    }
                    How::Root => {
                        let path = path.clone();
                        let gone = Arc::new(Mutex::new(Gone::default()));
                        let noted = gone.clone();
                        let done = self
                            .with_helper(move |helper| {
                                let mut noted = noted.lock().unwrap_or_else(|p| p.into_inner());
                                remove_tree_as_root_noting(helper, &path, &mut noted)
                            })
                            .await
                            .map_err(Into::into);
                        let gone =
                            std::mem::take(&mut *gone.lock().unwrap_or_else(|p| p.into_inner()));
                        (done, gone)
                    }
                    How::Skip => (Ok(()), Gone::default()),
                };
                let Err(err) = done else {
                    if how[index] != How::Trash {
                        self.deleted.push(path.clone());
                    }
                    break;
                };
                // Part of it went before the failure: named by the first
                if let Some(first) = gone.first {
                    self.deleted.push(first);
                    self.deleted_more += gone.count - 1;
                }
                let reason = crate::operation::failure_text(path, &*err);
                log::warn!("failed to delete {}: {err}", path.display());
                // Stopped by a cancel, it is a cancel, which puts back
                if self.controller.is_cancelled() {
                    return Err(OperationError::from_state(
                        crate::operation::ControllerState::Cancelled,
                        &self.controller,
                    ));
                }
                if self.controller.is_failed() {
                    return Err(OperationError::from_err(reason, &self.controller));
                }
                let failed = Blocked::Failed {
                    path: path.clone(),
                    reason: reason.clone(),
                };
                let ask = Ask {
                    count: 1,
                    retry: true,
                    abort: true,
                    ..Ask::default()
                };
                match (self.on_blocked)(failed, ask).await {
                    BlockedAnswer::Retry => {}
                    BlockedAnswer::Abort => {
                        self.controller.abort();
                        return Err(self.cancelled());
                    }
                    BlockedAnswer::Cancel => return Err(self.cancelled()),
                    _ => return Err(OperationError::from_err(reason, &self.controller)),
                }
            }
        }
        self.controller.set_progress(1.0);
        Ok(())
    }

    /// Removes trashed items for good: `(item, where the trash keeps it,
    /// its .trashinfo)`. One this user may not remove asks "Delete
    /// permanently as root" / Skip / Cancel; any other failure asks Cancel /
    /// Abort / Try again. Nothing removed comes back.
    pub async fn purge(
        &mut self,
        items: Vec<(trash::TrashItem, PathBuf, Option<PathBuf>)>,
    ) -> Result<(), OperationError> {
        self.scope = root::Scope {
            sources: items
                .iter()
                .flat_map(|(_, files, info)| std::iter::once(files.clone()).chain(info.clone()))
                .collect(),
            destinations: Vec::new(),
        };
        let mut rest: Option<BlockedAnswer> = None;
        let total = items.len();
        for (index, (item, files, info)) in items.into_iter().enumerate() {
            self.controller
                .check()
                .await
                .map_err(|s| OperationError::from_state(s, &self.controller))?;
            self.controller.set_progress(index as f32 / total as f32);
            let mut not_granted = false;
            let mut as_root = false;
            loop {
                let done: io::Result<()> = if as_root {
                    let (files, info) = (files.clone(), info.clone());
                    self.with_helper(move |helper| {
                        match remove_tree_as_root(helper, &files) {
                            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
                            _ => {}
                        }
                        match info {
                            Some(info) => helper.call(&Request::Remove(info)),
                            None => Ok(()),
                        }
                    })
                    .await
                } else {
                    let item = item.clone();
                    compio::runtime::spawn_blocking(move || crate::trashing::purge(&item))
                        .await
                        .map_err(|_| io::Error::other("the trash's worker stopped"))
                        .and_then(|result| result)
                };
                let Err(err) = done else {
                    break;
                };
                log::warn!("failed to remove {} from the trash: {err}", files.display());
                if self.controller.is_cancelled() {
                    return Err(OperationError::from_state(
                        crate::operation::ControllerState::Cancelled,
                        &self.controller,
                    ));
                }
                if self.controller.is_failed() {
                    return Err(OperationError::from_err(
                        crate::operation::failure_text(&files, &err),
                        &self.controller,
                    ));
                }
                // This user may not: root may
                if err.kind() == io::ErrorKind::PermissionDenied && !as_root {
                    let blocked = Blocked::Delete(files.clone());
                    let answer = match rest {
                        Some(answer) => answer,
                        None => {
                            let ask = Ask {
                                count: total - index,
                                root: Some(self.helper.is_some()),
                                not_granted,
                                skip: true,
                                ..Ask::default()
                            };
                            (self.on_blocked)(blocked.clone(), ask).await
                        }
                    };
                    match answer {
                        BlockedAnswer::Skip(all) => {
                            if all {
                                rest = Some(BlockedAnswer::Skip(false));
                            }
                            break;
                        }
                        BlockedAnswer::RetryAsRoot(all) => match self.ensure_helper().await {
                            Ok(()) => {
                                if all {
                                    rest = Some(BlockedAnswer::RetryAsRoot(false));
                                }
                                as_root = true;
                            }
                            Err(StartError::NotGranted) => not_granted = true,
                            Err(StartError::Failed(err)) => {
                                return Err(OperationError::from_err(
                                    crate::operation::failure_text(&files, &err),
                                    &self.controller,
                                ));
                            }
                        },
                        _ => return Err(self.cancelled()),
                    }
                    continue;
                }
                let failed = Blocked::Failed {
                    path: files.clone(),
                    reason: crate::operation::failure_text(&files, &err),
                };
                let ask = Ask {
                    count: 1,
                    retry: true,
                    abort: true,
                    ..Ask::default()
                };
                match (self.on_blocked)(failed, ask).await {
                    BlockedAnswer::Retry => {}
                    BlockedAnswer::Abort => {
                        self.controller.abort();
                        return Err(self.cancelled());
                    }
                    _ => return Err(self.cancelled()),
                }
            }
        }
        self.controller.set_progress(1.0);
        Ok(())
    }

    /// Sends `path` to the trash, noting it for undo.
    async fn trash_one(&mut self, path: &Path) -> Result<(), Box<dyn Error>> {
        let (target, delete) = (path.to_path_buf(), self.trash.delete);
        let trashed = compio::runtime::spawn_blocking(move || {
            // The trash names an entry by its folder's real path
            let target = target
                .parent()
                .and_then(|parent| parent.canonicalize().ok())
                .zip(target.file_name())
                .map_or(target.clone(), |(parent, name)| parent.join(name));
            delete(&target)
        })
        .await
        .map_err(|_| io::Error::other("the trash's worker stopped"))?;
        match trashed {
            Ok(item) => {
                self.op_sel.trash_items.push(item);
                Ok(())
            }
            Err(err) => Err(err as Box<dyn Error>),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Place;
    use crate::operation::recursive::Context;
    use crate::operation::{Ask, Blocked, BlockedAnswer, Controller};
    use std::cell::RefCell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use test_log::test;

    /// A local folder always has somewhere in a trash to go: its drive's
    /// own, or the home trash
    #[test]
    fn a_local_item_has_a_trash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        fs::write(&a, "a").expect("write");
        let place = crate::operation::recursive::REAL_TRASH.place;
        assert_ne!(place(&a), Place::Nowhere);
    }

    /// A delete whose questions are answered by `answer`; returns what was
    /// asked and the result.
    async fn delete_with(
        ctx: Context,
        paths: Vec<PathBuf>,
        permanently: bool,
        answer: fn(&Blocked) -> BlockedAnswer,
    ) -> (
        Vec<(Blocked, Ask)>,
        Result<(), crate::operation::OperationError>,
        Context,
    ) {
        let asked: Rc<RefCell<Vec<(Blocked, Ask)>>> = Rc::default();
        let seen = asked.clone();
        let mut ctx = ctx.on_blocked(move |blocked, ask| {
            let reply = answer(&blocked);
            seen.borrow_mut().push((blocked, ask));
            Box::pin(async move { reply })
        });
        let result = ctx.delete(paths, permanently).await;
        let asked = asked.borrow().clone();
        (asked, result, ctx)
    }

    #[test(compio::test)]
    async fn trashed_items_are_recorded_for_undo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").expect("write");
        fs::create_dir(&b).expect("mkdir");
        fs::write(b.join("inner"), "inner").expect("write");

        let (asked, result, ctx) = delete_with(
            Context::new(Controller::default()),
            vec![a.clone(), b.clone()],
            false,
            |_| BlockedAnswer::Cancel,
        )
        .await;
        result.expect("deleted");
        assert!(asked.is_empty());
        assert!(!a.exists() && !b.exists());
        let trashed: Vec<_> = ctx
            .op_sel
            .trash_items
            .iter()
            .map(trash::TrashItem::original_path)
            .collect();
        assert_eq!(trashed, vec![a, b]);
    }

    #[test(compio::test)]
    async fn an_item_already_gone_fails_before_anything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        fs::write(&a, "a").expect("write");
        let (_, result, _) = delete_with(
            Context::new(Controller::default()),
            vec![a.clone(), dir.path().join("gone")],
            false,
            |_| BlockedAnswer::Cancel,
        )
        .await;
        result.expect_err("one is gone");
        assert!(a.exists(), "nothing was deleted");
    }

    /// No trash on the drive: Delete permanently, with "Same for the rest"
    /// covering the next one
    #[test(compio::test)]
    async fn without_a_trash_it_asks_and_deletes_for_good() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").expect("write");
        fs::write(&b, "b").expect("write");
        let mut ctx = Context::new(Controller::default());
        ctx.trash.place = |_: &Path| Place::Nowhere;

        let (asked, result, ctx) = delete_with(ctx, vec![a.clone(), b.clone()], false, |_| {
            BlockedAnswer::Permanently(true)
        })
        .await;
        result.expect("deleted");
        assert_eq!(asked.len(), 1, "the rest goes the same way");
        assert!(matches!(asked[0].0, Blocked::NoTrash(_)));
        assert!(asked[0].1.permanently && asked[0].1.count == 2);
        assert!(asked[0].1.root.is_none());
        assert!(!a.exists() && !b.exists());
        assert!(ctx.op_sel.trash_items.is_empty(), "nothing to undo");
    }

    #[test(compio::test)]
    async fn a_skipped_item_stays() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        fs::write(&a, "a").expect("write");
        let mut ctx = Context::new(Controller::default());
        ctx.trash.place = |_: &Path| Place::Nowhere;
        let (_, result, _) =
            delete_with(ctx, vec![a.clone()], false, |_| BlockedAnswer::Skip(false)).await;
        result.expect("done");
        assert!(a.exists());
    }

    /// A trash entry on another drive comes back whole, its listing gone;
    /// one whose place is taken stays in the trash, nothing made over it
    #[test]
    fn a_trash_entry_comes_back_across_drives() {
        let Ok(trash) = tempfile::tempdir_in("/dev/shm") else {
            return;
        };
        let home = tempfile::tempdir().expect("tempdir");
        let kept = trash.path().join("files/d");
        fs::create_dir_all(kept.join("sub")).expect("mkdir");
        fs::write(kept.join("sub/f"), b"F").expect("write");
        std::os::unix::fs::symlink("sub/f", kept.join("l")).expect("link");
        fs::create_dir(trash.path().join("info")).expect("mkdir");
        let info = trash.path().join("info/d.trashinfo");
        fs::write(&info, b"[Trash Info]").expect("write");
        let item = |parent: &Path| trash::TrashItem {
            id: info.clone().into(),
            name: "d".into(),
            original_parent: parent.to_path_buf(),
            time_deleted: 1,
        };

        // Taken: it stays in the trash
        let taken = home.path().join("taken");
        fs::create_dir_all(taken.join("d")).expect("mkdir");
        super::restore_entry(item(&taken)).expect_err("its place is taken");
        assert!(kept.exists() && info.exists());
        assert!(fs::read_dir(taken.join("d")).expect("d").next().is_none());

        // Free, in a folder gone since: made again, and it comes back
        let gone = home.path().join("gone/deeper");
        super::restore_entry(item(&gone)).expect("restored");
        assert_eq!(fs::read(gone.join("d/sub/f")).expect("f"), b"F");
        assert_eq!(
            fs::read_link(gone.join("d/l")).expect("a link"),
            Path::new("sub/f")
        );
        assert!(!kept.exists() && !info.exists());
    }

    /// Deleting for good goes through folder handles: a link inside goes
    /// itself, never what it points at, and a folder handle opened without
    /// following refuses a link, as one swapped in mid-way would be
    #[test]
    fn deleting_for_good_never_follows_a_link() {
        use crate::operation::root::proto::open_dir_at;
        use std::os::fd::AsFd;
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).expect("mkdir");
        fs::write(outside.join("keep"), "keep").expect("write");
        let top = dir.path().join("top");
        fs::create_dir_all(top.join("sub")).expect("mkdir");
        fs::write(top.join("sub/f"), "f").expect("write");
        std::os::unix::fs::symlink(&outside, top.join("sub/link")).expect("link");

        let parent = open_dir_at(None, dir.path().as_os_str(), true).expect("parent");
        std::os::unix::fs::symlink(&outside, dir.path().join("swapped")).expect("link");
        let err = open_dir_at(Some(parent.as_fd()), "swapped".as_ref(), false)
            .expect_err("a link is not opened as a folder");
        assert!(matches!(
            err.raw_os_error(),
            Some(libc::ELOOP | libc::ENOTDIR)
        ));

        let mut gone = super::Gone::default();
        super::remove_tree_noting(&top, &mut gone).expect("removed");
        assert!(!top.exists());
        assert_eq!(gone.count, 4, "sub/f, sub/link, sub and top");
        assert_eq!(fs::read(outside.join("keep")).expect("kept"), b"keep");
    }

    /// Removing an entry takes writing to its folder, not reading it: a
    /// folder this user may not list still gives up what is named in it
    #[test]
    fn deleting_takes_no_reading_of_the_folder_it_is_in() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let folder = dir.path().join("unlisted");
        fs::create_dir_all(folder.join("sub")).expect("mkdir");
        fs::write(folder.join("f"), "f").expect("write");
        fs::write(folder.join("sub/g"), "g").expect("write");
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o300)).expect("chmod");
        let listed = fs::read_dir(&folder).is_ok();

        let file = super::remove_tree(&folder.join("f"));
        let sub = super::remove_tree(&folder.join("sub"));
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).expect("chmod");
        if listed {
            // Running with privileges that ignore the mode
            return;
        }
        file.expect("a file in it goes");
        sub.expect("a folder in it goes, with what it holds");
        assert!(fs::read_dir(&folder).expect("listed").next().is_none());
    }

    /// A folder deleted for good that fails part-way still names what went
    /// when the delete is cancelled
    #[test(compio::test)]
    async fn a_cancel_names_what_went_of_a_folder_that_failed_part_way() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let top = dir.path().join("top");
        // `f` can go; then `inner` cannot leave `locked`
        let locked = top.join("locked");
        fs::create_dir_all(locked.join("inner")).expect("mkdir");
        fs::write(locked.join("inner/f"), "f").expect("write");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).expect("chmod");
        if crate::operation::cannot_write(&locked) != Some(false) {
            return;
        }

        let (asked, result, _) = delete_with(
            Context::new(Controller::default()),
            vec![top.clone()],
            true,
            |_| BlockedAnswer::Cancel,
        )
        .await;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod");

        let err = result.expect_err("cancelled");
        assert!(matches!(asked[0].0, Blocked::Failed { .. }));
        assert!(!locked.join("inner/f").exists());
        assert_eq!(
            err.to_string(),
            crate::fl!("deleted-for-good", name = "f", more = 0)
        );
    }

    /// The controller the next test's trash cancels, as the user would
    /// while the last item goes
    static CANCEL_DURING: std::sync::Mutex<Option<Controller>> = std::sync::Mutex::new(None);

    /// Cancelled while the last item goes, the delete still brings
    /// everything back
    #[test(compio::test)]
    async fn a_cancel_during_the_last_item_brings_it_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        fs::write(&a, "a").expect("write");
        let controller = Controller::default();
        *CANCEL_DURING.lock().expect("lock") = Some(controller.clone());
        let mut ctx = Context::new(controller);
        ctx.trash.delete = |path: &Path| {
            let done = (crate::operation::recursive::TRASH.delete)(path);
            if let Some(controller) = CANCEL_DURING.lock().expect("lock").as_ref() {
                controller.cancel();
            }
            done
        };
        let (_, result, ctx) =
            delete_with(ctx, vec![a.clone()], false, |_| BlockedAnswer::Cancel).await;
        result.expect_err("cancelled");
        assert!(a.exists(), "it came back from the trash");
        assert!(ctx.op_sel.trash_items.is_empty());
    }

    /// Cancelled mid-way, what reached the trash comes back, and what went
    /// for good is named
    #[test(compio::test)]
    async fn a_cancel_midway_brings_back_what_was_trashed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        for path in [&a, &b, &c] {
            fs::write(path, "x").expect("write");
        }
        let mut ctx = Context::new(Controller::default());
        // `a` goes to the trash; `b` for good; `c` fails to go
        ctx.trash.place = |path: &Path| {
            if path.ends_with("b") {
                Place::Nowhere
            } else {
                Place::Here
            }
        };
        ctx.trash.delete = |path: &Path| {
            if path.ends_with("c") {
                return Err(std::io::Error::other("stuck").into());
            }
            (super::super::TRASH.delete)(path)
        };
        let (asked, result, _) = delete_with(
            ctx,
            vec![a.clone(), b.clone(), c.clone()],
            false,
            |blocked| match blocked {
                Blocked::NoTrash(_) => BlockedAnswer::Permanently(false),
                _ => BlockedAnswer::Cancel,
            },
        )
        .await;
        let err = result.expect_err("cancelled");
        assert_eq!(asked.len(), 2);
        assert!(asked[1].1.abort && asked[1].1.retry && !asked[1].1.skip);
        assert!(a.exists(), "a came back from the trash");
        assert!(!b.exists());
        assert!(c.exists());
        assert_eq!(
            err.to_string(),
            crate::fl!("deleted-for-good", name = "b", more = 0)
        );
    }

    /// What went to the trash is recorded as the trash gave it back, for undo
    #[test(compio::test)]
    async fn the_delete_records_each_trashed_item() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").expect("write");
        fs::write(&b, "b").expect("write");
        let ctx = Context::new(Controller::default());
        let (_, result, ctx) = delete_with(ctx, vec![a.clone(), b.clone()], false, |_| {
            BlockedAnswer::Cancel
        })
        .await;
        result.expect("deleted");
        let mut back: Vec<PathBuf> = ctx
            .op_sel
            .trash_items
            .iter()
            .map(trash::TrashItem::original_path)
            .collect();
        back.sort();
        assert_eq!(back, vec![a, b]);
    }

    /// An entry with no `.trashinfo` is removed from the trash like any
    #[test(compio::test)]
    async fn purging_takes_an_orphan_too() {
        let top = tempfile::tempdir().expect("tempdir");
        let bin = crate::trashing::drive_bin(top.path(), true).expect("bin");
        fs::create_dir_all(bin.root.join("files/o/sub")).expect("mkdir");
        let (orphan, _) = crate::trashing::list_bin(&bin).pop().expect("listed");
        let (files, info) = crate::operation::in_trash(&orphan);
        let mut ctx = Context::new(Controller::default());
        ctx.purge(vec![(orphan, files, info)])
            .await
            .expect("purged");
        assert_eq!(fs::read_dir(bin.root.join("files")).expect("ls").count(), 0);
    }
}
