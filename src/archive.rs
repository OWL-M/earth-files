use crate::mime_icon::mime_for_path;
use crate::operation::{Controller, ControllerState, OpReader, OperationError, OperationErrorType};
use crate::ui::iced::futures;
use jiff::Zoned;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;
use zip::result::{ZipError, ZipResult};

pub mod index;
pub mod open_copy;

pub const SUPPORTED_ARCHIVE_TYPES: &[&str] = &[
    "application/gzip",
    "application/x-compressed-tar",
    "application/x-tar",
    "application/zip",
    "application/x-7z-compressed",
    "application/zstd",
    "application/x-zstd-compressed-tar",
    #[cfg(feature = "bzip2")]
    "application/x-bzip",
    #[cfg(feature = "bzip2")]
    "application/x-bzip-compressed-tar",
    #[cfg(feature = "bzip2")]
    "application/x-bzip2",
    #[cfg(feature = "bzip2")]
    "application/x-bzip2-compressed-tar",
    #[cfg(feature = "lzma-rust2")]
    "application/x-xz",
    #[cfg(feature = "lzma-rust2")]
    "application/x-xz-compressed-tar",
    #[cfg(feature = "lzma-rust2")]
    "application/x-lzma",
    #[cfg(feature = "lzma-rust2")]
    "application/x-lzma-compressed-tar",
];

/// The endings an archive's name drops to name what it extracts to, longest
/// first: a compound one before the compression it ends in.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    ".tar.bz2",
    ".tar.gz",
    ".tar.lzma",
    ".tar.xz",
    ".tar.zst",
    ".tbz2",
    ".tbz",
    ".tgz",
    ".tlz",
    ".txz",
    ".tzst",
    ".tar",
    ".zip",
    ".7z",
    ".bz2",
    ".gz",
    ".lzma",
    ".xz",
    ".zst",
];

/// What extracting an archive did: what it wrote, for flushing to disk, and
/// what it could not extract whole, by name in the archive and why.
///
/// Damage to some entries does not stop the rest, as `7z x` goes on past
/// them; what was written of a damaged entry stays, to be looked at.
#[derive(Debug, Default)]
pub struct Extracted {
    pub written: Vec<PathBuf>,
    pub dirs: HashSet<PathBuf>,
    pub damaged: Vec<(PathBuf, String)>,
}

#[cfg(test)]
pub fn extract(
    path: &Path,
    new_dir: &Path,
    password: &Option<String>,
    controller: &Controller,
) -> Result<Extracted, OperationError> {
    extract_only(path, new_dir, password, None, controller)
}

/// As [`extract`], of `only` what it names where it names anything. What comes
/// out is where it is in the archive, folders above it included.
#[cfg(test)]
pub fn extract_only(
    path: &Path,
    new_dir: &Path,
    password: &Option<String>,
    only: Option<&index::Only>,
    controller: &Controller,
) -> Result<Extracted, OperationError> {
    extract_watched(path, new_dir, password, only, None, controller)
}

/// How much an archive grows, past which it is asked about: a real one rarely
/// gets here, and one built to fill a drive always does.
const SUSPICIOUS_RATIO: u64 = 100;
const SUSPICIOUS_SIZE: u64 = 1_000_000_000;
/// What an extraction of unknown size leaves free at most, in bytes. It is less
/// on a smaller drive, see [`Watch::new`].
const DRIVE_MARGIN: u64 = 256 * 1024 * 1024;
/// How much is written between looks at the room left on the drive.
const LOOK_EVERY: u64 = 16 * 1024 * 1024;

/// Whether an archive of `archive` bytes growing to `expanded` is worth asking
/// about.
pub fn suspicious(expanded: u64, archive: u64) -> bool {
    expanded > SUSPICIOUS_SIZE && expanded > archive.saturating_mul(SUSPICIOUS_RATIO)
}

/// How much an archive says it expands to.
#[derive(Clone, Copy, Debug)]
pub struct Expected {
    pub size: u64,
    /// Whether that is all that gets written: not when only part of it could be
    /// listed, nor when a name comes twice, as the earlier one is written too.
    pub complete: bool,
}

/// What `path`, or `only` what it names, expands to, where that can be known
/// without decompressing it: a compressed tar has to be read through, and is
/// known only if it was for browsing.
pub fn expected_size(
    path: &Path,
    password: Option<&str>,
    only: Option<&index::Only>,
    controller: &Controller,
) -> Option<Expected> {
    let cheap = matches!(
        mime_for_path(path, false).as_str(),
        "application/zip" | "application/x-7z-compressed" | "application/x-tar"
    );
    if !cheap && only.is_none() && index::held(path).is_none() {
        return None;
    }
    let index = index::read_shared(path, password, controller).ok()?;
    let (size, known) = match only {
        // What a chosen hard link shares its data with comes out on the way
        Some(only) => {
            let only = only.clone().with_hard_link_targets(&index);
            index.size_of(&only.chosen_and_needed())
        }
        None => index.total_size(),
    };
    Some(Expected {
        size,
        complete: known && !index.truncated && index.damaged.is_none() && !index.replaced,
    })
}

/// Refuses what cannot fit in the room left where `dir` is.
pub fn check_room(size: u64, dir: &Path) -> Result<(), OperationError> {
    match crate::operation::free_space(dir) {
        Some(free) if size > free => Err(OperationError::from_err(crate::fl!(
            "not-enough-space",
            needed = crate::tab::format_size(size),
            free = crate::tab::format_size(free)
        ))),
        _ => Ok(()),
    }
}

/// Keeps an eye on an extraction whose size could not be known first, as it
/// writes: it stops short of filling the drive, and asks once when the archive
/// has grown suspiciously.
pub struct Watch {
    dir: PathBuf,
    archive_size: u64,
    margin: u64,
    suspicious_from: u64,
    /// Told how far it has grown; whether to go on.
    ask: Box<dyn Fn(u64) -> bool + Send>,
    written: std::cell::Cell<u64>,
    looked_at: std::cell::Cell<u64>,
    asked: std::cell::Cell<bool>,
}

impl Watch {
    /// The margin is a tenth of what is free now where that is less than
    /// [`DRIVE_MARGIN`]: a nearly full drive can still take a small archive.
    pub fn new(dir: &Path, archive_size: u64, ask: Box<dyn Fn(u64) -> bool + Send>) -> Self {
        let free = crate::operation::free_space(dir).unwrap_or(u64::MAX);
        Self {
            dir: dir.to_path_buf(),
            archive_size,
            margin: DRIVE_MARGIN.min(free / 10),
            suspicious_from: SUSPICIOUS_SIZE,
            ask,
            written: std::cell::Cell::new(0),
            looked_at: std::cell::Cell::new(0),
            asked: std::cell::Cell::new(false),
        }
    }

    /// Whether the question was put already: it is not put again, and the room
    /// is still kept.
    pub fn asked_already(self, asked: bool) -> Self {
        self.asked.set(asked);
        self
    }

    /// `count` more bytes are about to be written. An error ends the extraction:
    /// they would leave the drive nearly full, or the answer was to stop.
    fn about_to_write(&self, count: u64) -> io::Result<()> {
        let written = self.written.get().saturating_add(count);
        self.written.set(written);
        if written.saturating_sub(self.looked_at.get()) >= LOOK_EVERY.min(self.margin.max(1)) {
            self.looked_at.set(written);
            if let Some(free) = crate::operation::free_space(&self.dir)
                && free < self.margin.saturating_add(count)
            {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    crate::fl!(
                        "extract-drive-nearly-full",
                        free = crate::tab::format_size(free)
                    ),
                ));
            }
        }
        if !self.asked.get()
            && written > self.suspicious_from
            && written > self.archive_size.saturating_mul(SUSPICIOUS_RATIO)
        {
            self.asked.set(true);
            if !(self.ask)(written) {
                return Err(io::Error::other("stopped"));
            }
        }
        Ok(())
    }
}

/// What a decoder gives, told to a [`Watch`].
struct Watched<'a, R> {
    inner: R,
    watch: &'a Watch,
}

impl<R: Read> Read for Watched<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buf)?;
        self.watch.about_to_write(count as u64)?;
        Ok(count)
    }
}

/// As [`extract_only`], with what it writes watched if a [`Watch`] is given.
pub fn extract_watched(
    path: &Path,
    new_dir: &Path,
    password: &Option<String>,
    only: Option<&index::Only>,
    watch: Option<&Watch>,
    controller: &Controller,
) -> Result<Extracted, OperationError> {
    let mime = mime_for_path(path, false);
    let password = password.as_deref();
    // A tar's hard link needs what it links to, written earlier in the stream
    // than it can be asked for. Known from the index, usually held already
    let with_targets = only
        .filter(|_| mime.as_str().contains("tar"))
        .and_then(|only| {
            let index = index::read_shared(path, None, controller).ok()?;
            Some(only.clone().with_hard_link_targets(&index))
        });
    let only = with_targets.as_ref().or(only);
    let mut out = Extracted::default();
    match mime.as_str() {
        "application/zip" => {
            zip_extract_file(path, new_dir, password, only, watch, controller, &mut out).map_err(
                |e| match e {
                    ZipError::UnsupportedArchive(ZipError::PASSWORD_REQUIRED)
                    | ZipError::InvalidPassword => {
                        OperationError::from_kind(OperationErrorType::PasswordRequired)
                    }
                    _ => stopped(controller).unwrap_or_else(|| OperationError::from_err(e)),
                },
            )?;
        }
        "application/x-7z-compressed" => {
            sevenz_extract(path, new_dir, password, only, watch, controller, &mut out)?;
        }
        "application/x-tar" => {
            let reader =
                OpReader::new(path, controller.clone()).map_err(OperationError::from_err)?;
            unpack_tar(
                tar::Archive::new(reader),
                new_dir,
                path,
                only,
                watch,
                controller,
                &mut out,
            )?;
        }
        "application/x-compressed-tar" => {
            extract_compressed(
                path,
                new_dir,
                (Codec::Gzip, true),
                only,
                watch,
                controller,
                &mut out,
            )?;
        }
        "application/gzip" => {
            extract_compressed(
                path,
                new_dir,
                (Codec::Gzip, false),
                only,
                watch,
                controller,
                &mut out,
            )?;
        }
        "application/x-zstd-compressed-tar" => {
            extract_compressed(
                path,
                new_dir,
                (Codec::Zstd, true),
                only,
                watch,
                controller,
                &mut out,
            )?;
        }
        "application/zstd" => {
            extract_compressed(
                path,
                new_dir,
                (Codec::Zstd, false),
                only,
                watch,
                controller,
                &mut out,
            )?;
        }
        #[cfg(feature = "bzip2")]
        "application/x-bzip-compressed-tar" | "application/x-bzip2-compressed-tar" => {
            extract_compressed(
                path,
                new_dir,
                (Codec::Bzip2, true),
                only,
                watch,
                controller,
                &mut out,
            )?;
        }
        #[cfg(feature = "bzip2")]
        "application/x-bzip" | "application/x-bzip2" => {
            extract_compressed(
                path,
                new_dir,
                (Codec::Bzip2, false),
                only,
                watch,
                controller,
                &mut out,
            )?;
        }
        #[cfg(feature = "lzma-rust2")]
        "application/x-xz-compressed-tar" => {
            extract_compressed(
                path,
                new_dir,
                (Codec::Xz, true),
                only,
                watch,
                controller,
                &mut out,
            )?;
        }
        #[cfg(feature = "lzma-rust2")]
        "application/x-xz" => {
            extract_compressed(
                path,
                new_dir,
                (Codec::Xz, false),
                only,
                watch,
                controller,
                &mut out,
            )?;
        }
        #[cfg(feature = "lzma-rust2")]
        "application/x-lzma-compressed-tar" => {
            extract_compressed(
                path,
                new_dir,
                (Codec::Lzma, true),
                only,
                watch,
                controller,
                &mut out,
            )?;
        }
        #[cfg(feature = "lzma-rust2")]
        "application/x-lzma" => {
            extract_compressed(
                path,
                new_dir,
                (Codec::Lzma, false),
                only,
                watch,
                controller,
                &mut out,
            )?;
        }
        _ => {
            return Err(OperationError::from_err(format!(
                "unsupported mime type {mime:?}"
            )));
        }
    }
    Ok(out)
}

/// The operation's own stop, if the user stopped it: an error then is the
/// stop showing through a read, not damage.
fn stopped(controller: &Controller) -> Option<OperationError> {
    match controller.state() {
        state @ (ControllerState::Cancelled | ControllerState::Failed) => {
            Some(OperationError::from_state(state))
        }
        _ => None,
    }
}

/// Waits while the operation is paused, and fails once it is stopped.
fn check(controller: &Controller) -> io::Result<()> {
    futures::executor::block_on(controller.check())
        .map_err(|state| io::Error::other(OperationError::from_state(state)))
}

/// Whether `err`, met while extracting, ends the extraction rather than
/// marking an entry damaged: the user stopped it, or the destination
/// refuses what is written, a full or read-only drive or no permission,
/// which is no fault of the archive's.
fn ends_extraction(err: &io::Error, controller: &Controller) -> Option<OperationError> {
    if let Some(stop) = stopped(controller) {
        return Some(stop);
    }
    matches!(
        err.kind(),
        io::ErrorKind::StorageFull
            | io::ErrorKind::QuotaExceeded
            | io::ErrorKind::ReadOnlyFilesystem
            | io::ErrorKind::PermissionDenied
    )
    .then(|| OperationError::from_err(io::Error::new(err.kind(), err.to_string())))
}

/// The archive's own name, for damage to the archive as a whole.
fn whole(path: &Path) -> PathBuf {
    PathBuf::from(path.file_name().unwrap_or(path.as_os_str()))
}

/// A compression that wraps a whole tar, or a single file.
#[derive(Clone, Copy)]
enum Codec {
    Gzip,
    Zstd,
    #[cfg(feature = "bzip2")]
    Bzip2,
    #[cfg(feature = "lzma-rust2")]
    Xz,
    #[cfg(feature = "lzma-rust2")]
    Lzma,
}

/// `reader` decompressed with `codec`. Several gzip or bzip2 members one
/// after another read as one, as `gzip -d` and `bzip2 -d` take them.
fn decoder<'a>(codec: Codec, reader: impl Read + 'a) -> io::Result<Box<dyn Read + 'a>> {
    let reader = io::BufReader::new(reader);
    Ok(match codec {
        Codec::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(reader)),
        Codec::Zstd => Box::new(zstd::stream::read::Decoder::with_buffer(reader)?),
        #[cfg(feature = "bzip2")]
        Codec::Bzip2 => Box::new(bzip2::bufread::MultiBzDecoder::new(reader)),
        #[cfg(feature = "lzma-rust2")]
        Codec::Xz => Box::new(lzma_rust2::XzReader::new(reader, true)),
        #[cfg(feature = "lzma-rust2")]
        Codec::Lzma => Box::new(lzma_rust2::LzmaReader::new_mem_limit(
            reader,
            u32::MAX,
            None,
        )?),
    })
}

/// Extracts `path`, compressed with `codec`: a tar inside, unpacked, when
/// `tar` says so or its first block is a tar header; otherwise the one file
/// it compresses, named after the archive less its extension.
fn extract_compressed(
    path: &Path,
    dst: &Path,
    (codec, tar): (Codec, bool),
    only: Option<&index::Only>,
    watch: Option<&Watch>,
    controller: &Controller,
    out: &mut Extracted,
) -> Result<(), OperationError> {
    let tar = tar || holds_a_tar(path, codec);
    let reader = OpReader::new(path, controller.clone()).map_err(OperationError::from_err)?;
    let reader = decoder(codec, reader).map_err(OperationError::from_err)?;
    if tar {
        unpack_tar(
            tar::Archive::new(reader),
            dst,
            path,
            only,
            watch,
            controller,
            out,
        )
    } else {
        let reader: Box<dyn Read + '_> = match watch {
            Some(watch) => Box::new(Watched {
                inner: reader,
                watch,
            }),
            None => reader,
        };
        write_single(reader, dst, path, controller, out)
    }
}

/// Whether the data `path` compresses with `codec` starts with a tar
/// header: a `.gz` typed by its content alone may be either.
fn holds_a_tar(path: &Path, codec: Codec) -> bool {
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let Ok(mut reader) = decoder(codec, file) else {
        return false;
    };
    let mut header = [0; 512];
    reader.read_exact(&mut header).is_ok() && &header[257..262] == b"ustar"
}

/// Writes the one file `reader` decompresses into `dst`, named after
/// `archive` less its compression's extension. Data that stops short is
/// kept as far as it goes, as damaged.
fn write_single(
    mut reader: Box<dyn Read + '_>,
    dst: &Path,
    archive: &Path,
    controller: &Controller,
    out: &mut Extracted,
) -> Result<(), OperationError> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let name = archive.file_name().unwrap_or_default();
    let bytes = name.as_bytes();
    let stem = [".gz", ".bz2", ".xz", ".lzma", ".zst"]
        .iter()
        .find_map(|ext| bytes.strip_suffix(ext.as_bytes()))
        .filter(|stem| !stem.is_empty() && *stem != b"." && *stem != b"..")
        .unwrap_or(bytes);
    let name = std::ffi::OsStr::from_bytes(stem);
    let to = dst.join(name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&to)
        .map_err(OperationError::from_err)?;
    let mut buffer = vec![0; 1024 * 1024];
    let mut any = false;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(count) => count,
            Err(err) => {
                if let Some(end) = ends_extraction(&err, controller) {
                    return Err(end);
                }
                // Not a byte of it: not compressed this way at all
                if !any {
                    drop(file);
                    let _ = fs::remove_file(&to);
                    return Err(OperationError::from_err(err));
                }
                out.damaged.push((PathBuf::from(name), err.to_string()));
                break;
            }
        };
        any = true;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .map_err(OperationError::from_err)?;
    }
    out.written.push(to);
    Ok(())
}

/// Unpacks `archive` into `dst` entry by entry: one that fails is noted as
/// damaged and the next is tried, and a stream that breaks off keeps what
/// came before it. Folders are made last, as `tar::Archive::unpack` makes
/// them, so a folder's own mode cannot keep what is in it from being
/// written; paths that would leave `dst` are refused there too.
fn unpack_tar<R: Read>(
    mut archive: tar::Archive<R>,
    dst: &Path,
    path: &Path,
    only: Option<&index::Only>,
    watch: Option<&Watch>,
    controller: &Controller,
    out: &mut Extracted,
) -> Result<(), OperationError> {
    let fail = |err: io::Error, name: PathBuf, out: &mut Extracted| {
        if let Some(end) = ends_extraction(&err, controller) {
            return Err(end);
        }
        out.damaged.push((name, err.to_string()));
        Ok(())
    };
    let entries = archive.entries().map_err(OperationError::from_err)?;
    let mut directories = Vec::new();
    let mut any = false;
    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => entry,
            // Not even a first entry: no tar at all, as `7z x` would not
            // open it either
            Err(err) if !any => {
                return Err(stopped(controller).unwrap_or_else(|| OperationError::from_err(err)));
            }
            // The stream broke: what follows cannot be found
            Err(err) => {
                fail(err, whole(path), out)?;
                break;
            }
        };
        any = true;
        if only.is_some_and(|only| !only.wants(&entry.path_bytes(), false)) {
            continue;
        }
        let name = entry
            .path()
            .map_or_else(|_| whole(path), std::borrow::Cow::into_owned);
        if entry.header().entry_type() == tar::EntryType::Directory {
            directories.push((name, entry));
            continue;
        }
        // By what it states, which a tar cannot write past: before a byte of
        // it, and not for what a selection passes over
        let size = entry.size();
        if let Some(Err(err)) = watch.map(|watch| watch.about_to_write(size)) {
            fail(err, name, out)?;
            continue;
        }
        if let Err(err) = entry.unpack_in(dst) {
            fail(err, name, out)?;
        }
    }
    // Deepest first, so a folder that takes away writing comes last
    directories.sort_by(|(a, _), (b, _)| b.cmp(a));
    for (name, mut entry) in directories {
        if let Err(err) = entry.unpack_in(dst) {
            fail(err, name, out)?;
        }
    }
    Ok(())
}

/// Makes `path` and the folders above it, writable by this user until all
/// that goes in them is in, and notes each for flushing.
fn make_writable_dir_all(path: &Path, target_dirs: &mut HashSet<PathBuf>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    target_dirs.insert(path.to_path_buf());
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(0o700 | fs::metadata(path)?.permissions().mode()),
    )
}

/// Refuses an entry whose path reaches its place through a symlink.
///
/// A name of its own cannot hold `..` or start at the root, but an archive
/// can hold a symlink entry and then a second entry underneath it: without
/// this check the second entry is written through the link, outside the
/// destination. An archive that legitimately wants to be unpacked over its
/// own symlinked directory is refused too, which is the safe way round.
fn reject_symlinked_path(root: &Path, outpath: &Path) -> io::Result<()> {
    let Ok(relative) = outpath.strip_prefix(root) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "entry escapes the destination",
        ));
    };
    let mut prefix = root.to_path_buf();
    for component in relative.components() {
        prefix.push(component);
        match fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "entry is placed through a symbolic link",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether a symlink entry at `link` pointing at `target` would resolve
/// outside `root`. Resolved lexically: the link is not followed, so this
/// holds even when the target does not exist yet.
fn target_escapes(root: &Path, link: &Path, target: &Path) -> bool {
    let mut resolved = if target.is_absolute() {
        PathBuf::new()
    } else {
        match link.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return true,
        }
    };
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() {
                    return true;
                }
            }
            other => resolved.push(other),
        }
    }
    !resolved.starts_with(root)
}

/// Where an entry named `name` goes under `root`: refused when the name
/// starts at the root or climbs out with `..`. A backslash counts as a
/// separator, as in names from Windows.
fn enclosed(root: &Path, name: &str) -> io::Result<PathBuf> {
    let name = name.replace('\\', "/");
    let mut out = root.to_path_buf();
    let mut any = false;
    for component in Path::new(&name).components() {
        match component {
            Component::Normal(part) => {
                out.push(part);
                any = true;
            }
            Component::CurDir => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("entry {name:?} escapes the destination"),
                ));
            }
        }
    }
    if !any {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "entry has no name",
        ));
    }
    Ok(out)
}

/// The kind bits of a unix mode: a symbolic link.
const S_IFLNK: u32 = 0o120_000;
const S_IFMT: u32 = 0o170_000;

/// Extracts the 7z archive `path` into `dst`. Folders and empty files come
/// first; then each block of data is decoded on its own, so a block that
/// breaks loses only what is left of it, and an entry whose data fails its
/// check is kept as damaged, as `7z x` keeps it.
fn sevenz_extract(
    path: &Path,
    dst: &Path,
    password: Option<&str>,
    only: Option<&index::Only>,
    watch: Option<&Watch>,
    controller: &Controller,
    out: &mut Extracted,
) -> Result<(), OperationError> {
    use sevenz_rust2::{Archive, ArchiveEntry, BlockDecoder, Error, Password};
    use std::os::unix::fs::OpenOptionsExt;

    let password_error = |err: &Error| {
        matches!(err, Error::PasswordRequired | Error::MaybeBadPassword(_))
            .then(|| OperationError::from_kind(OperationErrorType::PasswordRequired))
    };
    let mut source = fs::File::open(path).map_err(OperationError::from_err)?;
    let has_password = password.is_some_and(|password| !password.is_empty());
    let password = password.map_or_else(Password::empty, Password::from);
    let archive = Archive::read(&mut source, &password)
        .map_err(|err| password_error(&err).unwrap_or_else(|| OperationError::from_err(err)))?;
    let wanted =
        |name: &str| only.is_none_or(|only: &index::Only| only.wants(name.as_bytes(), true));

    // An entry's unix mode, when the archive was made on unix and keeps it
    let unix_mode = |entry: &ArchiveEntry| {
        let attributes = entry.windows_attributes();
        (entry.has_windows_attributes && attributes & 0x8000 != 0).then_some(attributes >> 16)
    };
    let mut modes = Vec::new();
    let mut times = Vec::new();
    let note = |entry: &ArchiveEntry, to: &Path, modes: &mut Vec<_>, times: &mut Vec<_>| {
        let mode = unix_mode(entry);
        if let Some(mode) = mode.filter(|mode| mode & S_IFMT != S_IFLNK) {
            modes.push((to.to_path_buf(), mode));
        }
        if entry.has_last_modified_date {
            let link = mode.is_some_and(|mode| mode & S_IFMT == S_IFLNK);
            times.push((
                to.to_path_buf(),
                std::time::SystemTime::from(entry.last_modified_date()),
                link,
            ));
        }
    };

    for entry in &archive.files {
        if entry.is_anti_item() || !wanted(&entry.name) {
            continue;
        }
        let to = enclosed(dst, &entry.name).map_err(OperationError::from_err)?;
        reject_symlinked_path(dst, &to).map_err(OperationError::from_err)?;
        if entry.is_directory() {
            make_writable_dir_all(&to, &mut out.dirs).map_err(OperationError::from_err)?;
            note(entry, &to, &mut modes, &mut times);
        } else if !entry.has_stream || entry.size == 0 {
            if let Some(parent) = to.parent() {
                make_writable_dir_all(parent, &mut out.dirs).map_err(OperationError::from_err)?;
            }
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&to)
                .map_err(OperationError::from_err)?;
            out.written.push(to.clone());
            note(entry, &to, &mut modes, &mut times);
        }
    }

    let total: u64 = archive
        .files
        .iter()
        .filter(|entry| wanted(&entry.name))
        .map(|entry| entry.size)
        .sum();
    let mut done = 0u64;
    let mut buffer = vec![0; 1024 * 1024];
    for block_index in 0..archive.blocks.len() {
        // Its contents encrypted, with a password given: data that fails to
        // read fails for a wrong password far more likely than for damage,
        // and the user is asked again rather than told it is damaged
        let encrypted = has_password
            && archive.blocks[block_index].coders.iter().any(|coder| {
                coder.encoder_method_id() == sevenz_rust2::EncoderMethod::ID_AES256_SHA256
            });
        let decoder = BlockDecoder::new(1, block_index, &archive, &password, &mut source);
        let with_data: Vec<String> = decoder
            .entries()
            .iter()
            .filter(|entry| entry.has_stream && entry.size > 0 && wanted(&entry.name))
            .map(|entry| entry.name.clone())
            .collect();
        // Nothing wanted of this block: it is not decoded at all
        if with_data.is_empty() {
            continue;
        }
        let mut seen = 0;
        let mut ended: Option<OperationError> = None;
        let result = decoder.for_each_entries(&mut |entry, reader| {
            if !(entry.has_stream && entry.size > 0) || !wanted(&entry.name) {
                return Ok(true);
            }
            seen += 1;
            let written = (|| -> Result<(), OperationError> {
                let to = enclosed(dst, &entry.name).map_err(OperationError::from_err)?;
                reject_symlinked_path(dst, &to).map_err(OperationError::from_err)?;
                if let Some(parent) = to.parent() {
                    make_writable_dir_all(parent, &mut out.dirs)
                        .map_err(OperationError::from_err)?;
                }
                let damaged = |err: io::Error, out: &mut Extracted| {
                    if let Some(end) = ends_extraction(&err, controller) {
                        return Err(end);
                    }
                    if encrypted {
                        return Err(OperationError::from_kind(
                            OperationErrorType::PasswordRequired,
                        ));
                    }
                    out.damaged
                        .push((PathBuf::from(&entry.name), err.to_string()));
                    Ok(())
                };
                if unix_mode(entry).is_some_and(|mode| mode & S_IFMT == S_IFLNK) {
                    // A link target is at most PATH_MAX: never sized by the
                    // archive's own claim
                    let mut target = Vec::new();
                    if let Err(err) = reader.take(libc::PATH_MAX as u64).read_to_end(&mut target) {
                        return damaged(err, out);
                    }
                    use std::os::unix::ffi::OsStringExt;
                    let target = PathBuf::from(std::ffi::OsString::from_vec(target));
                    if target_escapes(dst, &to, &target) {
                        return Err(OperationError::from_err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "symbolic link points outside the destination",
                        )));
                    }
                    std::os::unix::fs::symlink(&target, &to).map_err(OperationError::from_err)?;
                } else {
                    // O_NOFOLLOW so the last component cannot be a symlink
                    // an earlier entry planted
                    let mut file = fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&to)
                        .map_err(OperationError::from_err)?;
                    loop {
                        check(controller).map_err(|_| {
                            stopped(controller)
                                .unwrap_or_else(|| OperationError::from_msg("stopped"))
                        })?;
                        let count = match reader.read(&mut buffer) {
                            Ok(count) => count,
                            Err(err) => {
                                out.written.push(to.clone());
                                return damaged(err, out);
                            }
                        };
                        if count == 0 {
                            break;
                        }
                        if let Some(Err(err)) =
                            watch.map(|watch| watch.about_to_write(count as u64))
                        {
                            out.written.push(to.clone());
                            return damaged(err, out);
                        }
                        file.write_all(&buffer[..count])
                            .map_err(OperationError::from_err)?;
                        done += count as u64;
                        controller.set_progress(done as f32 / total.max(1) as f32);
                    }
                }
                out.written.push(to.clone());
                note(entry, &to, &mut modes, &mut times);
                Ok(())
            })();
            match written {
                Ok(()) => Ok(true),
                Err(end) => {
                    ended = Some(end);
                    Ok(false)
                }
            }
        });
        if let Some(end) = ended {
            return Err(end);
        }
        // The block could not be decoded on: what was not reached of it is
        // lost, and the next block is tried
        if let Err(err) = result {
            if let Some(stop) = stopped(controller) {
                return Err(stop);
            }
            if let Some(password) = password_error(&err) {
                return Err(password);
            }
            if encrypted {
                return Err(OperationError::from_kind(
                    OperationErrorType::PasswordRequired,
                ));
            }
            let reason = err.to_string();
            for name in with_data.into_iter().skip(seen) {
                out.damaged.push((PathBuf::from(name), reason.clone()));
            }
        }
    }

    // Times first: a folder whose mode takes away search would keep what is
    // in it from being reached. A mode change leaves times as they are
    for (path, time, link) in times {
        let time = filetime::FileTime::from_system_time(time);
        let set = if link {
            // `set_file_mtime` follows the link, which would let a link
            // entry retimestamp a file outside the destination
            filetime::set_symlink_file_times(&path, time, time)
        } else {
            filetime::set_file_mtime(&path, time)
        };
        if let Err(err) = set {
            log::warn!("failed to set the time of {}: {err}", path.display());
        }
    }
    use std::os::unix::fs::PermissionsExt;
    modes.sort_by_key(|(path, _): &(PathBuf, u32)| std::cmp::Reverse(path.components().count()));
    for (path, mode) in modes {
        // Only the permission bits: the archive author does not get to hand
        // out setuid, setgid or sticky
        fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o777))
            .map_err(OperationError::from_err)?;
    }
    Ok(())
}

/// Extracts the zip `path` into `dst`. When its central directory, the
/// index at its end, cannot be read, its entries are read one after
/// another from the start instead, as far as they go.
fn zip_extract_file(
    path: &Path,
    dst: &Path,
    password: Option<&str>,
    only: Option<&index::Only>,
    watch: Option<&Watch>,
    controller: &Controller,
    out: &mut Extracted,
) -> ZipResult<()> {
    let file = fs::File::open(path)?;
    match zip::ZipArchive::new(io::BufReader::new(file)) {
        Ok(mut archive) => zip_extract(
            &mut archive,
            dst,
            password,
            only,
            watch,
            controller.clone(),
            out,
        ),
        Err(err @ ZipError::InvalidArchive(_)) if password.is_none() => {
            let mut reader = io::BufReader::new(fs::File::open(path)?);
            let mut entries = ZipEntries::new(dst, out, false);
            entries.watch = watch;
            let mut index = 0;
            loop {
                match zip::read::read_zipfile_from_stream(&mut reader) {
                    Ok(Some(file)) => {
                        if only.is_none_or(|only| {
                            file.name()
                                .is_ok_and(|name| only.wants(name.as_bytes(), true))
                        }) {
                            entries.entry(file, (index, 0), controller)?;
                        }
                        index += 1;
                    }
                    Ok(None) => break,
                    // The entries end where the index should have begun, or
                    // one is damaged: the archive is noted damaged below
                    Err(_) => break,
                }
            }
            if index == 0 {
                return Err(err);
            }
            entries.out.damaged.push((whole(path), err.to_string()));
            entries.finish()
        }
        Err(err) => Err(err),
    }
}

/// What a zip extraction keeps between entries.
struct ZipEntries<'a> {
    root: &'a Path,
    out: &'a mut Extracted,
    buffer: Vec<u8>,
    by_unix_mode: Vec<(PathBuf, u32)>,
    by_last_modified: Vec<(PathBuf, zip::DateTime, bool)>,
    /// A password was given: an encrypted entry that fails to read most
    /// likely fails for a wrong one, not for damage
    password: bool,
    watch: Option<&'a Watch>,
}

impl<'a> ZipEntries<'a> {
    fn new(root: &'a Path, out: &'a mut Extracted, password: bool) -> Self {
        Self {
            root,
            out,
            buffer: vec![0; 4 * 1024 * 1024],
            by_unix_mode: Vec::new(),
            by_last_modified: Vec::new(),
            password,
            watch: None,
        }
    }

    /// Extracts `file`, the `index`th of `total` (0 when not known). Data
    /// that fails to read, or fails its checksum, is kept as damaged.
    fn entry<R: Read>(
        &mut self,
        mut file: zip::read::ZipFile<'_, R>,
        (index, total): (usize, usize),
        controller: &Controller,
    ) -> ZipResult<()> {
        use std::os::unix::fs::OpenOptionsExt;

        let filepath = file
            .enclosed_name()
            .ok_or(ZipError::InvalidArchive("Invalid file path".into()))?;
        let outpath = self.root.join(&filepath);
        reject_symlinked_path(self.root, &outpath)?;

        // Given its time once it exists: one queued for an entry that was
        // never made would fail at the end, and take the whole extraction
        // with it
        let last_modified = file.last_modified();
        let timed = |outpath: &Path, link: bool, by_last_modified: &mut Vec<_>| {
            if let Some(last_modified) = last_modified {
                by_last_modified.push((outpath.to_path_buf(), last_modified, link));
            }
        };

        if file.is_dir() {
            make_writable_dir_all(&outpath, &mut self.out.dirs)?;
            timed(&outpath, false, &mut self.by_last_modified);
            if let Some(mode) = file.unix_mode() {
                self.by_unix_mode.push((outpath, mode));
            }
            return Ok(());
        }

        if let Some(parent) = outpath.parent() {
            make_writable_dir_all(parent, &mut self.out.dirs)?;
        }

        let wrong_password = self.password && file.encrypted();
        let damaged = |err: io::Error, out: &mut Extracted| -> ZipResult<()> {
            if ends_extraction(&err, controller).is_some() {
                return Err(err.into());
            }
            if wrong_password {
                return Err(ZipError::InvalidPassword);
            }
            out.damaged.push((filepath.clone(), err.to_string()));
            Ok(())
        };

        if file.is_symlink() {
            // The declared size comes from the archive, so never size a
            // buffer by it; a link target is at most PATH_MAX anyway, and
            // symlink(2) refuses one that long, so a cut-off target cannot
            // be created silently
            let mut target = Vec::new();
            if let Err(err) = (&mut file)
                .take(libc::PATH_MAX as u64)
                .read_to_end(&mut target)
            {
                return damaged(err, self.out);
            }
            use std::os::unix::ffi::OsStringExt;
            let target = std::ffi::OsString::from_vec(target);
            if target_escapes(self.root, &outpath, Path::new(&target)) {
                return Err(ZipError::InvalidArchive(
                    "symbolic link points outside the destination".into(),
                ));
            }
            std::os::unix::fs::symlink(&target, outpath.as_path())?;
            timed(&outpath, true, &mut self.by_last_modified);
            self.out.written.push(outpath);
            return Ok(());
        }

        let size = file.size();
        // O_NOFOLLOW so the final component cannot be a symlink planted by an
        // earlier entry, which `File::create` would happily write through
        let mut outfile = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&outpath)?;
        timed(&outpath, false, &mut self.by_last_modified);
        let mode = file.unix_mode();
        let mut current = 0;
        loop {
            check(controller)?;
            let count = match file.read(&mut self.buffer) {
                Ok(count) => count,
                Err(err) => {
                    self.out.written.push(outpath.clone());
                    if let Some(mode) = mode {
                        self.by_unix_mode.push((outpath, mode));
                    }
                    return damaged(err, self.out);
                }
            };
            if count == 0 {
                break;
            }
            if let Some(Err(err)) = self.watch.map(|watch| watch.about_to_write(count as u64)) {
                self.out.written.push(outpath.clone());
                if let Some(mode) = mode {
                    self.by_unix_mode.push((outpath, mode));
                }
                return damaged(err, self.out);
            }
            outfile.write_all(&self.buffer[..count])?;
            current += count as u64;

            if total > 0 && current < size {
                let file_progress = current as f32 / size as f32;
                let total_progress = (index as f32 + file_progress) / total as f32;
                controller.set_progress(total_progress);
            }
        }

        // Check for real permissions, which we'll set in a second pass
        if let Some(mode) = mode {
            self.by_unix_mode.push((outpath.clone(), mode));
        }
        self.out.written.push(outpath);
        Ok(())
    }

    /// Gives what was extracted its times, then its modes.
    fn finish(self) -> ZipResult<()> {
        // Times first: a folder whose mode takes away search would keep what
        // is in it from being reached. A mode change leaves times as they are
        for (path, last_modified, is_symlink) in self.by_last_modified {
            if let Some(modified) = zip_date_time_to_system_time(last_modified) {
                let file_time = filetime::FileTime::from_system_time(modified);
                if is_symlink {
                    // `set_file_mtime` follows the link, which would let a symlink
                    // entry alone retimestamp a file outside the destination
                    let accessed = fs::symlink_metadata(&path)
                        .map(|metadata| filetime::FileTime::from_last_access_time(&metadata))
                        .unwrap_or(file_time);
                    filetime::set_symlink_file_times(&path, accessed, file_time)?;
                } else {
                    filetime::set_file_mtime(&path, file_time)?;
                }
            }
        }

        use std::cmp::Reverse;
        use std::os::unix::fs::PermissionsExt;

        let mut by_unix_mode = self.by_unix_mode;
        // Ensure we update children's permissions before making a parent unwritable
        by_unix_mode.sort_by_key(|(path, _)| Reverse(path.components().count()));
        for (path, mode) in by_unix_mode {
            // Only the permission bits: the archive author does not get to hand
            // out setuid, setgid or sticky
            fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o777))?;
        }
        Ok(())
    }
}

// From https://docs.rs/zip/latest/zip/read/struct.ZipArchive.html#method.extract,
// with cancellation, progress and damaged entries passed over added
fn zip_extract<R: io::Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    directory: &Path,
    password: Option<&str>,
    only: Option<&index::Only>,
    watch: Option<&Watch>,
    controller: Controller,
    out: &mut Extracted,
) -> ZipResult<()> {
    let total_files = archive.len();
    let mut entries = ZipEntries::new(directory, out, password.is_some());
    entries.watch = watch;
    for i in 0..total_files {
        check(&controller)?;
        controller.set_progress(i as f32 / total_files as f32);

        // Taken first: a damaged entry's error still borrows the archive
        let name = archive.name_for_index(i).and_then(Result::ok).map_or_else(
            || PathBuf::from(format!("#{i}")),
            |name| PathBuf::from(&*name),
        );
        if only.is_some_and(|only| !only.wants(name.as_os_str().as_encoded_bytes(), true)) {
            continue;
        }
        let file = match password {
            None => archive.by_index(i),
            Some(pwd) => archive.by_index_decrypt(i, pwd.as_bytes()),
        };
        let err = match file {
            Ok(file) => {
                entries.entry(file, (i, total_files), &controller)?;
                continue;
            }
            Err(err) => err,
        };
        match err {
            ZipError::UnsupportedArchive(ZipError::PASSWORD_REQUIRED)
            | ZipError::InvalidPassword => return Err(err),
            ZipError::Io(ref io) if ends_extraction(io, &controller).is_some() => return Err(err),
            // This entry's header is damaged: the next may not be
            _ => entries.out.damaged.push((name, err.to_string())),
        }
    }
    entries.finish()
}

fn zip_date_time_to_system_time(date_time: zip::DateTime) -> Option<SystemTime> {
    let dt = DateTime::new(
        date_time.year() as i16,
        date_time.month() as i8,
        date_time.day() as i8,
        date_time.hour() as i8,
        date_time.minute() as i8,
        date_time.second() as i8,
        0,
    )
    .ok()?;
    TimeZone::system()
        .to_ambiguous_zoned(dt)
        .later()
        .ok()
        .map(SystemTime::from)
}

pub fn system_time_to_zip_date_time(system_time: SystemTime) -> Option<zip::DateTime> {
    let date_time = Zoned::try_from(system_time).ok()?;

    zip::DateTime::from_date_and_time(
        date_time.year() as u16,
        date_time.month() as u8,
        date_time.day() as u8,
        date_time.hour() as u8,
        date_time.minute() as u8,
        date_time.second() as u8,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_options() -> zip::write::SimpleFileOptions {
        zip::write::SimpleFileOptions::default()
    }

    /// An archive holding a symlink out of the destination and a file beneath
    /// it must not write through the link
    #[test]
    fn zip_entry_under_a_symlink_cannot_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("victim.txt"), b"ORIGINAL").unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let zip_path = tmp.path().join("evil.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        writer
            .add_symlink("link", outside.to_str().unwrap(), zip_options())
            .unwrap();
        writer.start_file("link/victim.txt", zip_options()).unwrap();
        writer.write_all(b"PWNED").unwrap();
        writer.finish().unwrap();

        let result = extract(&zip_path, &dest, &None, &Controller::default());
        assert!(result.is_err(), "escaping archive must be refused");
        assert_eq!(
            fs::read_to_string(outside.join("victim.txt")).unwrap(),
            "ORIGINAL"
        );
    }

    /// A symlink entry on its own must not retimestamp its target either
    #[test]
    fn zip_symlink_entry_cannot_retimestamp_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, b"ORIGINAL").unwrap();
        let before = fs::metadata(&outside).unwrap().modified().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let zip_path = tmp.path().join("touch.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        writer
            .add_symlink(
                "link",
                outside.to_str().unwrap(),
                zip_options().last_modified_time(
                    zip::DateTime::from_date_and_time(1990, 1, 1, 0, 0, 0).unwrap(),
                ),
            )
            .unwrap();
        writer.finish().unwrap();

        let _ = extract(&zip_path, &dest, &None, &Controller::default());
        assert_eq!(fs::metadata(&outside).unwrap().modified().unwrap(), before);
        assert_eq!(fs::read_to_string(&outside).unwrap(), "ORIGINAL");
    }

    /// A symlink entry whose header claims an absurd uncompressed size must
    /// not make the extractor allocate that much before reading a byte
    #[test]
    fn zip_symlink_declared_size_is_not_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let zip_path = tmp.path().join("huge.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        writer
            .add_symlink("link", "a.txt", zip_options().large_file(true))
            .unwrap();
        writer.finish().unwrap();

        // `large_file` puts the sizes in zip64 extra fields, whose layout is
        // id 0x0001, length 16, uncompressed size, compressed size (u64 LE);
        // claim u64::MAX uncompressed in each, leaving the real data alone
        let mut bytes = fs::read(&zip_path).unwrap();
        let mut marker = vec![0x01, 0x00, 0x10, 0x00];
        marker.extend_from_slice(&("a.txt".len() as u64).to_le_bytes());
        let mut patched = 0;
        let mut i = 0;
        while i + marker.len() <= bytes.len() {
            if bytes[i..i + marker.len()] == marker[..] {
                bytes[i + 4..i + 12].copy_from_slice(&u64::MAX.to_le_bytes());
                patched += 1;
            }
            i += 1;
        }
        assert!(patched > 0, "zip64 size fields not found");
        fs::write(&zip_path, &bytes).unwrap();

        extract(&zip_path, &dest, &None, &Controller::default()).unwrap();
        assert_eq!(
            fs::read_link(dest.join("link")).unwrap(),
            Path::new("a.txt")
        );
    }

    /// An ordinary archive, including a symlink that stays inside, still works
    #[test]
    fn zip_ordinary_archive_still_extracts() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let zip_path = tmp.path().join("ok.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        writer.start_file("a.txt", zip_options()).unwrap();
        writer.write_all(b"hello").unwrap();
        writer.add_directory("sub", zip_options()).unwrap();
        writer.start_file("sub/b.txt", zip_options()).unwrap();
        writer.write_all(b"world").unwrap();
        writer
            .add_symlink("inside", "a.txt", zip_options())
            .unwrap();
        writer.finish().unwrap();

        extract(&zip_path, &dest, &None, &Controller::default()).unwrap();
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "hello");
        assert_eq!(fs::read_to_string(dest.join("sub/b.txt")).unwrap(), "world");
        assert_eq!(fs::read_to_string(dest.join("inside")).unwrap(), "hello");
    }

    /// A setuid bit chosen by the archive author must not survive extraction
    #[test]
    fn zip_setuid_bit_is_dropped() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let zip_path = tmp.path().join("suid.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        writer
            .start_file("tool", zip_options().unix_permissions(0o755))
            .unwrap();
        writer.write_all(b"#!/bin/sh").unwrap();
        writer.finish().unwrap();

        // The writer masks permissions to 0o777, so plant the setuid bit in the
        // central directory's external attributes by hand (offset 38 of the
        // 0x02014b50 header)
        let mut bytes = fs::read(&zip_path).unwrap();
        let header = bytes
            .windows(4)
            .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        let attrs = ((0o100000u32 | 0o4755) << 16).to_le_bytes();
        bytes[header + 38..header + 42].copy_from_slice(&attrs);
        fs::write(&zip_path, bytes).unwrap();

        extract(&zip_path, &dest, &None, &Controller::default()).unwrap();
        let mode = fs::metadata(dest.join("tool"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o755, "got {mode:o}");
    }

    /// Flips a byte of the first `marker` in `path`, inside its data.
    fn damage(path: &Path, marker: &[u8]) {
        let mut bytes = fs::read(path).unwrap();
        let at = bytes
            .windows(marker.len())
            .position(|w| w == marker)
            .expect("marker in the archive");
        bytes[at + marker.len() / 2] ^= 0xff;
        fs::write(path, bytes).unwrap();
    }

    /// A 7z of `entries`, stored, each in a block of its own: their data
    /// can be found, and damaged, byte for byte.
    fn write_7z(path: &Path, entries: &[(&str, &[u8])]) {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderMethod};
        let mut writer = ArchiveWriter::create(path).unwrap();
        writer.set_content_methods(vec![EncoderMethod::COPY.into()]);
        for (name, data) in entries {
            writer
                .push_archive_entry(ArchiveEntry::new_file(name), Some(*data))
                .unwrap();
        }
        writer.finish().unwrap();
    }

    /// The names of what `extract` found damaged.
    fn damaged(out: &Extracted) -> Vec<PathBuf> {
        out.damaged.iter().map(|(name, _)| name.clone()).collect()
    }

    /// A 7z is offered, and extracts
    #[test]
    fn sevenz_extracts() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("ok.7z");
        write_7z(&archive, &[("a.txt", b"alpha"), ("sub/b.txt", b"beta")]);
        assert!(SUPPORTED_ARCHIVE_TYPES.contains(&mime_for_path(&archive, false).as_str()));
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let out = extract(&archive, &dest, &None, &Controller::default()).unwrap();
        assert!(out.damaged.is_empty(), "{:?}", out.damaged);
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(fs::read(dest.join("sub/b.txt")).unwrap(), b"beta");
    }

    /// An entry whose data fails its check is kept and named damaged; the
    /// rest of the 7z still extracts, as `7z x` goes on past it
    #[test]
    fn sevenz_damaged_entry_does_not_stop_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("hurt.7z");
        write_7z(
            &archive,
            &[
                ("a.txt", b"FIRST-ENTRY-DATA"),
                ("b.txt", b"SECOND-ENTRY-DATA"),
            ],
        );
        damage(&archive, b"FIRST-ENTRY-DATA");
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let out = extract(&archive, &dest, &None, &Controller::default()).unwrap();
        assert_eq!(damaged(&out), [PathBuf::from("a.txt")]);
        assert!(dest.join("a.txt").exists(), "what was read of it is kept");
        assert_eq!(fs::read(dest.join("b.txt")).unwrap(), b"SECOND-ENTRY-DATA");
    }

    /// A 7z entry named out of the destination is refused, and nothing is
    /// written outside
    #[test]
    fn sevenz_entry_cannot_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("evil.7z");
        write_7z(&archive, &[("../outside.txt", b"PWNED")]);
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        assert!(extract(&archive, &dest, &None, &Controller::default()).is_err());
        assert!(!tmp.path().join("outside.txt").exists());
    }

    /// A zip entry whose data fails its checksum is kept and named
    /// damaged; the next entry still extracts
    #[test]
    fn zip_damaged_entry_does_not_stop_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_path = tmp.path().join("hurt.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        let stored = zip_options().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("a.txt", stored).unwrap();
        writer.write_all(b"FIRST-ENTRY-DATA").unwrap();
        writer.start_file("b.txt", stored).unwrap();
        writer.write_all(b"SECOND-ENTRY-DATA").unwrap();
        writer.finish().unwrap();
        damage(&zip_path, b"FIRST-ENTRY-DATA");
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let out = extract(&zip_path, &dest, &None, &Controller::default()).unwrap();
        assert_eq!(damaged(&out), [PathBuf::from("a.txt")]);
        assert!(dest.join("a.txt").exists());
        assert_eq!(fs::read(dest.join("b.txt")).unwrap(), b"SECOND-ENTRY-DATA");
    }

    /// A zip whose central directory is gone is read entry by entry from
    /// its start, and says its index was damaged
    #[test]
    fn zip_without_its_central_directory_still_extracts() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_path = tmp.path().join("cut.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        writer.start_file("a.txt", zip_options()).unwrap();
        writer.write_all(b"alpha").unwrap();
        writer.start_file("b.txt", zip_options()).unwrap();
        writer.write_all(b"beta").unwrap();
        writer.finish().unwrap();
        let bytes = fs::read(&zip_path).unwrap();
        let central = bytes
            .windows(4)
            .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        fs::write(&zip_path, &bytes[..central]).unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let out = extract(&zip_path, &dest, &None, &Controller::default()).unwrap();
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(fs::read(dest.join("b.txt")).unwrap(), b"beta");
        assert_eq!(damaged(&out), [PathBuf::from("cut.zip")]);
    }

    /// A tar of `entries`, compressed with `compress`.
    fn tar_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, *data).unwrap();
        }
        tar.into_inner().unwrap()
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), Default::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    /// A tar.gz cut short keeps what came before the break, and says the
    /// archive was damaged
    #[test]
    fn a_tar_gz_cut_short_keeps_what_came_before() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("cut.tar.gz");
        let first = vec![b'a'; 64 * 1024];
        let second: Vec<u8> = (0..256 * 1024).map(|i| (i * 7919 % 251) as u8).collect();
        let bytes = gzip(&tar_of(&[("first.bin", &first), ("second.bin", &second)]));
        fs::write(&archive, &bytes[..bytes.len() * 2 / 3]).unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let out = extract(&archive, &dest, &None, &Controller::default()).unwrap();
        assert_eq!(fs::read(dest.join("first.bin")).unwrap(), first);
        assert!(!out.damaged.is_empty());
    }

    /// Something that is no archive, only named like one, still fails
    #[test]
    fn what_is_no_archive_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("fake.tar.gz");
        fs::write(&archive, b"\x1f\x8b\x08\x00garbage, not deflate").unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        assert!(extract(&archive, &dest, &None, &Controller::default()).is_err());
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 0);
    }

    /// A plain `.gz` gives the one file it compresses, named without `.gz`
    #[test]
    fn a_plain_gz_gives_its_file() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("notes.txt.gz");
        fs::write(&archive, gzip(b"hello")).unwrap();
        assert!(SUPPORTED_ARCHIVE_TYPES.contains(&mime_for_path(&archive, false).as_str()));
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let out = extract(&archive, &dest, &None, &Controller::default()).unwrap();
        assert!(out.damaged.is_empty());
        assert_eq!(fs::read(dest.join("notes.txt")).unwrap(), b"hello");
    }

    /// A `.tar.zst` is offered, and unpacks
    #[test]
    fn a_tar_zst_unpacks() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("ok.tar.zst");
        let compressed = zstd::stream::encode_all(&tar_of(&[("a.txt", b"zstd")])[..], 3).unwrap();
        fs::write(&archive, compressed).unwrap();
        assert!(SUPPORTED_ARCHIVE_TYPES.contains(&mime_for_path(&archive, false).as_str()));
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        extract(&archive, &dest, &None, &Controller::default()).unwrap();
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"zstd");
    }

    /// Every type named for extraction is one the type database knows by
    /// name: a misspelt one would never be offered
    #[test]
    fn the_types_offered_are_ones_files_are_given() {
        for name in [
            "a.7z",
            "a.zip",
            "a.tar",
            "a.tar.gz",
            "a.gz",
            "a.tar.zst",
            "a.zst",
            "a.tar.bz2",
            "a.bz2",
            "a.tar.xz",
            "a.xz",
            "a.tar.lzma",
            "a.lzma",
        ] {
            let mime = mime_for_path(Path::new(name), true);
            assert!(
                SUPPORTED_ARCHIVE_TYPES.contains(&mime.as_str()),
                "{name}: {mime}"
            );
        }
    }

    /// A wrong password for a 7z whose contents are encrypted, its header
    /// not, is asked about again: not taken for damage, with an empty file
    /// for an answer
    #[test]
    fn a_wrong_7z_password_is_asked_about_again() {
        use sevenz_rust2::encoder_options::AesEncoderOptions;
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderMethod, Password};
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("locked.7z");
        let mut writer = ArchiveWriter::create(&archive).unwrap();
        writer.set_encrypt_header(false);
        writer.set_content_methods(vec![
            AesEncoderOptions::new(Password::from("right")).into(),
            EncoderMethod::LZMA2.into(),
        ]);
        writer
            .push_archive_entry(ArchiveEntry::new_file("secret.txt"), Some(&b"inside"[..]))
            .unwrap();
        writer.finish().unwrap();

        let dest = tmp.path().join("wrong");
        fs::create_dir(&dest).unwrap();
        let err = extract(
            &archive,
            &dest,
            &Some("wrong".into()),
            &Controller::default(),
        )
        .expect_err("a wrong password");
        assert!(
            matches!(err.kind, OperationErrorType::PasswordRequired),
            "{err:?}"
        );

        let dest = tmp.path().join("right");
        fs::create_dir(&dest).unwrap();
        extract(
            &archive,
            &dest,
            &Some("right".into()),
            &Controller::default(),
        )
        .unwrap();
        assert_eq!(fs::read(dest.join("secret.txt")).unwrap(), b"inside");
    }

    /// A damaged symlink entry is passed over without failing the rest:
    /// it was never made, so it is given no time at the end
    #[test]
    fn a_damaged_zip_symlink_does_not_fail_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_path = tmp.path().join("link.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        let stored = zip_options()
            .compression_method(zip::CompressionMethod::Stored)
            .last_modified_time(zip::DateTime::from_date_and_time(2001, 1, 1, 0, 0, 0).unwrap());
        writer
            .add_symlink("link", "LINK-TARGET-NAME", stored)
            .unwrap();
        writer.start_file("b.txt", stored).unwrap();
        writer.write_all(b"healthy").unwrap();
        writer.finish().unwrap();
        damage(&zip_path, b"LINK-TARGET-NAME");
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let out = extract(&zip_path, &dest, &None, &Controller::default()).unwrap();
        assert_eq!(damaged(&out), [PathBuf::from("link")]);
        assert!(fs::symlink_metadata(dest.join("link")).is_err());
        assert_eq!(fs::read(dest.join("b.txt")).unwrap(), b"healthy");
    }

    fn gz_of(path: &Path, bytes: usize) {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(
            fs::File::create(path).unwrap(),
            flate2::Compression::fast(),
        );
        gz.write_all(&vec![0; bytes]).unwrap();
        gz.finish().unwrap();
    }

    fn watch_on(
        dir: &Path,
        asks: std::sync::Arc<std::sync::atomic::AtomicU32>,
        go_on: bool,
    ) -> Watch {
        let mut watch = Watch::new(
            dir,
            1,
            Box::new(move |_| {
                asks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                go_on
            }),
        );
        watch.suspicious_from = 1000;
        watch
    }

    #[test]
    fn growth_is_suspicious_past_both_limits() {
        assert!(suspicious(2_000_000_000, 1_000_000));
        // Much, but not for its size
        assert!(!suspicious(2_000_000_000, 100_000_000));
        // A lot for its size, but not much
        assert!(!suspicious(1_000_000, 1));
    }

    /// Asked about once as it grows, and going on takes it to the end
    #[test]
    fn a_stream_is_asked_about_once_and_goes_on() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.gz");
        gz_of(&archive, 4 << 20);
        let out = tmp.path().join("out");
        fs::create_dir(&out).unwrap();
        let asks = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let watch = watch_on(&out, std::sync::Arc::clone(&asks), true);
        extract_watched(
            &archive,
            &out,
            &None,
            None,
            Some(&watch),
            &Controller::default(),
        )
        .unwrap();
        assert_eq!(asks.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(fs::metadata(out.join("a")).unwrap().len(), 4 << 20);
    }

    /// The answer to stop ends it, in a tar as in a single file
    #[test]
    fn a_compressed_tar_stops_at_the_answer() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar.gz");
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(4 << 20);
        header.set_mode(0o644);
        tar.append_data(&mut header, "big", &vec![0; 4 << 20][..])
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap().flush().unwrap();
        let out = tmp.path().join("out");
        fs::create_dir(&out).unwrap();
        let controller = Controller::default();
        let asks = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let stopping = controller.clone();
        let counted = std::sync::Arc::clone(&asks);
        let mut watch = Watch::new(
            &out,
            1,
            Box::new(move |_| {
                counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                stopping.cancel();
                false
            }),
        );
        watch.suspicious_from = 1000;
        let done = extract_watched(&archive, &out, &None, None, Some(&watch), &controller);
        assert!(done.is_err_and(|err| err.is_cancelled()));
        assert_eq!(asks.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// Short of filling the drive, it stops by itself, and says so
    #[test]
    fn a_stream_stops_short_of_the_margin() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.gz");
        gz_of(&archive, 32 << 20);
        let out = tmp.path().join("out");
        fs::create_dir(&out).unwrap();
        let asks = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut watch = watch_on(&out, std::sync::Arc::clone(&asks), true);
        watch.suspicious_from = u64::MAX;
        watch.margin = u64::MAX;
        let err = extract_watched(
            &archive,
            &out,
            &None,
            None,
            Some(&watch),
            &Controller::default(),
        )
        .unwrap_err();
        assert!(!err.is_cancelled());
        assert!(fs::metadata(out.join("a")).unwrap().len() < 32 << 20);
        assert_eq!(asks.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    /// A small margin is looked at as often as it needs
    #[test]
    fn a_small_margin_leaves_a_small_archive_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let watch = Watch::new(tmp.path(), 1 << 40, Box::new(|_| true));
        assert!(watch.margin <= DRIVE_MARGIN);
        assert!(watch.about_to_write(1 << 20).is_ok());
    }

    /// An entry that holds more than the list says is cut off there, and named.
    /// The zip crate's reader does the holding: a 7z entry's reader is bounded
    /// too, and a tar's is by its format
    #[test]
    fn a_zip_entry_past_its_stated_size_is_damaged() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.zip");
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("bomb", options).unwrap();
        zip.write_all(&vec![0; 1 << 20]).unwrap();
        zip.start_file("after", options).unwrap();
        zip.write_all(b"after").unwrap();
        let mut bytes = zip.finish().unwrap().into_inner();
        let at = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .unwrap();
        bytes[at + 24..at + 28].copy_from_slice(&16_u32.to_le_bytes());
        fs::write(&archive, bytes).unwrap();
        let out = tmp.path().join("out");
        fs::create_dir(&out).unwrap();
        let done = extract_only(&archive, &out, &None, None, &Controller::default()).unwrap();
        let damaged: Vec<_> = done.damaged.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(damaged, [PathBuf::from("bomb")]);
        assert!(fs::metadata(out.join("bomb")).unwrap().len() <= 16);
        assert_eq!(fs::read(out.join("after")).unwrap(), b"after");
    }

    #[test]
    fn what_does_not_fit_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(check_room(u64::MAX, tmp.path()).is_err());
        assert!(check_room(1, tmp.path()).is_ok());
    }

    /// Known only where listing costs little, or was done already to browse
    #[test]
    fn a_compressed_tar_is_known_only_once_read() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar.gz");
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        tar.append_data(&mut header, "x", &b"abc"[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        let controller = Controller::default();
        assert!(expected_size(&archive, None, None, &controller).is_none());
        index::read_shared(&archive, None, &controller).unwrap();
        let expected = expected_size(&archive, None, None, &controller).unwrap();
        assert_eq!(expected.size, 3);
        assert!(expected.complete);
    }

    fn asks() -> std::sync::Arc<std::sync::atomic::AtomicU32> {
        std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0))
    }

    fn tar_sized(entries: &[(&str, usize)]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, size) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(*size as u64);
            header.set_mode(0o644);
            tar.append_data(&mut header, *name, &vec![0; *size][..])
                .unwrap();
        }
        tar.into_inner().unwrap()
    }

    /// Every way an archive is written tells the watch, not only a decoder's
    #[test]
    fn every_format_is_watched() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let zip_path = tmp.path().join("a.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        zip.start_file("big", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&vec![0; 1 << 20]).unwrap();
        zip.finish().unwrap();
        let sevenz_path = tmp.path().join("a.7z");
        write_7z(&sevenz_path, &[("big", &vec![0; 1 << 20])]);
        let tar_path = tmp.path().join("a.tar");
        fs::write(&tar_path, tar_sized(&[("big", 1 << 20)])).unwrap();

        for archive in [zip_path, sevenz_path, tar_path] {
            let out = tmp.path().join(format!(
                "out-{}",
                archive.extension().unwrap().to_string_lossy()
            ));
            fs::create_dir(&out).unwrap();
            let asked = asks();
            let watch = watch_on(&out, std::sync::Arc::clone(&asked), true);
            extract_watched(
                &archive,
                &out,
                &None,
                None,
                Some(&watch),
                &Controller::default(),
            )
            .unwrap();
            assert_eq!(
                asked.load(std::sync::atomic::Ordering::Relaxed),
                1,
                "{archive:?}"
            );
            assert_eq!(fs::metadata(out.join("big")).unwrap().len(), 1 << 20);
        }
    }

    /// What a selection passes over in the stream is not what it grows by
    #[test]
    fn a_selection_is_watched_only_for_what_it_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar");
        fs::write(&archive, tar_sized(&[("unwanted", 1 << 20), ("small", 10)])).unwrap();
        let out = tmp.path().join("out");
        fs::create_dir(&out).unwrap();
        let asked = asks();
        let watch = watch_on(&out, std::sync::Arc::clone(&asked), true);
        let only = index::Only::new(&[PathBuf::from("small")]);
        extract_watched(
            &archive,
            &out,
            &None,
            Some(&only),
            Some(&watch),
            &Controller::default(),
        )
        .unwrap();
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert!(out.join("small").exists());
    }

    /// One entry bigger than the room stops before a byte of it
    #[test]
    fn a_tar_entry_too_big_for_the_margin_is_not_begun() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar");
        fs::write(&archive, tar_sized(&[("big", 32 << 20)])).unwrap();
        let out = tmp.path().join("out");
        fs::create_dir(&out).unwrap();
        let mut watch = watch_on(&out, asks(), true);
        watch.suspicious_from = u64::MAX;
        watch.margin = crate::operation::free_space(&out).unwrap();
        extract_watched(
            &archive,
            &out,
            &None,
            None,
            Some(&watch),
            &Controller::default(),
        )
        .unwrap_err();
        assert!(!out.join("big").exists());
    }

    /// An earlier version of a name is written too, so the last one's size is
    /// no bound on what comes out
    #[test]
    fn a_replaced_name_leaves_the_size_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar");
        fs::write(&archive, tar_sized(&[("a", 1 << 20), ("a", 1)])).unwrap();
        let controller = Controller::default();
        let expected = expected_size(&archive, None, None, &controller).unwrap();
        assert_eq!(expected.size, 1);
        assert!(!expected.complete);
        let only = index::Only::new(&[PathBuf::from("a")]);
        assert!(
            !expected_size(&archive, None, Some(&only), &controller)
                .unwrap()
                .complete
        );
    }

    /// A name that only repeats an empty file holds nothing back
    #[test]
    fn repeating_an_empty_name_leaves_the_size_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar");
        fs::write(&archive, tar_sized(&[("a", 0), ("a", 5)])).unwrap();
        let expected = expected_size(&archive, None, None, &Controller::default()).unwrap();
        assert_eq!(expected.size, 5);
        assert!(expected.complete);
    }

    /// A hard link holds nothing, but what it links to comes out for it
    #[test]
    fn a_chosen_hard_link_counts_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar");
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(1 << 20);
        header.set_mode(0o644);
        tar.append_data(&mut header, "target", &vec![0; 1 << 20][..])
            .unwrap();
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Link);
        link.set_size(0);
        link.set_mode(0o644);
        tar.append_link(&mut link, "link", "target").unwrap();
        fs::write(&archive, tar.into_inner().unwrap()).unwrap();
        let only = index::Only::new(&[PathBuf::from("link")]);
        let expected = expected_size(&archive, None, Some(&only), &Controller::default()).unwrap();
        assert_eq!(expected.size, 1 << 20);
    }

    /// A file that a later name makes a folder of was written all the same
    #[test]
    fn a_file_made_a_folder_leaves_the_size_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar");
        fs::write(&archive, tar_sized(&[("a", 1 << 20), ("a/b", 1)])).unwrap();
        let expected = expected_size(&archive, None, None, &Controller::default()).unwrap();
        assert_eq!(expected.size, 1);
        assert!(!expected.complete);
    }

    /// As is a file coming after what is already a folder by its name
    #[test]
    fn a_file_over_a_folder_leaves_the_size_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar");
        fs::write(&archive, tar_sized(&[("a/b", 1), ("a", 1 << 20)])).unwrap();
        let expected = expected_size(&archive, None, None, &Controller::default()).unwrap();
        assert_eq!(expected.size, 1);
        assert!(!expected.complete);
    }

    /// An empty file costs nothing to lose
    #[test]
    fn an_empty_file_made_a_folder_leaves_the_size_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar");
        fs::write(&archive, tar_sized(&[("a", 0), ("a/b", 1)])).unwrap();
        let expected = expected_size(&archive, None, None, &Controller::default()).unwrap();
        assert!(expected.complete);
    }

    /// Asked already, it does not ask again, and still keeps the room
    #[test]
    fn asked_already_keeps_only_the_room() {
        let tmp = tempfile::tempdir().unwrap();
        let asked = asks();
        let mut watch =
            watch_on(tmp.path(), std::sync::Arc::clone(&asked), true).asked_already(true);
        assert!(watch.about_to_write(1 << 20).is_ok());
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 0);
        watch.margin = u64::MAX;
        assert!(watch.about_to_write(32 << 20).is_err());
    }
}
