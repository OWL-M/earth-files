//! The content check behind a name-first listing.
//!
//! A folder scanned with `Typing::NameFirst` types its files by name, so it
//! can be shown before any of them is opened. This reads each file the way
//! the scan used to -- the head of the file for its type, and an image's
//! header for its size -- one file at a time, and reports in batches, so the
//! listing takes on the real types as they arrive.

use std::cell::{Cell, RefCell};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant};

use mime_guess::{Mime, mime};
use tokio::sync::mpsc;

use crate::config::IconSizes;
use crate::ui::widget;

/// A file a name-first scan typed by name.
#[derive(Clone, Debug)]
pub struct Unchecked {
    pub path: PathBuf,
    pub name: String,
    /// The type its name gave
    pub mime: Mime,
}

/// What reading one file found.
#[derive(Clone, Debug)]
pub struct Checked {
    pub path: PathBuf,
    /// Pixel size, for an image
    pub dims: Option<(u32, u32)>,
    /// Set when the content gives a different type from the name
    pub retyped: Option<Retyped>,
}

/// What follows from a file's type, rebuilt here for a new one because
/// building it may read the file: a desktop entry's own icon and name.
#[derive(Clone, Debug)]
pub struct Retyped {
    pub mime: Mime,
    /// A new display name, when the file turned out to be a desktop entry
    /// or stopped being one
    pub display_name: Option<String>,
    pub icon_handle_grid: widget::icon::Handle,
    pub icon_handle_list: widget::icon::Handle,
    pub icon_handle_list_condensed: widget::icon::Handle,
}

/// Results sent at once, at most. On a warm cache a folder's files are read
/// faster than a frame is drawn, and one message per file would flood the
/// update loop.
const BATCH_MAX: usize = 64;

/// How long a result may wait for others before it is sent anyway. On a cold
/// spinning drive a file takes about 20 ms, so a batch then holds a handful
/// and types still arrive several times a second. Checked as each file
/// finishes, so a result can wait this long plus one more file's read.
const BATCH_WAIT: Duration = Duration::from_millis(100);

/// Reads `file` as a scan by content would.
#[must_use]
pub fn check(file: &Unchecked, sizes: IconSizes) -> Checked {
    // As the scan does: a broken link has no target to describe, so it is
    // described by the link itself
    let metadata = std::fs::metadata(&file.path)
        .or_else(|_| std::fs::symlink_metadata(&file.path))
        .ok();
    let mime = crate::mime_icon::mime_for_path(&file.path, metadata.as_ref(), false);
    let dims = if mime.type_() == mime::IMAGE {
        image_size(&file.path)
    } else {
        None
    };
    let retyped = (mime != file.mime).then(|| {
        let (is_desktop, grid, list, condensed) = crate::tab::file_icons(&file.path, &mime, sizes);
        let was_desktop = file.mime == "application/x-desktop";
        Retyped {
            // Not looked up in gvfs either, for the same reason: xdg-mime
            // never turns a file into or out of a desktop entry by content
            // alone. A `.desktop` name wins over content on its own (a
            // unique file-name match returns before content is sniffed at
            // all), and content sniffed as `application/x-desktop` is
            // downgraded to `text/plain` whenever the file name is known --
            // which it always is here.
            display_name: (is_desktop != was_desktop).then(|| {
                crate::tab::display_name_for_file(&file.path, &file.name, false, is_desktop)
            }),
            mime,
            icon_handle_grid: grid,
            icon_handle_list: list,
            icon_handle_list_condensed: condensed,
        }
    });
    Checked {
        path: file.path.clone(),
        dims,
        retyped,
    }
}

/// An image's pixel size, its format read from its content.
///
/// `image::image_dimensions` goes by the extension alone, so an image without
/// a matching one -- which is exactly what this check exists to find -- would
/// never get a size. The user chose content here (2026-09-27); scans by
/// content keep reading by extension, as they always have.
fn image_size(path: &Path) -> Option<(u32, u32)> {
    image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

thread_local! {
    /// Set while [`quietly`] runs something on this thread.
    static QUIET: Cell<bool> = const { Cell::new(false) };
    /// What the panic hook saw of a panic [`quietly`] is about to catch.
    static CAUGHT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Runs `f`, catching a panic in it and returning what the panic said and
/// where, instead of letting the panic hook print it to stderr: the caller
/// logs it, as the thumbnailers' noise is logged rather than printed.
///
/// The hook is the process's, so it is wrapped once, and the wrapper only
/// keeps quiet on a thread that is inside this call; a panic anywhere else is
/// printed as it always was.
fn quietly<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    static WRAP_HOOK: Once = Once::new();
    WRAP_HOOK.call_once(|| {
        let printing = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if QUIET.get() {
                CAUGHT.set(Some(info.to_string()));
            } else {
                printing(info);
            }
        }));
    });

    QUIET.set(true);
    let result = std::panic::catch_unwind(AssertUnwindSafe(f));
    QUIET.set(false);
    result.map_err(|_| CAUGHT.take().unwrap_or_else(|| "it panicked".to_owned()))
}

/// Checks `files` in order, sending the results in batches, until all are
/// sent or nobody is listening any more. Returns how many were checked.
///
/// Blocking: every check reads the disk. Run it on a blocking worker.
#[must_use]
pub fn run(files: &[Unchecked], sizes: IconSizes, results: &mpsc::Sender<Vec<Checked>>) -> usize {
    let mut batch = Vec::new();
    let mut batch_started = Instant::now();
    let mut checked = 0;
    for file in files {
        // The listing was replaced or its tab closed: the receiver went with
        // the subscription, and nothing more needs reading
        if results.is_closed() {
            return checked;
        }
        if batch.is_empty() {
            batch_started = Instant::now();
        }
        // A panic in `check` (an image decoder on a corrupt file, say) must
        // not end the run silently: that would leave every later file
        // unchecked -- no thumbnails, no sizes, and nothing on screen says
        // why. Caught here, the file keeps its name-based type and the loop
        // carries on.
        let checked_file = quietly(|| check(file, sizes)).unwrap_or_else(|panic| {
            log::warn!(
                "failed to check the type of {}: {panic}",
                file.path.display()
            );
            Checked {
                path: file.path.clone(),
                dims: None,
                retyped: None,
            }
        });
        batch.push(checked_file);
        checked += 1;
        if (batch.len() >= BATCH_MAX || batch_started.elapsed() >= BATCH_WAIT)
            && results.blocking_send(std::mem::take(&mut batch)).is_err()
        {
            return checked;
        }
    }
    if !batch.is_empty() {
        let _ = results.blocking_send(batch);
    }
    checked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_panic_is_caught_with_its_message_and_place() {
        let caught = quietly(|| -> () { panic!("a corrupt header") })
            .expect_err("the panic should be caught");
        assert!(caught.contains("a corrupt header"), "no message: {caught}");
        assert!(caught.contains("type_check.rs"), "no location: {caught}");

        // Only the panic in hand is kept quiet
        assert_eq!(quietly(|| 7).ok(), Some(7));
        assert!(!QUIET.get(), "the thread was left quiet");
    }

    fn unchecked(path: PathBuf) -> Unchecked {
        Unchecked {
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path,
            mime: mime::APPLICATION_OCTET_STREAM,
        }
    }

    #[test]
    fn a_check_nobody_listens_to_stops_before_reading() {
        let (results, received) = mpsc::channel(4);
        drop(received);
        let files = vec![unchecked(PathBuf::from("/nonexistent/a")); 3];
        assert_eq!(run(&files, IconSizes::default(), &results), 0);
    }

    #[test]
    fn every_file_is_reported_once_in_order() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut files = Vec::new();
        for name in ["a.txt", "b.txt", "c"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"plain text")?;
            files.push(unchecked(path));
        }

        let (results, mut received) = mpsc::channel(8);
        assert_eq!(run(&files, IconSizes::default(), &results), 3);
        drop(results);

        let mut paths = Vec::new();
        while let Ok(batch) = received.try_recv() {
            paths.extend(batch.into_iter().map(|checked| checked.path));
        }
        let expected: Vec<PathBuf> = files.into_iter().map(|file| file.path).collect();
        assert_eq!(paths, expected);
        Ok(())
    }

    #[test]
    fn a_file_whose_content_agrees_with_its_name_is_not_retyped() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"plain text")?;
        let file = Unchecked {
            mime: crate::mime_icon::mime_for_path(&path, None, true),
            ..unchecked(path)
        };
        assert!(check(&file, IconSizes::default()).retyped.is_none());
        Ok(())
    }

    #[test]
    fn an_unnamed_jpeg_is_retyped_and_measured() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let named = dir.path().join("photo.jpg");
        image::RgbImage::new(4, 2)
            .save(&named)
            .expect("the test image should be written");
        let path = dir.path().join("photo");
        std::fs::rename(&named, &path)?;

        let checked = check(&unchecked(path), IconSizes::default());
        assert_eq!(checked.dims, Some((4, 2)));
        let retyped = checked.retyped.expect("the content is a JPEG");
        assert_eq!(retyped.mime, mime::IMAGE_JPEG);
        assert!(
            retyped.display_name.is_none(),
            "not a desktop entry either way"
        );
        Ok(())
    }

    #[test]
    fn a_broken_symlink_is_retyped_as_the_scan_would_type_it() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("broken");
        std::os::unix::fs::symlink("/nonexistent/target/xyz", &path)?;

        // What a content scan gives for the same broken link, so this test
        // tracks the scan instead of a hardcoded guess.
        let (_, items) =
            crate::tab::Location::Path(dir.path().to_path_buf()).scan(IconSizes::default());
        let scanned = items
            .iter()
            .find(|item| item.name == "broken")
            .expect("the scan should list the broken link");

        let checked = check(&unchecked(path), IconSizes::default());
        let retyped = checked.retyped.expect("a broken link is not octet-stream");
        assert_eq!(retyped.mime, scanned.mime);
        Ok(())
    }

    #[test]
    fn batches_are_capped_and_files_stay_in_order() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut files = Vec::new();
        for i in 0..70 {
            let path = dir.path().join(format!("file{i}.txt"));
            std::fs::write(&path, b"plain text")?;
            files.push(unchecked(path));
        }

        let (results, mut received) = mpsc::channel(70);
        assert_eq!(run(&files, IconSizes::default(), &results), 70);
        drop(results);

        let mut paths = Vec::new();
        while let Ok(batch) = received.try_recv() {
            assert!(batch.len() <= BATCH_MAX, "batch exceeded BATCH_MAX");
            paths.extend(batch.into_iter().map(|checked| checked.path));
        }
        let expected: Vec<PathBuf> = files.into_iter().map(|file| file.path).collect();
        assert_eq!(paths, expected);
        Ok(())
    }
}
