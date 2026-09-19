// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Messages handled by the shell's daemon, adapted from `cosmic::Action`.
//!
//! `DbusActivation` is omitted because this build never enabled
//! `single-instance`. [`Action::Exwl`] carries requests to `iced_exwlshell`.
//!
//! exwlshell requires `Message: TryInto<ExwlShellCustomActionWithId,
//! Error = Message>`. Its `#[to_exwlshell_message]` macro implements this by
//! injecting eighteen variants (`NewPopUp`, `PopUpReposition`, `RemoveWindow`,
//! `NewLayerShell`, `Lock`, …). Seventeen cover surfaces this app never opens,
//! but all would need handling in [`Action::map`] and [`Action::flatten`],
//! which run on every popup view.
//!
//! A single variant holds `ExwlShellCustomActionWithId`, the macro's output
//! type. The shell constructs its requests in `shell::runner`, and `TryInto`
//! takes three lines. The macro's `popup_open` and `base_window_open` helpers
//! combine `IcedId::unique()` with `Task::done`; [`exwl`] reproduces them.

use iced_exwlshell::actions::{
    ExwlShellCustomAction, ExwlShellCustomActionWithId, IcedNewPopupSettings,
    IcedXdgWindowSettings,
};
use iced_core::window;

use crate::ui::{app, surface};

/// An application message, for the application itself.
pub const fn app<M>(message: M) -> Action<M> {
    Action::App(message)
}

/// An internal message, for the shell.
pub const fn cosmic<M>(message: app::Action) -> Action<M> {
    Action::Cosmic(message)
}

/// Do nothing.
pub const fn none<M>() -> Action<M> {
    Action::None
}

/// Wrap a surface action, typically produced by a widget, to be handled by the shell.
pub const fn surface<M>(action: surface::Action<M>) -> Action<M> {
    Action::Surface(action)
}

#[derive(Clone, Debug)]
#[must_use]
pub enum Action<M> {
    /// Messages from the application, for the application.
    App(M),
    /// Internal messages to be handled by the shell.
    Cosmic(app::Action),
    /// Surface (popup) requests, handled by the shell.
    Surface(surface::Action<M>),
    /// A request for the `iced_exwlshell` runtime itself. Intercepted before
    /// `update` ever sees it.
    Exwl(ExwlShellCustomActionWithId),
    /// Do nothing.
    None,
}

impl<M: 'static> Action<M> {
    /// Map the application message inside, leaving the shell's own variants untouched.
    #[must_use]
    pub fn map<N: 'static>(self, f: impl Fn(M) -> N + Clone + Send + Sync + 'static) -> Action<N> {
        match self {
            Action::App(message) => Action::App(f(message)),
            Action::Cosmic(action) => Action::Cosmic(action),
            Action::Surface(action) => Action::Surface(action.map(f)),
            Action::Exwl(action) => Action::Exwl(action),
            Action::None => Action::None,
        }
    }
}

impl<M: 'static> Action<Action<M>> {
    /// Collapse a doubly wrapped action, as produced by widgets whose message
    /// type is already an [`Action`], into a single one.
    #[must_use]
    pub fn flatten(self) -> Action<M> {
        match self {
            Action::App(action) => action,
            Action::Cosmic(action) => Action::Cosmic(action),
            Action::Surface(action) => Action::Surface(action.flatten()),
            Action::Exwl(action) => Action::Exwl(action),
            Action::None => Action::None,
        }
    }
}

impl<M> From<M> for Action<M> {
    fn from(value: M) -> Self {
        Self::App(value)
    }
}

/// What `iced_exwlshell::daemon` requires of its message type.
///
/// Everything that is not an [`Action::Exwl`] is handed back untouched and
/// reaches `update` as normal; this is exactly the shape
/// `#[to_exwlshell_message]` generates.
impl<M> TryInto<ExwlShellCustomActionWithId> for Action<M> {
    type Error = Self;

    fn try_into(self) -> Result<ExwlShellCustomActionWithId, Self::Error> {
        match self {
            Action::Exwl(action) => Ok(action),
            other => Err(other),
        }
    }
}

/// Constructors for the exwlshell requests this app makes.
///
/// The surface id is allocated here rather than by the runtime, so it can be
/// stored in widget state before the surface exists, which every menu in this
/// app relies on, since it must recognise its own popup's `Closed` event.
pub mod exwl {
    use super::{
        Action, ExwlShellCustomAction, ExwlShellCustomActionWithId, IcedNewPopupSettings,
        IcedXdgWindowSettings, window,
    };

    /// Open the application's base `xdg_toplevel`.
    #[must_use]
    pub fn base_window<M>(id: window::Id, settings: IcedXdgWindowSettings) -> Action<M> {
        Action::Exwl(ExwlShellCustomActionWithId::new(
            None,
            ExwlShellCustomAction::NewBaseWindow { settings, id },
        ))
    }

    /// Open a popup on `settings.parent`.
    #[must_use]
    pub fn popup<M>(id: window::Id, settings: IcedNewPopupSettings) -> Action<M> {
        Action::Exwl(ExwlShellCustomActionWithId::new(
            None,
            ExwlShellCustomAction::NewPopUp { settings, id },
        ))
    }

    /// Move an already-mapped popup.
    ///
    /// `settings.parent` is ignored by the protocol: xdg forbids reparenting a
    /// mapped popup.
    ///
    /// Note that this must never be driven by a click on the parent surface.
    /// While a popup holds the pointer grab the compositor swallows such a
    /// click and dismisses the popup first, so the reposition would arrive for
    /// a surface that no longer exists. Drive it from inside the popup, or from
    /// state that is not pointer-derived.
    #[must_use]
    pub fn reposition_popup<M>(id: window::Id, settings: IcedNewPopupSettings) -> Action<M> {
        Action::Exwl(ExwlShellCustomActionWithId::new(
            Some(id),
            ExwlShellCustomAction::PopUpReposition { settings },
        ))
    }

    /// Destroy a surface.
    #[must_use]
    pub fn remove_window<M>(id: window::Id) -> Action<M> {
        Action::Exwl(ExwlShellCustomActionWithId::new(
            Some(id),
            ExwlShellCustomAction::RemoveWindow,
        ))
    }
}
