//! What browsing folders takes, the same in the main window and the file
//! chooser: watching the folders shown, telling what a change on disk means
//! for a listing, and reading changed items again. Each window decides what
//! to do with the answers.

use std::path::{Path, PathBuf};

use notify_debouncer_full::notify::{self, RecommendedWatcher};
use notify_debouncer_full::{DebouncedEvent, Debouncer, RecommendedCache};
use rustc_hash::FxHashSet;

use crate::config::IconSizes;
use crate::tab::{self, Item, ItemMetadata, Location, Tab};

/// A file watcher, and the folders it watches.
pub struct Watcher {
    debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
    watched: FxHashSet<PathBuf>,
}

impl Watcher {
    pub fn new(debouncer: Debouncer<RecommendedWatcher, RecommendedCache>) -> Self {
        Self {
            debouncer,
            watched: FxHashSet::default(),
        }
    }

    /// Watches `paths` and nothing else: what is no longer shown is let go,
    /// and what is newly shown is watched.
    pub fn watch_only(&mut self, paths: FxHashSet<PathBuf>) {
        for path in self.watched.difference(&paths) {
            match self.debouncer.unwatch(path) {
                Ok(()) => log::debug!("unwatching {}", path.display()),
                Err(err) => log::debug!("failed to unwatch {}: {}", path.display(), err),
            }
        }
        for path in paths.difference(&self.watched) {
            match self
                .debouncer
                .watch(path, notify::RecursiveMode::NonRecursive)
            {
                Ok(()) => log::debug!("watching {}", path.display()),
                Err(err) => log::debug!("failed to watch {}: {}", path.display(), err),
            }
        }
        self.watched = paths;
    }
}

/// What changes on disk mean for a listing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Something came, went or was renamed: the folder is read again.
    pub reload: bool,
    /// Listed files whose data or metadata changed, each once: only they
    /// are read again.
    pub changed: Vec<PathBuf>,
}

/// What `events` mean for what `tab` lists, if it lists a folder.
pub fn changes(tab: &Tab, events: &[DebouncedEvent]) -> Option<Changes> {
    let folder = tab.location.path_opt()?;
    let mut changes = Changes::default();
    // A burst -- an archive being unpacked, a program rewriting one file
    // over and over -- names the same path many times, and re-reading it
    // once per mention is work with nothing to show for it.
    for event in events {
        for path in event.paths.iter().filter(|path| path.starts_with(folder)) {
            if let notify::EventKind::Modify(
                notify::event::ModifyKind::Metadata(_) | notify::event::ModifyKind::Data(_),
            ) = event.kind
            {
                if lists(tab, path) && !changes.changed.contains(path) {
                    changes.changed.push(path.clone());
                }
            } else {
                // Any other event reloads the whole folder
                changes.reload = true;
                break;
            }
        }
    }
    Some(changes)
}

/// Whether `tab` lists the file at `path`, read from disk.
fn lists(tab: &Tab, path: &Path) -> bool {
    tab.items_opt.as_ref().is_some_and(|items| {
        items.iter().any(|item| {
            item.path_opt().map(PathBuf::as_path) == Some(path)
                && matches!(item.metadata, ItemMetadata::Path { .. })
        })
    })
}

/// Reads `paths` again as items. Blocking: it goes to the disk.
pub fn reread(paths: Vec<PathBuf>, sizes: IconSizes) -> Vec<(PathBuf, Box<Item>)> {
    paths
        .into_iter()
        .filter_map(|path| match tab::item_from_path(&path, sizes) {
            Ok(item) => Some((path, Box::new(item))),
            Err(err) => {
                log::warn!("failed to reload {}: {err}", path.display());
                None
            }
        })
        .collect()
}

/// Takes into `tab` the items read again for `location`'s listing
/// `listing`, in batch `batch`: whether any was taken.
///
/// None is when the tab went elsewhere or read its folder from scratch
/// meanwhile, as they describe a listing it no longer shows. Opening or
/// closing an empty search keeps the listing.
pub fn adopt_refreshed(
    tab: &mut Tab,
    location: &Location,
    listing: u64,
    batch: u64,
    rebuilt: Vec<(PathBuf, Box<Item>)>,
) -> bool {
    if !tab.location.lists_same_as(location) || tab.listing() != listing {
        return false;
    }
    // Batches overlap: two bursts naming the same file start two workers,
    // and the older one can finish last. Whatever was read more recently
    // for a path is what the tab keeps.
    let fresh: Vec<_> = rebuilt
        .into_iter()
        .filter(|(path, _)| tab.accept_refresh(listing, batch, path))
        .collect();
    let mut adopted = false;
    if let Some(items) = &mut tab.items_opt {
        for (path, fresh) in fresh {
            // One item per path in a listing, so the first match is the only one
            if let Some(item) = items.iter_mut().find(|item| item.path_opt() == Some(&path)) {
                item.adopt(*fresh);
                adopted = true;
            }
        }
    }
    if adopted {
        // A refreshed file may sort elsewhere now
        tab.invalidate_order();
    }
    adopted
}

/// Where `location_request` leads, asked on a worker: a mount that has
/// stopped answering would stop the interface with it. The answer for the
/// tab that asked, as its step `request`; `None` when the worker failed.
pub async fn resolve_location(
    request: u64,
    location_request: tab::LocationRequest,
) -> Option<tab::Message> {
    match tokio::task::spawn_blocking(move || tab::resolve_location(&location_request)).await {
        Ok(resolved) => Some(tab::Message::LocationResolved(request, resolved)),
        Err(err) => {
            log::warn!("failed to resolve a location: {err}");
            None
        }
    }
}

/// What the network location `uri` is, as the mounter finds it, round the
/// network and back. The answer for the tab that asked, as its step
/// `request`.
pub async fn resolve_network(request: u64, uri: String) -> tab::Message {
    let resolved = crate::mounter::dir_info(&uri)
        .await
        .map(|(uri, display_name, path_opt)| Location::Network(uri, display_name, path_opt));
    tab::Message::NetworkResolved(request, uri, resolved)
}

#[cfg(test)]
mod tests {
    use super::{Changes, adopt_refreshed, changes, reread};
    use crate::config::{IconSizes, TabConfig, ThumbCfg};
    use crate::tab::{Location, Tab};
    use notify_debouncer_full::DebouncedEvent;
    use notify_debouncer_full::notify::Event;
    use notify_debouncer_full::notify::event::{CreateKind, DataChange, EventKind, ModifyKind};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    /// A tab listing `dir`, as read from disk.
    fn listing(dir: &Path) -> Tab {
        let mut tab = Tab::new(
            Location::Path(dir.to_path_buf()),
            TabConfig::default(),
            ThumbCfg::default(),
            None,
            std::borrow::Cow::Borrowed("Undefined"),
            None,
        );
        let items = crate::tab::scan_path(&dir.to_path_buf(), IconSizes::default());
        tab.set_items(items);
        tab
    }

    fn event(kind: EventKind, path: &Path) -> DebouncedEvent {
        DebouncedEvent::new(
            Event::new(kind).add_path(path.to_path_buf()),
            Instant::now(),
        )
    }

    #[test]
    fn a_listed_file_changed_is_read_again_once_and_anything_else_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, "a").unwrap();
        let tab = listing(dir.path());
        let written = EventKind::Modify(ModifyKind::Data(DataChange::Content));

        let burst = [event(written, &a), event(written, &a)];
        assert_eq!(
            changes(&tab, &burst),
            Some(Changes {
                reload: false,
                changed: vec![a.clone()],
            })
        );
        // Not listed: nothing to read again
        let unlisted = [event(written, &dir.path().join("b"))];
        assert_eq!(changes(&tab, &unlisted), Some(Changes::default()));
        // Elsewhere: nothing at all
        let elsewhere = [event(
            EventKind::Create(CreateKind::File),
            Path::new("/x/y"),
        )];
        assert_eq!(changes(&tab, &elsewhere), Some(Changes::default()));

        let created = [event(
            EventKind::Create(CreateKind::File),
            &dir.path().join("c"),
        )];
        assert!(changes(&tab, &created).unwrap().reload);
    }

    #[test]
    fn items_read_again_are_taken_only_into_the_listing_they_were_read_for() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, "a").unwrap();
        let mut tab = listing(dir.path());
        let location = tab.location.clone();
        std::fs::write(&a, "longer").unwrap();
        let read = |paths: Vec<PathBuf>| reread(paths, IconSizes::default());

        let (listing_now, batch) = (tab.listing(), tab.next_refresh_batch());
        assert!(!adopt_refreshed(
            &mut tab,
            &location,
            listing_now.wrapping_add(1),
            batch,
            read(vec![a.clone()]),
        ));
        assert!(!adopt_refreshed(
            &mut tab,
            &Location::Path(PathBuf::from("/elsewhere")),
            listing_now,
            batch,
            read(vec![a.clone()]),
        ));
        assert!(adopt_refreshed(
            &mut tab,
            &location,
            listing_now,
            batch,
            read(vec![a])
        ));
    }
}
