// SPDX-License-Identifier: GPL-3.0-only

//! Filesystem steps that never follow a link they were not asked to, and
//! never replace what is in their way: renaming without replacing, and
//! working on a folder's entries through a handle on the folder, so one
//! swapped for a link meanwhile is refused rather than followed.
//!
//! Like the helper's protocol, this file stands alone: it uses only the
//! standard library and `libc`, so `earth-files-helper`, which runs as root,
//! includes it by path, and the app uses the very same steps.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// Rename `from` to `to`, refusing to replace anything already at `to`.
///
/// `renameat2` makes the check and the rename one step, so nothing can appear
/// at the destination in between. Kernels and filesystems that do not support
/// the flag answer `EINVAL`, `ENOSYS` or `EOPNOTSUPP`. There anything but a
/// folder is linked at `to`, which fails on a name taken, then unlinked at
/// `from`: one step again. A folder, or a filesystem without hard links,
/// falls back to looking first, which leaves a small window but still never
/// overwrites what we can see. The check is on the link itself, so a
/// dangling symlink at the destination counts as occupied.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
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
            if !fs::symlink_metadata(from)?.is_dir() {
                // A link is made without following one at `from`
                match fs::hard_link(from, to) {
                    Ok(()) => {
                        return fs::remove_file(from).inspect_err(|_| {
                            // Both names are the one file: the new goes
                            let _ = fs::remove_file(to);
                        });
                    }
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Err(err),
                    // No hard links here, or not across these folders
                    Err(_) => {}
                }
            }
            if fs::symlink_metadata(to).is_ok() {
                return Err(io::Error::from_raw_os_error(libc::EEXIST));
            }
            fs::rename(from, to)
        }
        _ => Err(err),
    }
}

/// `name` as the system takes it.
fn c_name(name: &std::ffi::OsStr) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(name.as_bytes())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))
}

/// Opens the folder `name` in `dir` (or `name` as a path, without `dir`).
/// With `follow` false a link there is refused, not followed: the handle is
/// that folder, or nothing.
pub fn open_dir_at(
    dir: Option<std::os::fd::BorrowedFd<'_>>,
    name: &std::ffi::OsStr,
    follow: bool,
) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = c_name(name)?;
    let mut flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    if !follow {
        flags |= libc::O_NOFOLLOW;
    }
    let at = dir.map_or(libc::AT_FDCWD, |dir| dir.as_raw_fd());
    // SAFETY: a valid descriptor or AT_FDCWD, and a CString that outlives
    // the call
    let fd = unsafe { libc::openat(at, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` just returned it, and nothing else owns it
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// A handle on the folder at `path` for working on what is in it by name,
/// not for listing it: it takes no permission to read the folder, only what
/// each step on an entry takes (`O_PATH`).
pub fn open_path(path: &Path) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;

    let path = c_name(path.as_os_str())?;
    // SAFETY: a CString that outlives the call
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `open` just returned it, and nothing else owns it
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// The names in the folder open as `dir`, without `.` and `..`, read
/// through that handle.
pub fn names_in(dir: std::os::fd::BorrowedFd<'_>) -> io::Result<Vec<OsString>> {
    use std::os::fd::IntoRawFd;

    // The stream takes the descriptor it is given: it gets a copy
    let copy = dir.try_clone_to_owned()?.into_raw_fd();
    // SAFETY: `copy` is a descriptor of an open folder, owned from here by
    // the stream, or closed below when there is none
    let stream = unsafe { libc::fdopendir(copy) };
    if stream.is_null() {
        let err = io::Error::last_os_error();
        // SAFETY: still ours: the stream was not made
        unsafe { libc::close(copy) };
        return Err(err);
    }
    let mut names = Vec::new();
    let result = loop {
        // SAFETY: errno is this thread's; it is cleared to tell the end of
        // the folder from an error, as `readdir` says both with null
        unsafe { *libc::__errno_location() = 0 };
        // SAFETY: `stream` is open until `closedir` below
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let err = io::Error::last_os_error();
            break if err.raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(err)
            };
        }
        // SAFETY: `readdir` gave an entry whose name ends in a nul
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(OsString::from_vec(name.to_vec()));
        }
    };
    // SAFETY: opened above, closed once, with its descriptor
    unsafe { libc::closedir(stream) };
    result.map(|()| names)
}

/// What `name` in `dir` is, not following it when it is a link.
pub fn stat_at(dir: std::os::fd::BorrowedFd<'_>, name: &std::ffi::OsStr) -> io::Result<libc::stat> {
    use std::os::fd::AsRawFd;

    let name = c_name(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: a valid descriptor, a CString that outlives the call, and room
    // for the answer
    if unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fstatat` returned 0, so it filled `stat`
    Ok(unsafe { stat.assume_init() })
}

/// Where the link `name` in `dir` points.
pub fn read_link_at(
    dir: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::OsStr,
) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;

    let name = c_name(name)?;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: a valid descriptor, a CString that outlives the call, and a
    // buffer of the length given
    let len = unsafe {
        libc::readlinkat(
            dir.as_raw_fd(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if len < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(len as usize);
    Ok(PathBuf::from(OsString::from_vec(buf)))
}

/// Removes `name` from `dir`: a folder, already empty, with `folder`.
pub fn unlink_at(
    dir: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::OsStr,
    folder: bool,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let name = c_name(name)?;
    let flags = if folder { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: a valid descriptor and a CString that outlives the call
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Whether this user clearly cannot add entries to the folder `dir` or
/// remove them from it: `Some(true)` for a read-only filesystem, `Some(false)`
/// for lack of permission, and `None` for yes or for any answer that is not
/// a clear no.
pub fn cannot_write(dir: &Path) -> Option<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir_c = CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: the pointer comes from a CString that outlives the call
    if unsafe { libc::access(dir_c.as_ptr(), libc::W_OK | libc::X_OK) } == 0 {
        return None;
    }
    match io::Error::last_os_error().kind() {
        io::ErrorKind::PermissionDenied => Some(false),
        io::ErrorKind::ReadOnlyFilesystem => Some(true),
        _ => None,
    }
}

/// What a removal that may fail part-way got done: the first path it
/// removed, and how many in all.
#[derive(Debug, Default)]
pub struct Gone {
    pub first: Option<PathBuf>,
    pub count: usize,
}

impl Gone {
    pub fn note(&mut self, path: &Path) {
        if self.first.is_none() {
            self.first = Some(path.to_path_buf());
        }
        self.count += 1;
    }
}

/// Deletes `path` for good, a folder with everything in it. A link goes
/// itself, never what it points at.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    remove_tree_noting(path, &mut Gone::default(), &|| Ok(()))
}

/// [`remove_tree`], asking `go_on` before each entry.
pub fn remove_tree_checked(path: &Path, go_on: &dyn Fn() -> io::Result<()>) -> io::Result<()> {
    remove_tree_noting(path, &mut Gone::default(), go_on)
}

/// [`remove_tree`], noting in `gone` what went, so a failure part-way still
/// says what can no longer come back.
///
/// It goes through folder handles, as `std::fs::remove_dir_all` does: a
/// folder swapped for a link while it runs is never followed, so nothing
/// outside `path` can go.
pub fn remove_tree_noting(
    path: &Path,
    gone: &mut Gone,
    go_on: &dyn Fn() -> io::Result<()>,
) -> io::Result<()> {
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
    remove_at(parent.as_fd(), name, path, gone, go_on)
}

/// Removes `name` from the folder open as `dir`, everything in it first
/// when it is a folder; `shown` is its path, for what is noted in `gone`.
fn remove_at(
    dir: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::OsStr,
    shown: &Path,
    gone: &mut Gone,
    go_on: &dyn Fn() -> io::Result<()>,
) -> io::Result<()> {
    go_on()?;
    let is_dir = stat_at(dir, name)?.st_mode & libc::S_IFMT == libc::S_IFDIR;
    if is_dir {
        // Opened without following: swapped for a link since, it fails
        let folder = open_dir_at(Some(dir), name, false)?;
        for child in names_in(folder.as_fd())? {
            remove_at(folder.as_fd(), &child, &shown.join(&child), gone, go_on)?;
        }
    }
    unlink_at(dir, name, is_dir)?;
    gone.note(shown);
    Ok(())
}
