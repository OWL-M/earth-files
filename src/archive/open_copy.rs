//! Temporary copies of what is opened from inside an archive: an application
//! can only be given a file on disk.
//!
//! They live in `$XDG_RUNTIME_DIR/earth-files/archive-open/<process>/`, private
//! to the user. Each process has its own folder, as a new window is a new
//! process and must not take away what an earlier one has open.

use super::index::{self, Only};
use crate::mime_app::Mime;
use crate::mime_icon::mime_for_path;
use crate::operation::{Controller, OperationError};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Tells one opening's folder from another's in this process.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Why something chosen was not given to an application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// It could not be extracted whole.
    Damaged,
    /// A link, or anything else that holds no data of its own. Opening one
    /// would open whatever it points to, outside the archive.
    NotAFile,
    /// A program: it has no business running because it was double-clicked
    /// inside an archive.
    Program,
}

/// What came of opening some entries of an archive.
#[derive(Clone, Debug)]
pub struct Copies {
    /// Holds all the copies, to be removed once they are no longer wanted.
    pub dir: PathBuf,
    /// Each copy, with the type to open it as.
    pub copies: Vec<(Mime, PathBuf)>,
    /// What did not come out, by its name in the archive.
    pub refused: Vec<(PathBuf, Refused)>,
}

/// Where every process keeps its copies, if the session has a place for them.
fn root() -> io::Result<PathBuf> {
    let dirs = crate::xdg_dirs::BASE_DIRS
        .as_ref()
        .ok_or_else(|| io::Error::other("no base directories"))?;
    let runtime = dirs.runtime_dir().map_err(io::Error::other)?;
    Ok(runtime.join("earth-files").join("archive-open"))
}

/// Whether `path` is one of the copies, which are gone before long and so
/// nothing to remember.
pub fn holds(path: &Path) -> bool {
    root().is_ok_and(|root| path.starts_with(root))
}

/// This process's folder of copies.
pub fn own_dir() -> io::Result<PathBuf> {
    let root = root()?;
    let parent = root
        .parent()
        .ok_or_else(|| io::Error::other("no parent for the copies"))?;
    private_dir(parent)?;
    private_dir(&root)?;
    let own = root.join(std::process::id().to_string());
    private_dir(&own)?;
    Ok(own)
}

/// Makes sure `path` is a real folder of this user's that only they can enter.
/// A link found there is refused, not followed.
fn private_dir(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Err(err) if err.kind() != io::ErrorKind::AlreadyExists => return Err(err),
        _ => {}
    }
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no preconditions and cannot fail
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other(format!(
            "{} is not a private folder",
            path.display()
        )));
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The type to open a copy as, or why it is not to be opened. A desktop entry
/// would run its command line, so it is only text here.
fn open_as(mime: Mime) -> Result<Mime, Refused> {
    match mime.as_str() {
        "application/x-desktop" => Ok(Mime::TEXT_PLAIN),
        "application/x-executable"
        | "application/x-pie-executable"
        | "application/x-sharedlib"
        | "application/vnd.appimage" => Err(Refused::Program),
        _ => Ok(mime),
    }
}

/// Extracts `entries` of `archive` as copies in a new folder inside `own`. Each
/// comes out under its own name, private and not executable.
///
/// Nothing is left behind unless there are copies to show for it.
pub fn copy_out(
    own: &Path,
    archive: &Path,
    entries: &[PathBuf],
    password: &Option<String>,
    controller: &Controller,
) -> Result<Copies, OperationError> {
    let dir = own.join(NEXT.fetch_add(1, Ordering::Relaxed).to_string());
    // Numbers are not used twice in a process: anything there is from an
    // earlier process of this one's number, and not to be mixed with this
    remove(&dir);
    private_dir(&dir).map_err(OperationError::from_err)?;
    let copied = copy_into(&dir, archive, entries, password, controller);
    if !copied
        .as_ref()
        .is_ok_and(|copies| !copies.copies.is_empty())
    {
        remove(&dir);
    }
    copied
}

fn copy_into(
    dir: &Path,
    archive: &Path,
    entries: &[PathBuf],
    password: &Option<String>,
    controller: &Controller,
) -> Result<Copies, OperationError> {
    let staging = dir.join("extracting");
    private_dir(&staging).map_err(OperationError::from_err)?;
    let only = Only::new(entries);
    let done = super::extract_only(archive, &staging, password, Some(&only), controller)?;
    open_up(&staging).map_err(OperationError::from_err)?;

    // Damage named by nothing chosen is in what a chosen hard link shares its
    // data with, or in the archive as a whole: any of them may be affected
    let damaged: Vec<PathBuf> = done
        .damaged
        .iter()
        .map(|(name, _)| index::clean(name.as_os_str().as_bytes(), false).unwrap_or(name.clone()))
        .collect();
    let all_damaged = damaged.iter().any(|name| !entries.contains(name));

    let mut copies = Vec::with_capacity(entries.len());
    let mut refused = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let moved = if all_damaged || damaged.contains(entry) {
            Err(Refused::Damaged)
        } else {
            take_out(&staging.join(entry), &dir.join(i.to_string()), entry)
                .map_err(OperationError::from_err)?
        };
        match moved {
            Ok(copy) => copies.push(copy),
            Err(why) => refused.push((entry.clone(), why)),
        }
    }
    remove_tree(&staging).map_err(OperationError::from_err)?;
    Ok(Copies {
        dir: dir.to_path_buf(),
        copies,
        refused,
    })
}

/// Moves the extracted `from` into a new folder `to` of its own, under its name
/// in the archive, as a copy fit to open.
fn take_out(from: &Path, to: &Path, entry: &Path) -> io::Result<Result<(Mime, PathBuf), Refused>> {
    let Some(name) = entry.file_name() else {
        return Ok(Err(Refused::NotAFile));
    };
    let metadata = match fs::symlink_metadata(from) {
        Ok(metadata) => metadata,
        // Named by the archive, yet it never came out
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Err(Refused::Damaged)),
        Err(err) => return Err(err),
    };
    if !metadata.is_file() {
        return Ok(Err(Refused::NotAFile));
    }
    private_dir(to)?;
    let copy = to.join(name);
    fs::rename(from, &copy)?;
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o600))?;
    Ok(open_as(mime_for_path(&copy, false)).map(|mime| (mime, copy)))
}

/// Lets this user into every folder under `path`, however the archive stated
/// them, so what is inside can be moved and removed. Links are not followed.
fn open_up(path: &Path) -> io::Result<()> {
    let mut waiting = vec![path.to_path_buf()];
    while let Some(dir) = waiting.pop() {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                waiting.push(entry.path());
            }
        }
    }
    Ok(())
}

fn remove_tree(path: &Path) -> io::Result<()> {
    open_up(path)?;
    fs::remove_dir_all(path)
}

/// Removes a folder of copies. Failing leaves it for the next start.
pub fn remove(dir: &Path) {
    if let Err(err) = remove_tree(dir)
        && err.kind() != io::ErrorKind::NotFound
    {
        log::warn!("failed to remove {}: {err}", dir.display());
    }
}

/// Removes what processes no longer running left behind, as a crash would.
pub fn remove_leftovers() {
    if let Ok(root) = root() {
        remove_leftovers_in(&root, &|pid| Path::new("/proc").join(pid).exists());
    }
}

/// `running` tells whether the process of that number is still there. Anything
/// in `root` not named after a running process is removed. This process is one
/// of them: this runs alongside the first openings, whose copies are in its
/// folder. A link found in place of `root` is not followed.
fn remove_leftovers_in(root: &Path, running: &dyn Fn(&str) -> bool) {
    if !fs::symlink_metadata(root).is_ok_and(|metadata| metadata.is_dir()) {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.bytes().all(|b| b.is_ascii_digit()) && running(&name) {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            remove(&entry.path());
        } else if let Err(err) = fs::remove_file(entry.path()) {
            log::warn!("failed to remove {}: {err}", entry.path().display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(path: &Path, files: &[(&str, &[u8], u32)]) {
        let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, data, mode) in files {
            let options: zip::write::SimpleFileOptions =
                zip::write::SimpleFileOptions::default().unix_permissions(*mode);
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    fn open(own: &Path, archive: &Path, entries: &[&str]) -> Copies {
        let entries: Vec<PathBuf> = entries.iter().map(PathBuf::from).collect();
        copy_out(own, archive, &entries, &None, &Controller::default()).unwrap()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_copy_is_private_and_not_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.zip");
        zip_of(
            &archive,
            &[
                ("deep/down/run.txt", b"hi", 0o755),
                ("no.txt", b"no", 0o644),
            ],
        );
        let own = tmp.path().join("own");
        private_dir(&own).unwrap();

        let opened = open(&own, &archive, &["deep/down/run.txt"]);
        assert!(opened.refused.is_empty());
        let [(_, copy)] = opened.copies.as_slice() else {
            panic!("{opened:?}");
        };
        assert_eq!(copy.file_name().unwrap(), "run.txt");
        assert_eq!(fs::read(copy).unwrap(), b"hi");
        assert_eq!(mode(copy), 0o600);
        assert_eq!(mode(copy.parent().unwrap()), 0o700);
        assert_eq!(mode(&opened.dir), 0o700);
        assert!(!opened.dir.join("extracting").exists());
    }

    /// Two of one name, from different folders, are both there to open
    #[test]
    fn copies_of_one_name_do_not_meet() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.zip");
        zip_of(
            &archive,
            &[("x/same.txt", b"x", 0o644), ("y/same.txt", b"y", 0o644)],
        );
        let opened = open(tmp.path(), &archive, &["x/same.txt", "y/same.txt"]);
        let read: Vec<Vec<u8>> = opened
            .copies
            .iter()
            .map(|(_, copy)| fs::read(copy).unwrap())
            .collect();
        assert_eq!(read, [b"x".to_vec(), b"y".to_vec()]);
    }

    #[test]
    fn a_desktop_entry_opens_as_text() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.zip");
        let entry = b"[Desktop Entry]\nType=Application\nName=x\nExec=/bin/false\n";
        zip_of(&archive, &[("run.desktop", entry, 0o755)]);
        let opened = open(tmp.path(), &archive, &["run.desktop"]);
        let [(mime, copy)] = opened.copies.as_slice() else {
            panic!("{opened:?}");
        };
        assert_eq!(*mime, Mime::TEXT_PLAIN);
        assert_eq!(mode(copy), 0o600);
    }

    #[test]
    fn a_program_is_not_offered_to_open() {
        assert_eq!(open_as(Mime::APPLICATION_X_DESKTOP), Ok(Mime::TEXT_PLAIN));
        for program in [
            "application/x-executable",
            "application/x-pie-executable",
            "application/x-sharedlib",
            "application/vnd.appimage",
        ] {
            assert_eq!(open_as(program.parse().unwrap()), Err(Refused::Program));
        }
        assert_eq!(open_as(Mime::TEXT_PLAIN), Ok(Mime::TEXT_PLAIN));
    }

    /// A link would open whatever it points to, outside the archive
    #[test]
    fn a_link_is_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let secret = tmp.path().join("secret");
        fs::write(&secret, b"secret").unwrap();
        let archive = tmp.path().join("a.tar");
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o644);
        tar.append_link(&mut header, "link", &secret).unwrap();
        fs::write(&archive, tar.into_inner().unwrap()).unwrap();

        let own = tmp.path().join("own");
        private_dir(&own).unwrap();
        let opened = open(&own, &archive, &["link"]);
        assert!(opened.copies.is_empty());
        assert_eq!(opened.refused, [(PathBuf::from("link"), Refused::NotAFile)]);
        // With nothing to open there is nothing to keep
        assert!(fs::read_dir(&own).unwrap().next().is_none());
        assert_eq!(fs::read(&secret).unwrap(), b"secret");
    }

    /// Cut short by the end of the file: what came out of it is not whole
    #[test]
    fn a_damaged_entry_is_not_opened() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar");
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(4096);
        header.set_mode(0o644);
        tar.append_data(&mut header, "big", &[b'x'; 4096][..])
            .unwrap();
        let mut bytes = tar.into_inner().unwrap();
        bytes.truncate(512 + 1024);
        fs::write(&archive, bytes).unwrap();

        let own = tmp.path().join("own");
        private_dir(&own).unwrap();
        let opened = open(&own, &archive, &["big"]);
        assert!(opened.copies.is_empty());
        assert_eq!(opened.refused, [(PathBuf::from("big"), Refused::Damaged)]);
        assert!(fs::read_dir(&own).unwrap().next().is_none());
    }

    #[test]
    fn stopping_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.zip");
        zip_of(&archive, &[("a.txt", b"a", 0o644)]);
        let own = tmp.path().join("own");
        private_dir(&own).unwrap();
        let controller = Controller::default();
        controller.cancel();
        let entries = [PathBuf::from("a.txt")];
        let copied = copy_out(&own, &archive, &entries, &None, &controller);
        assert!(copied.is_err_and(|err| err.is_cancelled()), "not stopped");
        assert!(fs::read_dir(&own).unwrap().next().is_none());
    }

    /// A folder the archive made unwritable still goes when it is removed
    #[test]
    fn a_shut_folder_is_still_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("dir");
        fs::create_dir_all(dir.join("shut")).unwrap();
        fs::write(dir.join("shut/file"), b"x").unwrap();
        fs::set_permissions(dir.join("shut"), fs::Permissions::from_mode(0o500)).unwrap();
        remove(&dir);
        assert!(!dir.exists());
    }

    #[test]
    fn only_what_no_running_process_owns_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir_all(root.join("100/1")).unwrap();
        fs::create_dir_all(root.join("200/1")).unwrap();
        fs::create_dir_all(root.join("other")).unwrap();
        fs::write(root.join("file"), b"x").unwrap();
        // Links in it go, what they point to stays
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("kept"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("300")).unwrap();

        remove_leftovers_in(&root, &|pid| pid == "100");

        let mut left: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["100"]);
        assert!(outside.join("kept").exists());
    }

    /// This process counts as running like any other: the copies it has just
    /// made are in its folder, and a real look at /proc finds it
    #[test]
    fn a_process_own_copies_survive_the_look_for_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        let own = std::process::id().to_string();
        let copy = tmp.path().join(&own).join("0/0/file");
        fs::create_dir_all(copy.parent().unwrap()).unwrap();
        fs::write(&copy, b"x").unwrap();
        remove_leftovers_in(tmp.path(), &|pid| Path::new("/proc").join(pid).exists());
        assert!(copy.exists());
    }

    /// What an earlier process of this number left under the next number is
    /// not found among the new copies
    #[test]
    fn a_stale_folder_is_not_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.zip");
        zip_of(&archive, &[("a.txt", b"a", 0o644)]);
        let own = tmp.path().join("own");
        // Other tests take numbers too: enough for it to land on one of these
        let next = NEXT.load(Ordering::Relaxed);
        for number in next..next + 1000 {
            let stale = own.join(number.to_string());
            fs::create_dir_all(&stale).unwrap();
            fs::write(stale.join("stale"), b"x").unwrap();
        }
        let opened = open(&own, &archive, &["a.txt"]);
        assert!(opened.dir.starts_with(&own));
        assert!(!opened.dir.join("stale").exists());
    }

    /// A link in place of the folder is not a way to remove what it points to
    #[test]
    fn a_linked_root_is_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("kept"), b"x").unwrap();
        let root = tmp.path().join("root");
        std::os::unix::fs::symlink(&outside, &root).unwrap();
        remove_leftovers_in(&root, &|_| false);
        assert!(outside.join("kept").exists());
    }

    #[test]
    fn a_linked_private_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(private_dir(&link).is_err());
        assert_eq!(mode(&outside), 0o755);
    }
}
