// SPDX-License-Identifier: GPL-3.0-only

//! Renaming several items at once from one dialog.
//!
//! A template names every item from a pattern with tags for the original name
//! and a running number; find and replace edits every name the same way. Both
//! produce a preview that flags the names that cannot be applied.

use std::{collections::HashMap, path::Path};

use crate::fl;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Template,
    Replace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    pub mode: Mode,
    pub template: String,
    pub find: String,
    pub replace: String,
}

impl Settings {
    /// The template starts as the original-name tag, so the dialog opens with
    /// the names unchanged and the user adds to them
    pub fn new(tags: &Tags) -> Self {
        Self {
            mode: Mode::Template,
            template: tags.name.clone(),
            find: String::new(),
            replace: String::new(),
        }
    }
}

/// The tags a template may contain; they are localized, like the dialog
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tags {
    pub name: String,
    pub number: String,
}

impl Tags {
    pub fn localized() -> Self {
        Self {
            name: fl!("batch-rename-tag-name"),
            number: fl!("batch-rename-tag-number"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub old: String,
    pub new: String,
    /// The new name cannot be applied: it is invalid, taken on disk, or
    /// another item gets the same name
    pub conflict: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Preview {
    pub rows: Vec<Row>,
    /// Rows whose name changes
    pub changed: usize,
    pub conflicts: usize,
}

impl Preview {
    pub fn ready(&self) -> bool {
        self.changed > 0 && self.conflicts == 0
    }
}

/// Fill in a template's tags in one pass.
///
/// Substituting one tag and then the other would rescan the text just
/// inserted, so a file whose own name contains the number tag would have part
/// of its name replaced. Walking the template once means only the template's
/// own tags are ever matched.
fn apply_template(template: &str, tags: &Tags, name: &str, number: &str) -> String {
    let mut out = String::with_capacity(template.len() + name.len());
    let mut rest = template;
    while !rest.is_empty() {
        if !tags.name.is_empty()
            && let Some(tail) = rest.strip_prefix(tags.name.as_str())
        {
            out.push_str(name);
            rest = tail;
        } else if !tags.number.is_empty()
            && let Some(tail) = rest.strip_prefix(tags.number.as_str())
        {
            out.push_str(number);
            rest = tail;
        } else {
            let next = rest.chars().next().expect("rest is not empty");
            out.push(next);
            rest = &rest[next.len_utf8()..];
        }
    }
    out
}

/// The new name of each of `names`, in order
pub fn new_names(names: &[String], settings: &Settings, tags: &Tags) -> Vec<String> {
    match settings.mode {
        Mode::Template => {
            // Numbers are padded to the width of the last one, so they sort
            let width = names.len().to_string().len();
            names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    apply_template(
                        &settings.template,
                        tags,
                        name,
                        &format!("{:0width$}", i + 1),
                    )
                })
                .collect()
        }
        Mode::Replace => names
            .iter()
            .map(|name| {
                if settings.find.is_empty() {
                    name.clone()
                } else {
                    name.replace(&settings.find, &settings.replace)
                }
            })
            .collect(),
    }
}

/// Every old and new name with its conflicts, for items in `parent`.
///
/// `check_existing` stats the destination of every changed name. It is worth
/// it locally, where it turns a failed rename into a warning the user sees
/// before pressing the button, but on a network mount those stats are slow
/// enough to be worse than the error. Either way this is only a warning: the
/// rename refuses to replace an existing file on its own, because the preview
/// is a snapshot and the folder can change while the dialog is open.
pub fn preview(
    parent: &Path,
    names: &[String],
    settings: &Settings,
    tags: &Tags,
    check_existing: bool,
) -> Preview {
    let new = new_names(names, settings, tags);
    let mut uses: HashMap<&str, usize> = HashMap::new();
    for name in &new {
        *uses.entry(name.as_str()).or_default() += 1;
    }
    let mut preview = Preview::default();
    for (old, new) in names.iter().zip(new.iter()) {
        let changed = old != new;
        let invalid = new.is_empty() || new == "." || new == ".." || new.contains('/');
        let taken = check_existing && changed && parent.join(new).exists();
        let conflict = invalid || taken || uses[new.as_str()] > 1;
        preview.changed += usize::from(changed);
        preview.conflicts += usize::from(conflict);
        preview.rows.push(Row {
            old: old.clone(),
            new: new.clone(),
            conflict,
        });
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags() -> Tags {
        Tags {
            name: "[Original name]".into(),
            number: "[1, 2, 3]".into(),
        }
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn template(template: &str) -> Settings {
        Settings {
            mode: Mode::Template,
            template: template.into(),
            find: String::new(),
            replace: String::new(),
        }
    }

    fn replace(find: &str, replace: &str) -> Settings {
        Settings {
            mode: Mode::Replace,
            template: String::new(),
            find: find.into(),
            replace: replace.into(),
        }
    }

    #[test]
    fn template_substitutes_name_and_padded_number() {
        let names = names(&[
            "a.txt", "b.txt", "c.txt", "d.txt", "e.txt", "f.txt", "g.txt", "h.txt", "i.txt",
            "j.txt",
        ]);
        let new = new_names(&names, &template("[1, 2, 3] [Original name]"), &tags());
        assert_eq!(new[0], "01 a.txt");
        assert_eq!(new[9], "10 j.txt");
    }

    #[test]
    fn template_does_not_reinterpret_tag_text_coming_from_a_filename() {
        // A file literally named after the number tag must keep its name
        let names = names(&["[1, 2, 3].txt", "b.txt"]);
        let new = new_names(&names, &template("[Original name]"), &tags());
        assert_eq!(new, names);

        // And the tags still apply to the template's own text around it
        let new = new_names(&names, &template("[1, 2, 3] [Original name]"), &tags());
        assert_eq!(new, vec!["1 [1, 2, 3].txt", "2 b.txt"]);
    }

    #[test]
    fn template_without_tags_gives_every_item_the_same_name() {
        let new = new_names(&names(&["a", "b"]), &template("same"), &tags());
        assert_eq!(new, vec!["same", "same"]);
    }

    #[test]
    fn replace_edits_every_name_and_ignores_an_empty_find() {
        let names = names(&["IMG_1.jpg", "IMG_2.jpg", "other.jpg"]);
        let new = new_names(&names, &replace("IMG_", "holiday-"), &tags());
        assert_eq!(new, vec!["holiday-1.jpg", "holiday-2.jpg", "other.jpg"]);
        assert_eq!(new_names(&names, &replace("", "x"), &tags()), names);
    }

    #[test]
    fn preview_counts_changes_and_flags_duplicates_and_invalid_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("taken"), b"").unwrap();
        let tags = tags();

        // Nothing changes: not ready, no conflicts
        let unchanged = preview(
            dir.path(),
            &names(&["a", "b"]),
            &template("[Original name]"),
            &tags,
            true,
        );
        assert_eq!((unchanged.changed, unchanged.conflicts), (0, 0));
        assert!(!unchanged.ready());

        // Two items mapping to one name conflict with each other
        let same = preview(
            dir.path(),
            &names(&["a", "b"]),
            &template("same"),
            &tags,
            true,
        );
        assert_eq!(same.conflicts, 2);
        assert!(!same.ready());

        // A name that exists on disk is taken; a slash is invalid
        let taken = preview(
            dir.path(),
            &names(&["a", "b"]),
            &replace("a", "taken"),
            &tags,
            true,
        );
        assert!(taken.rows[0].conflict);
        assert!(!taken.rows[1].conflict);
        let slash = preview(
            dir.path(),
            &names(&["a"]),
            &replace("a", "x/y"),
            &tags,
            true,
        );
        assert!(slash.rows[0].conflict);

        // An item keeping its own name is neither changed nor taken
        let keep = preview(
            dir.path(),
            &names(&["taken", "b"]),
            &replace("b", "c"),
            &tags,
            true,
        );
        assert_eq!((keep.changed, keep.conflicts), (1, 0));
        assert!(keep.ready());
    }
}
