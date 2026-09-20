// SPDX-License-Identifier: GPL-3.0-only

//! Keeps the machine from sleeping or shutting down while file operations run.

use std::os::fd::OwnedFd;

/// Ask logind to block sleep and shutdown for `why`.
///
/// The lock holds for as long as the returned fd lives, so the caller keeps it
/// alive exactly as long as the operations run. `None` when logind is not
/// there or refuses, in which case operations just run without the lock.
pub async fn block_sleep_and_shutdown(why: &str) -> Option<OwnedFd> {
    let connection = match zbus::Connection::system().await {
        Ok(connection) => connection,
        Err(err) => {
            log::debug!("system bus unavailable, not inhibiting sleep: {err}");
            return None;
        }
    };
    let reply = match connection
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Inhibit",
            &("shutdown:sleep", "Earth Files", why, "block"),
        )
        .await
    {
        Ok(reply) => reply,
        Err(err) => {
            log::debug!("logind refused the inhibit lock: {err}");
            return None;
        }
    };
    let fd: zbus::zvariant::OwnedFd = reply.body().deserialize().ok()?;
    Some(fd.into())
}
