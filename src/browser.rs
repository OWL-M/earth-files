//! What browsing folders takes, the same in the main window and the file
//! chooser: watching the folders shown, telling what a change on disk means
//! for a listing, and reading changed items again. Each window decides what
//! to do with the answers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use notify_debouncer_full::notify::{self, RecommendedWatcher};
use notify_debouncer_full::{DebouncedEvent, Debouncer, RecommendedCache};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::IconSizes;
use crate::tab::{self, Item, ItemMetadata, Location, SearchLocation, Tab};

/// What a [`Watcher`] registers folders with: the debouncer in the app, a
/// stand-in in tests.
pub trait WatchBackend {
    fn watch(&mut self, path: &Path) -> notify::Result<()>;
    fn unwatch(&mut self, path: &Path) -> notify::Result<()>;
}

impl WatchBackend for Debouncer<RecommendedWatcher, RecommendedCache> {
    fn watch(&mut self, path: &Path) -> notify::Result<()> {
        Debouncer::watch(self, path, notify::RecursiveMode::NonRecursive)
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        Debouncer::unwatch(self, path)
    }
}

/// The first wait before a failed registration is tried again. Each failure
/// after it doubles the wait, up to [`RETRY_MAX`].
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// A registration still to be made, or undone, and when to try next.
#[derive(Clone, Copy, Debug)]
struct Retry {
    failures: u32,
    due: Instant,
}

impl Retry {
    fn after_failure(failures: u32, now: Instant) -> Self {
        let wait = RETRY_FIRST
            .saturating_mul(1 << failures.saturating_sub(1).min(16))
            .min(RETRY_MAX);
        Self {
            failures,
            due: now + wait,
        }
    }
}

/// A file watcher, and the folders it watches.
///
/// What is shown and what is registered are kept apart: a folder whose
/// registration failed is still wanted, and is tried again with a growing
/// wait until it succeeds or is no longer shown. Until then the windows say
/// that it does not update by itself.
pub struct Watcher<B = Debouncer<RecommendedWatcher, RecommendedCache>> {
    backend: B,
    /// The folders registered with the backend
    registered: FxHashSet<PathBuf>,
    /// Wanted folders whose registration failed
    failed: FxHashMap<PathBuf, Retry>,
    /// Registered folders no longer wanted, whose unwatch failed
    stale: FxHashMap<PathBuf, Retry>,
    /// The retry timer running, by its token and when it fires
    timer: Option<(u64, Instant)>,
    next_token: u64,
}

impl<B: WatchBackend> Watcher<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            registered: FxHashSet::default(),
            failed: FxHashMap::default(),
            stale: FxHashMap::default(),
            timer: None,
            next_token: 0,
        }
    }

    /// Watches `paths` and nothing else: what is no longer shown is let go,
    /// and what is newly shown is watched.
    pub fn watch_only(&mut self, paths: FxHashSet<PathBuf>, now: Instant) {
        // A folder no longer shown is not tried again
        self.failed.retain(|path, _| paths.contains(path));
        // Shown again while its unwatch was failing: it is still registered
        self.stale.retain(|path, _| !paths.contains(path));
        let gone: Vec<PathBuf> = self
            .registered
            .iter()
            .filter(|path| !paths.contains(*path) && !self.stale.contains_key(*path))
            .cloned()
            .collect();
        for path in gone {
            self.unwatch(path, 0, now);
        }
        let new: Vec<PathBuf> = paths
            .iter()
            .filter(|path| !self.registered.contains(*path) && !self.failed.contains_key(*path))
            .cloned()
            .collect();
        for path in new {
            self.watch(path, 0, now);
        }
    }

    /// Tries again every registration, and every unwatch, that is due.
    pub fn retry(&mut self, now: Instant) {
        let due = |retries: &FxHashMap<PathBuf, Retry>| -> Vec<(PathBuf, u32)> {
            retries
                .iter()
                .filter(|(_, retry)| retry.due <= now)
                .map(|(path, retry)| (path.clone(), retry.failures))
                .collect()
        };
        for (path, failures) in due(&self.failed) {
            self.failed.remove(&path);
            self.watch(path, failures, now);
        }
        for (path, failures) in due(&self.stale) {
            self.stale.remove(&path);
            self.unwatch(path, failures, now);
        }
    }

    /// Makes a failed `path` due now: the user asked to reload it, so it is
    /// worth one more try without waiting out the backoff.
    pub fn retry_now(&mut self, path: &Path, now: Instant) {
        if let Some(retry) = self.failed.get_mut(path) {
            retry.due = now;
        }
    }

    /// Whether `path` is shown but not watched.
    pub fn is_failing(&self, path: &Path) -> bool {
        self.failed.contains_key(path)
    }

    /// The retry timer to start, as its token and when it should fire, if
    /// one is needed and none running fires early enough.
    pub fn schedule(&mut self) -> Option<(u64, Instant)> {
        let due = self
            .failed
            .values()
            .chain(self.stale.values())
            .map(|retry| retry.due)
            .min()?;
        if self.timer.is_some_and(|(_, at)| at <= due) {
            return None;
        }
        self.next_token += 1;
        self.timer = Some((self.next_token, due));
        self.timer
    }

    /// The timer `token` fired: retries what is due, if it is the timer
    /// running. A timer replaced by an earlier one does nothing, so only
    /// one chain of timers is ever alive.
    pub fn fire(&mut self, token: u64, now: Instant) -> bool {
        if self.timer.is_none_or(|(running, _)| running != token) {
            return false;
        }
        self.timer = None;
        self.retry(now);
        true
    }

    fn watch(&mut self, path: PathBuf, failures: u32, now: Instant) {
        match self.backend.watch(&path) {
            Ok(()) => {
                if failures > 0 {
                    log::info!("watching {} after {failures} failures", path.display());
                } else {
                    log::debug!("watching {}", path.display());
                }
                self.registered.insert(path);
            }
            Err(err) => {
                // Said once; the retries after it only at debug
                if failures == 0 {
                    log::warn!("failed to watch {}: {err}", path.display());
                } else {
                    log::debug!("failed to watch {} again: {err}", path.display());
                }
                self.failed
                    .insert(path, Retry::after_failure(failures + 1, now));
            }
        }
    }

    fn unwatch(&mut self, path: PathBuf, failures: u32, now: Instant) {
        match self.backend.unwatch(&path) {
            Ok(()) => {
                log::debug!("unwatching {}", path.display());
                self.registered.remove(&path);
            }
            // Already gone, as when the folder was removed
            Err(err) if matches!(err.kind, notify::ErrorKind::WatchNotFound) => {
                log::debug!("{} was no longer watched", path.display());
                self.registered.remove(&path);
            }
            // Still registered: kept on record, and let go of later
            Err(err) => {
                if failures == 0 {
                    log::warn!("failed to unwatch {}: {err}", path.display());
                } else {
                    log::debug!("failed to unwatch {} again: {err}", path.display());
                }
                self.stale
                    .insert(path, Retry::after_failure(failures + 1, now));
            }
        }
    }
}

/// Waits until `watcher`'s next retry is due and answers the timer's token
/// for [`Watcher::fire`], if a new timer is needed. Sleeps on the runtime,
/// so nothing waits on the UI thread.
pub fn retry_timer<B: WatchBackend>(
    watcher: &mut Watcher<B>,
) -> Option<impl Future<Output = u64> + Send + 'static> {
    let (token, due) = watcher.schedule()?;
    Some(async move {
        tokio::time::sleep_until(tokio::time::Instant::from_std(due)).await;
        token
    })
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
    //
    // Both lookups are by set once a batch has more than a few: asked of
    // the item list and of the changes so far, a burst of thousands of
    // writes in a folder of tens of thousands of files took seconds on the
    // event loop. The set of what the tab lists costs a pass over every
    // item, so a batch of a few changes, the usual one, looks them up
    // directly instead.
    let mut listed: Option<FxHashSet<&Path>> = None;
    let mut lookups = 0;
    let mut taken: FxHashSet<&Path> = FxHashSet::default();
    for event in events {
        for path in event.paths.iter().filter(|path| path.starts_with(folder)) {
            if let notify::EventKind::Modify(
                notify::event::ModifyKind::Metadata(_) | notify::event::ModifyKind::Data(_),
            ) = event.kind
            {
                lookups += 1;
                if listed.is_none() && lookups > DIRECT_LOOKUPS {
                    listed = Some(listed_paths(tab));
                }
                let is_listed = match &listed {
                    Some(listed) => listed.contains(path.as_path()),
                    None => lists(tab, path),
                };
                if is_listed && taken.insert(path) {
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

/// How many changed files a batch looks up in the item list directly
/// before gathering what it lists into a set.
const DIRECT_LOOKUPS: usize = 8;

/// Whether `tab` lists the file at `path`, read from disk.
fn lists(tab: &Tab, path: &Path) -> bool {
    tab.items_opt.as_ref().is_some_and(|items| {
        items.iter().any(|item| {
            item.path_opt().map(PathBuf::as_path) == Some(path)
                && matches!(item.metadata, ItemMetadata::Path { .. })
        })
    })
}

/// The files `tab` lists as read from disk, by path.
fn listed_paths(tab: &Tab) -> FxHashSet<&Path> {
    tab.items_opt
        .iter()
        .flatten()
        .filter(|item| matches!(item.metadata, ItemMetadata::Path { .. }))
        .filter_map(|item| item.path_opt().map(PathBuf::as_path))
        .collect()
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

/// Whether `tab` takes the read `scan` of `location`, asked for as
/// `requested`: only the latest read started is, as an earlier one landing
/// after it, or before it, describes the folder as it was. Opening or
/// closing an empty search meanwhile lists the same folder, and reads
/// nothing itself: the read is still wanted.
pub fn wants_scan(tab: &Tab, scan: u64, requested: &Location, location: &Location) -> bool {
    tab.is_latest_scan(scan)
        && (tab.location.lists_same_as(location) || tab.location.lists_same_as(requested))
}

/// Takes into `tab` a read it wants: the folder above and the items. The
/// icons they need, warmed on a worker: they are drawn with placeholders
/// until then.
pub fn take_scan(
    tab: &mut Tab,
    parent_item_opt: Option<Box<Item>>,
    items: Vec<Item>,
) -> tab::IconWarmup {
    tab.parent_item_opt = parent_item_opt;
    tab.set_items(items);
    tab.refresh_icons(tab.config.icon_sizes)
}

/// What `tab` is searching for, while it searches.
pub fn search_term(tab: &Tab) -> Option<&str> {
    match &tab.location {
        Location::Search(_, term, ..) => Some(term),
        _ => None,
    }
}

/// Where searching `tab` for `term` goes, or with `None`, where ending its
/// search goes back to; and whether the search field is to have the
/// keyboard, as it does while searching. `None` where there is nothing to
/// search, or no search to end.
pub fn search_target(tab: &Tab, term: Option<String>) -> Option<(Location, bool)> {
    match term {
        Some(term) => {
            let searched = if let Some(path) = tab.location.path_opt() {
                SearchLocation::Path(path.clone())
            } else if tab.location.is_recents() {
                SearchLocation::Recents
            } else if tab.location.is_trash() {
                SearchLocation::Trash
            } else {
                return None;
            };
            let options = tab.search_options_for_query();
            Some((
                Location::Search(searched, term, options, std::time::Instant::now()),
                true,
            ))
        }
        None => match &tab.location {
            Location::Search(searched, ..) => Some((
                match searched {
                    SearchLocation::Path(path) => Location::Path(path.clone()),
                    SearchLocation::Recents => Location::Recents,
                    SearchLocation::Trash => Location::Trash,
                },
                false,
            )),
            _ => None,
        },
    }
}

/// Warms the icons `warmup` names, on a worker: the answer for the tab once
/// they are ready, `None` when the worker failed.
pub async fn warm_icons(warmup: tab::IconWarmup, sizes: IconSizes) -> Option<tab::Message> {
    tokio::task::spawn_blocking(move || tab::warm_icons(&warmup, sizes))
        .await
        .ok()
        .map(|()| tab::Message::IconsReady)
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
    use super::{
        Changes, adopt_refreshed, changes, reread, search_target, search_term, wants_scan,
    };
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

    /// A batch looked up in the items directly, one that turns to the set at
    /// its last lookup, and one mostly looked up in the set give the same
    /// answer: each listed file once, in the order first named, and nothing
    /// that is not listed
    #[test]
    fn a_large_batch_is_taken_as_a_small_one_is() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<PathBuf> = (0..40)
            .map(|i| dir.path().join(format!("f{i:02}")))
            .collect();
        for file in &files {
            std::fs::write(file, "").unwrap();
        }
        let unlisted = dir.path().join("unlisted");
        let tab = listing(dir.path());
        let written = EventKind::Modify(ModifyKind::Data(DataChange::Content));
        for lookups in [
            super::DIRECT_LOOKUPS,
            super::DIRECT_LOOKUPS + 1,
            super::DIRECT_LOOKUPS * 4,
        ] {
            // In threes: a file, an unlisted path, the same file again; one
            // event, and one lookup, each. The last is a file not named
            // before, so it is taken only if the last lookup, made directly
            // or through the set as the batch's size has it, finds it
            let mut named: Vec<&PathBuf> = (0..lookups - 1)
                .map(|i| if i % 3 == 1 { &unlisted } else { &files[i / 3] })
                .collect();
            named.push(files.last().unwrap());
            let burst: Vec<_> = named.iter().map(|path| event(written, path)).collect();
            let mut expected: Vec<PathBuf> = Vec::new();
            for path in named.into_iter().filter(|path| **path != unlisted) {
                if !expected.contains(path) {
                    expected.push(path.clone());
                }
            }
            assert_eq!(
                changes(&tab, &burst),
                Some(Changes {
                    reload: false,
                    changed: expected,
                }),
                "{lookups} lookups"
            );
        }
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

    /// Only the latest read started is taken, and only for what the tab
    /// still lists: the folder it shows, or the one it was asked to go to
    #[test]
    fn only_the_latest_read_of_what_is_listed_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = listing(dir.path());
        let here = tab.location.clone();
        let elsewhere = Location::Path(PathBuf::from("/elsewhere"));
        let earlier = tab.start_scan();
        let latest = tab.start_scan();

        assert!(wants_scan(&tab, latest, &here, &here));
        assert!(!wants_scan(&tab, earlier, &here, &here), "superseded");
        assert!(
            !wants_scan(&tab, latest, &elsewhere, &elsewhere),
            "gone elsewhere"
        );
        // Read under its canonical name, asked for by the one the tab has
        assert!(wants_scan(&tab, latest, &here, &elsewhere));
    }

    /// A search of a folder goes back to the folder; there is nothing to
    /// search where a tab lists no folder, recents or trash
    #[test]
    fn a_search_goes_back_to_what_it_searched() {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = listing(dir.path());
        assert_eq!(search_term(&tab), None);
        assert!(search_target(&tab, None).is_none(), "no search to end");

        let (searching, focus) = search_target(&tab, Some("needle".into())).unwrap();
        assert!(focus);
        tab.change_location(&searching, None);
        assert_eq!(search_term(&tab), Some("needle"));

        let (back, focus) = search_target(&tab, None).unwrap();
        assert!(!focus);
        assert_eq!(back, Location::Path(dir.path().to_path_buf()));

        tab.change_location(
            &Location::Network("network:///".into(), "Network".into(), None),
            None,
        );
        assert!(
            search_target(&tab, Some("x".into())).is_none(),
            "nothing to search"
        );
    }

    mod watcher {
        use super::super::{RETRY_FIRST, RETRY_MAX, WatchBackend, Watcher};
        use notify_debouncer_full::notify;
        use rustc_hash::FxHashSet;
        use std::path::{Path, PathBuf};
        use std::time::{Duration, Instant};

        /// A backend that fails the paths it is told to, and records every
        /// call made to it.
        #[derive(Default)]
        struct Fake {
            failing_watch: FxHashSet<PathBuf>,
            failing_unwatch: FxHashSet<PathBuf>,
            /// Unwatches answered "not found", as for a removed folder
            gone: FxHashSet<PathBuf>,
            watches: Vec<PathBuf>,
            unwatches: Vec<PathBuf>,
        }

        impl WatchBackend for Fake {
            fn watch(&mut self, path: &Path) -> notify::Result<()> {
                self.watches.push(path.to_path_buf());
                if self.failing_watch.contains(path) {
                    Err(notify::Error::generic("no space left on device"))
                } else {
                    Ok(())
                }
            }

            fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
                self.unwatches.push(path.to_path_buf());
                if self.gone.contains(path) {
                    Err(notify::Error::watch_not_found())
                } else if self.failing_unwatch.contains(path) {
                    Err(notify::Error::generic("input/output error"))
                } else {
                    Ok(())
                }
            }
        }

        impl Fake {
            fn watches_of(&self, path: &Path) -> usize {
                self.watches.iter().filter(|p| *p == path).count()
            }
        }

        fn set(paths: &[&Path]) -> FxHashSet<PathBuf> {
            paths.iter().map(|p| p.to_path_buf()).collect()
        }

        fn failing(paths: &[&Path]) -> Watcher<Fake> {
            Watcher::new(Fake {
                failing_watch: set(paths),
                ..Fake::default()
            })
        }

        #[test]
        fn a_failed_watch_is_retried_on_a_timer_until_it_succeeds() {
            let a = Path::new("/a");
            let start = Instant::now();
            let mut watcher = failing(&[a]);
            watcher.watch_only(set(&[a]), start);
            assert!(
                watcher.is_failing(a),
                "failed registration not recorded as active"
            );
            assert!(!watcher.registered.contains(a));

            // The same paths again change nothing: no navigation retries it
            watcher.watch_only(set(&[a]), start);
            assert_eq!(watcher.backend.watches_of(a), 1);

            let (token, due) = watcher.schedule().expect("a retry is scheduled");
            assert_eq!(due, start + RETRY_FIRST);
            // Nothing new to schedule while that timer runs
            assert!(watcher.schedule().is_none());

            // Fires, still failing: tried once more and waits longer
            assert!(watcher.fire(token, due));
            assert_eq!(watcher.backend.watches_of(a), 2);
            assert!(watcher.is_failing(a));
            let (token, due2) = watcher.schedule().expect("retried again");
            assert_eq!(due2, due + 2 * RETRY_FIRST);

            // The cause went away: the next timer registers it
            watcher.backend.failing_watch.clear();
            assert!(watcher.fire(token, due2));
            assert!(!watcher.is_failing(a), "still degraded after recovery");
            assert!(watcher.registered.contains(a));
            assert!(watcher.schedule().is_none(), "nothing left to retry");
        }

        #[test]
        fn a_successful_watch_is_registered_once() {
            let (a, b) = (Path::new("/a"), Path::new("/b"));
            let now = Instant::now();
            let mut watcher = Watcher::new(Fake::default());
            watcher.watch_only(set(&[a]), now);
            watcher.watch_only(set(&[a, b]), now);
            watcher.watch_only(set(&[a, b]), now);
            watcher.retry(now + RETRY_MAX * 4);
            assert_eq!(watcher.backend.watches_of(a), 1);
            assert_eq!(watcher.backend.watches_of(b), 1);
            assert!(watcher.schedule().is_none());
        }

        #[test]
        fn a_path_no_longer_shown_is_not_retried() {
            let a = Path::new("/a");
            let now = Instant::now();
            let mut watcher = failing(&[a]);
            watcher.watch_only(set(&[a]), now);
            let (token, due) = watcher.schedule().unwrap();
            watcher.watch_only(set(&[]), now);
            assert!(!watcher.is_failing(a));
            // The timer already running finds nothing to do
            assert!(watcher.fire(token, due));
            watcher.retry(now + RETRY_MAX * 4);
            assert_eq!(watcher.backend.watches_of(a), 1);
            assert!(watcher.schedule().is_none());
        }

        #[test]
        fn navigating_away_during_recovery_never_restores_the_old_watch() {
            let (old, new) = (Path::new("/old"), Path::new("/new"));
            let now = Instant::now();
            let mut watcher = failing(&[old]);
            watcher.watch_only(set(&[old]), now);
            let (token, due) = watcher.schedule().unwrap();
            // Left before the retry, and the cause is gone by the time it fires
            watcher.watch_only(set(&[new]), now);
            watcher.backend.failing_watch.clear();
            watcher.fire(token, due);
            watcher.retry(due + RETRY_MAX);
            assert_eq!(watcher.backend.watches_of(old), 1);
            assert!(!watcher.registered.contains(old));
            assert!(watcher.registered.contains(new));
        }

        #[test]
        fn a_failed_unwatch_stays_on_record_and_is_let_go_later() {
            let a = Path::new("/a");
            let now = Instant::now();
            let mut watcher = Watcher::new(Fake {
                failing_unwatch: set(&[a]),
                ..Fake::default()
            });
            watcher.watch_only(set(&[a]), now);
            watcher.watch_only(set(&[]), now);
            // Still registered, so not forgotten, and not shown as failing
            assert!(watcher.registered.contains(a));
            assert!(!watcher.is_failing(a));
            let (token, due) = watcher.schedule().expect("the unwatch is retried");

            // Shown again before then: it is still registered, so it is not
            // registered twice, and no longer let go
            watcher.watch_only(set(&[a]), now);
            assert_eq!(watcher.backend.watches_of(a), 1);
            watcher.fire(token, due);
            assert_eq!(watcher.backend.unwatches.len(), 1);
            assert!(watcher.registered.contains(a));

            // Hidden again, and this time the unwatch goes through on retry
            watcher.watch_only(set(&[]), now);
            watcher.backend.failing_unwatch.clear();
            let (token, due) = watcher.schedule().unwrap();
            watcher.fire(token, due);
            assert!(watcher.registered.is_empty());
            assert!(watcher.schedule().is_none());
        }

        #[test]
        fn an_unwatch_of_a_vanished_watch_is_dropped() {
            let a = Path::new("/a");
            let now = Instant::now();
            let mut watcher = Watcher::new(Fake {
                gone: set(&[a]),
                ..Fake::default()
            });
            watcher.watch_only(set(&[a]), now);
            watcher.watch_only(set(&[]), now);
            assert!(watcher.registered.is_empty());
            assert!(watcher.schedule().is_none(), "nothing to retry");
        }

        #[test]
        fn retries_back_off_up_to_a_cap() {
            let a = Path::new("/a");
            let mut now = Instant::now();
            let mut watcher = failing(&[a]);
            watcher.watch_only(set(&[a]), now);
            let mut waits = Vec::new();
            for _ in 0..10 {
                let (token, due) = watcher.schedule().unwrap();
                waits.push(due - now);
                now = due;
                watcher.fire(token, now);
            }
            assert_eq!(waits[..3], [RETRY_FIRST, RETRY_FIRST * 2, RETRY_FIRST * 4]);
            assert!(waits.windows(2).all(|w| w[0] <= w[1]));
            assert_eq!(*waits.last().unwrap(), RETRY_MAX);
            assert_eq!(watcher.backend.watches_of(a), 11);
        }

        #[test]
        fn a_replaced_timer_does_nothing_when_it_fires() {
            let (a, b) = (Path::new("/a"), Path::new("/b"));
            let now = Instant::now();
            let mut watcher = failing(&[a, b]);
            watcher.watch_only(set(&[a]), now);
            // Fail a several times so its next try is far off
            for _ in 0..4 {
                let (token, due) = watcher.schedule().unwrap();
                watcher.fire(token, due);
            }
            let (late, late_due) = watcher.schedule().unwrap();
            // b fails now, due sooner: a new timer replaces the late one
            watcher.watch_only(set(&[a, b]), now + Duration::from_secs(1));
            let (early, early_due) = watcher.schedule().expect("an earlier timer");
            assert!(early_due < late_due);
            assert!(watcher.fire(early, early_due));
            let before = watcher.backend.watches.len();
            assert!(!watcher.fire(late, late_due), "the replaced timer ran");
            assert_eq!(watcher.backend.watches.len(), before);
        }

        #[test]
        fn reload_makes_a_failed_path_due_now() {
            let a = Path::new("/a");
            let now = Instant::now();
            let mut watcher = failing(&[a]);
            watcher.watch_only(set(&[a]), now);
            watcher.backend.failing_watch.clear();
            watcher.retry_now(a, now);
            let (token, due) = watcher.schedule().unwrap();
            assert_eq!(due, now);
            watcher.fire(token, due);
            assert!(!watcher.is_failing(a));
        }
    }
}
