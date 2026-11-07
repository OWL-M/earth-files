//! What an archive holds, read without unpacking it: names, sizes and dates,
//! in memory, with nothing written to disk.
//!
//! Zip and 7z keep a list to read. A tar is walked from header to header,
//! and a compressed one has to be decompressed to find them, its contents
//! thrown away as it goes. Nothing is kept of any file's contents, so an
//! archive claiming to expand to any size is harmless to look at.

use super::{Codec, S_IFLNK, S_IFMT, check, decoder, holds_a_tar, stopped};
use crate::mime_icon::mime_for_path;
use crate::operation::{Controller, OpReader, OperationError, OperationErrorType};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

/// Past this many entries a listing stops, and says it is incomplete.
const MAX_ENTRIES: usize = 1_000_000;

/// Past this many bytes of names it stops too: however few entries there are,
/// a crafted archive's long names would otherwise take any amount of memory.
const MAX_PATH_BYTES: usize = 256 * 1024 * 1024;

/// The types of archive that can be looked inside: the rest compress a single
/// file, which Extract already gives.
pub const BROWSABLE_TYPES: &[&str] = &[
    "application/zip",
    "application/x-7z-compressed",
    "application/x-tar",
    "application/x-compressed-tar",
    "application/x-zstd-compressed-tar",
    #[cfg(feature = "bzip2")]
    "application/x-bzip-compressed-tar",
    #[cfg(feature = "bzip2")]
    "application/x-bzip2-compressed-tar",
    #[cfg(feature = "lzma-rust2")]
    "application/x-xz-compressed-tar",
    #[cfg(feature = "lzma-rust2")]
    "application/x-lzma-compressed-tar",
];

/// What to extract of an archive: these paths and all under those of them that
/// are folders, by the names an [`ArchiveIndex`] gives them.
#[derive(Clone, Debug, Default)]
pub struct Only {
    asked: HashSet<PathBuf>,
    /// Entries needed only for what was asked to come out: the targets of hard
    /// links, written to where it is staged and not brought out of it.
    needed: HashSet<PathBuf>,
}

impl Only {
    /// Only `paths`.
    pub fn new(paths: &[PathBuf]) -> Self {
        Self {
            asked: paths.iter().cloned().collect(),
            needed: HashSet::new(),
        }
    }

    /// This, needing as well the entries the hard links it wants in `index` are
    /// links to, followed as far as they go. A tar's hard link holds no data of
    /// its own: without its target it comes out as nothing.
    pub fn with_hard_link_targets(mut self, index: &ArchiveIndex) -> Self {
        let mut waiting: Vec<&PathBuf> = index
            .hard_links
            .iter()
            .filter(|(link, _)| {
                link.ancestors()
                    .any(|ancestor| self.asked.contains(ancestor))
            })
            .flat_map(|(_, targets)| targets)
            .collect();
        while let Some(path) = waiting.pop() {
            if self.needed.insert(path.clone())
                && let Some(targets) = index.hard_links.get(path)
            {
                waiting.extend(targets);
            }
        }
        self
    }

    /// Whether the entry the archive calls `raw` is wanted.
    pub fn wants(&self, raw: &[u8], backslashes: bool) -> bool {
        clean(raw, backslashes).is_some_and(|path| {
            self.needed.contains(&path)
                || path
                    .ancestors()
                    .any(|ancestor| self.asked.contains(ancestor))
        })
    }

    /// What gets written of those asked for: the tops, and what hard links among
    /// them need.
    pub fn chosen_and_needed(&self) -> Vec<PathBuf> {
        self.tops()
            .into_iter()
            .chain(self.needed.iter().cloned())
            .collect()
    }

    /// The paths asked for that are not already in another of them, each of
    /// which is one thing at the top of what comes out.
    pub fn tops(&self) -> Vec<PathBuf> {
        let mut tops: Vec<PathBuf> = self
            .asked
            .iter()
            .filter(|path| {
                !path
                    .ancestors()
                    .skip(1)
                    .any(|ancestor| self.asked.contains(ancestor))
            })
            .cloned()
            .collect();
        tops.sort();
        tops
    }
}

/// What an entry is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// A file.
    File,
    /// A folder, named by the archive or only through what is in it.
    Folder,
    /// A link, shown but never followed.
    Link,
}

/// One thing in an archive.
#[derive(Clone, Debug)]
pub struct IndexEntry {
    /// Where it is in the archive, cleaned up and safe to join to a folder.
    pub path: PathBuf,
    /// What it is.
    pub kind: EntryKind,
    /// How big the archive says it is, when it says.
    pub size: Option<u64>,
    /// When it was last changed, when the archive says.
    pub modified: Option<SystemTime>,
    /// Its unix mode, when the archive keeps one.
    pub mode: Option<u32>,
    /// Whether its contents need a password.
    pub encrypted: bool,
}

impl IndexEntry {
    /// Its name, the last part of its path.
    pub fn name(&self) -> &OsStr {
        self.path
            .file_name()
            .unwrap_or_else(|| self.path.as_os_str())
    }

    /// Whether Show Hidden decides if it shows.
    pub fn hidden(&self) -> bool {
        self.name().as_bytes().first() == Some(&b'.')
    }
}

/// What an archive holds.
#[derive(Clone, Debug)]
pub struct ArchiveIndex {
    entries: Vec<IndexEntry>,
    by_path: HashMap<PathBuf, usize>,
    children: HashMap<PathBuf, Vec<usize>>,
    /// Every hard link a tar holds, by its name, to what it links to: all of
    /// them, not only the entry that wins a name. Extracting replays the stream,
    /// so a link that a later entry replaces still needs its target on the way.
    hard_links: HashMap<PathBuf, HashSet<PathBuf>>,
    /// How many targets `hard_links` holds. A name can link to any number of
    /// them, so they have a cap of their own beside the entries'.
    hard_link_targets: usize,
    /// How many entries were left out for names that reach outside the
    /// archive.
    pub unsafe_names: usize,
    /// Data was listed under a name and then lost to what else came by that
    /// name, or to a folder of it: extracting writes it all the same, and it
    /// is in no entry's size.
    pub replaced: bool,
    /// The listing stopped, at [`MAX_ENTRIES`] or [`MAX_PATH_BYTES`].
    pub truncated: bool,
    /// The listing ended early on damage, and why.
    pub damaged: Option<String>,
    max_entries: usize,
    max_path_bytes: usize,
    path_bytes: usize,
}

impl Default for ArchiveIndex {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            by_path: HashMap::new(),
            children: HashMap::new(),
            hard_links: HashMap::new(),
            hard_link_targets: 0,
            unsafe_names: 0,
            replaced: false,
            truncated: false,
            damaged: None,
            max_entries: MAX_ENTRIES,
            max_path_bytes: MAX_PATH_BYTES,
            path_bytes: 0,
        }
    }
}

impl ArchiveIndex {
    /// How many entries it holds, folders included.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry at `path`, the root having none.
    pub fn entry(&self, path: &Path) -> Option<&IndexEntry> {
        self.by_path.get(path).map(|&index| &self.entries[index])
    }

    /// What is directly in the folder `inside`: `None` when there is no such
    /// folder. The root is the empty path.
    pub fn listing(&self, inside: &Path) -> Option<Vec<&IndexEntry>> {
        let known = inside.as_os_str().is_empty()
            || self
                .entry(inside)
                .is_some_and(|entry| entry.kind == EntryKind::Folder);
        known.then(|| {
            self.children.get(inside).map_or_else(Vec::new, |children| {
                children.iter().map(|&index| &self.entries[index]).collect()
            })
        })
    }

    /// Everything under the folder `inside`, however deep, each with its own
    /// path; none when there is no such folder. The root is the empty path.
    pub fn descendants(&self, inside: &Path) -> Vec<&IndexEntry> {
        let mut found = Vec::new();
        let mut stack: Vec<usize> = self.listing_indices(inside);
        while let Some(index) = stack.pop() {
            let entry = &self.entries[index];
            found.push(entry);
            if let Some(children) = self.children.get(&entry.path) {
                stack.extend(children);
            }
        }
        found
    }

    fn listing_indices(&self, inside: &Path) -> Vec<usize> {
        self.children.get(inside).cloned().unwrap_or_default()
    }

    /// The expanded size of everything, and whether every size was known.
    pub fn total_size(&self) -> (u64, bool) {
        self.sum(self.entries.iter())
    }

    /// The expanded size of `paths` with all that is in those of them that
    /// are folders, and whether every size was known.
    pub fn size_of(&self, paths: &[PathBuf]) -> (u64, bool) {
        let mut stack: Vec<usize> = paths
            .iter()
            .filter_map(|path| self.by_path.get(path).copied())
            .collect();
        let mut found = Vec::new();
        // A search can select a folder and what is in it: counted once
        let mut seen = HashSet::new();
        while let Some(index) = stack.pop() {
            if !seen.insert(index) {
                continue;
            }
            let entry = &self.entries[index];
            found.push(entry);
            if let Some(children) = self.children.get(&entry.path) {
                stack.extend(children);
            }
        }
        self.sum(found.into_iter())
    }

    fn sum<'a>(&self, entries: impl Iterator<Item = &'a IndexEntry>) -> (u64, bool) {
        let mut known = true;
        let mut total = 0_u64;
        for entry in entries.filter(|entry| entry.kind == EntryKind::File) {
            match entry.size {
                Some(size) => total = total.saturating_add(size),
                None => known = false,
            }
        }
        (total, known)
    }

    /// Adds what the archive calls `raw`. Returns false once full.
    fn add(&mut self, raw: &[u8], backslashes: bool, entry: Listed) -> bool {
        let Some(path) = clean(raw, backslashes) else {
            // The root's own entry, "./", names nothing and is no danger
            if !only_current_dir(raw) {
                self.unsafe_names += 1;
            }
            return true;
        };
        if let Some(parent) = path.parent()
            && !self.add_folders(parent)
        {
            return false;
        }
        let Listed {
            kind,
            size,
            modified,
            mode,
            encrypted,
            hard_link_to,
        } = entry;
        if let Some(target) = hard_link_to
            && !self.add_hard_link(&path, target)
        {
            return false;
        }
        if let Some(&index) = self.by_path.get(&path) {
            let old = &mut self.entries[index];
            self.replaced |= old.kind == EntryKind::File && old.size != Some(0);
            // What is in a folder keeps it a folder, whatever came after
            if !(self.children.contains_key(&path) && kind != EntryKind::Folder) {
                old.kind = kind;
                old.size = size;
            } else {
                self.replaced |= kind == EntryKind::File && size != Some(0);
            }
            old.modified = modified;
            old.mode = mode;
            old.encrypted = encrypted;
            return true;
        }
        self.push(IndexEntry {
            path,
            kind,
            size,
            modified,
            mode,
            encrypted,
        })
    }

    /// Notes that the hard link `link` links to `target`, once however often an
    /// archive repeats it. Returns false, and notes nothing, once full.
    fn add_hard_link(&mut self, link: &Path, target: PathBuf) -> bool {
        let targets = self.hard_links.get(link);
        if targets.is_some_and(|targets| targets.contains(&target)) {
            return true;
        }
        let name_bytes = if targets.is_none() {
            link.as_os_str().len()
        } else {
            0
        };
        let path_bytes = self.path_bytes + name_bytes + target.as_os_str().len();
        if self.hard_link_targets >= self.max_entries || path_bytes > self.max_path_bytes {
            self.truncated = true;
            return false;
        }
        self.path_bytes = path_bytes;
        self.hard_link_targets += 1;
        self.hard_links
            .entry(link.to_path_buf())
            .or_default()
            .insert(target);
        true
    }

    /// Makes sure `path` and the folders above it have entries, as an archive
    /// may name a folder only through what is in it, and that it is a folder:
    /// what has something in it is one, whatever came before by its name.
    /// Returns false once full.
    fn add_folders(&mut self, path: &Path) -> bool {
        let mut missing = Vec::new();
        for ancestor in path.ancestors() {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            if let Some(&index) = self.by_path.get(ancestor) {
                let entry = &mut self.entries[index];
                if entry.kind != EntryKind::Folder {
                    self.replaced |= entry.kind == EntryKind::File && entry.size != Some(0);
                    entry.kind = EntryKind::Folder;
                    entry.size = None;
                }
                break;
            }
            missing.push(ancestor);
        }
        missing.into_iter().rev().all(|folder| {
            self.push(IndexEntry {
                path: folder.to_path_buf(),
                kind: EntryKind::Folder,
                size: None,
                modified: None,
                mode: None,
                encrypted: false,
            })
        })
    }

    /// Returns false, and adds nothing, once full.
    fn push(&mut self, entry: IndexEntry) -> bool {
        let path_bytes = self.path_bytes + entry.path.as_os_str().len();
        if self.entries.len() >= self.max_entries || path_bytes > self.max_path_bytes {
            self.truncated = true;
            return false;
        }
        self.path_bytes = path_bytes;
        let index = self.entries.len();
        let parent = entry.path.parent().unwrap_or(Path::new("")).to_path_buf();
        self.children.entry(parent).or_default().push(index);
        self.by_path.insert(entry.path.clone(), index);
        self.entries.push(entry);
        true
    }
}

/// An entry as an archive states it.
struct Listed {
    kind: EntryKind,
    size: Option<u64>,
    modified: Option<SystemTime>,
    mode: Option<u32>,
    encrypted: bool,
    hard_link_to: Option<PathBuf>,
}

/// `raw` as a path inside the archive: without `.` and empty parts, so `./a`,
/// `a/./b` and `a//b/` name what they mean, and `None` when it reaches outside
/// or names nothing, or is longer than a path can be, which no folder could
/// take. `backslashes`: zip and 7z may separate with them, where in a tar it is
/// part of a name.
pub(super) fn clean(raw: &[u8], backslashes: bool) -> Option<PathBuf> {
    if raw.len() > libc::PATH_MAX as usize {
        return None;
    }
    if raw.first() == Some(&b'/') || (backslashes && raw.first() == Some(&b'\\')) {
        return None;
    }
    let mut path = PathBuf::new();
    for part in raw.split(|&byte| byte == b'/' || (backslashes && byte == b'\\')) {
        match part {
            b"" | b"." => {}
            b".." => return None,
            part => path.push(OsStr::from_bytes(part)),
        }
    }
    (!path.as_os_str().is_empty()).then_some(path)
}

/// Whether `raw` names only the archive's root, as `./` does.
fn only_current_dir(raw: &[u8]) -> bool {
    raw.split(|&byte| byte == b'/')
        .all(|part| matches!(part, b"" | b"."))
        && raw.first() != Some(&b'/')
}

/// Reads what the archive at `path` holds. `password` is for a 7z whose list
/// itself is encrypted; zip and tar list without one.
///
/// Damage ends the listing where it is, and [`ArchiveIndex::damaged`] says so:
/// only what could not be listed at all is an error.
pub fn index(
    path: &Path,
    password: Option<&str>,
    controller: &Controller,
) -> Result<ArchiveIndex, OperationError> {
    let mime = mime_for_path(path, false);
    let mut out = ArchiveIndex::default();
    match mime.as_str() {
        "application/zip" => index_zip(path, controller, &mut out)?,
        "application/x-7z-compressed" => index_sevenz(path, password, controller, &mut out)?,
        "application/x-tar" => {
            let mut archive =
                tar::Archive::new(fs::File::open(path).map_err(OperationError::from_err)?);
            let entries = archive
                .entries_with_seek()
                .map_err(OperationError::from_err)?;
            index_tar(entries, controller, &mut out)?;
        }
        "application/x-compressed-tar" => {
            index_compressed(path, Codec::Gzip, true, controller, &mut out)?;
        }
        "application/gzip" => index_compressed(path, Codec::Gzip, false, controller, &mut out)?,
        "application/x-zstd-compressed-tar" => {
            index_compressed(path, Codec::Zstd, true, controller, &mut out)?;
        }
        "application/zstd" => index_compressed(path, Codec::Zstd, false, controller, &mut out)?,
        #[cfg(feature = "bzip2")]
        "application/x-bzip-compressed-tar" | "application/x-bzip2-compressed-tar" => {
            index_compressed(path, Codec::Bzip2, true, controller, &mut out)?;
        }
        #[cfg(feature = "bzip2")]
        "application/x-bzip" | "application/x-bzip2" => {
            index_compressed(path, Codec::Bzip2, false, controller, &mut out)?;
        }
        #[cfg(feature = "lzma-rust2")]
        "application/x-xz-compressed-tar" => {
            index_compressed(path, Codec::Xz, true, controller, &mut out)?;
        }
        #[cfg(feature = "lzma-rust2")]
        "application/x-xz" => index_compressed(path, Codec::Xz, false, controller, &mut out)?,
        #[cfg(feature = "lzma-rust2")]
        "application/x-lzma-compressed-tar" => {
            index_compressed(path, Codec::Lzma, true, controller, &mut out)?;
        }
        #[cfg(feature = "lzma-rust2")]
        "application/x-lzma" => index_compressed(path, Codec::Lzma, false, controller, &mut out)?,
        _ => {
            return Err(OperationError::from_err(format!(
                "unsupported mime type {mime:?}"
            )));
        }
    }
    Ok(out)
}

/// Waits while the listing is paused, and ends it once it is stopped.
fn go_on(controller: &Controller) -> Result<(), OperationError> {
    check(controller)
        .map_err(|err| stopped(controller).unwrap_or_else(|| OperationError::from_err(err)))
}

/// What a decompressor gives, stopping when the operation does. The file under
/// it is read too rarely to notice: a highly compressed archive turns one read
/// of it into gigabytes.
struct Stoppable<R> {
    reader: R,
    controller: Controller,
}

impl<R: Read> Read for Stoppable<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        check(&self.controller)?;
        self.reader.read(buf)
    }
}

/// A file that compresses a single file has nothing to look inside.
fn index_compressed(
    path: &Path,
    codec: Codec,
    tar: bool,
    controller: &Controller,
    out: &mut ArchiveIndex,
) -> Result<(), OperationError> {
    if !(tar || holds_a_tar(path, codec)) {
        return Err(OperationError::from_err(io::Error::new(
            io::ErrorKind::InvalidData,
            "it compresses a single file",
        )));
    }
    let reader = OpReader::new(path, controller.clone()).map_err(OperationError::from_err)?;
    let reader = decoder(codec, reader).map_err(OperationError::from_err)?;
    let mut archive = tar::Archive::new(Stoppable {
        reader,
        controller: controller.clone(),
    });
    let entries = archive.entries().map_err(OperationError::from_err)?;
    index_tar(entries, controller, out)
}

fn index_tar<R: Read>(
    entries: tar::Entries<'_, R>,
    controller: &Controller,
    out: &mut ArchiveIndex,
) -> Result<(), OperationError> {
    let mut any = false;
    for entry in entries {
        go_on(controller)?;
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                if let Some(end) = stopped(controller) {
                    return Err(end);
                }
                // Not even a first entry: no tar at all
                if !any {
                    return Err(OperationError::from_err(err));
                }
                out.damaged = Some(err.to_string());
                return Ok(());
            }
        };
        any = true;
        let header = entry.header();
        let kind = match header.entry_type() {
            tar::EntryType::Directory => EntryKind::Folder,
            tar::EntryType::Symlink | tar::EntryType::Link => EntryKind::Link,
            _ => EntryKind::File,
        };
        let hard_link_to = (header.entry_type() == tar::EntryType::Link)
            .then(|| entry.link_name_bytes())
            .flatten()
            .and_then(|target| clean(&target, false));
        let listed = Listed {
            hard_link_to,
            kind,
            size: (kind == EntryKind::File).then(|| entry.size()),
            modified: header
                .mtime()
                .ok()
                .and_then(|secs| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs))),
            mode: header.mode().ok(),
            encrypted: false,
        };
        if !out.add(&entry.path_bytes(), false, listed) {
            return Ok(());
        }
    }
    Ok(())
}

fn index_zip(
    path: &Path,
    controller: &Controller,
    out: &mut ArchiveIndex,
) -> Result<(), OperationError> {
    let file = fs::File::open(path).map_err(OperationError::from_err)?;
    let mut archive = match zip::ZipArchive::new(io::BufReader::new(file)) {
        Ok(archive) => archive,
        // Its list is lost: the entries are read from the start of the file,
        // as extraction reads them
        Err(err) => return index_zip_stream(path, err, controller, out),
    };
    for i in 0..archive.len() {
        go_on(controller)?;
        let Ok(file) = archive.by_index_raw(i) else {
            out.damaged = Some(format!("entry {i} could not be read"));
            continue;
        };
        let listed = zip_listed(&file, true);
        let raw = zip_name(&file);
        if !out.add(&raw, true, listed) {
            return Ok(());
        }
    }
    Ok(())
}

fn index_zip_stream(
    path: &Path,
    err: zip::result::ZipError,
    controller: &Controller,
    out: &mut ArchiveIndex,
) -> Result<(), OperationError> {
    // Each entry's data is read past when it is dropped, however big: through
    // the operation's reader, so a stop reaches that too
    let file = OpReader::new(path, controller.clone()).map_err(OperationError::from_err)?;
    let mut reader = io::BufReader::new(file);
    let mut any = false;
    while let Ok(Some(file)) = zip::read::read_zipfile_from_stream(&mut reader) {
        go_on(controller)?;
        any = true;
        // A size only comes after the data of an entry streamed in: unknown
        let listed = zip_listed(&file, false);
        let raw = zip_name(&file);
        if !out.add(&raw, true, listed) {
            return Ok(());
        }
    }
    // A stop ends the reads, which looks just like the entries ending
    go_on(controller)?;
    if !any {
        return Err(OperationError::from_err(err));
    }
    out.damaged = Some(err.to_string());
    Ok(())
}

/// An entry's name, decoded as extraction names it, since an old zip's names
/// are not UTF-8. One that cannot be decoded keeps its bytes.
fn zip_name<R: Read>(file: &zip::read::ZipFile<'_, R>) -> Vec<u8> {
    file.name().map_or_else(
        |_| file.name_raw().to_vec(),
        |name| name.as_bytes().to_vec(),
    )
}

fn zip_listed<R: Read>(file: &zip::read::ZipFile<'_, R>, sizes_known: bool) -> Listed {
    let mode = file.unix_mode();
    let kind = if file.is_dir() {
        EntryKind::Folder
    } else if mode.is_some_and(|mode| mode & S_IFMT == S_IFLNK) {
        EntryKind::Link
    } else {
        EntryKind::File
    };
    let size = file.size();
    Listed {
        hard_link_to: None,
        kind,
        size: (kind == EntryKind::File && (sizes_known || size > 0)).then_some(size),
        modified: file
            .last_modified()
            .and_then(super::zip_date_time_to_system_time),
        mode,
        encrypted: file.encrypted(),
    }
}

fn index_sevenz(
    path: &Path,
    password: Option<&str>,
    controller: &Controller,
    out: &mut ArchiveIndex,
) -> Result<(), OperationError> {
    use sevenz_rust2::{Archive, EncoderMethod, Error, Password};

    let mut source = fs::File::open(path).map_err(OperationError::from_err)?;
    let password = password.map_or_else(Password::empty, Password::from);
    let archive = Archive::read(&mut source, &password).map_err(|err| {
        if matches!(err, Error::PasswordRequired | Error::MaybeBadPassword(_)) {
            OperationError::from_kind(OperationErrorType::PasswordRequired)
        } else {
            OperationError::from_err(err)
        }
    })?;
    for (i, entry) in archive.files.iter().enumerate() {
        go_on(controller)?;
        if entry.is_anti_item() {
            continue;
        }
        let attributes = entry.windows_attributes();
        let mode =
            (entry.has_windows_attributes && attributes & 0x8000 != 0).then_some(attributes >> 16);
        let kind = if entry.is_directory() {
            EntryKind::Folder
        } else if mode.is_some_and(|mode| mode & S_IFMT == S_IFLNK) {
            EntryKind::Link
        } else {
            EntryKind::File
        };
        let encrypted = archive.stream_map.file_block_index[i].is_some_and(|block| {
            archive.blocks[block]
                .coders
                .iter()
                .any(|coder| coder.encoder_method_id() == EncoderMethod::ID_AES256_SHA256)
        });
        let listed = Listed {
            hard_link_to: None,
            kind,
            size: (kind == EntryKind::File).then_some(entry.size),
            modified: entry
                .has_last_modified_date
                .then(|| SystemTime::from(entry.last_modified_date())),
            mode,
            encrypted,
        };
        if !out.add(entry.name.as_bytes(), true, listed) {
            return Ok(());
        }
    }
    Ok(())
}

/// An index being looked at, and what it was read from.
struct Shared {
    stamp: Stamp,
    index: Arc<ArchiveIndex>,
    password: Option<String>,
}

/// The archive as it was when read: its modified time and size.
type Stamp = (Option<SystemTime>, u64);

/// The indexes of the archives some tab is in or can go back to: a listing is
/// scanned on a worker, which has no tab to ask, and a compressed tar would
/// otherwise be decompressed again for each of its folders.
static SHARED: Mutex<Option<HashMap<PathBuf, Shared>>> = Mutex::new(None);

fn stamp(path: &Path) -> io::Result<Stamp> {
    let metadata = fs::metadata(path)?;
    Ok((metadata.modified().ok(), metadata.len()))
}

/// The index of `path`, read now unless it is held and the archive has not
/// changed since. A held password is used when none is given.
pub fn read_shared(
    path: &Path,
    password: Option<&str>,
    controller: &Controller,
) -> Result<Arc<ArchiveIndex>, OperationError> {
    let now = stamp(path).map_err(OperationError::from_err)?;
    let held_password = {
        let shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        let held = shared.as_ref().and_then(|shared| shared.get(path));
        if let Some(held) = held.filter(|held| held.stamp == now) {
            return Ok(held.index.clone());
        }
        held.and_then(|held| held.password.clone())
    };
    let password = password.map(str::to_owned).or(held_password);
    let read = Arc::new(index(path, password.as_deref(), controller)?);
    SHARED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .insert(
            path.to_path_buf(),
            Shared {
                stamp: now,
                index: read.clone(),
                password,
            },
        );
    Ok(read)
}

/// The index of `path` if it is held, without asking the disk: for what runs
/// on the event loop.
pub fn held(path: &Path) -> Option<Arc<ArchiveIndex>> {
    SHARED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()?
        .get(path)
        .map(|held| held.index.clone())
}

/// Lets go of every index but those of `archives`.
pub fn keep_shared(archives: &[&Path]) {
    if let Some(shared) = SHARED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        shared.retain(|path, _| archives.contains(&path.as_path()));
    }
}

#[cfg(test)]
mod tests {
    use super::{ArchiveIndex, EntryKind, Listed, clean, index, keep_shared, read_shared};
    use crate::operation::{Controller, OperationErrorType};
    use std::io::Write;
    use std::path::{Path, PathBuf};

    fn file(size: u64) -> Listed {
        Listed {
            hard_link_to: None,
            kind: EntryKind::File,
            size: Some(size),
            modified: None,
            mode: None,
            encrypted: false,
        }
    }

    fn names(index: &ArchiveIndex, inside: &str) -> Vec<String> {
        let mut names: Vec<String> = index
            .listing(Path::new(inside))
            .expect("a folder")
            .iter()
            .map(|entry| entry.name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    fn tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(1_000);
            tar.append_data(&mut header, name, *data).unwrap();
        }
        tar.into_inner().unwrap()
    }

    /// `./a`, `a/./b` and `a//b/` name what they mean, as GVFS reads them;
    /// what reaches outside the archive names nothing
    #[test]
    fn names_are_cleaned_up() {
        for (raw, cleaned) in [("a", "a"), ("./a", "a"), ("a/./b", "a/b"), ("a//b/", "a/b")] {
            assert_eq!(clean(raw.as_bytes(), false), Some(PathBuf::from(cleaned)));
        }
        for raw in ["../x", "a/../../x", "/etc/passwd", "", "./", "."] {
            assert_eq!(clean(raw.as_bytes(), false), None, "{raw}");
        }
        assert_eq!(clean(br"a\b", true), Some(PathBuf::from("a/b")));
        assert_eq!(clean(br"a\b", false), Some(PathBuf::from(r"a\b")));
        assert_eq!(clean(br"..\x", true), None);
    }

    /// A name that is not UTF-8 stays as it is
    #[test]
    fn a_name_in_other_bytes_is_kept() {
        let mut out = ArchiveIndex::default();
        assert!(out.add(b"dir/caf\xe9", false, file(1)));
        let entry = out.entry(Path::new(std::ffi::OsStr::new("dir"))).unwrap();
        assert_eq!(entry.kind, EntryKind::Folder);
        let listing = out.listing(Path::new("dir")).unwrap();
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(listing[0].name().as_bytes(), b"caf\xe9");
    }

    #[test]
    fn folders_named_only_through_their_contents_are_listed() {
        let mut out = ArchiveIndex::default();
        out.add(b"a/b/c.txt", false, file(3));
        assert_eq!(names(&out, ""), ["a"]);
        assert_eq!(names(&out, "a"), ["b"]);
        assert_eq!(names(&out, "a/b"), ["c.txt"]);
        assert!(out.listing(Path::new("a/b/c.txt")).is_none(), "a file");
        assert!(out.listing(Path::new("nope")).is_none());
    }

    /// The later of two entries of one name wins, as extraction would leave it
    #[test]
    fn the_later_of_two_entries_wins() {
        let mut out = ArchiveIndex::default();
        out.add(b"a.txt", false, file(1));
        out.add(b"./a.txt", false, file(2));
        assert_eq!(out.len(), 1);
        assert_eq!(out.entry(Path::new("a.txt")).unwrap().size, Some(2));
    }

    /// A folder keeps its contents, whatever comes after it by its name
    #[test]
    fn a_file_named_as_a_folder_leaves_it_a_folder() {
        let mut out = ArchiveIndex::default();
        out.add(b"a/b.txt", false, file(1));
        out.add(b"a", false, file(2));
        assert_eq!(out.entry(Path::new("a")).unwrap().kind, EntryKind::Folder);
        assert_eq!(names(&out, "a"), ["b.txt"]);
    }

    /// What reaches outside is left out and counted; the root's own entry is
    /// no danger and is not
    #[test]
    fn names_reaching_outside_are_counted() {
        let mut out = ArchiveIndex::default();
        out.add(b"../x", false, file(1));
        out.add(b"/etc/passwd", false, file(1));
        out.add(b"./", false, file(0));
        out.add(b"ok", false, file(1));
        assert_eq!(out.unsafe_names, 2);
        assert_eq!(names(&out, ""), ["ok"]);
    }

    #[test]
    fn sizes_add_up_and_say_when_one_is_unknown() {
        let mut out = ArchiveIndex::default();
        out.add(b"a/one", false, file(10));
        out.add(b"a/two", false, file(5));
        out.add(b"b", false, file(100));
        assert_eq!(out.total_size(), (115, true));
        assert_eq!(out.size_of(&[PathBuf::from("a")]), (15, true));
        assert_eq!(
            out.size_of(&[PathBuf::from("a/one"), PathBuf::from("b")]),
            (110, true)
        );
        out.add(
            b"c",
            false,
            Listed {
                size: None,
                ..file(0)
            },
        );
        assert_eq!(out.total_size(), (115, false));
    }

    /// However deep, from the folder asked for and no further out
    #[test]
    fn descendants_reach_all_the_way_down_from_a_folder() {
        let mut out = ArchiveIndex::default();
        out.add(b"a/one", false, file(1));
        out.add(b"a/b/two", false, file(1));
        out.add(b"c", false, file(1));
        let paths = |inside: &str| {
            let mut paths: Vec<PathBuf> = out
                .descendants(Path::new(inside))
                .iter()
                .map(|entry| entry.path.clone())
                .collect();
            paths.sort();
            paths
        };
        assert_eq!(
            paths("a"),
            [
                PathBuf::from("a/b"),
                PathBuf::from("a/b/two"),
                PathBuf::from("a/one")
            ]
        );
        assert_eq!(paths("").len(), 5);
        assert!(paths("nope").is_empty());
    }

    #[test]
    fn dot_names_are_hidden() {
        let mut out = ArchiveIndex::default();
        out.add(b".secret", false, file(1));
        out.add(b"plain", false, file(1));
        assert!(out.entry(Path::new(".secret")).unwrap().hidden());
        assert!(!out.entry(Path::new("plain")).unwrap().hidden());
    }

    #[test]
    fn a_zip_lists_its_entries_and_sizes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.zip");
        write_zip(&path, &[("a.txt", b"12345"), ("dir/b.txt", b"123")]);
        let out = index(&path, None, &Controller::default()).unwrap();
        assert_eq!(names(&out, ""), ["a.txt", "dir"]);
        assert_eq!(out.entry(Path::new("dir/b.txt")).unwrap().size, Some(3));
        assert_eq!(out.total_size(), (8, true));
        assert!(out.damaged.is_none());
    }

    /// With its list cut away the entries are read from the start, and it says
    /// it is damaged
    #[test]
    fn a_zip_without_its_list_still_lists() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.zip");
        write_zip(&path, &[("a.txt", b"12345"), ("b.txt", b"123")]);
        let bytes = std::fs::read(&path).unwrap();
        let cut = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        std::fs::write(&path, &bytes[..cut]).unwrap();
        let out = index(&path, None, &Controller::default()).unwrap();
        assert_eq!(names(&out, ""), ["a.txt", "b.txt"]);
        assert!(out.damaged.is_some());
    }

    #[test]
    fn a_tar_lists_its_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.tar");
        std::fs::write(&path, tar_bytes(&[("./a.txt", b"12345"), ("d/b", b"1")])).unwrap();
        let out = index(&path, None, &Controller::default()).unwrap();
        assert_eq!(names(&out, ""), ["a.txt", "d"]);
        let a = out.entry(Path::new("a.txt")).unwrap();
        assert_eq!(a.size, Some(5));
        assert_eq!(a.mode, Some(0o644));
        assert!(a.modified.is_some());
    }

    #[test]
    fn a_tar_gz_lists_its_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.tar.gz");
        let mut gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::default(),
        );
        gz.write_all(&tar_bytes(&[("a.txt", b"12345")])).unwrap();
        gz.finish().unwrap();
        let out = index(&path, None, &Controller::default()).unwrap();
        assert_eq!(names(&out, ""), ["a.txt"]);
    }

    /// Cut short, it keeps what came before and says so
    #[test]
    fn a_tar_gz_cut_short_lists_what_came_before() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.tar.gz");
        let data: Vec<u8> = (0..200_000_u32).flat_map(u32::to_le_bytes).collect();
        let mut gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        gz.write_all(&tar_bytes(&[
            ("first", b"1"),
            ("second", &data),
            ("third", b"3"),
        ]))
        .unwrap();
        gz.finish().unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(len / 2)
            .unwrap();
        let out = index(&path, None, &Controller::default()).unwrap();
        assert!(out.entry(Path::new("first")).is_some());
        assert!(out.entry(Path::new("third")).is_none());
        assert!(out.damaged.is_some());
    }

    #[test]
    fn a_seven_z_lists_its_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&path).unwrap();
        writer
            .push_archive_entry(
                sevenz_rust2::ArchiveEntry::new_file("d/a.txt"),
                Some(std::io::Cursor::new(b"12345".to_vec())),
            )
            .unwrap();
        writer.finish().unwrap();
        let out = index(&path, None, &Controller::default()).unwrap();
        assert_eq!(names(&out, ""), ["d"]);
        assert_eq!(out.entry(Path::new("d/a.txt")).unwrap().size, Some(5));
        assert!(!out.entry(Path::new("d/a.txt")).unwrap().encrypted);
    }

    /// Past the cap it stops and says so, and a name it already has still
    /// takes its later entry
    #[test]
    fn the_listing_stops_at_the_cap() {
        let mut out = ArchiveIndex {
            max_entries: 2,
            ..ArchiveIndex::default()
        };
        assert!(out.add(b"one", false, file(1)));
        assert!(out.add(b"two", false, file(1)));
        assert!(!out.truncated);
        assert!(out.add(b"one", false, file(2)));
        assert!(!out.add(b"three", false, file(1)));
        assert!(out.truncated);
        assert_eq!(out.len(), 2);
    }

    /// The folders a name brings count too: they cannot carry it past the cap
    #[test]
    fn folders_count_toward_the_cap() {
        let mut out = ArchiveIndex {
            max_entries: 2,
            ..ArchiveIndex::default()
        };
        assert!(!out.add(b"a/b/c/d", false, file(1)));
        assert!(out.truncated);
        assert_eq!(out.len(), 2);
    }

    /// Few entries with long names stop too
    #[test]
    fn the_listing_stops_at_its_budget_of_names() {
        let mut out = ArchiveIndex {
            max_path_bytes: 10,
            ..ArchiveIndex::default()
        };
        assert!(out.add(b"12345", false, file(1)));
        assert!(!out.add(b"678901", false, file(1)));
        assert!(out.truncated);
        assert_eq!(out.len(), 1);
    }

    fn hard_link(target: &str) -> Listed {
        Listed {
            hard_link_to: Some(PathBuf::from(target)),
            kind: EntryKind::Link,
            size: None,
            modified: None,
            mode: None,
            encrypted: false,
        }
    }

    /// A tar repeating one hard link costs nothing more for it
    #[test]
    fn a_repeated_hard_link_is_held_once() {
        let mut out = ArchiveIndex::default();
        for _ in 0..1000 {
            assert!(out.add(b"bbbb", false, hard_link("c")));
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out.hard_link_targets, 1);
        assert_eq!(out.hard_links[Path::new("bbbb")].len(), 1);
        // The entry's name, then the link's name and its target
        assert_eq!(out.path_bytes, 4 + 4 + 1);
        assert!(!out.truncated);
    }

    /// One name linking to ever more targets adds no entries, and still stops
    #[test]
    fn hard_link_targets_stop_at_the_cap() {
        let mut out = ArchiveIndex {
            max_entries: 2,
            ..ArchiveIndex::default()
        };
        assert!(out.add(b"a", false, hard_link("x")));
        assert!(out.add(b"a", false, hard_link("y")));
        assert!(!out.truncated);
        assert!(!out.add(b"a", false, hard_link("z")));
        assert!(out.truncated);
        assert_eq!(out.len(), 1);
        assert_eq!(out.hard_link_targets, 2);
    }

    /// Hard link targets are names held like any other
    #[test]
    fn hard_link_targets_count_toward_the_budget_of_names() {
        let mut out = ArchiveIndex {
            max_path_bytes: 10,
            ..ArchiveIndex::default()
        };
        assert!(out.add(b"a", false, hard_link("123")));
        assert!(!out.add(b"a", false, hard_link("456789")));
        assert!(out.truncated);
        assert_eq!(out.hard_link_targets, 1);
    }

    /// An encrypted 7z's list needs the password; a wrong or missing one is
    /// asked about, the right one lists it
    #[test]
    fn a_seven_z_with_an_encrypted_list_asks_for_the_password() {
        use sevenz_rust2::encoder_options::AesEncoderOptions;
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderMethod, Password};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.7z");
        let mut writer = ArchiveWriter::create(&path).unwrap();
        writer.set_content_methods(vec![
            AesEncoderOptions::new(Password::from("secret")).into(),
            EncoderMethod::LZMA2.into(),
        ]);
        writer
            .push_archive_entry(
                ArchiveEntry::new_file("a.txt"),
                Some(std::io::Cursor::new(b"12345".to_vec())),
            )
            .unwrap();
        writer.finish().unwrap();
        let err = index(&path, None, &Controller::default()).expect_err("no password");
        assert!(
            matches!(err.kind, OperationErrorType::PasswordRequired),
            "{err:?}"
        );
        let out = index(&path, Some("secret"), &Controller::default()).unwrap();
        assert!(out.entry(Path::new("a.txt")).unwrap().encrypted);
    }

    /// A zip lists without its password, and says which entries need one
    #[test]
    fn an_encrypted_zip_lists_without_a_password() {
        use zip::unstable::write::FileOptionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file(
            "a.txt",
            zip::write::SimpleFileOptions::default()
                .with_deprecated_encryption(b"secret")
                .unwrap(),
        )
        .unwrap();
        zip.write_all(b"12345").unwrap();
        zip.finish().unwrap();
        let out = index(&path, None, &Controller::default()).unwrap();
        assert!(out.entry(Path::new("a.txt")).unwrap().encrypted);
        assert_eq!(out.entry(Path::new("a.txt")).unwrap().size, Some(5));
    }

    /// A name nested far beyond any path a system takes is left out, not
    /// walked: on a test thread's 2 MB stack, recursing through it aborts
    #[test]
    fn a_name_nested_too_deep_is_left_out() {
        let mut out = ArchiveIndex::default();
        assert!(out.add("a/".repeat(32_768).as_bytes(), false, file(1)));
        assert_eq!(out.unsafe_names, 1);
        assert!(out.is_empty());
    }

    /// As deep as a path can go is still listed
    #[test]
    fn a_name_as_deep_as_a_path_goes_is_listed() {
        let mut out = ArchiveIndex::default();
        assert!(out.add("a/".repeat(2_000).as_bytes(), false, file(1)));
        assert_eq!(out.unsafe_names, 0);
        assert_eq!(out.len(), 2_000);
    }

    /// A time past what the system can hold is unknown, not a panic
    #[test]
    fn a_tar_time_out_of_range_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.tar");
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_mtime(u64::MAX);
        tar.append_data(&mut header, "a", &b"1"[..]).unwrap();
        std::fs::write(&path, tar.into_inner().unwrap()).unwrap();
        let out = index(&path, None, &Controller::default()).unwrap();
        assert_eq!(out.entry(Path::new("a")).unwrap().modified, None);
    }

    /// What has something in it is a folder, in either order
    #[test]
    fn a_file_with_children_after_it_becomes_a_folder() {
        let mut out = ArchiveIndex::default();
        out.add(b"a", false, file(5));
        out.add(b"a/b", false, file(1));
        assert_eq!(out.entry(Path::new("a")).unwrap().kind, EntryKind::Folder);
        assert_eq!(out.entry(Path::new("a")).unwrap().size, None);
        assert_eq!(names(&out, "a"), ["b"]);
        assert_eq!(out.total_size(), (1, true));
    }

    /// A zip name in the old encoding reads as extraction reads it
    #[test]
    fn a_zip_name_reads_as_extraction_names_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.zip");
        write_zip(&path, &[("cafX.txt", b"1")]);
        let bytes = std::fs::read(&path).unwrap();
        let patched: Vec<u8> = bytes
            .windows(1)
            .enumerate()
            .map(|(i, w)| {
                if bytes[i..].starts_with(b"X.txt") {
                    0x82 // e acute in CP437
                } else {
                    w[0]
                }
            })
            .collect();
        std::fs::write(&path, patched).unwrap();

        let dest = tmp.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        crate::archive::extract(&path, &dest, &None, &Controller::default()).unwrap();
        let extracted: Vec<_> = std::fs::read_dir(&dest)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();

        let out = index(&path, None, &Controller::default()).unwrap();
        let listed = out.listing(Path::new("")).unwrap();
        assert_eq!(listed[0].name(), extracted[0]);
        assert_eq!(listed[0].name().to_str(), Some("caf\u{e9}.txt"));
    }

    /// Stopped while a zip without its list is read, it ends as stopped, even
    /// where there was no entry left to look at it from
    #[test]
    fn cancelling_a_zip_read_from_its_start_ends_as_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.zip");
        std::fs::write(&path, b"no list and no entries").unwrap();
        let controller = Controller::default();
        controller.cancel();
        let err = index(&path, None, &controller).expect_err("stopped");
        assert!(err.is_cancelled(), "{err:?}");
    }

    /// However little the file under it is read, a stop reaches the listing
    #[test]
    fn a_decompressed_stream_stops_with_the_operation() {
        use std::io::Read;
        let controller = Controller::default();
        let mut reader = super::Stoppable {
            reader: std::io::repeat(0),
            controller: controller.clone(),
        };
        let mut buffer = [0; 16];
        assert!(reader.read(&mut buffer).is_ok());
        controller.cancel();
        assert!(reader.read(&mut buffer).is_err());
    }

    /// Read once and held until the archive changes, or nothing wants it
    #[test]
    fn a_shared_index_is_held_until_the_archive_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("shared.zip");
        write_zip(&path, &[("a", b"1")]);
        let controller = Controller::default();
        let first = read_shared(&path, None, &controller).unwrap();
        let again = read_shared(&path, None, &controller).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &again), "held");

        write_zip(&path, &[("a", b"1"), ("bigger", b"22")]);
        let changed = read_shared(&path, None, &controller).unwrap();
        assert_eq!(changed.len(), 2, "read again");

        keep_shared(&[]);
        let fresh = read_shared(&path, None, &controller).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&changed, &fresh), "let go of");
        keep_shared(&[]);
    }

    /// A path wanted brings what is under it, by the name it would have in a
    /// listing however the archive writes it, and nothing beside it
    #[test]
    fn only_wants_what_was_asked_for_and_what_is_under_it() {
        let only = super::Only::new(&[
            PathBuf::from("a/b"),
            PathBuf::from("a/b/c"),
            PathBuf::from("d"),
        ]);
        assert!(only.wants(b"./a/b/file", false));
        assert!(only.wants(b"a/b", false));
        assert!(only.wants(br"a\b\file", true));
        assert!(only.wants(b"d", false));
        assert!(!only.wants(b"a/other", false));
        assert!(
            !only.wants(b"a", false),
            "the folder above is not asked for"
        );
        assert!(!only.wants(b"../d", false));
        assert_eq!(only.tops(), [PathBuf::from("a/b"), PathBuf::from("d")]);
    }

    /// A folder and what is in it, as a search can select both, count once
    #[test]
    fn a_selection_inside_a_selected_folder_counts_once() {
        let mut out = ArchiveIndex::default();
        out.add(b"d/ten", false, file(10));
        assert_eq!(
            out.size_of(&[PathBuf::from("d"), PathBuf::from("d/ten")]),
            (10, true)
        );
    }

    /// A hard link chosen without what it links to still comes out, with the
    /// data, and that other entry stays behind what is brought out
    #[test]
    fn a_hard_link_chosen_alone_brings_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.tar");
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        tar.append_data(&mut header, "d/original", &b"data"[..])
            .unwrap();
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Link);
        link.set_size(0);
        link.set_mode(0o644);
        tar.append_link(&mut link, "e/alias", "d/original").unwrap();
        std::fs::write(&path, tar.into_inner().unwrap()).unwrap();

        let listed = index(&path, None, &Controller::default()).unwrap();
        let only = super::Only::new(&[PathBuf::from("e/alias")]);
        assert!(!only.wants(b"d/original", false));
        let only = only.with_hard_link_targets(&listed);
        assert!(only.wants(b"d/original", false));
        assert_eq!(only.tops(), [PathBuf::from("e/alias")], "not brought out");

        let dest = tmp.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        let done = crate::archive::extract_only(
            &path,
            &dest,
            &None,
            Some(&super::Only::new(&[PathBuf::from("e/alias")])),
            &Controller::default(),
        )
        .unwrap();
        assert!(done.damaged.is_empty(), "{:?}", done.damaged);
        assert_eq!(std::fs::read(dest.join("e/alias")).unwrap(), b"data");
        keep_shared(&[]);
    }

    /// Extracting replays the stream, so a link through a name that a later
    /// entry replaces still needs what it linked to back then
    #[test]
    fn a_hard_link_through_a_replaced_name_keeps_its_data() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.tar");
        let mut tar = tar::Builder::new(Vec::new());
        let plain = |size: u64| {
            let mut header = tar::Header::new_gnu();
            header.set_size(size);
            header.set_mode(0o644);
            header
        };
        let link = || {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Link);
            header.set_size(0);
            header.set_mode(0o644);
            header
        };
        tar.append_data(&mut plain(4), "x", &b"data"[..]).unwrap();
        tar.append_link(&mut link(), "a", "x").unwrap();
        tar.append_link(&mut link(), "b", "a").unwrap();
        tar.append_data(&mut plain(3), "a", &b"new"[..]).unwrap();
        std::fs::write(&path, tar.into_inner().unwrap()).unwrap();

        let dest = tmp.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        let done = crate::archive::extract_only(
            &path,
            &dest,
            &None,
            Some(&super::Only::new(&[PathBuf::from("b")])),
            &Controller::default(),
        )
        .unwrap();
        assert!(done.damaged.is_empty(), "{:?}", done.damaged);
        assert_eq!(std::fs::read(dest.join("b")).unwrap(), b"data");
        keep_shared(&[]);
    }

    /// What is not chosen is never written, in any format: skipping it only
    /// afterwards would still cost the room and the time
    #[test]
    fn what_is_not_chosen_is_never_written() {
        let tmp = tempfile::tempdir().unwrap();
        let entries: [(&str, &[u8]); 3] = [
            ("d/a.txt", b"aaaa"),
            ("d/b.txt", b"bbbb"),
            ("c.txt", b"cccc"),
        ];

        let zip = tmp.path().join("t.zip");
        write_zip(&zip, &entries);
        let tar = tmp.path().join("t.tar");
        std::fs::write(&tar, tar_bytes(&entries)).unwrap();
        let gz = tmp.path().join("t.tar.gz");
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&gz).unwrap(),
            flate2::Compression::default(),
        );
        encoder.write_all(&tar_bytes(&entries)).unwrap();
        encoder.finish().unwrap();
        let seven = tmp.path().join("t.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&seven).unwrap();
        for (name, data) in entries {
            writer
                .push_archive_entry(sevenz_rust2::ArchiveEntry::new_file(name), Some(data))
                .unwrap();
        }
        writer.finish().unwrap();

        let only = super::Only::new(&[PathBuf::from("d/b.txt")]);
        for (i, archive) in [zip, tar, gz, seven].iter().enumerate() {
            let dest = tmp.path().join(format!("out{i}"));
            std::fs::create_dir(&dest).unwrap();
            crate::archive::extract_only(
                archive,
                &dest,
                &None,
                Some(&only),
                &Controller::default(),
            )
            .unwrap();
            assert_eq!(
                std::fs::read(dest.join("d/b.txt")).unwrap(),
                b"bbbb",
                "{archive:?}"
            );
            assert!(!dest.join("d/a.txt").exists(), "{archive:?}");
            assert!(!dest.join("c.txt").exists(), "{archive:?}");
        }
    }

    /// In a solid 7z what is not chosen shares a block with what is: it is read
    /// past, and the one chosen still comes out right
    #[test]
    fn a_solid_seven_z_gives_only_what_is_chosen() {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.7z");
        let mut writer = ArchiveWriter::create(&path).unwrap();
        let contents: [&[u8]; 3] = [b"aaaa", b"bbbb", b"cccc"];
        writer
            .push_archive_entries(
                vec![
                    ArchiveEntry::new_file("d/a.txt"),
                    ArchiveEntry::new_file("d/b.txt"),
                    ArchiveEntry::new_file("c.txt"),
                ],
                contents.into_iter().map(SourceReader::new).collect(),
            )
            .unwrap();
        writer.finish().unwrap();

        let dest = tmp.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        crate::archive::extract_only(
            &path,
            &dest,
            &None,
            Some(&super::Only::new(&[PathBuf::from("d/b.txt")])),
            &Controller::default(),
        )
        .unwrap();
        assert_eq!(std::fs::read(dest.join("d/b.txt")).unwrap(), b"bbbb");
        assert!(!dest.join("d/a.txt").exists());
        assert!(!dest.join("c.txt").exists());
    }

    /// What a listing calls an old zip's entry is what extraction goes by, or
    /// choosing it would extract nothing
    #[test]
    fn an_entry_chosen_by_its_listed_name_is_extracted() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.zip");
        write_zip(&path, &[("cafX.txt", b"1"), ("other.txt", b"2")]);
        let bytes = std::fs::read(&path).unwrap();
        let patched: Vec<u8> = bytes
            .iter()
            .enumerate()
            .map(|(i, byte)| {
                if bytes[i..].starts_with(b"X.txt") {
                    0x82 // e acute in CP437
                } else {
                    *byte
                }
            })
            .collect();
        std::fs::write(&path, patched).unwrap();

        let out = index(&path, None, &Controller::default()).unwrap();
        let listed: Vec<PathBuf> = out
            .listing(Path::new(""))
            .unwrap()
            .iter()
            .map(|entry| entry.path.clone())
            .filter(|path| path != Path::new("other.txt"))
            .collect();
        let dest = tmp.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        crate::archive::extract_only(
            &path,
            &dest,
            &None,
            Some(&super::Only::new(&listed)),
            &Controller::default(),
        )
        .unwrap();
        let names: Vec<_> = std::fs::read_dir(&dest)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("caf\u{e9}.txt")]);
    }

    /// A bomb's stated sizes are read, never its contents
    #[test]
    fn listing_reads_no_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.zip");
        write_zip(&path, &[("big", &vec![0_u8; 8 * 1024 * 1024])]);
        let before: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        let out = index(&path, None, &Controller::default()).unwrap();
        assert_eq!(out.total_size(), (8 * 1024 * 1024, true));
        let after: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        assert_eq!(before.len(), after.len(), "nothing written");
    }

    #[test]
    fn what_is_no_archive_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.zip");
        std::fs::write(&path, b"not a zip at all").unwrap();
        assert!(index(&path, None, &Controller::default()).is_err());
    }

    /// A plain `.gz` compresses one file: there is nothing to look inside
    #[test]
    fn a_plain_gz_cannot_be_browsed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.gz");
        let mut gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::default(),
        );
        gz.write_all(b"plain words").unwrap();
        gz.finish().unwrap();
        assert!(index(&path, None, &Controller::default()).is_err());
    }

    /// Stopped by the user, it says so rather than give half a listing
    #[test]
    fn cancelling_stops_the_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.tar");
        std::fs::write(&path, tar_bytes(&[("a", b"1")])).unwrap();
        let controller = Controller::default();
        controller.cancel();
        let err = index(&path, None, &controller).expect_err("stopped");
        assert!(err.is_cancelled(), "{err:?}");
    }
}
