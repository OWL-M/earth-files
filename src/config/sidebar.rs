// SPDX-License-Identifier: GPL-3.0-only

//! The order of the part of the sidebar that can be rearranged: the pinned
//! entries, with Recents placed among them.
//!
//! The config keeps the two apart, `favorites` and `recents_position`, so that
//! an older build still reads it. Rearranging works on them put together as
//! one list of [`Entry`], and [`split`] turns that back into the two fields.

use super::Favorite;

/// One entry in the part of the sidebar that can be rearranged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Recents, which can move but never be unpinned.
    Recents,
    /// A pinned entry.
    Favorite(Favorite),
}

/// Where Recents sits among `len` pinned entries: before the pinned entry of
/// that index, or after all of them when it is `len`.
#[must_use]
pub fn recents_index(recents_position: Option<u16>, len: usize) -> usize {
    recents_position.map_or(0, usize::from).min(len)
}

/// Where the pinned entry `favorite_index` sits in [`entries`].
#[must_use]
pub fn favorite_entry(favorite_index: usize, recents_position: Option<u16>, len: usize) -> usize {
    if favorite_index >= recents_index(recents_position, len) {
        favorite_index + 1
    } else {
        favorite_index
    }
}

/// The pinned entries with Recents placed among them, top to bottom.
#[must_use]
pub fn entries(favorites: &[Favorite], recents_position: Option<u16>) -> Vec<Entry> {
    let mut entries: Vec<Entry> = favorites.iter().cloned().map(Entry::Favorite).collect();
    entries.insert(
        recents_index(recents_position, favorites.len()),
        Entry::Recents,
    );
    entries
}

/// Turns `entries` back into `favorites` and `recents_position`. Recents at
/// the top is stored as `None`, which is also what a config without the field
/// reads as.
#[must_use]
pub fn split(entries: Vec<Entry>) -> (Vec<Favorite>, Option<u16>) {
    let mut favorites = Vec::with_capacity(entries.len());
    let mut recents_position = None;
    for entry in entries {
        match entry {
            Entry::Recents => recents_position = u16::try_from(favorites.len()).ok(),
            Entry::Favorite(favorite) => favorites.push(favorite),
        }
    }
    (favorites, recents_position.filter(|&position| position > 0))
}

/// Moves the entry at `from` to just before the entry now at `before`, or to
/// the end when `before` is past the last one. Returns whether the order
/// changed: dropping an entry right before or after itself leaves it be.
pub fn move_entry(entries: &mut Vec<Entry>, from: usize, before: usize) -> bool {
    if from >= entries.len() {
        return false;
    }
    let before = before.min(entries.len());
    let to = if before > from { before - 1 } else { before };
    if to == from {
        return false;
    }
    let entry = entries.remove(from);
    entries.insert(to, entry);
    true
}

/// A change to the sidebar, by place in [`entries`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Move the entry at `from` to just before the entry at `before`, or to
    /// the end when `before` is past the last one.
    Move { from: usize, before: usize },
    /// Pin `favorite` just before the entry at `before`, or at the end.
    Pin { favorite: Favorite, before: usize },
    /// Unpin the entry at `at`.
    Unpin { at: usize },
    /// Pin `favorite` again at `at`, where an unpin took it from.
    Restore { favorite: Favorite, at: usize },
}

/// The config's two fields after an [`Edit`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edited {
    pub favorites: Vec<Favorite>,
    pub recents_position: Option<u16>,
    /// What an unpin took, and from where, for an undo to put back.
    pub unpinned: Option<(Favorite, usize)>,
}

/// Apply `edit` to the sidebar `favorites` and `recents_position` describe,
/// or `None` when it changes nothing: a move beside itself, pinning a folder
/// already pinned, or unpinning Recents, which only ever moves.
#[must_use]
pub fn edit(favorites: &[Favorite], recents_position: Option<u16>, edit: Edit) -> Option<Edited> {
    let pinned = |favorite: &Favorite| {
        favorites
            .iter()
            .any(|pinned| pinned.path_opt() == favorite.path_opt())
    };
    let mut entries = entries(favorites, recents_position);
    let mut unpinned = None;
    match edit {
        Edit::Move { from, before } => {
            if !move_entry(&mut entries, from, before) {
                return None;
            }
        }
        Edit::Pin {
            favorite,
            before: at,
        }
        | Edit::Restore { favorite, at } => {
            if pinned(&favorite) {
                return None;
            }
            let at = at.min(entries.len());
            entries.insert(at, Entry::Favorite(favorite));
        }
        Edit::Unpin { at } => {
            if !matches!(entries.get(at), Some(Entry::Favorite(_))) {
                return None;
            }
            if let Entry::Favorite(favorite) = entries.remove(at) {
                unpinned = Some((favorite, at));
            }
        }
    }
    let (favorites, recents_position) = split(entries);
    Some(Edited {
        favorites,
        recents_position,
        unpinned,
    })
}

/// Unpin the favorite at `favorite_index` in `favorites`, as [`edit`] with
/// [`Edit::Unpin`] does, for removals that name it by that index rather than
/// by its place among the entries.
#[must_use]
pub fn unpin_favorite(
    favorites: &[Favorite],
    recents_position: Option<u16>,
    favorite_index: usize,
) -> Option<Edited> {
    if favorite_index >= favorites.len() {
        return None;
    }
    let at = favorite_entry(favorite_index, recents_position, favorites.len());
    edit(favorites, recents_position, Edit::Unpin { at })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path(name: &str) -> Favorite {
        Favorite::Path(PathBuf::from(format!("/{name}")))
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Recents => String::from("recents"),
                Entry::Favorite(favorite) => favorite
                    .path_opt()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            })
            .collect()
    }

    #[test]
    fn recents_is_placed_among_the_pinned_entries() {
        let favorites = [path("a"), path("b")];
        assert_eq!(names(&entries(&favorites, None)), ["recents", "/a", "/b"]);
        assert_eq!(
            names(&entries(&favorites, Some(1))),
            ["/a", "recents", "/b"]
        );
        assert_eq!(
            names(&entries(&favorites, Some(2))),
            ["/a", "/b", "recents"]
        );
        // A position past the end, left by favorites removed since, is last.
        assert_eq!(
            names(&entries(&favorites, Some(9))),
            ["/a", "/b", "recents"]
        );
    }

    #[test]
    fn split_undoes_entries() {
        let favorites = vec![path("a"), path("b")];
        for recents_position in [None, Some(1), Some(2)] {
            assert_eq!(
                split(entries(&favorites, recents_position)),
                (favorites.clone(), recents_position)
            );
        }
        // At the top is stored as no position at all.
        assert_eq!(split(entries(&favorites, Some(0))), (favorites, None));
    }

    #[test]
    fn a_pinned_entry_is_found_past_recents() {
        // recents, /a, /b
        assert_eq!(favorite_entry(0, None, 2), 1);
        // /a, recents, /b
        assert_eq!(favorite_entry(0, Some(1), 2), 0);
        assert_eq!(favorite_entry(1, Some(1), 2), 2);
    }

    #[test]
    fn move_entry_lands_before_the_entry_named() {
        let mut list = entries(&[path("a"), path("b"), path("c")], None);
        // recents, /a, /b, /c: /c before /a.
        assert!(move_entry(&mut list, 3, 1));
        assert_eq!(names(&list), ["recents", "/c", "/a", "/b"]);
        // Recents to the end.
        assert!(move_entry(&mut list, 0, 4));
        assert_eq!(names(&list), ["/c", "/a", "/b", "recents"]);
        // Downward: /c before /b.
        assert!(move_entry(&mut list, 0, 2));
        assert_eq!(names(&list), ["/a", "/c", "/b", "recents"]);
    }

    fn edited(edited: Option<Edited>) -> (Vec<String>, Option<(Favorite, usize)>) {
        let edited = edited.expect("an edit");
        (
            names(&entries(&edited.favorites, edited.recents_position)),
            edited.unpinned,
        )
    }

    #[test]
    fn a_move_rearranges_pinned_entries_and_recents_alike() {
        let favorites = [path("a"), path("b")];
        assert_eq!(
            edited(edit(&favorites, None, Edit::Move { from: 0, before: 3 })),
            (vec!["/a".into(), "/b".into(), "recents".into()], None)
        );
        assert_eq!(
            edit(&favorites, None, Edit::Move { from: 1, before: 2 }),
            None
        );
    }

    #[test]
    fn a_folder_is_pinned_once() {
        let favorites = [path("a"), path("b")];
        assert_eq!(
            edited(edit(
                &favorites,
                Some(1),
                Edit::Pin {
                    favorite: path("c"),
                    before: 1,
                }
            )),
            (
                vec!["/a".into(), "/c".into(), "recents".into(), "/b".into()],
                None
            )
        );
        assert_eq!(
            edit(
                &favorites,
                None,
                Edit::Pin {
                    favorite: path("b"),
                    before: 0,
                }
            ),
            None
        );
    }

    #[test]
    fn unpinning_says_what_went_and_recents_never_goes() {
        let favorites = [path("a"), path("b")];
        assert_eq!(
            edited(edit(&favorites, None, Edit::Unpin { at: 2 })),
            (vec!["recents".into(), "/a".into()], Some((path("b"), 2)))
        );
        assert_eq!(edit(&favorites, None, Edit::Unpin { at: 0 }), None);
        assert_eq!(edit(&favorites, None, Edit::Unpin { at: 9 }), None);
    }

    #[test]
    fn an_undo_puts_the_entry_back_where_it_was() {
        let favorites = [path("a")];
        assert_eq!(
            edited(edit(
                &favorites,
                None,
                Edit::Restore {
                    favorite: path("b"),
                    at: 1,
                }
            )),
            (vec!["recents".into(), "/b".into(), "/a".into()], None)
        );
        // The list got shorter since: it goes last.
        assert_eq!(
            edited(edit(
                &favorites,
                None,
                Edit::Restore {
                    favorite: path("b"),
                    at: 7,
                }
            ))
            .0,
            ["recents", "/a", "/b"]
        );
        // Pinned again meanwhile: nothing to undo.
        assert_eq!(
            edit(
                &favorites,
                None,
                Edit::Restore {
                    favorite: path("a"),
                    at: 0,
                }
            ),
            None
        );
    }

    /// Removing a favorite some other way than by dragging, such as from its
    /// context menu, leaves Recents among the same neighbours.
    #[test]
    fn unpinning_a_favorite_keeps_recents_in_place() {
        let favorites = [path("a"), path("b"), path("c")];
        // /a, recents, /b, /c
        assert_eq!(
            edited(unpin_favorite(&favorites, Some(1), 0)),
            (
                vec!["recents".into(), "/b".into(), "/c".into()],
                Some((path("a"), 0))
            )
        );
        assert_eq!(
            edited(unpin_favorite(&favorites, Some(1), 2)).0,
            ["/a", "recents", "/b"]
        );
        assert_eq!(unpin_favorite(&favorites, Some(1), 3), None);
    }

    #[test]
    fn dropping_an_entry_beside_itself_changes_nothing() {
        let mut list = entries(&[path("a"), path("b")], None);
        assert!(!move_entry(&mut list, 1, 1));
        assert!(!move_entry(&mut list, 1, 2));
        assert!(!move_entry(&mut list, 7, 0));
        assert_eq!(names(&list), ["recents", "/a", "/b"]);
    }
}
