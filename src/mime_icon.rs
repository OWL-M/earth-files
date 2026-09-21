// SPDX-License-Identifier: GPL-3.0-only

use crate::ui::widget::icon;
use mime_guess::Mime;
use rustc_hash::FxHashMap;
use std::fs;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

pub const FALLBACK_MIME_ICON: &str = "text-x-generic";

#[derive(Debug, Eq, Hash, PartialEq)]
struct MimeIconKey {
    mime: Mime,
    size: u16,
}

#[derive(Default)]
pub struct MimeIconCache {
    cache: FxHashMap<MimeIconKey, Option<icon::Handle>>,
    pub shared_mime_info: xdg_mime::SharedMimeInfo,
}

impl MimeIconCache {
    fn get(&mut self, key: MimeIconKey) -> Option<icon::Handle> {
        self.cache
            .entry(key)
            .or_insert_with_key(|key| {
                let mut icon_names = self.shared_mime_info.lookup_icon_names(&key.mime);
                if icon_names.is_empty() {
                    return None;
                }
                let icon_name = icon_names.remove(0);
                let mut named = icon::from_name(icon_name).prefer_svg(true).size(key.size);
                if !icon_names.is_empty() {
                    let fallback_names =
                        icon_names.into_iter().map(std::borrow::Cow::from).collect();
                    named = named.fallback(Some(icon::IconFallback::Names(fallback_names)));
                }
                Some(named.handle())
            })
            .clone()
    }
}

pub static MIME_ICON_CACHE: LazyLock<Mutex<MimeIconCache>> =
    LazyLock::new(|| Mutex::new(MimeIconCache::default()));

pub fn mime_for_path(
    path: impl AsRef<Path>,
    metadata_opt: Option<&fs::Metadata>,
    remote: bool,
) -> Mime {
    let path = path.as_ref();
    let mime_icon_cache = MIME_ICON_CACHE.lock().unwrap();
    // Try the shared mime info cache first
    let mut gb = mime_icon_cache.shared_mime_info.guess_mime_type();
    gb.zero_size(false);
    if remote {
        if let Some(file_name) = path.file_name().and_then(std::ffi::OsStr::to_str) {
            gb.file_name(file_name);
        }
    } else {
        gb.path(path);
    }
    if let Some(metadata) = metadata_opt {
        gb.metadata(metadata.clone());
    }
    let guess = gb.guess();
    let guessed_mime = guess.mime_type();

    /// The answers `xdg-mime` gives for a directory, a symbolic link and an
    /// empty file. They are also what it gives for everything when no
    /// shared-mime-info database is installed.
    fn is_special(mime: &Mime) -> bool {
        matches!(
            mime.essence_str(),
            "inode/directory" | "inode/symlink" | "application/x-zerosize"
        )
    }

    /// Whether a special answer actually describes this file.
    ///
    /// With a database installed these answers are right and an extension
    /// guess must not override them. With no database the library reports
    /// `application/x-zerosize` for every file, and says it is certain, so
    /// certainty cannot tell the two apart. Checking the answer against the
    /// file can: without this, a system with no database sees every archive,
    /// image and text file as an empty one, refuses to extract anything and
    /// sorts everything into a single type.
    fn special_fits(mime: &Mime, path: &Path, metadata_opt: Option<&fs::Metadata>) -> bool {
        if mime.essence_str() == "inode/symlink" {
            // Any metadata the caller holds followed the link, so ask again
            return fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
        }
        let owned;
        let metadata = match metadata_opt {
            Some(metadata) => metadata,
            None => match fs::metadata(path) {
                Ok(metadata) => {
                    owned = metadata;
                    &owned
                }
                Err(_) => return false,
            },
        };
        match mime.essence_str() {
            "inode/directory" => metadata.is_dir(),
            _ => metadata.len() == 0,
        }
    }

    let usable = if is_special(guessed_mime) {
        // A remote guess sees only a name, and a name cannot show that
        // something is a directory, a link or empty
        !remote && special_fits(guessed_mime, path, metadata_opt)
    } else {
        !guess.uncertain()
    };

    if usable {
        guessed_mime.clone()
    } else {
        // Nothing usable came back, so go by the name
        mime_guess::from_path(path).first_or_octet_stream()
    }
}

pub fn mime_icon(mime: Mime, size: u16) -> icon::Handle {
    let mut mime_icon_cache = MIME_ICON_CACHE.lock().unwrap();
    match mime_icon_cache.get(MimeIconKey { mime, size }) {
        Some(handle) => handle,
        None => icon::from_name(FALLBACK_MIME_ICON)
            .prefer_svg(true)
            .size(size)
            .handle(),
    }
}

pub fn parent_mime_types(mime: &Mime) -> Option<Vec<Mime>> {
    let mime_icon_cache = MIME_ICON_CACHE.lock().unwrap();
    mime_icon_cache.shared_mime_info.get_parents_aliased(mime)
}

pub fn is_mime_subclass_of(mime_type: &Mime, base: &Mime) -> bool {
    let mime_icon_cache = MIME_ICON_CACHE.lock().unwrap();

    mime_icon_cache
        .shared_mime_info
        .mime_type_subclass(mime_type, base)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file with content and a known extension must never be reported as a
    /// directory, a link or an empty file. Without a shared-mime-info
    /// database the library answers `application/x-zerosize` for everything,
    /// and says it is certain, which used to be taken at face value: archives
    /// then refused to extract and every file sorted as the same type. This
    /// holds whether or not a database is installed.
    #[test]
    fn a_file_with_content_is_never_guessed_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        for (name, expected) in [
            ("archive.zip", "application/zip"),
            ("picture.png", "image/png"),
            ("notes.txt", "text/plain"),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, b"some bytes that are not nothing").unwrap();
            let mime = mime_for_path(&path, None, false);
            assert_eq!(mime.essence_str(), expected, "for {name}");
        }

        // What an empty file reports depends on whether a database is
        // installed, so it is not asserted here; what matters is that a file
        // with content is never mistaken for one
    }
}
