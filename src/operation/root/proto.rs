// SPDX-License-Identifier: GPL-3.0-only

//! What passes between the app and `earth-files-helper`, the small program
//! that does an operation's steps as root, and the helper's side of it.
//!
//! This file stands alone: it uses only the standard library and `libc`, so
//! the helper binary includes it by path and nothing else of the app runs as
//! root. The app includes it too, to speak the same messages.
//!
//! Every message is a tag byte, then a count of fields, then each field as
//! its length and bytes. Paths travel as their raw bytes, so a name that is
//! not text arrives exactly as it is on disk.
//!
//! The first request names the operation's scope: the paths it copies or
//! moves from, and the folders it puts things into. The helper refuses any
//! later path outside them. A path's folder is resolved first, so `..` or a
//! link in a parent cannot reach outside; the last part of a path is never
//! followed when it is a link.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// What the app asks the helper to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// The operation's sources and the folders it writes into. The first
    /// request, and only the first.
    Scope {
        sources: Vec<PathBuf>,
        destinations: Vec<PathBuf>,
    },
    /// List a folder's entries.
    List(PathBuf),
    /// Copy a file: its bytes, then its owner, mode and times. Refuses a
    /// destination that exists.
    Copy { from: PathBuf, to: PathBuf },
    /// Make a folder with `from`'s owner and mode.
    Mkdir { from: PathBuf, to: PathBuf },
    /// Make a link at `to` pointing at `target`, owned as `from` is.
    Symlink {
        target: PathBuf,
        to: PathBuf,
        from: PathBuf,
    },
    /// Remove a file or link.
    Remove(PathBuf),
    /// Remove an empty folder.
    Rmdir(PathBuf),
    /// Rename within one filesystem.
    Rename { from: PathBuf, to: PathBuf },
    /// Stop the copy under way: it fails with `ECANCELED` and leaves
    /// nothing at its destination. Sent while no copy runs, it is answered
    /// Done.
    Stop,
}

/// One entry of a listed folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: OsString,
    pub kind: Kind,
    pub size: u64,
    /// Where a link points.
    pub target: Option<PathBuf>,
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Link,
    /// Sockets, FIFOs, devices: nothing a copy carries.
    Other,
}

/// What the helper answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Done; a copy says how many bytes, a listing gives its entries.
    Done,
    Copied(u64),
    Listed(Vec<Entry>),
    /// Bytes copied so far, sent while a copy runs; its reply follows.
    Progress(u64),
    /// It failed, with the system's error number.
    Failed(i32),
}

const SCOPE: u8 = 1;
const LIST: u8 = 2;
const COPY: u8 = 3;
const MKDIR: u8 = 4;
const SYMLINK: u8 = 5;
const REMOVE: u8 = 6;
const RMDIR: u8 = 7;
const RENAME: u8 = 8;
const STOP: u8 = 9;

const DONE: u8 = 100;
const COPIED: u8 = 101;
const LISTED: u8 = 102;
const PROGRESS: u8 = 103;
const FAILED: u8 = 104;

/// The most fields or bytes one message may claim, so a broken peer cannot
/// make the other side allocate without end.
const LIMIT: u32 = 1 << 24;

fn write_message(out: &mut impl Write, tag: u8, fields: &[&[u8]]) -> io::Result<()> {
    let mut message = Vec::new();
    message.push(tag);
    message.extend((fields.len() as u32).to_le_bytes());
    for field in fields {
        message.extend((field.len() as u32).to_le_bytes());
        message.extend(*field);
    }
    out.write_all(&message)?;
    out.flush()
}

fn read_u32(input: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    let value = u32::from_le_bytes(bytes);
    if value > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    Ok(value)
}

fn read_message(input: &mut impl Read) -> io::Result<(u8, Vec<Vec<u8>>)> {
    let mut tag = [0];
    input.read_exact(&mut tag)?;
    let count = read_u32(input)?;
    let mut fields = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let len = read_u32(input)?;
        let mut field = vec![0; len as usize];
        input.read_exact(&mut field)?;
        fields.push(field);
    }
    Ok((tag[0], fields))
}

fn path(field: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(field.to_vec()))
}

fn number(field: &[u8]) -> io::Result<u64> {
    let bytes: [u8; 8] = field
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad number"))?;
    Ok(u64::from_le_bytes(bytes))
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// Sends a request.
pub fn write_request(out: &mut impl Write, request: &Request) -> io::Result<()> {
    let b = |path: &Path| path.as_os_str().as_bytes().to_vec();
    let (tag, fields): (u8, Vec<Vec<u8>>) = match request {
        Request::Scope {
            sources,
            destinations,
        } => {
            // The two lists, an empty field between: no path is empty
            let mut fields: Vec<Vec<u8>> = sources.iter().map(|p| b(p)).collect();
            fields.push(Vec::new());
            fields.extend(destinations.iter().map(|p| b(p)));
            (SCOPE, fields)
        }
        Request::List(dir) => (LIST, vec![b(dir)]),
        Request::Copy { from, to } => (COPY, vec![b(from), b(to)]),
        Request::Mkdir { from, to } => (MKDIR, vec![b(from), b(to)]),
        Request::Symlink { target, to, from } => (SYMLINK, vec![b(target), b(to), b(from)]),
        Request::Remove(path) => (REMOVE, vec![b(path)]),
        Request::Rmdir(path) => (RMDIR, vec![b(path)]),
        Request::Rename { from, to } => (RENAME, vec![b(from), b(to)]),
        Request::Stop => (STOP, Vec::new()),
    };
    let fields: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
    write_message(out, tag, &fields)
}

/// Reads a request.
pub fn read_request(input: &mut impl Read) -> io::Result<Request> {
    let (tag, fields) = read_message(input)?;
    let one = |fields: &[Vec<u8>]| match fields {
        [a] => Ok(path(a)),
        _ => Err(bad("expected one path")),
    };
    let two = |fields: &[Vec<u8>]| match fields {
        [a, b] => Ok((path(a), path(b))),
        _ => Err(bad("expected two paths")),
    };
    Ok(match tag {
        SCOPE => {
            let split = fields
                .iter()
                .position(Vec::is_empty)
                .ok_or_else(|| bad("scope without a separator"))?;
            Request::Scope {
                sources: fields[..split].iter().map(|f| path(f)).collect(),
                destinations: fields[split + 1..].iter().map(|f| path(f)).collect(),
            }
        }
        LIST => Request::List(one(&fields)?),
        COPY => {
            let (from, to) = two(&fields)?;
            Request::Copy { from, to }
        }
        MKDIR => {
            let (from, to) = two(&fields)?;
            Request::Mkdir { from, to }
        }
        SYMLINK => match fields.as_slice() {
            [target, to, from] => Request::Symlink {
                target: path(target),
                to: path(to),
                from: path(from),
            },
            _ => return Err(bad("expected three paths")),
        },
        REMOVE => Request::Remove(one(&fields)?),
        RMDIR => Request::Rmdir(one(&fields)?),
        RENAME => {
            let (from, to) = two(&fields)?;
            Request::Rename { from, to }
        }
        STOP if fields.is_empty() => Request::Stop,
        _ => return Err(bad("unknown request")),
    })
}

/// Sends a reply.
pub fn write_reply(out: &mut impl Write, reply: &Reply) -> io::Result<()> {
    match reply {
        Reply::Done => write_message(out, DONE, &[]),
        Reply::Copied(bytes) => write_message(out, COPIED, &[&bytes.to_le_bytes()]),
        Reply::Progress(bytes) => write_message(out, PROGRESS, &[&bytes.to_le_bytes()]),
        Reply::Failed(errno) => write_message(out, FAILED, &[&i64::from(*errno).to_le_bytes()]),
        Reply::Listed(entries) => {
            // Four fields an entry: name, kind, size, link target
            let mut fields: Vec<Vec<u8>> = Vec::with_capacity(entries.len() * 4);
            for entry in entries {
                fields.push(entry.name.as_bytes().to_vec());
                fields.push(vec![match entry.kind {
                    Kind::File => b'f',
                    Kind::Dir => b'd',
                    Kind::Link => b'l',
                    Kind::Other => b'o',
                }]);
                fields.push(entry.size.to_le_bytes().to_vec());
                fields.push(
                    entry
                        .target
                        .as_ref()
                        .map(|t| t.as_os_str().as_bytes().to_vec())
                        .unwrap_or_default(),
                );
            }
            let fields: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
            write_message(out, LISTED, &fields)
        }
    }
}

/// Reads a reply.
pub fn read_reply(input: &mut impl Read) -> io::Result<Reply> {
    let (tag, fields) = read_message(input)?;
    Ok(match (tag, fields.as_slice()) {
        (DONE, []) => Reply::Done,
        (COPIED, [bytes]) => Reply::Copied(number(bytes)?),
        (PROGRESS, [bytes]) => Reply::Progress(number(bytes)?),
        (FAILED, [errno]) => Reply::Failed(number(errno)? as i64 as i32),
        (LISTED, fields) if fields.len() % 4 == 0 => {
            let mut entries = Vec::with_capacity(fields.len() / 4);
            for entry in fields.chunks(4) {
                let kind = match entry[1].as_slice() {
                    b"f" => Kind::File,
                    b"d" => Kind::Dir,
                    b"l" => Kind::Link,
                    _ => Kind::Other,
                };
                entries.push(Entry {
                    name: OsString::from_vec(entry[0].clone()),
                    kind,
                    size: number(&entry[2])?,
                    target: (kind == Kind::Link).then(|| path(&entry[3])),
                });
            }
            Reply::Listed(entries)
        }
        _ => return Err(bad("unknown reply")),
    })
}

/// The paths an operation may touch, resolved.
struct Scope {
    roots: Vec<PathBuf>,
}

impl Scope {
    fn new(sources: Vec<PathBuf>, destinations: Vec<PathBuf>) -> Self {
        let roots = sources
            .iter()
            .chain(&destinations)
            .filter_map(|root| resolve(root))
            .collect();
        Self { roots }
    }

    /// `path`, resolved, if it lies inside the scope.
    fn check(&self, path: &Path) -> io::Result<PathBuf> {
        let resolved = resolve(path).ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
        if self.roots.iter().any(|root| resolved.starts_with(root)) {
            Ok(resolved)
        } else {
            Err(io::Error::from_raw_os_error(libc::EACCES))
        }
    }
}

/// `path` with its folder resolved (links and `..` in it followed) and its
/// last part as it is, never followed.
fn resolve(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    if name == ".." {
        return None;
    }
    let parent = fs::canonicalize(path.parent()?).ok()?;
    Some(parent.join(name))
}

/// Runs the helper: reads requests from `input` and answers each on
/// `output`, until `input` closes. The first request must be the scope.
/// `input` is read unbuffered: a copy looks at it for [`Request::Stop`]
/// without waiting.
pub fn serve(mut input: impl Read + AsFd, mut output: impl Write) -> io::Result<()> {
    let scope = match read_request(&mut input)? {
        Request::Scope {
            sources,
            destinations,
        } => Scope::new(sources, destinations),
        _ => return Err(bad("the first request must be the scope")),
    };
    write_reply(&mut output, &Reply::Done)?;
    // A request that came while a copy ran, carried out after it
    let mut queued = None;
    loop {
        let next = match queued.take() {
            Some(request) => Ok(request),
            None => read_request(&mut input),
        };
        let request = match next {
            Ok(request) => request,
            // The app is done with it: the operation ended
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(err) => return Err(err),
        };
        let reply = match carry_out(&scope, request, &mut input, &mut queued, &mut output) {
            Ok(reply) => reply,
            Err(err) => Reply::Failed(err.raw_os_error().unwrap_or(libc::EIO)),
        };
        write_reply(&mut output, &reply)?;
    }
}

fn carry_out(
    scope: &Scope,
    request: Request,
    input: &mut (impl Read + AsFd),
    queued: &mut Option<Request>,
    output: &mut impl Write,
) -> io::Result<Reply> {
    match request {
        Request::Scope { .. } => Err(io::Error::from_raw_os_error(libc::EINVAL)),
        // The copy it was for had already ended
        Request::Stop => Ok(Reply::Done),
        Request::List(dir) => {
            let dir = scope.check(&dir)?;
            // Through the folder's handle, never through a link put in its
            // place: what is listed is that folder, whatever its path names
            let handle = open_dir_at(None, dir.as_os_str(), false)?;
            let mut entries = Vec::new();
            for name in names_in(handle.as_fd())? {
                let stat = stat_at(handle.as_fd(), &name)?;
                let kind = match stat.st_mode & libc::S_IFMT {
                    libc::S_IFLNK => Kind::Link,
                    libc::S_IFDIR => Kind::Dir,
                    libc::S_IFREG => Kind::File,
                    _ => Kind::Other,
                };
                let target = if kind == Kind::Link {
                    Some(read_link_at(handle.as_fd(), &name)?)
                } else {
                    None
                };
                entries.push(Entry {
                    name,
                    kind,
                    size: u64::try_from(stat.st_size).unwrap_or(0),
                    target,
                });
            }
            Ok(Reply::Listed(entries))
        }
        Request::Copy { from, to } => {
            let (from, to) = (scope.check(&from)?, scope.check(&to)?);
            copy(&from, &to, input, queued, output).map(Reply::Copied)
        }
        Request::Mkdir { from, to } => {
            let (from, to) = (scope.check(&from)?, scope.check(&to)?);
            let meta = fs::symlink_metadata(&from)?;
            match fs::create_dir(&to) {
                Err(err) if err.kind() != io::ErrorKind::AlreadyExists => return Err(err),
                _ => {}
            }
            std::os::unix::fs::lchown(&to, Some(meta.uid()), Some(meta.gid()))?;
            fs::set_permissions(&to, fs::Permissions::from_mode(meta.mode() & 0o7777))?;
            Ok(Reply::Done)
        }
        Request::Symlink { target, to, from } => {
            let (from, to) = (scope.check(&from)?, scope.check(&to)?);
            let meta = fs::symlink_metadata(&from)?;
            std::os::unix::fs::symlink(&target, &to)?;
            std::os::unix::fs::lchown(&to, Some(meta.uid()), Some(meta.gid()))?;
            Ok(Reply::Done)
        }
        Request::Remove(path) => {
            let path = scope.check(&path)?;
            if fs::symlink_metadata(&path)?.is_dir() {
                return Err(io::Error::from_raw_os_error(libc::EISDIR));
            }
            fs::remove_file(path).map(|()| Reply::Done)
        }
        Request::Rmdir(path) => {
            let path = scope.check(&path)?;
            fs::remove_dir(path).map(|()| Reply::Done)
        }
        Request::Rename { from, to } => {
            let (from, to) = (scope.check(&from)?, scope.check(&to)?);
            if fs::symlink_metadata(&to).is_ok() {
                return Err(io::Error::from_raw_os_error(libc::EEXIST));
            }
            fs::rename(from, to).map(|()| Reply::Done)
        }
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

/// How often a copy reports how far it got, in bytes.
const PROGRESS_EVERY: u64 = 8 << 20;

/// Whether the app asked to stop, looking at `input` without waiting: a
/// [`Request::Stop`], or the app gone. Any other request there is kept in
/// `queued` for after the copy, and nothing more is looked at until then.
fn stop_asked(input: &mut (impl Read + AsFd), queued: &mut Option<Request>) -> io::Result<bool> {
    use std::os::fd::AsRawFd;

    if queued.is_some() {
        return Ok(false);
    }
    let mut poll = libc::pollfd {
        fd: input.as_fd().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd, for the length given
    if unsafe { libc::poll(&mut poll, 1, 0) } <= 0 {
        return Ok(false);
    }
    match read_request(input) {
        Ok(Request::Stop) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(true),
        Ok(request) => {
            *queued = Some(request);
            Ok(false)
        }
        Err(err) => Err(err),
    }
}

/// Copies `from` to a new `to`: the bytes, then owner, mode and times as
/// `from` has them. Reports progress on `output` as it goes, and stops when
/// `input` asks it to. A copy that fails or stops part-way removes what it
/// made.
fn copy(
    from: &Path,
    to: &Path,
    input: &mut (impl Read + AsFd),
    queued: &mut Option<Request>,
    output: &mut impl Write,
) -> io::Result<u64> {
    let mut source = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(from)?;
    let meta = source.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let mut dest = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(to)?;
    let result = (|| {
        let mut buf = vec![0; 128 * 1024];
        let mut copied = 0u64;
        let mut reported = 0u64;
        loop {
            if stop_asked(input, queued)? {
                return Err(io::Error::from_raw_os_error(libc::ECANCELED));
            }
            let read = source.read(&mut buf)?;
            if read == 0 {
                break;
            }
            dest.write_all(&buf[..read])?;
            copied += read as u64;
            if copied - reported >= PROGRESS_EVERY {
                reported = copied;
                write_reply(output, &Reply::Progress(copied))?;
            }
        }
        std::os::unix::fs::fchown(&dest, Some(meta.uid()), Some(meta.gid()))?;
        dest.set_permissions(fs::Permissions::from_mode(meta.mode() & 0o7777))?;
        let mut times = fs::FileTimes::new();
        if let Ok(time) = meta.modified() {
            times = times.set_modified(time);
        }
        if let Ok(time) = meta.accessed() {
            times = times.set_accessed(time);
        }
        dest.set_times(times)?;
        dest.sync_all()?;
        Ok(copied)
    })();
    if result.is_err() {
        let _ = fs::remove_file(to);
    }
    result
}

/// Whether the helper may run: started through `pkexec` on a user's behalf,
/// which leaves that user's id in `PKEXEC_UID`.
pub fn started_by_pkexec() -> bool {
    std::env::var("PKEXEC_UID")
        .ok()
        .and_then(|uid| uid.parse::<u32>().ok())
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `input` as the helper reads it: from a pipe, already closed.
    fn fed(input: Vec<u8>) -> std::io::PipeReader {
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        writer.write_all(&input).expect("feed");
        reader
    }
    use std::io::Cursor;

    /// Runs `requests` through the helper's loop, in this process, as the
    /// current user: the replies, and the scope reply first.
    fn run(requests: &[Request]) -> Vec<Reply> {
        let mut input = Vec::new();
        for request in requests {
            write_request(&mut input, request).expect("encode");
        }
        let mut output = Vec::new();
        serve(fed(input), &mut output).expect("serve");
        let mut replies = Vec::new();
        let mut output = Cursor::new(output);
        while (output.position() as usize) < output.get_ref().len() {
            replies.push(read_reply(&mut output).expect("decode"));
        }
        replies
    }

    fn scope(sources: &[&Path], destinations: &[&Path]) -> Request {
        Request::Scope {
            sources: sources.iter().map(|p| p.to_path_buf()).collect(),
            destinations: destinations.iter().map(|p| p.to_path_buf()).collect(),
        }
    }

    #[test]
    fn requests_and_replies_come_through_as_sent() {
        let odd = PathBuf::from(OsString::from_vec(b"/tmp/not\xfftext".to_vec()));
        let requests = [
            scope(&[&odd], &[Path::new("/tmp")]),
            Request::Copy {
                from: odd.clone(),
                to: PathBuf::from("/tmp/x"),
            },
            Request::Symlink {
                target: PathBuf::from("a"),
                to: PathBuf::from("/tmp/l"),
                from: odd.clone(),
            },
        ];
        let mut bytes = Vec::new();
        for request in &requests {
            write_request(&mut bytes, request).expect("encode");
        }
        let mut bytes = Cursor::new(bytes);
        for request in &requests {
            assert_eq!(&read_request(&mut bytes).expect("decode"), request);
        }

        let listed = Reply::Listed(vec![
            Entry {
                name: OsString::from("f"),
                kind: Kind::File,
                size: 5,
                target: None,
            },
            Entry {
                name: OsString::from("l"),
                kind: Kind::Link,
                size: 1,
                target: Some(PathBuf::from("f")),
            },
        ]);
        for reply in [Reply::Done, Reply::Copied(7), Reply::Failed(13), listed] {
            let mut bytes = Vec::new();
            write_reply(&mut bytes, &reply).expect("encode");
            assert_eq!(read_reply(&mut Cursor::new(bytes)).expect("decode"), reply);
        }
    }

    #[test]
    fn a_copy_keeps_mode_and_times_and_refuses_a_taken_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (src, dst) = (dir.path().join("src"), dir.path().join("dst"));
        fs::create_dir_all(&src).expect("mkdir");
        fs::create_dir_all(&dst).expect("mkdir");
        let file = src.join("f");
        fs::write(&file, b"hello").expect("write");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).expect("chmod");

        let replies = run(&[
            scope(&[&file], &[&dst]),
            Request::Copy {
                from: file.clone(),
                to: dst.join("f"),
            },
            Request::Copy {
                from: file.clone(),
                to: dst.join("f"),
            },
        ]);
        assert_eq!(replies[1], Reply::Copied(5));
        assert_eq!(replies[2], Reply::Failed(libc::EEXIST));
        let (a, b) = (
            fs::metadata(&file).expect("stat"),
            fs::metadata(dst.join("f")).expect("stat"),
        );
        assert_eq!(fs::read(dst.join("f")).expect("read"), b"hello");
        assert_eq!(a.mode() & 0o7777, b.mode() & 0o7777);
        assert_eq!(a.uid(), b.uid());
        assert_eq!(a.modified().ok(), b.modified().ok());
    }

    #[test]
    fn nothing_outside_the_scope_is_touched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inside = dir.path().join("inside");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&inside).expect("mkdir");
        fs::create_dir_all(&outside).expect("mkdir");
        fs::write(outside.join("keep"), b"k").expect("write");
        // A link inside that leads outside
        std::os::unix::fs::symlink(&outside, inside.join("door")).expect("symlink");

        let replies = run(&[
            scope(&[], &[&inside]),
            Request::Remove(outside.join("keep")),
            Request::Remove(inside.join("..").join("outside").join("keep")),
            Request::Remove(inside.join("door").join("keep")),
            Request::List(outside.clone()),
        ]);
        for reply in &replies[1..] {
            assert_eq!(reply, &Reply::Failed(libc::EACCES), "{replies:?}");
        }
        assert!(outside.join("keep").exists());
    }

    #[test]
    fn a_link_at_the_end_is_removed_not_followed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        fs::write(&target, b"t").expect("write");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let replies = run(&[scope(&[], &[dir.path()]), Request::Remove(link.clone())]);
        assert_eq!(replies[1], Reply::Done);
        assert!(!link.exists() && fs::symlink_metadata(&link).is_err());
        assert!(target.exists(), "what the link pointed at stays");
    }

    #[test]
    fn a_listing_says_what_each_entry_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("f"), b"12345").expect("write");
        fs::create_dir(dir.path().join("d")).expect("mkdir");
        std::os::unix::fs::symlink("f", dir.path().join("l")).expect("symlink");

        let replies = run(&[
            scope(&[dir.path()], &[]),
            Request::List(dir.path().to_path_buf()),
        ]);
        let Reply::Listed(mut entries) = replies[1].clone() else {
            panic!("{replies:?}");
        };
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let seen: Vec<_> = entries
            .iter()
            .map(|e| {
                (
                    e.name.to_string_lossy().into_owned(),
                    e.kind,
                    e.target.clone(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("d".to_owned(), Kind::Dir, None),
                ("f".to_owned(), Kind::File, None),
                ("l".to_owned(), Kind::Link, Some(PathBuf::from("f"))),
            ]
        );
    }

    #[test]
    fn the_first_request_must_be_the_scope() {
        let mut input = Vec::new();
        write_request(&mut input, &Request::Remove(PathBuf::from("/tmp/x"))).expect("encode");
        assert!(serve(fed(input), Vec::new()).is_err());
    }
}
