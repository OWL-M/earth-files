// SPDX-License-Identifier: GPL-3.0-only

//! The application the backend runs.
//!
//! It has no window of its own. Each request from the bus opens a chooser in
//! a window, and closing that chooser answers the request. Several may be on
//! screen at once, because two applications can ask at the same time, so each
//! is kept against a serial number that identifies its request.

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use tokio::sync::mpsc;
use zbus::zvariant::OwnedObjectPath;

use crate::dialog::{Dialog, DialogChoice, DialogMessage, DialogResult, DialogSettings};
use crate::ui::app::Task;
use crate::ui::iced::{Subscription, stream, window};
use crate::ui::shell::{Application, Core};
use crate::ui::{Element, widget};

use super::convert::{self, RESPONSE_CANCELLED, RESPONSE_SUCCESS};
use super::service::{Answer, Incoming};

/// Requests that have arrived but have no chooser yet.
///
/// They travel out of band because an `Incoming` carries the channel its
/// answer goes back on, which cannot be cloned, and an application message
/// must be.
static QUEUE: LazyLock<Mutex<Vec<(u64, Incoming)>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// The receiving end of the bus side, taken by the subscription that drains it
static RECEIVER: Mutex<Option<mpsc::UnboundedReceiver<Incoming>>> = Mutex::new(None);

/// Hand the bus side's receiver over before the application starts
pub fn set_receiver(receiver: mpsc::UnboundedReceiver<Incoming>) {
    *RECEIVER.lock().unwrap_or_else(|err| err.into_inner()) = Some(receiver);
}

fn queue() -> std::sync::MutexGuard<'static, Vec<(u64, Incoming)>> {
    QUEUE.lock().unwrap_or_else(|err| err.into_inner())
}

#[derive(Clone, Debug)]
pub enum Message {
    /// A request arrived and is waiting in the queue
    Opened(u64),
    /// Something happened inside one of the choosers
    Dialog(u64, DialogMessage),
    /// A chooser finished, one way or the other
    Finished(u64, DialogResult),
    /// The caller withdrew its request before the user answered
    Closed(u64),
    None,
}

struct Session {
    handle: OwnedObjectPath,
    dialog: Dialog<Message>,
    request: convert::Request,
    reply: Option<tokio::sync::oneshot::Sender<Answer>>,
}

pub struct App {
    core: Core,
    sessions: HashMap<u64, Session>,
    connection: Option<zbus::Connection>,
}

pub struct Flags {
    pub connection: Option<zbus::Connection>,
}

impl App {
    /// Turn a queued request into a chooser on screen
    fn open(&mut self, serial: u64) -> Task<Message> {
        let found = {
            let mut queue = queue();
            queue
                .iter()
                .position(|(queued, _)| *queued == serial)
                .map(|at| queue.remove(at).1)
        };
        let Some(incoming) = found else {
            return Task::none();
        };

        let request = incoming.request;
        let mut settings = DialogSettings::new().kind(request.kind.clone());
        if let Some(folder) = request.folder.clone() {
            settings = settings.path(folder);
        }

        let (mut dialog, task) = Dialog::new(
            settings,
            move |message| Message::Dialog(serial, message),
            move |result| Message::Finished(serial, result),
        );

        let mut tasks = vec![task, dialog.set_title(request.title.clone())];
        if let Some(label) = &request.accept_label {
            dialog.set_accept_label(label);
        }
        if !request.choices.is_empty() {
            dialog.set_choices(request.choices.clone());
        }
        if !request.filters.is_empty() {
            tasks.push(dialog.set_filters(request.filters.clone(), request.filter_selected));
        }

        // Let the caller withdraw the request: the portal frontend calls
        // `Close` on this object when the application that asked goes away or
        // gives up, and the chooser should not outlive it.
        if let Some(connection) = self.connection.clone() {
            let handle = incoming.handle.clone();
            let (closed, withdrawn) = tokio::sync::oneshot::channel();
            tasks.push(Task::future(async move {
                super::service::export_request(&connection, &handle.as_ref(), closed).await;
                // Resolves when `Close` is called, and errors when the object
                // is withdrawn because the user answered first
                match withdrawn.await {
                    Ok(()) => crate::ui::action::app(Message::Closed(serial)),
                    Err(_) => crate::ui::action::app(Message::None),
                }
            }));
        }

        self.sessions.insert(
            serial,
            Session {
                handle: incoming.handle,
                dialog,
                request,
                reply: Some(incoming.reply),
            },
        );
        Task::batch(tasks)
    }

    /// Answer the caller and take the chooser off the screen.
    ///
    /// `result` is `None` when the caller withdrew the request rather than
    /// the user answering it.
    fn finish(&mut self, serial: u64, result: Option<DialogResult>) -> Task<Message> {
        let Some(mut session) = self.sessions.remove(&serial) else {
            return Task::none();
        };

        if let Some(reply) = session.reply.take() {
            let answer = match result {
                Some(DialogResult::Open(chosen)) if !chosen.is_empty() => {
                    let paths = session.request.selected_paths(chosen);
                    let (filters, selected) = session.dialog.filters();
                    Answer {
                        response: RESPONSE_SUCCESS,
                        results: convert::results(
                            &paths,
                            &chosen_choices(session.dialog.choices()),
                            selected.and_then(|at| filters.get(at)),
                        ),
                    }
                }
                _ => Answer {
                    response: RESPONSE_CANCELLED,
                    results: HashMap::new(),
                },
            };
            let _ = reply.send(answer);
        }

        let mut tasks = vec![window::close(session.dialog.window_id())];
        if let Some(connection) = self.connection.clone() {
            let handle = session.handle.clone();
            tasks.push(Task::future(async move {
                super::service::unexport_request(&connection, &handle.as_ref()).await;
                crate::ui::action::app(Message::None)
            }));
        }
        Task::batch(tasks)
    }
}

/// The chosen value of each choice, as the portal wants them back
fn chosen_choices(choices: &[DialogChoice]) -> Vec<(String, String)> {
    choices
        .iter()
        .map(|choice| match choice {
            DialogChoice::CheckBox { id, value, .. } => (
                id.clone(),
                if *value { "true" } else { "false" }.to_string(),
            ),
            DialogChoice::ComboBox {
                id,
                options,
                selected,
                ..
            } => (
                id.clone(),
                selected
                    .and_then(|at| options.get(at))
                    .map(|option| option.id.clone())
                    .unwrap_or_default(),
            ),
        })
        .collect()
}

impl Application for App {
    type Flags = Flags;
    type Message = Message;

    const APP_ID: &'static str = "com.owlm.EarthFilesPortal";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn title(&self, id: window::Id) -> &str {
        crate::dialog::window_title(
            self.sessions
                .values()
                .map(|session| (session.dialog.window_id(), session.dialog.title())),
            &self.core.title,
            id,
        )
    }

    fn init(core: Core, flags: Self::Flags) -> (Self, Task<Message>) {
        (
            Self {
                core,
                sessions: HashMap::new(),
                connection: flags.connection,
            },
            Task::none(),
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Opened(serial) => return self.open(serial),
            Message::Dialog(serial, message) => {
                if let Some(session) = self.sessions.get_mut(&serial) {
                    return session.dialog.update(message);
                }
            }
            Message::Finished(serial, result) => return self.finish(serial, Some(result)),
            Message::Closed(serial) => return self.finish(serial, None),
            Message::None => {}
        }
        Task::none()
    }

    /// A chooser the compositor closed is a cancelled request, not a window
    /// that quietly disappears while the caller waits for an answer.
    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        self.sessions
            .iter()
            .find(|(_, session)| session.dialog.window_id() == id)
            .map(|(serial, _)| Message::Closed(*serial))
    }

    fn view_window(&self, window_id: window::Id) -> Element<'_, Message> {
        for session in self.sessions.values() {
            if session.dialog.window_id() == window_id {
                return session.dialog.view(window_id);
            }
        }
        widget::text::body("").into()
    }

    fn view(&self) -> Element<'_, Message> {
        // The backend has no window of its own to draw
        widget::text::body("").into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![requests_subscription()];
        for serial in self.sessions.keys() {
            let session = &self.sessions[serial];
            subscriptions.push(
                session
                    .dialog
                    .subscription()
                    .with(*serial)
                    .map(|(serial, message)| Message::Dialog(serial, message)),
            );
        }
        Subscription::batch(subscriptions)
    }
}

/// Requests arriving from the bus, as application messages.
///
/// The request itself is left in [`QUEUE`]; only its serial travels as a
/// message, because the answer channel it carries cannot be cloned.
fn requests_subscription() -> Subscription<Message> {
    struct Requests;
    Subscription::run_with(TypeId::of::<Requests>(), |_| {
        stream::channel(4, |mut output| async move {
            let receiver = RECEIVER
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .take();
            let Some(mut receiver) = receiver else {
                log::error!("the request stream was already taken");
                std::future::pending::<()>().await;
                return;
            };
            let mut serial = 0_u64;
            while let Some(incoming) = receiver.recv().await {
                serial += 1;
                queue().push((serial, incoming));
                if crate::ui::iced::futures::SinkExt::send(&mut output, Message::Opened(serial))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        })
    })
}
