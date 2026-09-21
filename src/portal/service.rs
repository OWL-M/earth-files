// SPDX-License-Identifier: GPL-3.0-only

//! The D-Bus side of the backend.
//!
//! `xdg-desktop-portal` calls one of three methods and waits for the answer.
//! Each call hands a request to the application over a channel and waits on a
//! reply channel, so the chooser itself runs on the interface thread while
//! this side stays async.

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};

use super::convert::{Method, RESPONSE_OTHER, Request};

/// The bus name this backend owns
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.earthfiles";

/// What the application is asked to do
pub struct Incoming {
    pub handle: OwnedObjectPath,
    pub request: Request,
    /// Filled in with the answer. Dropping it answers "something went wrong",
    /// so a crash in the application does not leave the caller waiting.
    pub reply: oneshot::Sender<Answer>,
}

/// What the application answers with
pub struct Answer {
    pub response: u32,
    pub results: HashMap<String, OwnedValue>,
}

impl Answer {
    #[must_use]
    pub fn failed() -> Self {
        Self {
            response: RESPONSE_OTHER,
            results: HashMap::new(),
        }
    }
}

/// A request the caller may close before the user has answered
pub struct RequestObject {
    pub closed: Option<oneshot::Sender<()>>,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl RequestObject {
    /// The caller gave up, so take the chooser off the screen
    fn close(&mut self) {
        if let Some(closed) = self.closed.take() {
            let _ = closed.send(());
        }
    }
}

pub struct FileChooser {
    pub requests: mpsc::UnboundedSender<Incoming>,
}

impl FileChooser {
    async fn run(
        &self,
        handle: OwnedObjectPath,
        title: &str,
        options: HashMap<String, OwnedValue>,
        method: Method,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let request = Request::from_options(method, title, &options);
        let (reply, answer) = oneshot::channel();
        let incoming = Incoming {
            handle,
            request,
            reply,
        };
        if self.requests.send(incoming).is_err() {
            log::error!("the chooser is gone, so {method:?} cannot be answered");
            let failed = Answer::failed();
            return (failed.response, failed.results);
        }
        // Dropped without an answer counts as a failure rather than a hang
        let answer = answer.await.unwrap_or_else(|_| Answer::failed());
        (answer.response, answer.results)
    }
}

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    async fn open_file(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        log::debug!("OpenFile from {app_id:?} (parent {parent_window:?})");
        self.run(handle, title, options, Method::OpenFile).await
    }

    async fn save_file(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        log::debug!("SaveFile from {app_id:?} (parent {parent_window:?})");
        self.run(handle, title, options, Method::SaveFile).await
    }

    async fn save_files(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        log::debug!("SaveFiles from {app_id:?} (parent {parent_window:?})");
        self.run(handle, title, options, Method::SaveFiles).await
    }
}

/// Take the bus name and serve the interface.
///
/// Returns the connection, which must be kept alive for as long as the
/// backend is meant to answer.
pub async fn serve(
    requests: mpsc::UnboundedSender<Incoming>,
) -> Result<zbus::Connection, zbus::Error> {
    let connection = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at("/org/freedesktop/portal/desktop", FileChooser { requests })?
        .build()
        .await?;
    log::info!("serving {BUS_NAME}");
    Ok(connection)
}

/// Export a `Request` object so the caller can close this dialog
pub async fn export_request(
    connection: &zbus::Connection,
    handle: &ObjectPath<'_>,
    closed: oneshot::Sender<()>,
) {
    let object = RequestObject {
        closed: Some(closed),
    };
    if let Err(err) = connection.object_server().at(handle, object).await {
        log::warn!("could not export request {handle}: {err}");
    }
}

/// Withdraw a request object once its dialog is done with
pub async fn unexport_request(connection: &zbus::Connection, handle: &ObjectPath<'_>) {
    let _: Result<bool, _> = connection
        .object_server()
        .remove::<RequestObject, _>(handle)
        .await;
}

/// Not used yet: the frontend reads results from the method reply. Kept so the
/// unused-import lint does not hide a future need for it.
#[allow(dead_code)]
type Emitter<'a> = SignalEmitter<'a>;
