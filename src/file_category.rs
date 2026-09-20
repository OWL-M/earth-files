// SPDX-License-Identifier: GPL-3.0-only

//! Coarse file categories shown in the list view's Type column, and the
//! ordering used by the Type sort.

use crate::fl;
use crate::localize::LANGUAGE_SORTER;
use mime_guess::Mime;
use std::cmp::Ordering;
use std::fmt;

/// A coarse description of what a file is, derived from its mime type.
///
/// Declaration order is the order the Type sort groups categories in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum FileCategory {
    Folder,
    Image,
    Video,
    Audio,
    Text,
    Archive,
    Document,
    Program,
    Other,
}

// These lists cover both shared-mime-info and mime_guess spellings, since
// `mime_for_path`'s xdg-mime lookup can fall back to mime_guess.
const ARCHIVES: &[&str] = &[
    "application/zip",
    "application/x-tar",
    "application/gzip",
    "application/x-bzip2",
    "application/x-xz",
    "application/zstd",
    "application/x-7z-compressed",
    "application/vnd.rar",
    "application/x-compressed-tar",
    "application/x-bzip-compressed-tar",
    "application/x-bzip",
    "application/x-bzip2-compressed-tar",
    "application/x-xz-compressed-tar",
    "application/x-rar-compressed",
    "application/x-gzip",
    "application/x-compressed",
];

const DOCUMENTS: &[&str] = &[
    "application/pdf",
    "application/rtf",
    "application/epub+zip",
    "application/msword",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
];

const DOCUMENT_PREFIXES: &[&str] = &[
    "application/vnd.oasis.opendocument.",
    "application/vnd.openxmlformats-officedocument.",
];

const PROGRAMS: &[&str] = &[
    "application/x-executable",
    "application/x-sharedlib",
    "application/x-shellscript",
    "application/x-desktop",
    "application/vnd.microsoft.portable-executable",
    "application/x-sh",
];

impl FileCategory {
    /// The category for an item. `is_dir` wins over the mime, since
    /// directories may carry `inode/directory` or nothing useful.
    pub fn of(is_dir: bool, mime: &Mime) -> Self {
        if is_dir {
            return Self::Folder;
        }
        match mime.type_().as_str() {
            "image" => return Self::Image,
            "video" => return Self::Video,
            "audio" => return Self::Audio,
            "text" => return Self::Text,
            _ => {}
        }
        let essence = mime.essence_str();
        if ARCHIVES.contains(&essence) {
            Self::Archive
        } else if DOCUMENTS.contains(&essence)
            || DOCUMENT_PREFIXES
                .iter()
                .any(|prefix| essence.starts_with(prefix))
        {
            Self::Document
        } else if PROGRAMS.contains(&essence) {
            Self::Program
        } else {
            Self::Other
        }
    }
}

impl fmt::Display for FileCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Folder => fl!("file-type-folder"),
            Self::Image => fl!("file-type-image"),
            Self::Video => fl!("file-type-video"),
            Self::Audio => fl!("file-type-audio"),
            Self::Text => fl!("file-type-text"),
            Self::Archive => fl!("file-type-archive"),
            Self::Document => fl!("file-type-document"),
            Self::Program => fl!("file-type-program"),
            Self::Other => fl!("file-type-other"),
        };
        f.write_str(&text)
    }
}

/// The ordering behind the Type sort: by category, so the groups shown in
/// the Type column stay contiguous, then by mime essence (`type/subtype`,
/// parameters ignored) inside a category, then by name so the order inside
/// one type is stable and matches the Name sort.
pub fn compare_by_type(
    a_is_dir: bool,
    a_mime: &Mime,
    a_name: &str,
    b_is_dir: bool,
    b_mime: &Mime,
    b_name: &str,
) -> Ordering {
    FileCategory::of(a_is_dir, a_mime)
        .cmp(&FileCategory::of(b_is_dir, b_mime))
        .then_with(|| a_mime.essence_str().cmp(b_mime.essence_str()))
        .then_with(|| LANGUAGE_SORTER.compare(a_name, b_name))
}

#[cfg(test)]
mod tests {
    use super::{FileCategory, compare_by_type};
    use mime_guess::Mime;
    use std::cmp::Ordering;

    fn mime(s: &str) -> Mime {
        s.parse().expect("test mime should parse")
    }

    #[test]
    fn category_of_each_branch() {
        let cases: &[(bool, &str, FileCategory)] = &[
            (true, "inode/directory", FileCategory::Folder),
            (true, "image/png", FileCategory::Folder),
            (false, "image/png", FileCategory::Image),
            (false, "video/mp4", FileCategory::Video),
            (false, "audio/flac", FileCategory::Audio),
            (false, "text/plain", FileCategory::Text),
            (false, "text/html", FileCategory::Text),
            (false, "application/zip", FileCategory::Archive),
            (false, "application/x-compressed-tar", FileCategory::Archive),
            (
                false,
                "application/x-bzip2-compressed-tar",
                FileCategory::Archive,
            ),
            (false, "application/x-rar-compressed", FileCategory::Archive),
            (false, "application/pdf", FileCategory::Document),
            (
                false,
                "application/vnd.oasis.opendocument.text",
                FileCategory::Document,
            ),
            (
                false,
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                FileCategory::Document,
            ),
            (false, "application/x-executable", FileCategory::Program),
            (false, "application/x-shellscript", FileCategory::Program),
            (false, "application/x-sh", FileCategory::Program),
            (false, "application/octet-stream", FileCategory::Other),
            (false, "application/x-zerosize", FileCategory::Other),
        ];
        for (is_dir, m, expected) in cases {
            assert_eq!(
                FileCategory::of(*is_dir, &mime(m)),
                *expected,
                "is_dir={is_dir} mime={m}"
            );
        }
    }

    #[test]
    fn category_display_is_not_empty() {
        for category in [
            FileCategory::Folder,
            FileCategory::Image,
            FileCategory::Video,
            FileCategory::Audio,
            FileCategory::Text,
            FileCategory::Archive,
            FileCategory::Document,
            FileCategory::Program,
            FileCategory::Other,
        ] {
            assert!(!category.to_string().is_empty(), "{category:?}");
        }
    }

    #[test]
    fn compare_by_type_orders_category_then_mime_then_name() {
        let cmp = |a: &Mime, a_name: &str, b: &Mime, b_name: &str| {
            compare_by_type(false, a, a_name, false, b, b_name)
        };
        let png = mime("image/png");
        let txt = mime("text/plain");
        assert_eq!(cmp(&png, "b", &txt, "a"), Ordering::Less);
        assert_eq!(cmp(&txt, "a", &png, "b"), Ordering::Greater);
        assert_eq!(cmp(&png, "a", &png, "b"), Ordering::Less);
        assert_eq!(cmp(&png, "b", &png, "a"), Ordering::Greater);
        assert_eq!(cmp(&png, "a", &png, "a"), Ordering::Equal);

        // Categories stay contiguous even where the mime strings interleave:
        // application/pdf < application/x-iso9660-image < application/vnd.oasis...
        let pdf = mime("application/pdf");
        let iso = mime("application/x-iso9660-image");
        let odt = mime("application/vnd.oasis.opendocument.text");
        assert_eq!(cmp(&pdf, "a", &odt, "b"), Ordering::Less);
        assert_eq!(cmp(&odt, "a", &iso, "b"), Ordering::Less);
        assert_eq!(cmp(&iso, "a", &pdf, "b"), Ordering::Greater);

        // A directory is a Folder whatever its mime says
        let dir = mime("inode/directory");
        assert_eq!(
            compare_by_type(true, &dir, "z", false, &png, "a"),
            Ordering::Less
        );
    }
}
