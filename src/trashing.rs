//! The trash as the freedesktop.org Trash specification 1.0 lays it out:
//! the home trash, and on any other drive `.Trash/$uid` (in a shared,
//! sticky `.Trash`) or `.Trash-$uid` at the drive's root.
//!
//! An item goes to the trash on its own drive, and only by a rename: never
//! copied, so nothing half-copied is ever left in a trash's `files/`. An
//! entry of `files/` without a `.trashinfo` is still listed, with its origin
//! unknown: the spec has such an entry shown, not hidden.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// A trash folder, holding `files/` and `info/`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Bin {
    /// The trash folder itself.
    pub root: PathBuf,
    /// What a relative `Path=` in its `.trashinfo` files is relative to: the
    /// drive's root, or for the home trash the folder it is in.
    pub top: PathBuf,
    /// The home trash, whose `Path=` values are written absolute.
    pub home: bool,
}

/// This user's id.
pub(crate) fn uid() -> u32 {
    // SAFETY: `getuid` has no preconditions and cannot fail
    unsafe { libc::getuid() }
}

/// The home trash: `$XDG_DATA_HOME/Trash`, else `~/.local/share/Trash`.
pub fn home() -> Option<Bin> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))?;
    Some(Bin {
        root: data.join("Trash"),
        top: data,
        home: true,
    })
}

/// The nearest folder of `path` that exists, `path` included.
fn existing(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|ancestor| fs::symlink_metadata(ancestor).is_ok())
}

/// Whether this user may add entries to `dir` and remove them.
fn writable(dir: &Path) -> bool {
    crate::operation::cannot_write(dir).is_none() && dir.is_dir()
}

/// Makes `root`, `root/files` and `root/info`, mode 0700, where missing.
fn make_bin(root: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).recursive(true);
    for dir in [root.join("files"), root.join("info")] {
        builder.create(dir)?;
    }
    Ok(())
}

/// Whether `root` is a trash this user can use: a folder of theirs, not a
/// link. Missing, it is made when `make`, and otherwise only checked for
/// room to be made.
fn usable(root: &Path, make: bool) -> bool {
    match fs::symlink_metadata(root) {
        Ok(meta) => {
            meta.is_dir()
                && meta.uid() == uid()
                && writable(root)
                && (!make || make_bin(root).is_ok())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if make {
                fs::DirBuilder::new().mode(0o700).create(root).is_ok() && make_bin(root).is_ok()
            } else {
                root.parent().is_some_and(writable)
            }
        }
        Err(_) => false,
    }
}

/// Whether `shared` is a `.Trash` the spec lets every user keep a trash in:
/// a folder, not a link, with the sticky bit.
fn sticky(shared: &Path) -> bool {
    fs::symlink_metadata(shared).is_ok_and(|meta| meta.is_dir() && meta.mode() & libc::S_ISVTX != 0)
}

/// The trash on the drive mounted at `top`: `.Trash/$uid` in a sticky
/// `.Trash`, else `.Trash-$uid`. `None` when neither can be used, or made
/// when `make`.
pub fn drive_bin(top: &Path, make: bool) -> Option<Bin> {
    let bin = |root| Bin {
        root,
        top: top.to_path_buf(),
        home: false,
    };
    let shared = top.join(".Trash");
    if sticky(&shared) {
        let own = shared.join(uid().to_string());
        if usable(&own, make) {
            return Some(bin(own));
        }
    }
    let own = top.join(format!(".Trash-{}", uid()));
    usable(&own, make).then(|| bin(own))
}

/// The trashes there are on the drive mounted at `top`, making none.
pub fn drive_bins(top: &Path) -> Vec<Bin> {
    let bin = |root| Bin {
        root,
        top: top.to_path_buf(),
        home: false,
    };
    let is_dir = |path: &Path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
    let mut found = Vec::new();
    let shared = top.join(".Trash");
    let own = shared.join(uid().to_string());
    if sticky(&shared) && is_dir(&own) {
        found.push(bin(own));
    }
    let own = top.join(format!(".Trash-{}", uid()));
    if is_dir(&own) {
        found.push(bin(own));
    }
    found
}

/// The mounts, as `(mount point, filesystem type)`, from
/// `/proc/self/mountinfo`.
fn mounts() -> Vec<(PathBuf, String)> {
    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    mountinfo
        .lines()
        .filter_map(|line| {
            let (mount, fs) = line.split_once(" - ")?;
            let point = mount.split(' ').nth(4)?;
            Some((unescape(point), fs.split(' ').next()?.to_owned()))
        })
        .collect()
}

/// A mount point as `mountinfo` writes it, with spaces and the like as
/// octal escapes.
fn unescape(field: &str) -> PathBuf {
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
    PathBuf::from(OsString::from_vec(out))
}

/// The mount among `mounts` that the real path `path` is under: its mount
/// point and filesystem type.
fn mount_among(path: &Path, mounts: &[(PathBuf, String)]) -> Option<(PathBuf, String)> {
    mounts
        .iter()
        .filter(|(point, _)| path.starts_with(point))
        .max_by_key(|(point, _)| point.as_os_str().len())
        .cloned()
}

/// Whether a drive of this filesystem type takes a trash: a local one.
/// Network drives do not: nothing is trashed there, so their trashes are
/// never looked for, and asking them can hang.
fn takes_a_trash(fs_type: &str) -> bool {
    crate::tab::fs_type_kind(fs_type) == crate::tab::FsKind::Local
}

/// The trash `path` goes to: the home trash when it is on the same mount,
/// else its drive's own, made when `make`. `None` when there is none to use.
///
/// The same mount, not the same filesystem: a rename between two mounts of
/// one filesystem, a bind mount and the original, fails all the same.
pub fn bin_for(path: &Path, make: bool) -> Option<Bin> {
    let parent = existing(path.parent().unwrap_or(path))?
        .canonicalize()
        .ok()?;
    let mounts = mounts();
    let (top, fs_type) = mount_among(&parent, &mounts)?;
    if let Some(home) = home()
        && existing(&home.root)
            .and_then(|at| at.canonicalize().ok())
            .and_then(|at| mount_among(&at, &mounts))
            .is_some_and(|(home_top, _)| home_top == top)
    {
        return (!make || make_bin(&home.root).is_ok()).then_some(home);
    }
    if !takes_a_trash(&fs_type) {
        return None;
    }
    drive_bin(&top, make)
}

/// `path` as `Path=` holds it: bytes other than unreserved ones and `/`
/// as `%XX`.
pub(crate) fn encode(path: &Path) -> String {
    let mut out = String::new();
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `name` with `.n` before its extension: `notes.txt`, `notes.2.txt`, …
fn numbered(name: &OsStr, n: u32) -> OsString {
    if n == 1 {
        return name.to_owned();
    }
    let path = Path::new(name);
    let mut out = path.file_stem().unwrap_or(name).to_owned();
    out.push(format!(".{n}"));
    if let Some(ext) = path.extension() {
        out.push(".");
        out.push(ext);
    }
    out
}

/// Sends `path` to the trash on its own drive, made there when missing, and
/// returns its entry. Fails when its drive has no trash that can be used.
pub fn put(path: &Path) -> io::Result<trash::TrashItem> {
    let bin = bin_for(path, true)
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "its drive has no trash"))?;
    put_in(&bin, path)
}

/// Sends `path` to `bin`: its `.trashinfo` first, under a name free in both
/// `info/` and `files/`, then the item renamed beside it. A failed rename
/// takes the `.trashinfo` back. Nothing is ever copied.
pub(crate) fn put_in(bin: &Bin, path: &Path) -> io::Result<trash::TrashItem> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let recorded = if bin.home {
        path
    } else {
        path.strip_prefix(&bin.top).unwrap_or(path)
    };
    let now = jiff::Zoned::now();
    let body = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        encode(recorded),
        now.strftime("%Y-%m-%dT%H:%M:%S")
    );
    for n in 1..=10_000 {
        let candidate = numbered(name, n);
        let mut info_name = candidate.clone();
        info_name.push(".trashinfo");
        let info = bin.root.join("info").join(&info_name);
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&info)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        let kept = bin.root.join("files").join(&candidate);
        // An entry there without a `.trashinfo` is not this one's to replace
        if fs::symlink_metadata(&kept).is_ok() {
            drop(file);
            let _ = fs::remove_file(&info);
            continue;
        }
        let moved = file
            .write_all(body.as_bytes())
            .and_then(|()| file.sync_all())
            .and_then(|()| crate::operation::rename_no_replace(path, &kept));
        if let Err(err) = moved {
            let _ = fs::remove_file(&info);
            return Err(err);
        }
        return Ok(trash::TrashItem {
            id: info.into_os_string(),
            name: name.to_owned(),
            original_parent: path.parent().unwrap_or(path).to_path_buf(),
            time_deleted: now.timestamp().as_second(),
        });
    }
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

/// Whether the place `item` came from is unknown: an entry of `files/`
/// without a `.trashinfo`, or with one that cannot be read. It cannot be
/// put back, only deleted.
pub fn origin_unknown(item: &trash::TrashItem) -> bool {
    item.original_parent.as_os_str().is_empty()
}

/// A `DeletionDate=` value, in local time, as Unix seconds.
fn deletion_time(value: &str) -> Option<i64> {
    let civil: jiff::civil::DateTime = value.trim().parse().ok()?;
    let zoned = civil.to_zoned(jiff::tz::TimeZone::system()).ok()?;
    Some(zoned.timestamp().as_second())
}

/// The original path and the deletion time a `.trashinfo` gives.
fn read_info(info: &Path, bin: &Bin) -> Option<(PathBuf, i64)> {
    let text = fs::read_to_string(info).ok()?;
    let mut lines = text.lines().map(str::trim);
    lines.by_ref().find(|line| *line == "[Trash Info]")?;
    let (mut path, mut time) = (None, 0);
    for line in lines.take_while(|line| !line.starts_with('[')) {
        if let Some(value) = line.strip_prefix("Path=") {
            path = crate::trash::percent_decode(value);
        } else if let Some(value) = line.strip_prefix("DeletionDate=") {
            time = deletion_time(value).unwrap_or(0);
        }
    }
    let path = path.filter(|path| !path.as_os_str().is_empty())?;
    Some((
        if path.is_absolute() {
            path
        } else {
            bin.top.join(path)
        },
        time,
    ))
}

/// The trash whose folder is `root`, with what its `Path=` values are
/// relative to: the home trash's own folder, or the drive's root for
/// `$top/.Trash-$uid` and `$top/.Trash/$uid`.
pub(crate) fn bin_at(root: &Path) -> Bin {
    if let Some(home) = home().filter(|home| home.root == root) {
        return home;
    }
    let parent = root.parent().unwrap_or(root);
    let top = if root
        .file_name()
        .is_some_and(|name| name.as_bytes().starts_with(b".Trash-"))
    {
        parent
    } else {
        parent.parent().unwrap_or(parent)
    };
    Bin {
        root: root.to_path_buf(),
        top: top.to_path_buf(),
        home: false,
    }
}

/// Where the entry `name` of `bin`'s `files/` came from, as its
/// `.trashinfo` says.
pub(crate) fn origin_of(bin: &Bin, name: &OsStr) -> Option<PathBuf> {
    let mut info_name = name.to_owned();
    info_name.push(".trashinfo");
    read_info(&bin.root.join("info").join(info_name), bin).map(|(path, _)| path)
}

/// What one trash holds: each entry of its `files/`, with what its
/// `.trashinfo` says, or with its origin unknown when there is none to read.
pub fn list_bin(bin: &Bin) -> Vec<(trash::TrashItem, trash::TrashItemMetadata)> {
    let Ok(entries) = fs::read_dir(bin.root.join("files")) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let meta = fs::symlink_metadata(entry.path()).ok()?;
            let size = if meta.is_dir() {
                trash::TrashItemSize::Entries(fs::read_dir(entry.path()).map_or(0, Iterator::count))
            } else {
                trash::TrashItemSize::Bytes(meta.len())
            };
            let mut info_name = name.clone();
            info_name.push(".trashinfo");
            let info = bin.root.join("info").join(info_name);
            let item = match read_info(&info, bin) {
                Some((original, time_deleted)) => trash::TrashItem {
                    id: info.into_os_string(),
                    name: original
                        .file_name()
                        .map_or_else(|| name.clone(), OsStr::to_owned),
                    original_parent: original.parent().map(Path::to_path_buf).unwrap_or_default(),
                    time_deleted,
                },
                None => trash::TrashItem {
                    // A `.trashinfo` that cannot be read still goes with it
                    id: if fs::symlink_metadata(&info).is_ok() {
                        info.into_os_string()
                    } else {
                        entry.path().into_os_string()
                    },
                    name,
                    original_parent: PathBuf::new(),
                    time_deleted: 0,
                },
            };
            Some((item, trash::TrashItemMetadata { size }))
        })
        .collect()
}

/// Every trash there is now: the home trash, and the drive trashes on
/// mounted drives that take one (see [`takes_a_trash`]).
pub fn bins() -> Vec<Bin> {
    let mut bins: Vec<Bin> = home()
        .into_iter()
        .filter(|home| home.root.is_dir())
        .collect();
    for (point, fs_type) in mounts() {
        if takes_a_trash(&fs_type) {
            for bin in drive_bins(&point) {
                if !bins.contains(&bin) {
                    bins.push(bin);
                }
            }
        }
    }
    bins
}

/// Everything in every trash.
pub fn list() -> Vec<(trash::TrashItem, trash::TrashItemMetadata)> {
    bins().iter().flat_map(list_bin).collect()
}

/// Whether no trash holds anything in its `files/`.
pub fn is_empty() -> bool {
    bins().iter().all(|bin| {
        fs::read_dir(bin.root.join("files")).map_or(true, |mut entries| entries.next().is_none())
    })
}

/// Whether the drive mounted at `top` has anything in its trash, in
/// `files/` or in `info/`.
pub fn drive_has_items(top: &Path) -> bool {
    drive_bins(top).iter().any(|bin| {
        ["files", "info"].iter().any(|dir| {
            fs::read_dir(bin.root.join(dir)).is_ok_and(|mut entries| entries.next().is_some())
        })
    })
}

/// How new a record without its item may be and still be kept: another
/// program may be trashing right now, between writing the record and
/// renaming the item in.
const RECORD_IN_FLIGHT: std::time::Duration = std::time::Duration::from_secs(60);

/// Removes from each of `bins` the records in `info/` whose item is gone
/// from `files/`, as a purge cut short between the two leaves them, except
/// those changed within [`RECORD_IN_FLIGHT`]. They hold nothing and are
/// not listed, but they count when a drive is asked about before ejecting.
pub fn remove_stray_records(bins: &[Bin]) {
    let now = std::time::SystemTime::now();
    for bin in bins {
        let Ok(records) = fs::read_dir(bin.root.join("info")) else {
            continue;
        };
        for record in records.filter_map(Result::ok) {
            let path = record.path();
            let Some(name) = path
                .extension()
                .is_some_and(|ext| ext == "trashinfo")
                .then(|| path.file_stem())
                .flatten()
            else {
                continue;
            };
            let recent = record
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|modified| {
                    now.duration_since(modified)
                        .map_or(true, |age| age < RECORD_IN_FLIGHT)
                });
            if recent {
                continue;
            }
            // Only an item known to be gone leaves its record without one: one
            // that cannot be looked at may well be there
            match fs::symlink_metadata(bin.root.join("files").join(name)) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    log::warn!(
                        "kept {}: its item cannot be looked at: {err}",
                        path.display()
                    );
                    continue;
                }
                Ok(_) => continue,
            }
            if let Err(err) = fs::remove_file(&path)
                && err.kind() != io::ErrorKind::NotFound
            {
                log::warn!("failed to remove {}: {err}", path.display());
            }
        }
    }
}

/// Removes `item` from the trash for good: the item, then its `.trashinfo`
/// when it has one. Failing part-way, the `.trashinfo` stays, and so does
/// the entry in the trash.
pub fn purge(item: &trash::TrashItem) -> io::Result<()> {
    let (files, info) = crate::operation::in_trash(item);
    match crate::operation::recursive::remove_tree(&files) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    match info.map(fs::remove_file) {
        Some(Err(err)) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn drive() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn bin_in(top: &Path) -> Bin {
        drive_bin(top, true).expect("made")
    }

    #[test]
    fn mount_points_are_unescaped() {
        assert_eq!(
            unescape(r"/run/media/user/My\040Stick"),
            PathBuf::from("/run/media/user/My Stick")
        );
        assert_eq!(unescape("/"), PathBuf::from("/"));
    }

    #[test]
    fn a_drive_without_a_trash_gets_its_own() {
        let top = drive();
        let bin = bin_in(top.path());
        assert_eq!(bin.root, top.path().join(format!(".Trash-{}", uid())));
        let mode = fs::metadata(&bin.root).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(bin.root.join("files").is_dir() && bin.root.join("info").is_dir());
    }

    #[test]
    fn a_sticky_shared_trash_is_used() {
        let top = drive();
        let shared = top.path().join(".Trash");
        fs::create_dir(&shared).expect("mkdir");
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).expect("chmod");
        assert_eq!(bin_in(top.path()).root, shared.join(uid().to_string()));
    }

    #[test]
    fn a_shared_trash_without_the_sticky_bit_is_not_used() {
        let top = drive();
        let shared = top.path().join(".Trash");
        fs::create_dir(&shared).expect("mkdir");
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).expect("chmod");
        assert_eq!(
            bin_in(top.path()).root,
            top.path().join(format!(".Trash-{}", uid()))
        );
    }

    #[test]
    fn a_linked_trash_is_not_used() {
        let (top, elsewhere) = (drive(), drive());
        std::os::unix::fs::symlink(
            elsewhere.path(),
            top.path().join(format!(".Trash-{}", uid())),
        )
        .expect("link");
        assert!(drive_bin(top.path(), true).is_none());
    }

    #[test]
    fn a_drive_that_cannot_have_a_trash_has_none() {
        let top = drive();
        fs::set_permissions(top.path(), fs::Permissions::from_mode(0o555)).expect("chmod");
        let found = drive_bin(top.path(), false);
        fs::set_permissions(top.path(), fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(found.is_none());
    }

    #[test]
    fn trashing_renames_and_records_a_relative_path_on_a_drive() {
        let top = drive();
        fs::create_dir(top.path().join("dir")).expect("mkdir");
        let item = top.path().join("dir/café.txt");
        fs::write(&item, "x").expect("write");
        let bin = bin_in(top.path());
        let trashed = put_in(&bin, &item).expect("trashed");
        assert!(!item.exists());
        assert!(bin.root.join("files/café.txt").is_file());
        let info = fs::read_to_string(bin.root.join("info/café.txt.trashinfo")).expect("info");
        assert!(
            info.starts_with("[Trash Info]\nPath=dir/caf%C3%A9.txt\nDeletionDate="),
            "{info}"
        );
        assert_eq!(trashed.original_path(), item);
        assert_eq!(
            PathBuf::from(&trashed.id),
            bin.root.join("info/café.txt.trashinfo")
        );
    }

    #[test]
    fn the_home_trash_records_an_absolute_path() {
        let data = drive();
        let bin = Bin {
            root: data.path().join("Trash"),
            top: data.path().to_path_buf(),
            home: true,
        };
        make_bin(&bin.root).expect("made");
        let item = data.path().join("a b");
        fs::write(&item, "x").expect("write");
        put_in(&bin, &item).expect("trashed");
        let info = fs::read_to_string(bin.root.join("info/a b.trashinfo")).expect("info");
        assert!(
            info.contains(&format!("Path={}/a%20b\n", encode(data.path()))),
            "{info}"
        );
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        let top = drive();
        let bin = bin_in(top.path());
        for _ in 0..2 {
            let item = top.path().join("notes.txt");
            fs::write(&item, "x").expect("write");
            put_in(&bin, &item).expect("trashed");
        }
        assert!(bin.root.join("files/notes.2.txt").is_file());
        assert!(bin.root.join("info/notes.2.txt.trashinfo").is_file());
    }

    #[test]
    fn an_orphan_holding_the_name_is_left_alone() {
        let top = drive();
        let bin = bin_in(top.path());
        fs::write(bin.root.join("files/x"), "orphan").expect("write");
        let item = top.path().join("x");
        fs::write(&item, "new").expect("write");
        put_in(&bin, &item).expect("trashed");
        assert_eq!(
            fs::read_to_string(bin.root.join("files/x")).expect("read"),
            "orphan"
        );
        assert!(bin.root.join("files/x.2").is_file());
        assert!(!bin.root.join("info/x.trashinfo").exists());
    }

    #[test]
    fn a_failed_rename_takes_its_trashinfo_back() {
        let (top, other) = (drive(), drive());
        let bin = bin_in(top.path());
        put_in(&bin, &other.path().join("gone")).expect_err("nothing to move");
        assert_eq!(fs::read_dir(bin.root.join("info")).expect("ls").count(), 0);
        assert_eq!(fs::read_dir(bin.root.join("files")).expect("ls").count(), 0);
    }

    #[test]
    fn paths_are_percent_encoded_as_bytes() {
        assert_eq!(encode(Path::new("/a b/ü%")), "/a%20b/%C3%BC%25");
        assert_eq!(encode(Path::new("rel/x-y_z.~")), "rel/x-y_z.~");
    }

    #[test]
    fn listing_pairs_files_with_their_trashinfo_and_finds_orphans() {
        let top = drive();
        let bin = bin_in(top.path());
        let item = top.path().join("kept.txt");
        fs::write(&item, "abc").expect("write");
        put_in(&bin, &item).expect("trashed");
        fs::create_dir(bin.root.join("files/orphan")).expect("mkdir");
        fs::write(bin.root.join("files/orphan/a"), "a").expect("write");

        let mut listed = list_bin(&bin);
        listed.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        assert_eq!(listed.len(), 2);
        let (kept, kept_meta) = &listed[0];
        assert_eq!(kept.original_path(), item);
        assert!(kept.time_deleted > 0);
        assert!(matches!(kept_meta.size, trash::TrashItemSize::Bytes(3)));
        let (orphan, orphan_meta) = &listed[1];
        assert!(origin_unknown(orphan));
        assert_eq!(PathBuf::from(&orphan.id), bin.root.join("files/orphan"));
        assert!(matches!(orphan_meta.size, trash::TrashItemSize::Entries(1)));
    }

    #[test]
    fn a_trashinfo_that_cannot_be_read_gives_an_unknown_origin() {
        let top = drive();
        let bin = bin_in(top.path());
        fs::write(bin.root.join("files/x"), "x").expect("write");
        fs::write(bin.root.join("info/x.trashinfo"), "junk").expect("write");
        let listed = list_bin(&bin);
        assert!(origin_unknown(&listed[0].0));
        assert_eq!(
            PathBuf::from(&listed[0].0.id),
            bin.root.join("info/x.trashinfo")
        );
    }

    #[test]
    fn purging_an_orphan_removes_it() {
        let top = drive();
        let bin = bin_in(top.path());
        fs::create_dir_all(bin.root.join("files/o/deep")).expect("mkdir");
        let (orphan, _) = list_bin(&bin).pop().expect("listed");
        purge(&orphan).expect("purged");
        assert_eq!(fs::read_dir(bin.root.join("files")).expect("ls").count(), 0);
    }

    #[test]
    fn purging_removes_the_item_then_its_trashinfo() {
        let top = drive();
        let bin = bin_in(top.path());
        let item = top.path().join("f");
        fs::write(&item, "x").expect("write");
        let trashed = put_in(&bin, &item).expect("trashed");
        purge(&trashed).expect("purged");
        assert!(!bin.root.join("files/f").exists());
        assert!(!bin.root.join("info/f.trashinfo").exists());
    }

    #[test]
    fn a_purge_that_fails_keeps_the_trashinfo() {
        let top = drive();
        let bin = bin_in(top.path());
        let item = top.path().join("d");
        fs::create_dir(&item).expect("mkdir");
        fs::write(item.join("inner"), "x").expect("write");
        let trashed = put_in(&bin, &item).expect("trashed");
        // The folder's contents cannot be removed
        let kept = bin.root.join("files/d");
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o555)).expect("chmod");
        let purged = purge(&trashed);
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o755)).expect("chmod");
        purged.expect_err("could not remove");
        assert!(bin.root.join("info/d.trashinfo").exists());
    }

    #[test]
    fn a_drive_trash_counts_as_holding_items_by_files_or_info() {
        let top = drive();
        assert!(!drive_has_items(top.path()));
        let bin = bin_in(top.path());
        assert!(!drive_has_items(top.path()));
        fs::write(bin.root.join("info/x.trashinfo"), "").expect("write");
        assert!(drive_has_items(top.path()));
        fs::remove_file(bin.root.join("info/x.trashinfo")).expect("rm");
        fs::write(bin.root.join("files/x"), "").expect("write");
        assert!(drive_has_items(top.path()));
    }

    #[test]
    fn dates_are_read_in_local_time() {
        let at = deletion_time("2026-10-06T12:30:00").expect("parsed");
        let back = jiff::Timestamp::from_second(at)
            .expect("timestamp")
            .to_zoned(jiff::tz::TimeZone::system());
        assert_eq!(
            back.strftime("%Y-%m-%dT%H:%M:%S").to_string(),
            "2026-10-06T12:30:00"
        );
    }

    #[test]
    fn a_trash_folder_knows_its_drive_root() {
        assert_eq!(
            bin_at(Path::new("/run/media/u/S/.Trash-1000")).top,
            PathBuf::from("/run/media/u/S")
        );
        assert_eq!(
            bin_at(Path::new("/run/media/u/S/.Trash/1000")).top,
            PathBuf::from("/run/media/u/S")
        );
    }

    #[test]
    fn a_relative_origin_is_read_from_the_drive_root() {
        let top = drive();
        let bin = bin_in(top.path());
        fs::write(bin.root.join("files/x"), "x").expect("write");
        fs::write(
            bin.root.join("info/x.trashinfo"),
            "[Trash Info]\nPath=a/x\nDeletionDate=2026-10-06T12:00:00\n",
        )
        .expect("write");
        assert_eq!(
            origin_of(&bin_at(&bin.root), OsStr::new("x")),
            Some(top.path().join("a/x"))
        );
    }

    /// An orphan named like a `.trashinfo` is an item, not the record of
    /// the item whose name it carries: deleting it leaves that one alone
    #[test]
    fn purging_an_orphan_named_like_a_trashinfo_takes_only_it() {
        let top = drive();
        let bin = bin_in(top.path());
        let item = top.path().join("report");
        fs::write(&item, "kept").expect("write");
        put_in(&bin, &item).expect("trashed");
        fs::write(bin.root.join("files/report.trashinfo"), "orphan").expect("write");
        let (orphan, _) = list_bin(&bin)
            .into_iter()
            .find(|(item, _)| origin_unknown(item))
            .expect("listed");
        assert_eq!(
            crate::operation::in_trash(&orphan),
            (bin.root.join("files/report.trashinfo"), None)
        );
        purge(&orphan).expect("purged");
        assert!(!bin.root.join("files/report.trashinfo").exists());
        assert!(
            bin.root.join("files/report").exists(),
            "the other item stays"
        );
        assert!(bin.root.join("info/report.trashinfo").exists());
    }

    /// Inside a trashed folder, a `*.trashinfo` under a folder named `info`
    /// is still an item of that folder
    #[test]
    fn a_trashinfo_inside_a_trashed_folder_is_an_item() {
        let top = drive();
        let bin = bin_in(top.path());
        crate::trash::remember_folder(bin.root.clone());
        let inner = bin.root.join("files/d/info/x.trashinfo");
        fs::create_dir_all(inner.parent().expect("parent")).expect("mkdir");
        fs::write(&inner, "x").expect("write");
        let item = trash::TrashItem {
            id: inner.clone().into_os_string(),
            name: "x.trashinfo".into(),
            original_parent: PathBuf::new(),
            time_deleted: 0,
        };
        assert_eq!(crate::operation::in_trash(&item), (inner, None));
    }

    /// What may be trashed onto a drive is what the trash listing looks at:
    /// local drives, never network ones
    #[test]
    fn only_local_drives_take_a_trash() {
        assert!(takes_a_trash("ext4"));
        assert!(takes_a_trash("vfat"));
        assert!(takes_a_trash("fuseblk"));
        for remote in ["nfs4", "cifs", "fuse.sshfs", "fuse.gvfsd-fuse"] {
            assert!(!takes_a_trash(remote), "{remote}");
        }
    }

    /// A trash inside a folder named `files` that has an `info` beside it
    /// is still a trash: its `.trashinfo` is the record, not the item
    #[test]
    fn a_trash_under_a_folder_named_files_keeps_its_records() {
        let outer = drive();
        fs::create_dir(outer.path().join("info")).expect("mkdir");
        let top = outer.path().join("files/drive");
        fs::create_dir_all(&top).expect("mkdir");
        let bin = bin_in(&top);
        let item = top.join("x");
        fs::write(&item, "contents").expect("write");
        let trashed = put_in(&bin, &item).expect("trashed");
        assert_eq!(
            crate::operation::in_trash(&trashed),
            (
                bin.root.join("files/x"),
                Some(bin.root.join("info/x.trashinfo"))
            )
        );
    }

    /// A bind mount is a mount of its own: what is under it goes to the
    /// trash there, as a rename to the home trash could not reach it
    #[test]
    fn a_path_belongs_to_the_innermost_mount() {
        let mounts: Vec<(PathBuf, String)> =
            [("/", "ext4"), ("/home", "ext4"), ("/mnt/bind", "ext4")]
                .iter()
                .map(|(point, fs)| (PathBuf::from(point), (*fs).to_owned()))
                .collect();
        let of = |path: &str| mount_among(Path::new(path), &mounts).map(|(point, _)| point);
        assert_eq!(of("/mnt/bind/a/b"), Some(PathBuf::from("/mnt/bind")));
        assert_eq!(
            of("/home/u/.local/share/Trash"),
            Some(PathBuf::from("/home"))
        );
        assert_eq!(of("/mnt/other"), Some(PathBuf::from("/")));
        assert_ne!(of("/mnt/bind/a"), of("/home/u/.local/share/Trash"));
    }

    /// A record left without its item, by a purge cut short, goes when the
    /// trash is emptied, unless it is new enough to be a trashing still
    /// under way; a record with its item is never touched
    #[test]
    fn emptying_takes_records_left_without_items() {
        let top = drive();
        let bin = bin_in(top.path());
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let left = bin.root.join("info/left.trashinfo");
        fs::write(&left, "[Trash Info]\nPath=left\n").expect("write");
        filetime::set_file_mtime(&left, filetime::FileTime::from_system_time(old)).expect("mtime");
        let fresh = bin.root.join("info/fresh.trashinfo");
        fs::write(&fresh, "[Trash Info]\nPath=fresh\n").expect("write");
        let item = top.path().join("kept");
        fs::write(&item, "x").expect("write");
        put_in(&bin, &item).expect("trashed");
        let kept = bin.root.join("info/kept.trashinfo");
        filetime::set_file_mtime(&kept, filetime::FileTime::from_system_time(old)).expect("mtime");

        remove_stray_records(std::slice::from_ref(&bin));
        assert!(!left.exists(), "left without its item");
        assert!(fresh.exists(), "maybe a trashing under way");
        assert!(kept.exists(), "its item is there");
        assert!(drive_has_items(top.path()), "the kept item");

        fs::remove_file(bin.root.join("files/kept")).expect("rm");
        fs::remove_file(&kept).expect("rm");
        filetime::set_file_mtime(&fresh, filetime::FileTime::from_system_time(old)).expect("mtime");
        remove_stray_records(std::slice::from_ref(&bin));
        assert!(!drive_has_items(top.path()), "nothing left to ask about");
    }

    /// When an item cannot be looked at, its record stays: only an item
    /// known to be gone leaves a record without one
    #[test]
    fn records_stay_when_their_items_cannot_be_looked_at() {
        let top = drive();
        let bin = bin_in(top.path());
        let item = top.path().join("kept");
        fs::write(&item, "x").expect("write");
        put_in(&bin, &item).expect("trashed");
        let record = bin.root.join("info/kept.trashinfo");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        filetime::set_file_mtime(&record, filetime::FileTime::from_system_time(old))
            .expect("mtime");
        let files = bin.root.join("files");
        fs::set_permissions(&files, fs::Permissions::from_mode(0o000)).expect("chmod");
        remove_stray_records(std::slice::from_ref(&bin));
        fs::set_permissions(&files, fs::Permissions::from_mode(0o700)).expect("chmod");
        assert!(record.exists(), "its item may well be there");
        assert!(files.join("kept").exists());
    }
}
