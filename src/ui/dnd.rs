// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Wayland drag-and-drop using `wl_data_device_manager` directly.
//!
//! iced has no drag-and-drop API of its own, and the published
//! `smithay-clipboard` 0.7.3 has no DnD support. The fork with DnD uses version
//! 0.8.0, so a `[patch.crates-io]` against `clipboard_wayland`'s
//! `smithay-clipboard = "0.7"` is semver-incompatible and silently ignored.
//!
//! One `wl_data_device` handles:
//!
//! * Tab drag-to-reorder, with one MIME type and an unread placeholder payload.
//!   Local widget state determines the reorder without a data transfer.
//! * File drag-and-drop. [`start_drag_data`] offers paths in
//!   [`crate::clipboard`]'s MIME types to other applications. [`read_drop`]
//!   reads drops from any source into the `ClipboardPaste` used by paste.
//!   Drops within this app also use the protocol and need the threading rules
//!   below.
//! * MIME-typed clipboard transport. `set_selection`/`read_selection` support
//!   [`crate::ui::clipboard`]'s `write_data`/`read_data`. The wire format in
//!   [`crate::clipboard`] is unchanged.
//!
//! ## Connection and event queues
//!
//! [`crate::ui::shell::runner::run`] creates a [`Connection`] and passes it to
//! `iced_exwlshell` through `settings::Settings::with_connection`. This module
//! opens a second event queue on that connection. `wayland-client` binds each
//! object to the queue where it was created, so the queues dispatch their own
//! objects independently.
//!
//! ## Pointer serials
//!
//! `wl_data_device::start_drag` needs an implicit pointer grab's serial.
//! exwlshell stores it in `WindowState::button_serial`, but its only accessor,
//! `take_popup_grab_serial`, consumes it with
//! `button_serial.take().or(enter_serial)`. After the first caller, it returns
//! the enter serial, which `start_drag` rejects.
//!
//! This module binds its own `wl_pointer` on its queue without changing
//! exwlshell. `wl_seat::get_pointer` allows multiple pointer resources, and
//! the compositor sends each the same events and serials. Serials belong to
//! the seat, so this pointer receives the same press serial as exwlshell
//! without consuming its copy.
//!
//! The press reaches iced's thread and this module's thread independently.
//! If our thread has not reported the button down, [`start_drag`] waits up to
//! [`PRESS_WAIT`] for it to catch up. This avoids reusing an earlier press's
//! serial. wlroots validates grab serials and refuses stale ones; Hyprland
//! ignores the serial.
//!
//! ## Threading
//!
//! A dedicated thread owns and dispatches the queue, like the portal reads in
//! [`crate::ui::icon_theme`] and `ui::theme::detect_system_prefers_dark`. Each
//! thread drives its own loop without accessing the toolkit's loop.
//!
//! Requests need no thread wakeup: `wayland-client` proxies are `Send + Sync`
//! and `QueueHandle` is `Clone + Send + Sync`. [`start_drag`] creates the
//! `wl_data_source`, sends `start_drag` and flushes on iced's thread. The
//! dedicated thread handles events.
//!
//! The threads share [`SHARED`], holding its `Mutex` only for field updates.
//! The widget polls it in `update` and calls `Shell::request_redraw` while a
//! drag is live. During a Wayland drag the data device owns the pointer grab,
//! so iced receives no pointer events and must poll on redraw.

use std::borrow::Cow;
use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::ui::clipboard::AsMimeTypes;

use wayland_client::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{DndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_registry::{self, WlRegistry},
    wl_seat::{self, WlSeat},
    wl_surface::WlSurface,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child,
};

/// What the widget needs to know about a live drag.
///
/// `position` is surface-local, which is the same space iced lays widgets out
/// in for the base window at scale factor 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Drag {
    /// Where the pointer is, as `wl_data_device` reports it, or `None` while
    /// the drag is outside our surfaces.
    pub position: Option<(f32, f32)>,
    /// The compositor sent `wl_data_device::drop`.
    pub dropped: bool,
    /// The drag has ended: it was dropped, cancelled or left for good.
    pub ended: bool,
    /// Bumped on every change, so a poller can tell "nothing happened" from
    /// "moved back to where it was".
    pub generation: u64,
    /// The payload on offer is a file list: one of [`FILE_MIMES`] is among
    /// the MIME types advertised, and the drop can be turned into paths.
    ///
    /// False for the tab drag, whose single MIME type carries a placeholder.
    /// It is what lets one poller ignore the other's drags.
    pub files: bool,
}

/// The MIME types a drag has to advertise for its payload to be file paths.
///
/// Both are [`crate::clipboard`]'s, in order of decreasing preference: they are
/// exactly what the clipboard copy/paste path already speaks, so a drop and a
/// paste perform the same operation on the same bytes.
pub const FILE_MIMES: [&str; 2] = ["x-special/gnome-copied-files", "text/uri-list"];

fn is_file_mime(mime: &str) -> bool {
    FILE_MIMES.contains(&mime)
}

#[derive(Default)]
struct Shared {
    conn: Option<Connection>,
    qh: Option<QueueHandle<State>>,
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    manager_version: u32,
    /// Serial of the most recent pointer button press, from our own pointer.
    button_serial: Option<u32>,
    /// The application scale factor, as [`set_scale_factor`] was last told.
    scale_factor: f64,
    /// Whether our own pointer currently reports a button held down.
    ///
    /// This is how [`start_drag`] tells "the press that asked for this drag has
    /// been dispatched here" from "it is still sitting in our queue": iced's
    /// thread only ever asks for a drag while the button is down, so a `false`
    /// here means our thread has not caught up yet and the serial on hand, if
    /// any, belongs to some earlier press.
    button_pressed: bool,
    /// The surface that press landed on, `start_drag`'s origin.
    pointer_surface: Option<WlSurface>,
    source: Option<WlDataSource>,
    /// The MIME types the live drag we started offers, so the destination
    /// half knows what to accept when the drag comes back over our own
    /// surface. Empty whenever the drag in flight is somebody else's.
    offered: Vec<String>,
    /// A source whose drag is over but which the compositor has not finished
    /// with yet; see [`end_drag`].
    retired_source: Option<WlDataSource>,
    /// The offer behind the drag currently over one of our surfaces, with the
    /// MIME types it advertised.
    ///
    /// Kept alive past `drop`, unlike the tab drag's, because reading a
    /// dropped payload means calling `receive` on this offer and that can only
    /// happen after the drop. [`read_drop`] retires it; so does the next
    /// `enter`, for a drag nobody read.
    drag_offer: Option<(WlDataOffer, OfferMimes)>,
    /// The offer of a drag that has been dropped and not yet read.
    ///
    /// [`end_drag`] leaves this slot intact. Every watching widget sees
    /// `ended` in the same event batch and calls `end_drag`, before the queued
    /// `Command::DropFiles` starts the read. Destroying the offer in
    /// `end_drag` would silently discard the drop.
    dropped_offer: Option<(WlDataOffer, OfferMimes)>,
    drag: Option<Drag>,
    /// Most recent serial of any input event that can justify a request,
    /// a pointer press or a key press. `set_selection` needs one; unlike
    /// `start_drag` it does not need it to be a button in particular.
    last_serial: Option<u32>,
    /// The `wl_data_source` this app currently owns the selection with, if it
    /// still owns it.
    selection_source: Option<WlDataSource>,
    /// The offer the compositor last named as the clipboard, and the MIME
    /// types it advertised.
    selection_offer: Option<(WlDataOffer, OfferMimes)>,
    /// The offer before that one, destroyed only when a third arrives.
    ///
    /// Readers call `receive` under the same lock used to swap this offer,
    /// then drain the pipe without touching the proxy. Some compositors tie
    /// the transfer's lifetime to the offer, so it stays alive for one more
    /// selection change to protect a read that has just started.
    retired_offer: Option<WlDataOffer>,
}

impl Shared {
    /// Convert a `wl_data_device` surface-local coordinate into the space iced
    /// lays widgets out in. See [`set_scale_factor`].
    fn to_logical(&self, x: f64, y: f64) -> (f32, f32) {
        let scale = if self.scale_factor > 0.0 {
            self.scale_factor
        } else {
            1.0
        };
        ((x / scale) as f32, (y / scale) as f32)
    }

    /// Whether a drag is in flight, as opposed to none at all or one that has
    /// ended and is only being kept readable; see [`end_drag`].
    fn drag_is_live(&self) -> bool {
        self.drag.is_some_and(|drag| !drag.ended)
    }

    /// Start tracking a new drag carrying `files` or not.
    ///
    /// The generation carries on from whatever drag this replaces rather than
    /// restarting, so that no two drags can ever present a watcher with the
    /// same generation; a watcher that has only seen generation n would
    /// otherwise miss the first event of a new drag that also began at n.
    fn begin_drag(&mut self, files: bool) {
        self.drag = Some(Drag {
            files,
            generation: self.drag.map_or(0, |drag| drag.generation.wrapping_add(1)),
            ..Drag::default()
        });
    }

    fn bump(&mut self, f: impl FnOnce(&mut Drag)) {
        if let Some(drag) = self.drag.as_mut() {
            f(drag);
            drag.generation = drag.generation.wrapping_add(1);
        }
    }
}

/// The MIME types a `wl_data_offer` advertised, filled in by its own
/// `wl_data_offer::offer` events before the `selection`/`enter` that names it.
type OfferMimes = Arc<Mutex<Vec<String>>>;

/// What a `wl_data_source` we created is for. Carried as the proxy's user data
/// so a `send` can be answered without a lookup, and so a source that has
/// already been superseded still serves the transfer it was asked for.
#[derive(Clone)]
enum SourceRole {
    /// A drag. Holds the data it offered, for `send`, a placeholder for the
    /// tab drag, which nothing reads, and a real URI list for a file drag.
    Drag(Arc<dyn AsMimeTypes + Send + Sync>),
    /// The clipboard. Holds the data it offered, for `send`.
    Selection(Arc<dyn AsMimeTypes + Send + Sync>),
}

/// A drag payload of exactly one MIME type and a body nothing reads.
///
/// The tab drag's: the reorder is computed from local widget state, so the
/// transfer exists only because a `wl_data_source` must offer something.
struct Placeholder(String);

impl AsMimeTypes for Placeholder {
    fn available(&self) -> Cow<'static, [String]> {
        Cow::from(vec![self.0.clone()])
    }

    fn as_bytes(&self, mime_type: &str) -> Option<Cow<'static, [u8]>> {
        (mime_type == self.0).then(|| Cow::from(&b"drag"[..]))
    }
}

static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();

/// Signalled by the data device thread whenever `Shared::button_pressed`
/// changes. Paired with [`SHARED`]'s mutex and nothing else.
static BUTTON: Condvar = Condvar::new();

/// How long [`start_drag`] may wait for the data device thread to dispatch the
/// button press that justifies it.
///
/// The two threads read the same socket, so the press is already queued here
/// by the time iced acts on it; this covers only the scheduling gap between the
/// queue and our dispatch callback. Long enough to swallow that, short enough
/// that iced's thread never stalls visibly, and it is waited on only when the
/// press has demonstrably not arrived yet.
const PRESS_WAIT: Duration = Duration::from_millis(10);

fn shared() -> &'static Mutex<Shared> {
    SHARED.get_or_init(|| Mutex::new(Shared::default()))
}

/// Bind the data device on `conn` and start dispatching its queue.
///
/// Call once, from the runner, with the very connection handed to
/// `iced_exwlshell`. Failure is not fatal: every entry point below degrades to
/// "no drag is possible", which is exactly the behaviour before this module
/// existed.
pub fn init(conn: &Connection) {
    if let Err(err) = try_init(conn) {
        log::warn!("drag-and-drop unavailable: {err}");
    }
}

fn try_init(conn: &Connection) -> Result<(), String> {
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());

    let mut state = State::default();
    // Two round trips: the first delivers the globals, the second the
    // `wl_seat::capabilities` that tells us a pointer exists.
    queue
        .roundtrip(&mut state)
        .map_err(|err| format!("registry roundtrip failed: {err}"))?;
    queue
        .roundtrip(&mut state)
        .map_err(|err| format!("seat roundtrip failed: {err}"))?;

    let (Some(manager), Some(seat)) = (state.manager.clone(), state.seat.clone()) else {
        return Err(String::from("no wl_data_device_manager or wl_seat"));
    };
    let device = manager.get_data_device(&seat, &qh, ());

    {
        let mut shared = shared().lock().unwrap();
        shared.conn = Some(conn.clone());
        shared.qh = Some(qh.clone());
        shared.manager_version = manager.version();
        shared.manager = Some(manager);
        shared.device = Some(device);
    }

    std::thread::Builder::new()
        .name(String::from("earth-files-dnd"))
        .spawn(move || {
            loop {
                if let Err(err) = queue.blocking_dispatch(&mut state) {
                    log::debug!("data device queue closed: {err}");
                    return;
                }
            }
        })
        .map_err(|err| format!("could not spawn the data device thread: {err}"))?;

    log::info!("drag-and-drop: wl_data_device bound on a second event queue");
    Ok(())
}

/// Tell this module the application scale factor.
///
/// `wl_data_device` reports drag coordinates in the surface's own space, but
/// `iced_exwlshell` divides its pointer coordinates by the application scale
/// factor before handing them to iced (`multi_window/state.rs`), so iced lays
/// widgets out in `wayland / scale`. Without the same division a drag on a
/// scaled output is reported at the wrong place, far outside the tab bar for
/// any scale above 1, and the drop hint is never computed, so the drag starts
/// and then silently never reorders anything.
///
/// The runner calls this wherever it updates `Core`'s own scale factor.
pub fn set_scale_factor(factor: f32) {
    let factor = f64::from(factor);
    if factor > 0.0 {
        shared().lock().unwrap().scale_factor = factor;
    }
}

/// Begin a drag offering exactly `mime` and a payload nothing reads.
///
/// The tab drag's entry point. See [`start_drag_data`] for the general one.
#[must_use]
pub fn start_drag(mime: &str) -> bool {
    start_drag_data(Arc::new(Placeholder(mime.to_owned())))
}

/// Begin a drag offering everything `contents` advertises, from the surface the
/// pointer is on.
///
/// Returns `false` when there is no data device, no pressed-button serial to
/// grab with, or a drag is already running; the caller then simply does not
/// enter its dragging state.
#[must_use]
pub fn start_drag_data(contents: Arc<dyn AsMimeTypes + Send + Sync>) -> bool {
    let mut shared = shared().lock().unwrap();

    if shared.drag_is_live() {
        log::warn!("drag refused: a drag is already in flight");
        return false;
    }

    let (Some(conn), Some(qh), Some(manager), Some(device)) = (
        shared.conn.clone(),
        shared.qh.clone(),
        shared.manager.clone(),
        shared.device.clone(),
    ) else {
        log::warn!(
            "drag refused: the wl_data_device was never bound \
             (an earlier \"drag-and-drop unavailable\" warning says why)"
        );
        return false;
    };

    let mimes = contents.available().into_owned();
    if mimes.is_empty() {
        log::warn!("drag refused: the contents offer no MIME types");
        return false;
    }

    // The press that justifies this drag reaches iced's thread and ours
    // independently, and only ours carries the serial. Nothing orders the two,
    // so wait, briefly, and only when the button is not yet known to be down,
    // rather than grab with a serial from some earlier press.
    if !shared.button_pressed {
        let (guard, wait) = BUTTON
            .wait_timeout_while(shared, PRESS_WAIT, |shared| !shared.button_pressed)
            .unwrap();
        shared = guard;
        if wait.timed_out() {
            log::warn!(
                "drag: no pointer button press reached the data device thread within \
                 {PRESS_WAIT:?}; starting with whatever serial is on hand"
            );
        }
    }

    let Some(serial) = shared.button_serial else {
        log::warn!(
            "drag refused: our wl_pointer has seen no button press, so there is no \
             grab serial to start a drag with"
        );
        return false;
    };
    let Some(origin) = shared.pointer_surface.clone() else {
        log::warn!(
            "drag refused: our wl_pointer has never entered a surface, so there is no \
             origin surface to drag from"
        );
        return false;
    };

    let source = manager.create_data_source(&qh, SourceRole::Drag(contents));
    for mime in &mimes {
        source.offer(mime.clone());
    }
    if shared.manager_version >= 3 {
        source.set_actions(DndAction::Move | DndAction::Copy);
    }
    // No icon surface: the compositor keeps the ordinary cursor. The drop hint
    // the widget draws is what tells the user where the drop will land.
    device.start_drag(Some(&source), &origin, None, serial);
    let _ = conn.flush();

    shared.source = Some(source);
    shared.offered = mimes.clone();
    shared.begin_drag(mimes.iter().any(|mime| is_file_mime(mime)));

    log::debug!("started a drag: mimes={mimes:?} serial={serial}");
    true
}

/// The live drag, if any.
#[must_use]
pub fn drag() -> Option<Drag> {
    shared().lock().unwrap().drag
}

/// Tear the drag down once the widget has acted on it.
pub fn end_drag() {
    let mut shared = shared().lock().unwrap();
    // The source is not destroyed here. After a drop the compositor still has
    // a `send` to deliver for the payload the destination is only now reading,
    // and destroying the source cancels that transfer, so it is parked until
    // `cancelled` or `dnd_finished` says the compositor is done with it, and
    // that arm destroys it. Overwriting a source still parked here loses
    // nothing: its own event arm matches on the proxy the event carries, not on
    // what this field happens to hold.
    shared.retired_source = shared.source.take();
    shared.offered.clear();
    // The drag is marked over, not erased, and the generation deliberately
    // does not move, so every watcher sees the identical ended drag.
    //
    // Several widgets watch one drag and all of them poll it during the same
    // widget pass, calling this as they see it end. Erasing it here hid it from
    // every widget the pass had not reached yet, and a drop on one of them was
    // published by nobody: that is how a drop on a tab was lost, because the
    // nav bar is drawn first and ended the drag before the tab bar had looked.
    // A retired drag stays readable until [`start_drag_data`] replaces it, so
    // the order widgets happen to be visited in stops mattering.
    if let Some(drag) = shared.drag.as_mut() {
        drag.ended = true;
    }
    // Only an offer that was not dropped on can be retired here: a dropped
    // one has moved to `dropped_offer`, which belongs to `read_drop`. This runs
    // once per widget watching the drag, so it has to be safe to run again with
    // nothing left to do.
    if let Some((offer, _)) = shared.drag_offer.take() {
        offer.destroy();
    }
    // The data device owns the pointer during a drag, so the compositor may
    // swallow the release. Clear the button state so the next drag waits for
    // its own press.
    shared.button_pressed = false;
    if let Some(conn) = shared.conn.as_ref() {
        let _ = conn.flush();
    }
}

/// How long a single clipboard read may take, end to end.
///
/// A `wl_data_offer::receive` hands a pipe to another client, which is free to
/// write nothing and never close it. Without a bound, a paste from a
/// misbehaving source would hang forever; with one, it fails and the paste is
/// simply not performed. Generous enough for a large image over a pipe, short
/// enough that a wedged source is an annoyance rather than a freeze, and it
/// costs nothing anyway, because the wait happens on a thread of its own.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Refuse a clipboard payload larger than this rather than growing without
/// bound on a source that never stops writing.
const READ_LIMIT: usize = 64 * 1024 * 1024;

/// Take ownership of the clipboard, offering everything `contents` advertises.
///
/// Returns `false` when there is no data device or no input serial to justify
/// the request; the copy then simply does not happen, which is what the app
/// did before this existed.
///
/// Runs on iced's thread: it creates a proxy, sends two requests and flushes.
/// Answering `wl_data_source::send` is the data device thread's job.
#[must_use]
pub fn set_selection(contents: Arc<dyn AsMimeTypes + Send + Sync>) -> bool {
    let mut shared = shared().lock().unwrap();

    let (Some(conn), Some(qh), Some(manager), Some(device)) = (
        shared.conn.clone(),
        shared.qh.clone(),
        shared.manager.clone(),
        shared.device.clone(),
    ) else {
        log::trace!("set_selection ignored: the data device was never bound");
        return false;
    };
    let Some(serial) = shared.last_serial else {
        log::warn!("set_selection ignored: no input serial seen yet");
        return false;
    };

    let mimes = contents.available();
    if mimes.is_empty() {
        log::warn!("set_selection ignored: the contents offer no MIME types");
        return false;
    }

    let source = manager.create_data_source(&qh, SourceRole::Selection(contents));
    for mime in mimes.iter() {
        source.offer(mime.clone());
    }
    device.set_selection(Some(&source), serial);
    let _ = conn.flush();

    // The previous source is dropped, not destroyed: the compositor answers a
    // replaced source with `cancelled`, and that arm destroys it. Destroying it
    // here would race a `send` already in flight for it.
    shared.selection_source = Some(source);

    log::debug!("took the clipboard: serial={serial} mimes={mimes:?}");
    true
}

/// Read the clipboard as the first of `allowed` the current offer advertises.
///
/// Blocks for at most [`READ_TIMEOUT`]. Call it on a separate thread, as
/// [`crate::ui::clipboard::read_data`] does.
///
/// `None` covers every ordinary failure: an empty clipboard, an offer with no
/// MIME type in common with `allowed`, a source that never answers.
#[must_use]
pub fn read_selection(allowed: &[String]) -> Option<(Vec<u8>, String)> {
    // The lock is held across `receive` and the flush, and released before the
    // pipe is drained. That is what makes self-paste safe: the data device
    // thread is never blocked by a read, so it is free to dispatch the `send`
    // that this very read is waiting on. It is also what makes the offer proxy
    // safe to retire concurrently; no proxy is touched after the unlock.
    let (read_fd, mime, conn) = {
        let shared = shared().lock().unwrap();
        receive(&shared, shared.selection_offer.as_ref(), allowed, "read_selection")?
    };
    // Keep the connection alive for the length of the read; nothing else here
    // needs it.
    let _ = &conn;

    match read_to_end_bounded(read_fd, READ_TIMEOUT) {
        Ok(data) => {
            log::debug!("read {} clipboard bytes as {mime}", data.len());
            Some((data, mime))
        }
        Err(err) => {
            log::warn!("read_selection: {mime}: {err}");
            None
        }
    }
}

/// Read whatever was dropped on us, as the first of `allowed` the drag offer
/// advertises.
///
/// Blocks for the same reason as [`read_selection`].
/// [`crate::ui::clipboard::read_drop_data`] runs it on a separate thread.
///
/// When this app is both source and destination, the read uses the same
/// safeguards as self-paste. It releases the lock before draining the pipe so
/// the data device thread can dispatch `wl_data_source::send`. [`write_all`]
/// answers that event on another thread so payloads larger than the 64 KiB
/// pipe buffer do not block the dispatcher. [`READ_TIMEOUT`] bounds the read
/// so an unresponsive source fails the drop without freezing the window.
///
/// `finish` and `destroy` are sent from here rather than from the `drop` event,
/// because a destination may only finish an offer it has actually read.
#[must_use]
pub fn read_drop(allowed: &[String]) -> Option<(Vec<u8>, String)> {
    // The offer is taken out of the shared slot and owned here for the whole
    // read, rather than borrowed from it. That is what makes the read safe
    // against the several `end_drag` calls one drop produces: once this has the
    // offer there is nothing left in `Shared` for anyone else to destroy.
    let (offer, read_fd, mime, conn) = {
        let mut shared = shared().lock().unwrap();
        let Some(offer) = shared.dropped_offer.take() else {
            log::debug!("read_drop: nothing has been dropped");
            return None;
        };
        match receive(&shared, Some(&offer), allowed, "read_drop") {
            Some((read_fd, mime, conn)) => (offer.0, read_fd, mime, conn),
            None => {
                offer.0.destroy();
                let _ = shared.conn.as_ref().map(Connection::flush);
                return None;
            }
        }
    };

    let result = read_to_end_bounded(read_fd, READ_TIMEOUT);

    // `finish` is a protocol error on an offer that was never read, so it is
    // sent only when there is a completed transfer to finish.
    if result.is_ok() && offer.version() >= 3 {
        offer.finish();
    }
    offer.destroy();
    let _ = conn.flush();

    match result {
        Ok(data) => {
            log::debug!("read {} dropped bytes as {mime}", data.len());
            Some((data, mime))
        }
        Err(err) => {
            log::warn!("read_drop: {mime}: {err}");
            None
        }
    }
}

/// Ask `offer` for the most-preferred of `allowed` it advertises, and hand back
/// the read end of the pipe it will be written to.
///
/// Sends `receive` and flushes under the caller's lock so the proxy cannot be
/// retired between them. It does not touch the proxy afterwards, so the caller
/// can unlock before draining the pipe.
fn receive(
    shared: &Shared,
    offer: Option<&(WlDataOffer, OfferMimes)>,
    allowed: &[String],
    what: &str,
) -> Option<(OwnedFd, String, Connection)> {
    let conn = shared.conn.clone()?;
    let Some((offer, mimes)) = offer else {
        log::debug!("{what}: there is nothing on offer");
        return None;
    };

    let offered = mimes.lock().unwrap().clone();
    let Some(mime) = allowed.iter().find(|wanted| offered.contains(wanted)).cloned() else {
        log::debug!("{what}: nothing in {offered:?} is one of {allowed:?}");
        return None;
    };

    let (read_fd, write_fd) = match pipe() {
        Ok(pair) => pair,
        Err(err) => {
            log::warn!("{what}: could not create a pipe: {err}");
            return None;
        }
    };
    offer.receive(mime.clone(), write_fd.as_fd_borrowed());
    // Our own end of the write side must be closed, or the read never sees
    // EOF: the pipe stays open through this process's copy of the fd.
    drop(write_fd);
    let _ = conn.flush();
    Some((read_fd, mime, conn))
}

/// A `pipe2(O_CLOEXEC)` pair, read end first.
fn pipe() -> io::Result<(OwnedFd, WriteFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a two-element array, which is what `pipe2` writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both are fresh fds this process now owns.
    unsafe { Ok((OwnedFd::from_raw_fd(fds[0]), WriteFd(OwnedFd::from_raw_fd(fds[1])))) }
}

/// The write half of [`pipe`], only ever handed straight to `receive`.
struct WriteFd(OwnedFd);

impl WriteFd {
    fn as_fd_borrowed(&self) -> BorrowedFd<'_> {
        // SAFETY: borrowed for no longer than the owner lives.
        unsafe { BorrowedFd::borrow_raw(self.0.as_raw_fd()) }
    }
}

/// Drain `fd` to EOF, without ever blocking indefinitely.
///
/// The fd is put in non-blocking mode and waited on with `poll`, so the
/// deadline is enforced by the wait itself rather than by a watchdog that would
/// have to interrupt a blocked `read`. A source that writes nothing, writes
/// slowly forever, or never closes its end all end the same way: an error after
/// [`READ_TIMEOUT`], with the fd closed and the thread gone.
fn read_to_end_bounded(fd: OwnedFd, timeout: Duration) -> io::Result<Vec<u8>> {
    let raw = fd.as_raw_fd();
    // SAFETY: `raw` is owned by `fd` for the whole function.
    if unsafe { libc::fcntl(raw, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }

    let deadline = Instant::now() + timeout;
    let mut data = Vec::new();
    let mut buf = [0u8; 16 * 1024];

    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("the clipboard source did not finish within {timeout:?}"),
            ));
        };

        let mut poll_fd = libc::pollfd {
            fd: raw,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid `pollfd`, and a timeout that fits an `int`.
        let ready = unsafe {
            libc::poll(
                &raw mut poll_fd,
                1,
                remaining.as_millis().min(i32::from(i16::MAX) as u128) as libc::c_int,
            )
        };
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if ready == 0 {
            continue; // the deadline check at the top of the loop decides
        }

        // SAFETY: `raw` is open and `buf` is exactly `buf.len()` writable bytes.
        let n = unsafe { libc::read(raw, buf.as_mut_ptr().cast(), buf.len()) };
        if n == 0 {
            return Ok(data); // EOF: the source closed its end
        }
        if n < 0 {
            let err = io::Error::last_os_error();
            match err.kind() {
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => continue,
                _ => return Err(err),
            }
        }
        data.extend_from_slice(&buf[..n as usize]);
        if data.len() > READ_LIMIT {
            return Err(io::Error::other("the clipboard payload exceeded 64 MiB"));
        }
    }
}

/// Dispatch state for our queue. Lives on the data device thread.
#[derive(Default)]
struct State {
    manager: Option<WlDataDeviceManager>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    offer: Option<WlDataOffer>,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };

        match interface.as_str() {
            "wl_data_device_manager" => {
                state.manager =
                    Some(registry.bind::<WlDataDeviceManager, _, _>(name, version.min(3), qh, ()));
            }
            // The first seat only: this app is single-seat, as was the drag it
            // is restoring.
            "wl_seat" if state.seat.is_none() => {
                state.seat = Some(registry.bind::<WlSeat, _, _>(name, version.min(7), qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };

        if capabilities.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
            // A second pointer on the same seat, alongside exwlshell's. See
            // the module docs: this is how a valid `start_drag` serial is come
            // by without taking one from anybody.
            state.pointer = Some(seat.get_pointer(qh, ()));
        }
        if capabilities.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
            // Likewise a second keyboard, and for the same reason: Ctrl+C has
            // to justify `set_selection` with a serial, and a key press is the
            // only event that carries one when the pointer never moved.
            state.keyboard = Some(seat.get_keyboard(qh, ()));
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        _state: &mut Self,
        _pointer: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let mut shared = shared().lock().unwrap();
        match event {
            wl_pointer::Event::Enter {
                serial, surface, ..
            } => {
                shared.pointer_surface = Some(surface);
                // Not a grab serial, and deliberately not stored as one.
                let _ = serial;
            }
            wl_pointer::Event::Button {
                serial,
                state: WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } => {
                shared.button_serial = Some(serial);
                shared.last_serial = Some(serial);
                shared.button_pressed = true;
                // `start_drag` may be waiting on exactly this.
                BUTTON.notify_all();
            }
            wl_pointer::Event::Button {
                state: WEnum::Value(wl_pointer::ButtonState::Released),
                ..
            } => {
                shared.button_pressed = false;
            }
            // The surface is kept across `leave` on purpose: the compositor
            // sends one the instant a drag starts, and the origin surface must
            // outlive that.
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        _state: &mut Self,
        _keyboard: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Only the serial is wanted. The keymap fd and the key itself belong to
        // exwlshell's keyboard, which is the one iced actually listens to.
        if let wl_keyboard::Event::Key {
            serial,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            shared().lock().unwrap().last_serial = Some(serial);
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    // `wl_data_device::data_offer` is a compositor-created `new_id`, so the
    // queue has to be told what user data the child proxy gets.
    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferMimes::default()),
    ]);

    fn event(
        state: &mut Self,
        _device: &WlDataDevice,
        event: wl_data_device::Event,
        (): &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let mut shared = shared().lock().unwrap();
        match event {
            wl_data_device::Event::DataOffer { id } => {
                state.offer = Some(id);
            }
            wl_data_device::Event::Selection { id } => {
                // `state.offer` holds whichever offer arrived last; if this is
                // it, ownership moves to the selection slot and the drag half
                // must not destroy it.
                if state.offer.as_ref() == id.as_ref() {
                    state.offer = None;
                }
                // One selection change of grace before a superseded offer is
                // destroyed; see `Shared::retired_offer`.
                if let Some(offer) = shared.retired_offer.take() {
                    offer.destroy();
                }
                shared.retired_offer = shared.selection_offer.take().map(|(offer, _)| offer);
                shared.selection_offer = id.map(|offer| {
                    let mimes = offer
                        .data::<OfferMimes>()
                        .cloned()
                        .unwrap_or_default();
                    log::debug!("clipboard offers {:?}", mimes.lock().unwrap());
                    (offer, mimes)
                });
            }
            wl_data_device::Event::Enter { x, y, id, .. } => {
                // Whatever was last over us is gone, and nobody read it.
                if let Some((offer, _)) = shared.drag_offer.take() {
                    offer.destroy();
                }
                let mut files = false;
                if let Some(offer) = id.clone() {
                    let mimes = offer.data::<OfferMimes>().cloned().unwrap_or_default();
                    let offered = mimes.lock().unwrap().clone();
                    // `accept` is what makes the compositor consider the drop
                    // valid, and it also names the MIME type `read_drop` will
                    // ask for, so a file list wins over anything else the
                    // source happens to advertise alongside it. `ClipboardCopy`
                    // offers `text/plain` first, and accepting that would
                    // leave the drop with a newline-separated blob instead of
                    // the URI list the paste path parses.
                    let file_mime = FILE_MIMES
                        .iter()
                        .find(|mime| offered.iter().any(|have| have == *mime))
                        .map(|mime| (*mime).to_owned());
                    files = file_mime.is_some();
                    // Failing that, a drag of our own is answered with the
                    // first type it said it had, the tab drag's placeholder,
                    // and a drag from anywhere else is refused.
                    let accept = file_mime.or_else(|| {
                        shared
                            .offered
                            .iter()
                            .find(|mime| offered.contains(mime))
                            .cloned()
                    });
                    offer.accept(0, accept);
                    if offer.version() >= 3 {
                        // Accept both actions, preferring move. GTK drag
                        // sources offer only `Copy`; accepting only `Move`
                        // would leave no shared action and the compositor
                        // would cancel the drag. The negotiated action does
                        // not decide our operation: move-or-copy uses modifier
                        // held at the drop.
                        offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Move);
                    }
                    shared.drag_offer = Some((offer, mimes));
                    // Ownership has moved to `drag_offer`; the selection half
                    // must not destroy it from under the drag.
                    state.offer = None;
                }
                // A drag started by another client is only ever heard of here.
                // `is_live` rather than `is_some`: the drag that ended last is
                // kept readable until the next one starts (see `end_drag`), and
                // updating that one instead of starting a fresh one would
                // hand every watcher a drag already marked over, which is how
                // a drop from another application came to be refused outright.
                if !shared.drag_is_live() {
                    shared.begin_drag(files);
                }
                let scaled = shared.to_logical(x, y);
                shared.bump(|drag| {
                    drag.position = Some(scaled);
                    drag.files = files;
                });
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                let scaled = shared.to_logical(x, y);
                shared.bump(|drag| drag.position = Some(scaled));
            }
            wl_data_device::Event::Leave => {
                // A `leave` follows every `drop`. Clearing the position there
                // too would lose the very coordinates the drop is computed
                // from, so an ended drag keeps its last position.
                //
                // A drag of our own survives a leave: the pointer may come back
                // over the surface, and the source half still has the say in
                // when it is over. A drag from another client has no source
                // half here, so leaving is the end of it; otherwise the
                // poller would spin on a drag nothing will ever finish.
                let foreign = shared.offered.is_empty();
                shared.bump(|drag| {
                    if !drag.ended {
                        drag.position = None;
                        drag.ended = foreign;
                    }
                });
                let ended = shared.drag.is_some_and(|drag| drag.ended);
                if !ended && let Some((offer, _)) = shared.drag_offer.take() {
                    offer.destroy();
                }
                if let Some(offer) = state.offer.take() {
                    offer.destroy();
                }
            }
            wl_data_device::Event::Drop => {
                shared.bump(|drag| {
                    drag.dropped = true;
                    drag.ended = true;
                });
                // The offer outlives this event, in a slot nothing but
                // `read_drop` retires: that is where `receive` happens, and
                // `finish`/`destroy` are its to send once the payload is in
                // hand. An offer left here was never read: it was a tab drag
                // or a drop no widget claimed. A read takes the offer out of
                // this slot before draining, so this cannot destroy an offer
                // while a reader uses it.
                let unread = shared.dropped_offer.take();
                shared.dropped_offer = shared.drag_offer.take();
                if let Some((offer, _)) = unread {
                    offer.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataOffer, OfferMimes> for State {
    fn event(
        _state: &mut Self,
        _offer: &WlDataOffer,
        event: wl_data_offer::Event,
        mimes: &OfferMimes,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // The advertisement arrives as one event per type, before the
        // `selection` or `enter` that names the offer, so it is collected on the
        // offer's own user data rather than against any current selection.
        if let wl_data_offer::Event::Offer { mime_type } = event {
            mimes.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, SourceRole> for State {
    fn event(
        _state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        role: &SourceRole,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match (event, role) {
            (
                wl_data_source::Event::Send { mime_type, fd },
                SourceRole::Drag(contents) | SourceRole::Selection(contents),
            ) => {
                let bytes = contents.as_bytes(&mime_type).unwrap_or_default();
                log::debug!("serving {} bytes as {mime_type}", bytes.len());
                write_all(fd, &bytes);
            }
            (wl_data_source::Event::Cancelled, SourceRole::Selection(_)) => {
                // Another client took the clipboard, so this source will never
                // be asked again and the protocol says to destroy it.
                let mut shared = shared().lock().unwrap();
                if shared.selection_source.as_ref() == Some(source) {
                    shared.selection_source = None;
                }
                source.destroy();
            }
            (
                wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished,
                SourceRole::Drag(_),
            ) => {
                let mut shared = shared().lock().unwrap();
                shared.bump(|drag| drag.ended = true);
                // The compositor is done with this source, whichever slot still
                // holds it; see `end_drag`.
                if shared.source.as_ref() == Some(source) {
                    shared.source = None;
                }
                if shared.retired_source.as_ref() == Some(source) {
                    shared.retired_source = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
}

/// Answer a `send` by writing `bytes` to `fd` and closing it.
///
/// On a thread of its own, because this is called from the data device
/// thread and a pipe write blocks once the 64 KiB buffer fills. Self-paste is
/// exactly that case: the reader is another thread of ours that will drain it,
/// but only if this thread goes back to dispatching instead of sitting in
/// `write`. A payload larger than the buffer would otherwise deadlock the two
/// halves against each other.
fn write_all(fd: OwnedFd, bytes: &[u8]) {
    let bytes = bytes.to_vec();
    let spawned = std::thread::Builder::new()
        .name(String::from("earth-files-clipboard-send"))
        .spawn(move || {
            use std::io::Write as _;
            let mut file = std::fs::File::from(fd);
            // A reader that gives up mid-transfer leaves this thread with an
            // EPIPE, which is the expected end of that story, not an error.
            let _ = file.write_all(&bytes);
        });
    if let Err(err) = spawned {
        log::warn!("could not answer a clipboard send: {err}");
    }
}

delegate_noop!(State: ignore WlDataDeviceManager);

delegate_noop!(State: ignore WlSurface);

#[cfg(test)]
mod tests {
    use super::*;

    /// `wl_data_device` reports surface-local coordinates; iced lays out in
    /// `wayland / scale`, because `iced_exwlshell` divides its own pointer
    /// coordinates that way. Getting this wrong puts a drag on a scaled output
    /// far outside the widget it is actually over, so the drop hint is never
    /// computed and the drag silently does nothing.
    #[test]
    fn drag_coordinates_are_converted_to_iced_layout_space() {
        let mut shared = Shared::default();

        // Default-constructed means "never told", which must behave as 1.0
        // rather than dividing by zero.
        assert_eq!(shared.scale_factor, 0.0);
        assert_eq!(shared.to_logical(200.0, 100.0), (200.0, 100.0));

        shared.scale_factor = 2.0;
        assert_eq!(shared.to_logical(200.0, 100.0), (100.0, 50.0));

        shared.scale_factor = 1.5;
        assert_eq!(shared.to_logical(300.0, 150.0), (200.0, 100.0));
    }

    /// With no connection ever handed over, every entry point is inert rather
    /// than panicking, the state the app is in when `init` failed.
    #[test]
    fn start_drag_without_a_connection_is_a_no_op() {
        assert!(!start_drag("x-earth-files/tab-drag"));
        assert_eq!(drag(), None);
        end_drag();
    }

    #[test]
    fn a_source_that_never_writes_times_out_rather_than_hanging() {
        let (read_fd, write_fd) = pipe().unwrap();
        // The write end stays open and silent: the worst a misbehaving
        // clipboard source can do.
        let started = Instant::now();
        let err = read_to_end_bounded(read_fd, Duration::from_millis(200)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(write_fd);
    }

    /// The self-paste hazard in miniature: a payload several times the 64 KiB
    /// pipe buffer, served exactly as `wl_data_source::send` serves it, read
    /// exactly as `read_selection` reads it. Both halves have to be on their
    /// own threads or this deadlocks instead of failing.
    #[test]
    fn a_payload_larger_than_the_pipe_buffer_round_trips() {
        let payload = vec![b'z'; 512 * 1024];
        let (read_fd, write_fd) = pipe().unwrap();
        write_all(write_fd.0, &payload);
        let read = read_to_end_bounded(read_fd, Duration::from_secs(5)).unwrap();
        assert_eq!(read, payload);
    }

    /// A drag that ends leaves no button state behind: the compositor may
    /// never send the release that ended it, and the next drag has to wait for
    /// a press of its own rather than start on a stale serial.
    #[test]
    fn end_drag_forgets_the_button_state() {
        shared().lock().unwrap().button_pressed = true;
        end_drag();
        assert!(!shared().lock().unwrap().button_pressed);
    }

    /// The tab drag still offers exactly one MIME type and answers only that
    /// one, now that it shares `start_drag_data` with the file drag.
    #[test]
    fn the_placeholder_serves_only_its_own_mime() {
        let placeholder = Placeholder(String::from("x-earth-files/tab-drag"));
        assert_eq!(
            placeholder.available().as_ref(),
            ["x-earth-files/tab-drag".to_string()]
        );
        assert!(placeholder.as_bytes("x-earth-files/tab-drag").is_some());
        assert!(placeholder.as_bytes("text/uri-list").is_none());
        // No file list, so a drag carrying it must not be mistaken for one and
        // routed into the file view's drop handling.
        assert!(
            !placeholder
                .available()
                .iter()
                .any(|mime| is_file_mime(mime))
        );
    }

    /// A drop is only worth accepting in the MIME types the paste path can
    /// already parse, and in the order it prefers them; otherwise a drop would
    /// negotiate a format `ClipboardPaste` then rejects.
    #[test]
    fn the_file_mime_types_are_the_clipboards_own() {
        use crate::clipboard::ClipboardPaste;
        use crate::ui::clipboard::AllowedMimeTypes as _;

        assert_eq!(FILE_MIMES.as_slice(), ClipboardPaste::allowed().as_ref());
        assert!(FILE_MIMES.iter().all(|mime| is_file_mime(mime)));
        assert!(!is_file_mime("text/plain"));
    }

    /// Nothing was dropped, so there is nothing to read, and asking anyway is
    /// a `None`, not a block on a pipe that will never be written to.
    #[test]
    fn read_drop_without_an_offer_is_a_no_op() {
        let started = Instant::now();
        assert_eq!(read_drop(&[String::from("text/uri-list")]), None);
        assert!(started.elapsed() < READ_TIMEOUT);
    }

    /// A drag that has ended stays readable until the next one starts, so that
    /// every widget watching it sees the same ending whichever order they are
    /// visited in. Two things have to hold for that to work, and both of them
    /// were bugs before they did: an ended drag must not count as live, or it
    /// would refuse the next drag and a drop from another application; and no
    /// two drags may ever show a watcher the same generation, or a watcher that
    /// had seen the first would miss the start of the second.
    #[test]
    fn a_new_drag_is_live_and_never_repeats_a_generation() {
        let mut shared = Shared::default();
        assert!(!shared.drag_is_live());

        shared.begin_drag(true);
        assert!(shared.drag_is_live());
        assert!(shared.drag.unwrap().files);

        shared.bump(|drag| drag.position = Some((1.0, 2.0)));
        shared.drag.as_mut().unwrap().ended = true;
        let ended = shared.drag.unwrap();
        assert!(
            !shared.drag_is_live(),
            "an ended drag must not block the drag that replaces it"
        );

        shared.begin_drag(false);
        let started = shared.drag.unwrap();
        assert!(shared.drag_is_live());
        assert!(!started.files);
        assert_ne!(
            started.generation, ended.generation,
            "a watcher keyed on the generation would miss this drag entirely"
        );
    }

    #[test]
    fn generation_advances_on_every_change() {
        let mut shared = Shared {
            drag: Some(Drag::default()),
            ..Shared::default()
        };
        shared.bump(|drag| drag.position = Some((1.0, 2.0)));
        shared.bump(|drag| drag.position = Some((1.0, 2.0)));
        let drag = shared.drag.unwrap();
        assert_eq!(drag.generation, 2);
        assert_eq!(drag.position, Some((1.0, 2.0)));
    }
}
