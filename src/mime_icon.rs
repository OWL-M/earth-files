// SPDX-License-Identifier: GPL-3.0-only

use crate::mime_app::Mime;
use crate::ui::widget::icon;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use xdg_kit::mime::{MimeDatabase, PathOptions};

pub const FALLBACK_MIME_ICON: &str = "text-x-generic";

#[derive(Debug, Eq, Hash, PartialEq)]
struct MimeIconKey {
    mime: Mime,
    size: u16,
}

#[derive(Default)]
pub struct MimeIconCache {
    cache: FxHashMap<MimeIconKey, Option<icon::Handle>>,
}

impl MimeIconCache {
    fn get(&mut self, key: MimeIconKey) -> Option<icon::Handle> {
        self.cache
            .entry(key)
            .or_insert_with_key(|key| {
                let icon_name = SHARED_MIME_INFO.icon_name(&key.mime);
                let mut icon_names = vec![SHARED_MIME_INFO.generic_icon_name(&key.mime)];
                icon_names.retain(|generic| *generic != icon_name);
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

/// The shared-mime-info database.
///
/// Kept apart from [`MIME_ICON_CACHE`] deliberately. It is built once and only
/// ever queried afterwards, so it needs no mutex of its own; while it lived
/// inside one, every mime guess -- including the ones view code makes -- had
/// to queue behind whatever else held that mutex, and a full mime app reload
/// held it for its entire run. Shared, because the mime app associations
/// resolve aliases and parents through it too.
pub static SHARED_MIME_INFO: LazyLock<Arc<MimeDatabase>> = LazyLock::new(|| {
    Arc::new(match crate::xdg_dirs::BASE_DIRS.as_ref() {
        Some(base_dirs) => MimeDatabase::load(base_dirs),
        None => MimeDatabase::from_dirs(std::iter::empty::<PathBuf>()),
    })
});

/// The type of the file at `path`.
///
/// With `by_name`, only its name is looked at: for a remote file, where
/// reading is expensive, and for the first pass of a listing, whose content
/// check comes later. A name the database has no pattern for is
/// `application/octet-stream`. Otherwise the file itself is looked at, as
/// the database says: what kind of file it is, its name, and when the name
/// does not settle it, its first bytes. Content alone never makes a file a
/// desktop entry: only its name can.
pub fn mime_for_path(path: impl AsRef<Path>, by_name: bool) -> Mime {
    let path = path.as_ref();
    if !by_name {
        match SHARED_MIME_INFO.type_for_path(path, &PathOptions::default()) {
            Ok(found) => return found.mime,
            // Gone, or unreadable: the name is all there is to go by
            Err(err) => log::debug!("failed to type {}: {err}", path.display()),
        }
    }
    path.file_name()
        .and_then(|name| SHARED_MIME_INFO.type_for_name(name).best())
        .map_or(Mime::APPLICATION_OCTET_STREAM, |found| found.mime)
}

/// The icon for `mime` at `size`, but only if it has already been resolved.
///
/// Resolving one costs tens of milliseconds, because it searches the icon
/// theme on disk. Callers that run where a person is waiting ask for this,
/// draw [`placeholder_icon`] when it answers `None`, and let
/// [`warm_mime_icons`] fill the cache from a worker.
pub fn try_mime_icon(mime: &Mime, size: u16) -> Option<icon::Handle> {
    let cache = MIME_ICON_CACHE.lock().unwrap();
    cache
        .cache
        .get(&MimeIconKey {
            mime: mime.clone(),
            size,
        })
        .cloned()
        .flatten()
}

/// What the cache knows about one icon.
///
/// The three cases have to be told apart, and told apart in one look. A type
/// the icon theme has nothing for is remembered as such, so asking again
/// would only search the disk to learn the same thing. And asking twice,
/// once for the handle and once for whether it is known, leaves a gap in
/// which another thread fills the cache: the first answer says "no handle",
/// the second says "already known", and the caller neither draws the icon
/// nor asks anyone to resolve it.
#[derive(Clone, Debug)]
pub enum CachedIcon {
    /// Never looked up
    Unknown,
    /// Looked up, and the icon theme has nothing for it
    Missing,
    Found(icon::Handle),
}

/// What the cache knows about `mime` at `size`, in one look
pub fn lookup_mime_icon(mime: &Mime, size: u16) -> CachedIcon {
    let cache = MIME_ICON_CACHE.lock().unwrap();
    match cache.cache.get(&MimeIconKey {
        mime: mime.clone(),
        size,
    }) {
        None => CachedIcon::Unknown,
        Some(None) => CachedIcon::Missing,
        Some(Some(handle)) => CachedIcon::Found(handle.clone()),
    }
}

/// The generic icon drawn until the real one has been resolved.
///
/// Resolved once and kept, so standing in for an unresolved icon costs
/// nothing after the first time.
pub fn placeholder_icon(size: u16) -> icon::Handle {
    static PLACEHOLDERS: LazyLock<Mutex<FxHashMap<u16, icon::Handle>>> =
        LazyLock::new(|| Mutex::new(FxHashMap::default()));
    let mut placeholders = PLACEHOLDERS.lock().unwrap();
    placeholders
        .entry(size)
        .or_insert_with(|| {
            icon::from_name(FALLBACK_MIME_ICON)
                .prefer_svg(true)
                .size(size)
                .handle()
        })
        .clone()
}

/// Resolve every one of `mimes` at every one of `sizes` into the cache.
///
/// Slow by nature, so it belongs on a worker. Afterwards `try_mime_icon`
/// answers for all of them without touching the disk.
pub fn warm_mime_icons(mimes: &[Mime], sizes: &[u16]) {
    for mime in mimes {
        for size in sizes {
            // Taken and released per icon: the view asks for cached icons on
            // the same mutex, and holding it across the whole batch would
            // stall drawing for as long as the batch takes
            let mut cache = MIME_ICON_CACHE.lock().unwrap();
            let _ = cache.get(MimeIconKey {
                mime: mime.clone(),
                size: *size,
            });
        }
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

/// The types `mime` is a direct kind of, as the database says.
pub fn parent_mime_types(mime: &Mime) -> Vec<Mime> {
    SHARED_MIME_INFO.parents(mime)
}

/// Whether `mime_type` is `base` or a kind of it.
pub fn is_mime_subclass_of(mime_type: &Mime, base: &Mime) -> bool {
    SHARED_MIME_INFO.is_a(mime_type, base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A file with content is typed by its name, never as an empty file.
    /// Needs the shared-mime-info database, as the app does.
    #[test]
    fn a_file_with_content_is_typed_by_its_name() {
        let dir = tempfile::tempdir().unwrap();
        for (name, expected) in [
            ("archive.zip", "application/zip"),
            ("picture.png", "image/png"),
            ("notes.txt", "text/plain"),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, b"some bytes that are not nothing").unwrap();
            let mime = mime_for_path(&path, false);
            assert_eq!(mime, Mime::from_static(expected), "for {name}");
        }
    }

    /// Content alone does not make a desktop entry: the name has to say so
    /// too. Needs the shared-mime-info database, as the app does.
    #[test]
    fn only_a_desktop_name_makes_a_desktop_entry() {
        let dir = tempfile::tempdir().unwrap();
        let entry = b"[Desktop Entry]\nType=Application\nName=Viewer\nExec=viewer\n";
        let named = dir.path().join("viewer.desktop");
        let unnamed = dir.path().join("viewer");
        fs::write(&named, entry).unwrap();
        fs::write(&unnamed, entry).unwrap();
        assert_eq!(mime_for_path(&named, false), Mime::APPLICATION_X_DESKTOP);
        assert_eq!(mime_for_path(&unnamed, false), Mime::TEXT_PLAIN);
    }

    /// A name no pattern matches says nothing about the file.
    #[test]
    fn an_unknown_name_is_any_data() {
        let mime = mime_for_path("no-such-folder/file.no-such-extension", true);
        assert_eq!(mime, Mime::APPLICATION_OCTET_STREAM);
    }
}
