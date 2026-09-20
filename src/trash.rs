use crate::ui::widget;
use regex::Regex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::config::IconSizes;
use crate::tab::{Item, SearchItem};

/// Decode the percent escapes of a `.trashinfo` `Path=` line into a path.
///
/// The escapes stand for bytes, not characters: a path holding `é` is written
/// as two escapes. Collecting them as bytes and building the path from those
/// keeps any name the filesystem allows, including ones that are not UTF-8.
/// Pushing each byte as a `char` would instead read them as Latin-1 and
/// mangle every non-ASCII name.
fn percent_decode(s: &str) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let (mut out, mut bytes) = (Vec::with_capacity(s.len()), s.bytes());
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let (hi, lo) = (bytes.next()?, bytes.next()?);
            let digit = |x: u8| match x {
                b'0'..=b'9' => Some(x - b'0'),
                b'a'..=b'f' => Some(x - b'a' + 10),
                b'A'..=b'F' => Some(x - b'A' + 10),
                _ => None,
            };
            out.push(digit(hi)? << 4 | digit(lo)?);
        } else {
            out.push(byte);
        }
    }
    Some(PathBuf::from(OsString::from_vec(out)))
}

pub trait TrashExt {
    fn is_empty() -> bool;

    fn folders() -> Result<HashSet<PathBuf>, trash::Error>;

    fn scan(sizes: IconSizes) -> Vec<Item>;

    fn scan_search<F: Fn(SearchItem) -> bool + Sync>(callback: F, regex: &Regex);

    fn icon_symbolic(icon_size: u16) -> widget::icon::Handle {
        widget::icon::from_name(if Self::is_empty() {
            "user-trash-symbolic"
        } else {
            "user-trash-full-symbolic"
        })
        .size(icon_size)
        .handle()
    }
}

/// Derive the actual filesystem path of a trashed item from its .trashinfo path
/// (or return the id directly if it's already a filesystem path).
pub fn trash_item_path(item: &trash::TrashItem) -> Option<PathBuf> {
    let id_path = Path::new(&item.id);
    if id_path.extension().is_some_and(|e| e == "trashinfo") {
        let trash_root = id_path.parent()?.parent()?;
        let file_name = id_path.file_stem()?;
        Some(trash_root.join("files").join(file_name))
    } else {
        Some(PathBuf::from(&item.id))
    }
}

/// For a path inside a trash `files/` directory, reconstruct the original path
/// by reading the parent `.trashinfo` file.
///
/// Given `~/.local/share/Trash/files/folder/sub/file.txt`:
/// - The top-level trashed item is `folder`
/// - Read `~/.local/share/Trash/info/folder.trashinfo` to get the original path
/// - Compute: `<original_path>/sub/file.txt`
pub fn original_path_for_trash_child(p: &Path) -> Option<PathBuf> {
    let files = trash_files_dir(p)?;
    let root = files.parent()?;
    let top = p.strip_prefix(files).ok()?.components().next()?;
    let mut trashinfo_name = top.as_os_str().to_os_string();
    trashinfo_name.push(".trashinfo");
    let info = std::fs::read_to_string(root.join("info").join(trashinfo_name)).ok()?;
    let orig = percent_decode(info.lines().find_map(|l| l.strip_prefix("Path="))?.trim())?;
    let rel = p.strip_prefix(files.join(top)).ok()?;
    let mut result = orig;
    if !rel.as_os_str().is_empty() {
        result.push(rel);
    }
    Some(result)
}

static TRASH_FOLDERS: LazyLock<HashSet<PathBuf>> = LazyLock::new(|| {
    Trash::folders().unwrap_or_else(|e| {
        log::warn!("failed to list trash folders: {}", e);
        HashSet::new()
    })
});

fn is_trash_root(root: &Path) -> bool {
    if TRASH_FOLDERS.is_empty() {
        root.join("info").is_dir()
    } else {
        TRASH_FOLDERS.contains(root)
    }
}

fn trash_files_dir(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|a| a.ends_with("files") && a.parent().is_some_and(is_trash_root))
}

/// Check whether a path is inside any trash `files/` directory.
pub fn is_trash_path(path: &Path) -> bool {
    trash_files_dir(path).is_some()
}

pub struct Trash;

impl TrashExt for Trash {
    fn is_empty() -> bool {
        trash::os_limited::is_empty().unwrap_or(true)
    }

    fn folders() -> Result<HashSet<PathBuf>, trash::Error> {
        trash::os_limited::trash_folders()
    }

    fn scan(sizes: IconSizes) -> Vec<Item> {
        use crate::localize::LANGUAGE_SORTER;
        use crate::tab::item_from_trash_entry;
        use std::cmp::Ordering;

        let entries = match trash::os_limited::list() {
            Ok(entry) => entry,
            Err(err) => {
                log::warn!("failed to read trash items: {err}");
                return Vec::new();
            }
        };
        let mut items: Vec<_> = entries
            .into_iter()
            .filter_map(|entry| {
                let metadata = trash::os_limited::metadata(&entry)
                    .inspect_err(|err| {
                        log::warn!("failed to get metadata for trash item {entry:?}: {err}")
                    })
                    .ok()?;
                let item = item_from_trash_entry(entry, metadata, sizes);
                crate::tab::fill_image_dimensions(&item);
                Some(item)
            })
            .collect();
        items.sort_by(|a, b| match (a.metadata.is_dir(), b.metadata.is_dir()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => LANGUAGE_SORTER.compare(&a.display_name, &b.display_name),
        });
        items
    }

    fn scan_search<F: Fn(SearchItem) -> bool + Sync>(callback: F, regex: &Regex) {
        let entries = match trash::os_limited::list() {
            Ok(entries) => entries,
            Err(err) => {
                log::warn!("failed to read trash items: {err}");
                return;
            }
        };

        for entry in entries {
            if let Ok(metadata) = trash::os_limited::metadata(&entry).inspect_err(|err| {
                log::warn!("failed to get metadata for trash item {entry:?}: {err}")
            }) {
                let name = entry.name.to_string_lossy();
                if regex.is_match(&name) && !callback(SearchItem::Trash(entry, metadata)) {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_reads_escapes_as_bytes() {
        // Percent escapes stand for UTF-8 bytes, not Latin-1 characters
        assert_eq!(
            percent_decode("/home/u/Documents/caf%C3%A9.txt"),
            Some(PathBuf::from("/home/u/Documents/café.txt"))
        );
        assert_eq!(
            percent_decode("/home/u/%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82"),
            Some(PathBuf::from("/home/u/привет"))
        );

        // A name that is not valid UTF-8 survives as raw bytes
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            percent_decode("/home/u/%FF%FE"),
            Some(PathBuf::from(OsStr::from_bytes(&[
                b'/', b'h', b'o', b'm', b'e', b'/', b'u', b'/', 0xFF, 0xFE
            ])))
        );

        // Plain paths pass through, and malformed escapes are refused
        assert_eq!(
            percent_decode("/home/u/plain.txt"),
            Some(PathBuf::from("/home/u/plain.txt"))
        );
        assert_eq!(percent_decode("/home/u/bad%"), None);
        assert_eq!(percent_decode("/home/u/bad%zz"), None);
        assert_eq!(percent_decode("/home/u/bad%4"), None);
    }
}
