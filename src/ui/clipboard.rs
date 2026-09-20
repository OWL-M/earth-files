// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! MIME-typed clipboard reads and writes using `wl_data_device` directly.
//!
//! `iced_runtime::clipboard` supports only plain-text `read`/`write`; this
//! module adds `read_data`/`write_data` and their generic traits,
//! `AllowedMimeTypes`/`AsMimeTypes`.
//!
//! `iced_exwlshell` pins `smithay-clipboard 0.7.3` through
//! `window_clipboard 0.5.1`, which has no MIME-typed API. The fork with that API
//! uses version 0.8.0, so a `[patch.crates-io]` is semver-incompatible and
//! silently ignored. This module uses the `wl_data_device` already bound by
//! [`crate::ui::dnd`], which handles transport. The functions here provide the
//! `Task` interface; [`crate::clipboard`] defines the unchanged wire format.

use std::borrow::Cow;

/// Reads one offer given the mime types on offer: the data and the mime type read
type OfferReader = fn(&[String]) -> Option<(Vec<u8>, String)>;

pub use iced_runtime::clipboard::*;

/// Data that can be produced from a clipboard offer of one of its MIME types.
///
/// Vendored from `window_clipboard`'s `mime` crate (`mime/src/lib.rs:28`), which
/// is not a dependency of this crate.
pub trait AllowedMimeTypes: TryFrom<(Vec<u8>, String)> + Send + Sync + 'static {
    /// Allowed MIME types, in order of decreasing preference.
    fn allowed() -> Cow<'static, [String]>;
}

/// Data that can be offered to the clipboard under one of several MIME types.
///
/// Vendored from `window_clipboard`'s `mime` crate (`mime/src/lib.rs:39`).
pub trait AsMimeTypes {
    /// MIME types this data can be converted to.
    fn available(&self) -> Cow<'static, [String]>;

    /// Converts this data to bytes for the given MIME type, if possible.
    fn as_bytes(&self, mime_type: &str) -> Option<Cow<'static, [u8]>>;
}

/// Read the clipboard as the most-preferred of `T`'s MIME types the current
/// offer advertises.
///
/// Returns `None` if the clipboard is empty, the offer has no MIME type in
/// `T::allowed()`, the source never answers, or `T` rejects the bytes. These
/// failures cancel the paste without hanging or panicking.
///
/// The pipe read blocks on a separate thread and returns through a oneshot.
/// This keeps iced responsive and leaves the data device thread free to
/// dispatch `send` when this app is also the clipboard source.
pub fn read_data<T: AllowedMimeTypes>() -> iced_runtime::Task<Option<T>> {
    read_with("earth-files-clipboard-read", crate::ui::dnd::read_selection)
}

/// Read what was just dropped on us as the most-preferred of `T`'s MIME types
/// the drag offer advertises.
///
/// The same shape as [`read_data`], against the drag offer rather than the
/// selection, and for the same reason it is off iced's thread: this app is
/// frequently both ends of the drag, and the data device thread has to stay
/// free to serve the very transfer this read is waiting on.
pub fn read_drop_data<T: AllowedMimeTypes>() -> iced_runtime::Task<Option<T>> {
    read_with("earth-files-drop-read", crate::ui::dnd::read_drop)
}

/// The body of [`read_data`] and [`read_drop_data`]: drain one offer on a
/// thread named `thread`, then convert.
fn read_with<T: AllowedMimeTypes>(
    thread: &'static str,
    read: OfferReader,
) -> iced_runtime::Task<Option<T>> {
    let allowed = T::allowed().into_owned();

    iced_runtime::Task::future(async move {
        let (sender, receiver) = iced::futures::channel::oneshot::channel();

        if let Err(err) = std::thread::Builder::new()
            .name(String::from(thread))
            .spawn(move || {
                let _ = sender.send(read(&allowed));
            })
        {
            log::warn!("could not spawn the {thread} thread: {err}");
            return None;
        }

        let (data, mime) = receiver.await.ok().flatten()?;
        match T::try_from((data, mime.clone())) {
            Ok(contents) => Some(contents),
            Err(_) => {
                log::debug!("contents in {mime} could not be converted");
                None
            }
        }
    })
}

/// Take ownership of the clipboard, offering everything `contents` advertises.
///
/// Yields no message: the request is one round of `wl_data_source` traffic sent
/// straight from here, and the contents are handed to the data device thread,
/// which serves them for as long as this app owns the selection.
pub fn write_data<Message: Send + 'static>(
    contents: impl AsMimeTypes + Sync + Send + 'static,
) -> iced_runtime::Task<Message> {
    if !crate::ui::dnd::set_selection(std::sync::Arc::new(contents)) {
        log::warn!("the clipboard could not be taken; the copy will not be visible to paste");
    }
    iced_runtime::Task::none()
}
