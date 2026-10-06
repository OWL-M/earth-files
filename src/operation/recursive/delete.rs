//! Deleting: to the trash, or for good, and removing from the trash.
//!
//! Like a copy, a delete is checked whole before anything goes, and what it
//! cannot do is asked about one question at a time: see
//! [`Context::delete`].

use super::{Context, removal_block};
use crate::fl;
use crate::operation::root::proto::{Kind, Request};
use crate::operation::root::{self, Helper, StartError};
use crate::operation::{Ask, Blocked, BlockedAnswer, OperationError};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io;
use std::mem::Discriminant;
use std::path::{Path, PathBuf};

/// Where an item sent to the trash ends up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Place {
    /// Renamed into a trash on its own filesystem: the home trash, or the
    /// drive's own.
    Here,
    /// Copied into the home trash, on another drive: it takes room there.
    Home,
    /// Nowhere: a network or FUSE drive with no trash of its own.
    Nowhere,
}

/// The home trash, as the trash crate finds it.
fn home_trash() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))?;
    Some(data.join("Trash"))
}

/// The nearest folder of `path` that exists, `path` included.
fn existing(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|ancestor| fs::symlink_metadata(ancestor).is_ok())
}

/// The mount point `path` is under, from `/proc/self/mountinfo`.
fn mount_point(path: &Path) -> Option<PathBuf> {
    let path = existing(path)?.canonicalize().ok()?;
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").ok()?;
    mountinfo
        .lines()
        .filter_map(|line| line.split(' ').nth(4).map(unescape))
        .filter(|point| path.starts_with(point))
        .max_by_key(|point| point.as_os_str().len())
}

/// A mount point as `mountinfo` writes it, with spaces and the like as
/// octal escapes.
fn unescape(field: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;

    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && let Some(code) = bytes
                .get(i + 1..i + 4)
                .and_then(|digits| std::str::from_utf8(digits).ok())
                .and_then(|digits| u8::from_str_radix(digits, 8).ok())
        {
            out.push(code);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// Whether this user may write to `dir`.
fn writable(dir: &Path) -> bool {
    crate::operation::cannot_write(dir).is_none() && dir.is_dir()
}

/// Where `path` would go in the trash, worked out as the trash crate does
/// it: the home trash when it is on the same filesystem, else the drive's
/// own trash (`.Trash/$uid` in a shared sticky `.Trash`, or `.Trash-$uid`),
/// else the home trash after all, by copying.
///
/// Blocking: it asks the filesystem.
pub(super) fn trash_place(path: &Path) -> Place {
    use std::os::unix::fs::MetadataExt;

    let Ok(item) = fs::symlink_metadata(path) else {
        return Place::Here;
    };
    if let Some(home) = home_trash().as_deref().and_then(existing)
        && fs::metadata(home).is_ok_and(|home| home.dev() == item.dev())
    {
        return Place::Here;
    }
    let Some(top) = mount_point(path.parent().unwrap_or(path)) else {
        return Place::Here;
    };
    // SAFETY: `getuid` has no preconditions and cannot fail
    let uid = unsafe { libc::getuid() };
    let shared = top.join(".Trash");
    if fs::symlink_metadata(&shared)
        .is_ok_and(|meta| meta.is_dir() && meta.mode() & libc::S_ISVTX != 0)
    {
        let own = shared.join(uid.to_string());
        if writable(&own) || (fs::symlink_metadata(&own).is_err() && writable(&shared)) {
            return Place::Here;
        }
    }
    let own = top.join(format!(".Trash-{uid}"));
    match fs::symlink_metadata(&own) {
        Ok(meta) if meta.is_dir() && writable(&own) => return Place::Here,
        Err(_) if writable(&top) => return Place::Here,
        _ => {}
    }
    if super::check::is_remote(path.parent().unwrap_or(path)) {
        Place::Nowhere
    } else {
        Place::Home
    }
}

/// How much `path` takes: a file's size, or all a folder holds.
fn size_of(path: &Path) -> u64 {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => crate::operation::folder_totals(path).1,
        Ok(meta) => meta.len(),
        Err(_) => 0,
    }
}

/// Deletes `path` for good, a folder with everything in it. A link goes
/// itself, never what it points at.
fn remove_tree(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Deletes `path` for good through the helper, as root: a folder by
/// listing it and removing what it holds first.
fn remove_tree_as_root(helper: &mut Helper, path: &Path) -> io::Result<()> {
    match helper.call(&Request::Remove(path.to_path_buf())) {
        Err(err) if err.raw_os_error() == Some(libc::EISDIR) => {
            for entry in helper.list(path)? {
                let child = path.join(&entry.name);
                if entry.kind == Kind::Dir {
                    remove_tree_as_root(helper, &child)?;
                } else {
                    helper.call(&Request::Remove(child))?;
                }
            }
            helper.call(&Request::Rmdir(path.to_path_buf()))
        }
        result => result,
    }
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
    free_space: fn(&Path) -> Option<u64>,
) -> Result<Vec<Option<Blocked>>, String> {
    let mut problems = vec![None; paths.len()];
    let mut home = Vec::new();
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
        match place(path) {
            Place::Here => {}
            Place::Home => home.push(index),
            Place::Nowhere => problems[index] = Some(Blocked::NoTrash(path.clone())),
        }
    }
    // Copied into the home trash together: when they do not all fit, each
    // is asked about
    if !home.is_empty() {
        let needed: u64 = home.iter().map(|&index| size_of(&paths[index])).sum();
        let free = home_trash()
            .as_deref()
            .and_then(existing)
            .and_then(free_space);
        if free.is_some_and(|free| needed > free) {
            for index in home {
                problems[index] = Some(Blocked::TrashFull(paths[index].clone()));
            }
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
        self.resolve_trashed().await;
        if self.controller.is_aborted() {
            return result.map_err(|_| self.cancelled());
        }
        // Cancelled, even as the last item was going: all of it comes back
        if self.controller.is_cancelled() {
            let not_back = self.untrash().await;
            let gone = std::mem::take(&mut self.deleted);
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
            let (paths, place, free_space) = (paths.clone(), self.trash.place, self.free_space);
            compio::runtime::spawn_blocking(move || {
                check_deletes(&paths, permanently, place, free_space)
            })
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
                            permanently: matches!(
                                blocked,
                                Blocked::NoTrash(_) | Blocked::TrashFull(_)
                            ),
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
                let done: Result<(), Box<dyn Error>> = match how[index] {
                    How::Trash => self.trash_one(path).await,
                    How::Permanently => {
                        let path = path.clone();
                        compio::runtime::spawn_blocking(move || remove_tree(&path))
                            .await
                            .map_err(|_| io::Error::other("the delete's worker stopped"))
                            .and_then(|result| result)
                            .map_err(Into::into)
                    }
                    How::Root => {
                        let path = path.clone();
                        self.with_helper(move |helper| remove_tree_as_root(helper, &path))
                            .await
                            .map_err(Into::into)
                    }
                    How::Skip => Ok(()),
                };
                let Err(err) = done else {
                    if how[index] != How::Trash {
                        self.deleted.push(path.clone());
                    }
                    break;
                };
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
                    compio::runtime::spawn_blocking(move || trash::os_limited::purge_all([item]))
                        .await
                        .map_err(|_| io::Error::other("the trash's worker stopped"))
                        .and_then(|result| {
                            result.map_err(|err| match err {
                                trash::Error::FileSystem { source, .. } => source,
                                err => io::Error::other(err.to_string()),
                            })
                        })
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
            delete(&target).map(|()| target)
        })
        .await
        .map_err(|_| io::Error::other("the trash's worker stopped"))?;
        match trashed {
            Ok(original) => {
                self.replaced.push(original);
                Ok(())
            }
            Err(err) => Err(err as Box<dyn Error>),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Place, unescape};
    use crate::operation::recursive::Context;
    use crate::operation::{Ask, Blocked, BlockedAnswer, Controller};
    use std::cell::RefCell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use test_log::test;

    #[test]
    fn mount_points_are_unescaped() {
        assert_eq!(
            unescape(r"/run/media/user/My\040Stick"),
            PathBuf::from("/run/media/user/My Stick")
        );
        assert_eq!(unescape("/"), PathBuf::from("/"));
    }

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
}
