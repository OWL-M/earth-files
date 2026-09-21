//! Spike: can the shell run with no window of its own and open a chooser on
//! demand? That is the assumption a portal backend rests on.
//!
//! Run it and a file chooser appears with no parent window behind it. Pick
//! something or cancel and the result is printed.

use earth_files::dialog::{Dialog, DialogKind, DialogMessage, DialogResult, DialogSettings};
use earth_files::ui::app::Task;
use earth_files::ui::iced::Subscription;
use earth_files::ui::iced::window;
use earth_files::ui::shell::{self as app, Application, Core, Settings};
use earth_files::ui::{Element, widget};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_env("RUST_LOG"))
        .init();

    let settings = Settings::default().no_main_window(true);
    app::run::<App>(settings, ())?;
    Ok(())
}

#[derive(Clone, Debug)]
pub enum Message {
    DialogMessage(DialogMessage),
    DialogResult(DialogResult),
}

pub struct App {
    core: Core,
    dialog_opt: Option<Dialog<Message>>,
}

impl Application for App {
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "com.owlm.EarthFilesHeadlessSpike";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, (): Self::Flags) -> (Self, Task<Message>) {
        // Straight into a chooser: there is no window to click a button in
        let (dialog, task) = Dialog::new(
            DialogSettings::new().kind(DialogKind::OpenFile),
            Message::DialogMessage,
            Message::DialogResult,
        );
        (
            Self {
                core,
                dialog_opt: Some(dialog),
            },
            task,
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::DialogMessage(message) => {
                if let Some(dialog) = &mut self.dialog_opt {
                    return dialog.update(message);
                }
            }
            Message::DialogResult(result) => {
                println!("RESULT {result:?}");
                self.dialog_opt = None;
                std::process::exit(0);
            }
        }
        Task::none()
    }

    fn view_window(&self, window_id: window::Id) -> Element<'_, Message> {
        match &self.dialog_opt {
            Some(dialog) => dialog.view(window_id),
            None => widget::text::body("no dialog").into(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        widget::text::body("no main window").into()
    }

    fn subscription(&self) -> Subscription<Message> {
        self.dialog_opt
            .as_ref()
            .map_or_else(Subscription::none, |dialog| {
                dialog.subscription().map(Message::DialogMessage)
            })
    }
}
