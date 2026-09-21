// SPDX-License-Identifier: GPL-3.0-only

//! Translation between the portal's option and result dictionaries and the
//! chooser's own types.
//!
//! Kept apart from the service so it can be tested without a bus.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use zbus::zvariant::{OwnedValue, Value};

use crate::dialog::{
    DialogChoice, DialogChoiceOption, DialogFilter, DialogFilterPattern, DialogKind,
};

/// The response codes a backend may return, as the portal defines them
pub const RESPONSE_SUCCESS: u32 = 0;
pub const RESPONSE_CANCELLED: u32 = 1;
pub const RESPONSE_OTHER: u32 = 2;

/// Which of the three methods was called
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    OpenFile,
    SaveFile,
    /// Several files into one directory the user picks
    SaveFiles,
}

/// One request, in terms the chooser understands
#[derive(Clone, Debug)]
pub struct Request {
    pub method: Method,
    pub title: String,
    pub accept_label: Option<String>,
    pub kind: DialogKind,
    pub folder: Option<PathBuf>,
    pub filters: Vec<DialogFilter>,
    pub filter_selected: Option<usize>,
    pub choices: Vec<DialogChoice>,
    /// The names `SaveFiles` was asked to write, joined onto the chosen
    /// directory when the answer comes back
    pub save_names: Vec<std::ffi::OsString>,
}

fn get_bool(options: &HashMap<String, OwnedValue>, key: &str) -> bool {
    options
        .get(key)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

fn get_string(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    options
        .get(key)
        .and_then(|value| String::try_from(value.clone()).ok())
}

/// A path the portal passes as bytes. It is nul terminated by convention, and
/// it is filesystem bytes rather than text, so it is read as such.
fn get_path(options: &HashMap<String, OwnedValue>, key: &str) -> Option<PathBuf> {
    let bytes: Vec<u8> = options
        .get(key)
        .and_then(|value| Vec::<u8>::try_from(value.clone()).ok())?;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(&bytes);
    if bytes.is_empty() {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(bytes)))
}

/// `a(sa(us))`: a label and a list of `(kind, pattern)`, where kind 0 is a
/// glob and kind 1 is a mime type.
fn get_filters(value: &OwnedValue) -> Vec<DialogFilter> {
    let Ok(raw) = <Vec<(String, Vec<(u32, String)>)>>::try_from(value.clone()) else {
        return Vec::new();
    };
    raw.into_iter()
        .map(|(label, patterns)| DialogFilter {
            label,
            patterns: patterns
                .into_iter()
                .map(|(kind, pattern)| {
                    if kind == 1 {
                        DialogFilterPattern::Mime(pattern)
                    } else {
                        DialogFilterPattern::Glob(pattern)
                    }
                })
                .collect(),
        })
        .collect()
}

fn get_current_filter(value: &OwnedValue) -> Option<DialogFilter> {
    let (label, patterns) = <(String, Vec<(u32, String)>)>::try_from(value.clone()).ok()?;
    Some(DialogFilter {
        label,
        patterns: patterns
            .into_iter()
            .map(|(kind, pattern)| {
                if kind == 1 {
                    DialogFilterPattern::Mime(pattern)
                } else {
                    DialogFilterPattern::Glob(pattern)
                }
            })
            .collect(),
    })
}

/// `a(ssa(ss)s)`: id, label, options, and the initially selected option. An
/// empty option list means a check box, whose values are "true" and "false".
fn get_choices(value: &OwnedValue) -> Vec<DialogChoice> {
    let Ok(raw) = <Vec<(String, String, Vec<(String, String)>, String)>>::try_from(value.clone())
    else {
        return Vec::new();
    };
    raw.into_iter()
        .map(|(id, label, options, selected)| {
            if options.is_empty() {
                DialogChoice::CheckBox {
                    id,
                    label,
                    value: selected == "true",
                }
            } else {
                // An empty initial selection means the backend picks. The
                // answer has to be one of the offered ids, so falling back to
                // nothing selected would return an id the caller never
                // offered.
                let selected = options
                    .iter()
                    .position(|(option_id, _)| *option_id == selected)
                    .or(Some(0));
                DialogChoice::ComboBox {
                    id,
                    label,
                    options: options
                        .into_iter()
                        .map(|(id, label)| DialogChoiceOption { id, label })
                        .collect(),
                    selected,
                }
            }
        })
        .collect()
}

impl Request {
    /// Read a request out of the portal's option dictionary.
    #[must_use]
    pub fn from_options(
        method: Method,
        title: &str,
        options: &HashMap<String, OwnedValue>,
    ) -> Self {
        let filters = options.get("filters").map(get_filters).unwrap_or_default();
        let current = options.get("current_filter").and_then(get_current_filter);
        let filter_selected = current.as_ref().and_then(|current| {
            filters
                .iter()
                .position(|filter| filter.label == current.label)
        });
        // A filter may be given without being in the list, in which case it
        // applies on its own
        let (filters, filter_selected) = match (filter_selected, current) {
            (Some(index), _) => (filters, Some(index)),
            (None, Some(current)) if filters.is_empty() => (vec![current], Some(0)),
            _ => (filters, None),
        };

        // `current_file` names an existing file to start on; its directory is
        // the folder to show, and its name the one to suggest.
        //
        // Only a name that is valid UTF-8 can be suggested: the chooser
        // carries a name as text, and it decides from that text whether the
        // destination already exists and needs confirming. Handing it a lossy
        // rendering and swapping the real bytes back afterwards would mean
        // confirming one file and returning another, so a name that cannot be
        // shown is not suggested at all and the user types one.
        let current_file = get_path(options, "current_file");
        let suggested = current_file
            .as_ref()
            .and_then(|file| file.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_string);
        let folder = get_path(options, "current_folder").or_else(|| {
            current_file
                .as_ref()
                .and_then(|file| file.parent().map(std::path::Path::to_path_buf))
        });

        let multiple = get_bool(options, "multiple");
        let directory = get_bool(options, "directory");
        let kind = match method {
            // Picking where to put several files is picking a directory
            Method::SaveFiles => DialogKind::OpenFolder,
            Method::SaveFile => DialogKind::SaveFile {
                // `current_file` names an existing file being saved over, so
                // it supplies the name as well as the folder. Without this a
                // save-over request opens with an empty name and the accept
                // button disabled until the user retypes it.
                filename: get_string(options, "current_name")
                    .or_else(|| suggested.clone())
                    .unwrap_or_default(),
            },
            Method::OpenFile => match (directory, multiple) {
                (true, true) => DialogKind::OpenMultipleFolders,
                (true, false) => DialogKind::OpenFolder,
                (false, true) => DialogKind::OpenMultipleFiles,
                (false, false) => DialogKind::OpenFile,
            },
        };

        // Filesystem bytes, not text: converting them lossily would rename
        // what the caller asked for, and could collapse two names into one
        let save_names = options
            .get("files")
            .and_then(|value| <Vec<Vec<u8>>>::try_from(value.clone()).ok())
            .map(|names| {
                names
                    .into_iter()
                    .map(|name| {
                        let name = name.strip_suffix(&[0]).unwrap_or(&name).to_vec();
                        std::ffi::OsString::from_vec(name)
                    })
                    .collect()
            })
            .unwrap_or_default();

        Self {
            method,
            title: title.to_string(),
            accept_label: get_string(options, "accept_label"),
            kind,
            folder,
            filters,
            filter_selected,
            choices: options.get("choices").map(get_choices).unwrap_or_default(),
            save_names,
        }
    }

    /// The paths the caller should be told about.
    ///
    /// For `SaveFiles` the user chose a directory, and the names the caller
    /// supplied are joined onto it.
    #[must_use]
    pub fn selected_paths(&self, chosen: Vec<PathBuf>) -> Vec<PathBuf> {
        if self.method == Method::SaveFiles {
            let Some(directory) = chosen.first() else {
                return Vec::new();
            };
            return self
                .save_names
                .iter()
                .map(|name| directory.join(name))
                .collect();
        }
        chosen
    }
}

/// A path as the `file://` URI the portal returns
#[must_use]
pub fn path_to_uri(path: &std::path::Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(*byte as char);
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}

/// The results dictionary for a successful answer
#[must_use]
pub fn results(
    paths: &[PathBuf],
    choices: &[(String, String)],
    current_filter: Option<&DialogFilter>,
) -> HashMap<String, OwnedValue> {
    // `writable` is deliberately absent. Its documentation says the default
    // is false, but the frontend starts the document flags with write set and
    // only clears them when the key is present and false
    // (`src/file-chooser.c` in xdg-desktop-portal), so omitting it is the
    // permissive case, not the restrictive one. Setting it false here would
    // hand every sandboxed application a read-only copy of the file it just
    // asked to open.
    let mut results = HashMap::new();
    let uris: Vec<String> = paths.iter().map(|path| path_to_uri(path)).collect();
    if let Ok(value) = OwnedValue::try_from(Value::from(uris)) {
        results.insert("uris".to_string(), value);
    }
    if !choices.is_empty()
        && let Ok(value) = OwnedValue::try_from(Value::from(choices.to_vec()))
    {
        results.insert("choices".to_string(), value);
    }
    if let Some(filter) = current_filter {
        let patterns: Vec<(u32, String)> = filter
            .patterns
            .iter()
            .map(|pattern| match pattern {
                DialogFilterPattern::Glob(value) => (0, value.clone()),
                DialogFilterPattern::Mime(value) => (1, value.clone()),
            })
            .collect();
        if let Ok(value) = OwnedValue::try_from(Value::from((filter.label.clone(), patterns))) {
            results.insert("current_filter".to_string(), value);
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(pairs: Vec<(&str, Value<'static>)>) -> HashMap<String, OwnedValue> {
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_string(), OwnedValue::try_from(value).unwrap()))
            .collect()
    }

    #[test]
    fn open_file_reads_its_kind_from_the_options() {
        let plain = Request::from_options(Method::OpenFile, "Open", &options(vec![]));
        assert!(matches!(plain.kind, DialogKind::OpenFile));

        let many = Request::from_options(
            Method::OpenFile,
            "Open",
            &options(vec![("multiple", Value::from(true))]),
        );
        assert!(matches!(many.kind, DialogKind::OpenMultipleFiles));

        let folder = Request::from_options(
            Method::OpenFile,
            "Open",
            &options(vec![("directory", Value::from(true))]),
        );
        assert!(matches!(folder.kind, DialogKind::OpenFolder));

        let folders = Request::from_options(
            Method::OpenFile,
            "Open",
            &options(vec![
                ("directory", Value::from(true)),
                ("multiple", Value::from(true)),
            ]),
        );
        assert!(matches!(folders.kind, DialogKind::OpenMultipleFolders));
    }

    #[test]
    fn save_file_carries_the_suggested_name() {
        let request = Request::from_options(
            Method::SaveFile,
            "Save",
            &options(vec![("current_name", Value::from("report.pdf"))]),
        );
        match request.kind {
            DialogKind::SaveFile { filename } => assert_eq!(filename, "report.pdf"),
            other => panic!("expected a save dialog, got {other:?}"),
        }
    }

    #[test]
    fn saving_over_a_file_keeps_its_name() {
        // `current_file` names the file being saved over, so it supplies the
        // name as well as the folder; without it the accept button stays
        // disabled until the user retypes what they already chose
        let request = Request::from_options(
            Method::SaveFile,
            "Save",
            &options(vec![(
                "current_file",
                Value::from(b"/home/user/report.txt\0".to_vec()),
            )]),
        );
        match request.kind {
            DialogKind::SaveFile { filename } => assert_eq!(filename, "report.txt"),
            other => panic!("expected a save dialog, got {other:?}"),
        }
        assert_eq!(request.folder, Some(PathBuf::from("/home/user")));

        // An explicit name still wins
        let named = Request::from_options(
            Method::SaveFile,
            "Save",
            &options(vec![
                ("current_file", Value::from(b"/home/user/a.txt\0".to_vec())),
                ("current_name", Value::from("b.txt")),
            ]),
        );
        match named.kind {
            DialogKind::SaveFile { filename } => assert_eq!(filename, "b.txt"),
            other => panic!("expected a save dialog, got {other:?}"),
        }
    }

    #[test]
    fn a_name_that_cannot_be_shown_is_not_suggested() {
        // The chooser carries a name as text and decides from that text
        // whether the destination exists and needs confirming. Suggesting a
        // lossy rendering and swapping the real bytes back afterwards would
        // confirm one file and return another, so it is not suggested at all.
        let mut bytes = b"/tmp/a\xff.txt".to_vec();
        bytes.push(0);
        let request = Request::from_options(
            Method::SaveFile,
            "Save",
            &options(vec![("current_file", Value::from(bytes))]),
        );
        match &request.kind {
            DialogKind::SaveFile { filename } => {
                assert!(filename.is_empty(), "got {filename:?}");
            }
            other => panic!("expected a save dialog, got {other:?}"),
        }
        // The folder is still useful, so it is kept
        assert_eq!(request.folder, Some(PathBuf::from("/tmp")));

        // Whatever the user types is returned untouched
        let typed = PathBuf::from("/tmp/typed.txt");
        assert_eq!(request.selected_paths(vec![typed.clone()]), vec![typed]);
    }

    #[test]
    fn a_choice_with_no_initial_selection_still_answers_with_a_real_option() {
        // An empty initial selection asks the backend to pick. Answering with
        // an id the caller never offered would be worse than picking one.
        let choices = vec![(
            "encoding".to_string(),
            "Encoding".to_string(),
            vec![
                ("utf8".to_string(), "Unicode".to_string()),
                ("latin15".to_string(), "Western".to_string()),
            ],
            String::new(),
        )];
        let request = Request::from_options(
            Method::OpenFile,
            "Open",
            &options(vec![("choices", Value::from(choices))]),
        );
        match &request.choices[0] {
            DialogChoice::ComboBox { selected, .. } => assert_eq!(*selected, Some(0)),
            other => panic!("expected a combo box, got {other:?}"),
        }
    }

    #[test]
    fn save_names_that_are_not_text_are_left_alone() {
        // Filesystem bytes: converting them lossily renames what the caller
        // asked for, and can collapse two names onto one destination
        let names: Vec<Vec<u8>> = vec![b"a\xff.txt".to_vec(), b"a\xfe.txt".to_vec()];
        let request = Request::from_options(
            Method::SaveFiles,
            "Save",
            &options(vec![("files", Value::from(names))]),
        );
        let paths = request.selected_paths(vec![PathBuf::from("/tmp/out")]);
        assert_eq!(paths.len(), 2);
        assert_ne!(paths[0], paths[1], "distinct names must stay distinct");
        assert_eq!(
            paths[0].file_name().unwrap().as_bytes(),
            b"a\xff.txt",
            "the bytes the caller gave must survive"
        );
    }

    #[test]
    fn a_folder_is_read_from_nul_terminated_bytes() {
        let request = Request::from_options(
            Method::OpenFile,
            "Open",
            &options(vec![(
                "current_folder",
                Value::from(b"/home/user/docs\0".to_vec()),
            )]),
        );
        assert_eq!(request.folder, Some(PathBuf::from("/home/user/docs")));

        // A file instead names the folder holding it
        let from_file = Request::from_options(
            Method::SaveFile,
            "Save",
            &options(vec![(
                "current_file",
                Value::from(b"/home/user/docs/a.txt\0".to_vec()),
            )]),
        );
        assert_eq!(from_file.folder, Some(PathBuf::from("/home/user/docs")));
    }

    #[test]
    fn filters_keep_their_kinds_and_selection() {
        let filters = vec![
            ("Images".to_string(), vec![(1_u32, "image/*".to_string())]),
            ("Text".to_string(), vec![(0_u32, "*.txt".to_string())]),
        ];
        let request = Request::from_options(
            Method::OpenFile,
            "Open",
            &options(vec![
                ("filters", Value::from(filters)),
                (
                    "current_filter",
                    Value::from(("Text".to_string(), vec![(0_u32, "*.txt".to_string())])),
                ),
            ]),
        );
        assert_eq!(request.filters.len(), 2);
        assert!(matches!(
            request.filters[0].patterns[0],
            DialogFilterPattern::Mime(_)
        ));
        assert!(matches!(
            request.filters[1].patterns[0],
            DialogFilterPattern::Glob(_)
        ));
        assert_eq!(request.filter_selected, Some(1));
    }

    #[test]
    fn a_lone_filter_applies_on_its_own() {
        let request = Request::from_options(
            Method::OpenFile,
            "Open",
            &options(vec![(
                "current_filter",
                Value::from(("Only".to_string(), vec![(0_u32, "*".to_string())])),
            )]),
        );
        assert_eq!(request.filters.len(), 1);
        assert_eq!(request.filter_selected, Some(0));
    }

    #[test]
    fn choices_become_boxes_and_check_boxes() {
        let choices = vec![
            (
                "encoding".to_string(),
                "Encoding".to_string(),
                vec![
                    ("utf8".to_string(), "Unicode".to_string()),
                    ("latin15".to_string(), "Western".to_string()),
                ],
                "latin15".to_string(),
            ),
            (
                "reencode".to_string(),
                "Reencode".to_string(),
                Vec::<(String, String)>::new(),
                "true".to_string(),
            ),
        ];
        let request = Request::from_options(
            Method::OpenFile,
            "Open",
            &options(vec![("choices", Value::from(choices))]),
        );
        match &request.choices[0] {
            DialogChoice::ComboBox {
                id,
                selected,
                options,
                ..
            } => {
                assert_eq!(id, "encoding");
                assert_eq!(options.len(), 2);
                assert_eq!(*selected, Some(1), "the named option is the selected one");
            }
            other => panic!("expected a combo box, got {other:?}"),
        }
        match &request.choices[1] {
            DialogChoice::CheckBox { id, value, .. } => {
                assert_eq!(id, "reencode");
                assert!(*value);
            }
            other => panic!("expected a check box, got {other:?}"),
        }
    }

    #[test]
    fn save_files_joins_its_names_onto_the_chosen_directory() {
        let names: Vec<Vec<u8>> = vec![b"one.txt\0".to_vec(), b"two.txt".to_vec()];
        let request = Request::from_options(
            Method::SaveFiles,
            "Save",
            &options(vec![("files", Value::from(names))]),
        );
        assert!(matches!(request.kind, DialogKind::OpenFolder));
        assert_eq!(
            request.selected_paths(vec![PathBuf::from("/tmp/out")]),
            vec![
                PathBuf::from("/tmp/out/one.txt"),
                PathBuf::from("/tmp/out/two.txt")
            ]
        );
        // Nothing chosen means nothing to write
        assert!(request.selected_paths(Vec::new()).is_empty());
    }

    #[test]
    fn paths_become_escaped_file_uris() {
        assert_eq!(
            path_to_uri(std::path::Path::new("/home/user/a b.txt")),
            "file:///home/user/a%20b.txt"
        );
        assert_eq!(
            path_to_uri(std::path::Path::new("/home/user/café.txt")),
            "file:///home/user/caf%C3%A9.txt"
        );
        // A name that is not valid text still produces a usable URI
        let raw = PathBuf::from(OsStr::from_bytes(&[b'/', b't', 0xFF]));
        assert_eq!(path_to_uri(&raw), "file:///t%FF");
    }

    #[test]
    fn results_carry_uris_choices_and_the_filter() {
        let filter = DialogFilter {
            label: "Text".to_string(),
            patterns: vec![DialogFilterPattern::Glob("*.txt".to_string())],
        };
        let full = results(
            &[PathBuf::from("/tmp/a.txt")],
            &[("reencode".to_string(), "true".to_string())],
            Some(&filter),
        );
        assert_eq!(
            <Vec<String>>::try_from(full["uris"].clone()).unwrap(),
            vec!["file:///tmp/a.txt".to_string()]
        );
        assert!(full.contains_key("choices"));
        assert!(full.contains_key("current_filter"));

        // Nothing optional is invented when there is nothing to say
        let bare = results(&[PathBuf::from("/tmp/a.txt")], &[], None);
        assert!(!bare.contains_key("choices"));
        assert!(!bare.contains_key("current_filter"));
    }
}
