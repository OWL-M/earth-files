// SPDX-License-Identifier: GPL-3.0-only

//! The file chooser backend for `xdg-desktop-portal`.
//!
//! `xdg-desktop-portal` routes an application's request for a file chooser to
//! whichever backend the desktop prefers. This one answers with the same
//! chooser the file manager uses for its own "Move to" and "Copy to".
//!
//! It runs as one process with no window of its own, started on demand by
//! D-Bus activation and kept alive by systemd for the session. Requests
//! arrive on the bus, each opens a chooser, and the answer goes back when
//! that chooser closes.

pub mod app;
pub mod convert;
pub mod service;

use std::error::Error;

/// Run the backend.
///
/// # Errors
///
/// Fails if the bus name cannot be taken, which is what happens when another
/// backend already owns it, or if the shell cannot start.
pub fn main() -> Result<(), Box<dyn Error>> {
    crate::init_logging();
    crate::localize::localize();
    crate::ui::theme::custom::load();

    let (requests, receiver) = tokio::sync::mpsc::unbounded_channel();
    app::set_receiver(receiver);

    // The bus side needs a runtime of its own: the shell takes over this
    // thread, and the connection has to stay alive for as long as it runs.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let connection = runtime.block_on(service::serve(requests))?;
    let _guard = runtime.enter();

    let settings = crate::ui::shell::Settings::default().no_main_window(true);
    crate::ui::shell::run::<app::App>(
        settings,
        app::Flags {
            connection: Some(connection),
        },
    )?;
    Ok(())
}
