// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Surface requests a widget can publish for the shell to carry out.
//!
//! [`Action`] has exactly three variants, `Popup`, `DestroyPopup` and
//! `ResponsiveMenuBar`. Every popup builder captures its own content and takes
//! no `&App`, so nothing here needs `Any` or a downcast: the settings are a
//! concrete struct and the view is a concrete closure type.
//!
//! [`Positioner`] converts to exwlshell's [`IcedNewPopupSettings`] in one
//! place, [`PopupSettings::to_exwlshell`]. It has no `offset`, which
//! `xdg_positioner.set_offset` would carry but exwlshell never sends, and no
//! `reactive` flag, which exwlshell sets unconditionally for positioner
//! version 3 and up (`exwlshellev/src/lib.rs:3825`).

use crate::ui::Element;
use crate::ui::iced::{self, Rectangle, Size};
use iced_core::layout::Limits;
use iced_core::window;
use std::sync::Arc;

use iced_exwlshell::actions::IcedNewPopupSettings;
use iced_exwlshell::reexport::{PixelSize, PopupPlacement};
pub use iced_exwlshell::reexport::{PopupAnchor, PopupConstraintAdjustment, PopupGravity};
use iced_texture_cache::Corner;

/// Produces the content of a surface created from within a widget.
///
/// Typed on the message the widget publishes.
pub type View<M> = Arc<dyn Fn() -> Element<'static, crate::ui::Action<M>> + Send + Sync + 'static>;

/// Builds the settings for a popup, at the moment the shell acts on the request.
pub type Settings = Arc<dyn Fn() -> PopupSettings + Send + Sync + 'static>;

/// Where a popup sits relative to its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Positioner {
    /// The popup's own size. `None` means "one pixel", which no caller wants;
    /// every site sets it from a measured layout.
    pub size: Option<(u32, u32)>,
    /// The rectangle in the parent surface the popup is anchored to.
    pub anchor_rect: Rectangle<i32>,
    /// Which point of [`Self::anchor_rect`] the popup hangs from.
    pub anchor: PopupAnchor,
    /// Which way the popup grows from that point.
    pub gravity: PopupGravity,
    /// How the compositor may flip/slide/resize the popup to keep it on screen.
    pub constraint_adjustment: PopupConstraintAdjustment,
}

impl Default for Positioner {
    fn default() -> Self {
        Self {
            size: None,
            anchor_rect: Rectangle {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            anchor: PopupAnchor::None,
            gravity: PopupGravity::BottomRight,
            constraint_adjustment: PopupConstraintAdjustment::FlipX
                | PopupConstraintAdjustment::FlipY
                | PopupConstraintAdjustment::SlideX
                | PopupConstraintAdjustment::SlideY,
        }
    }
}

/// `PixelSize::px` panics on a zero axis, and both the popup size and the
/// anchor rect are computed from layout, which can legitimately produce one.
fn pixel_size(width: i32, height: i32) -> PixelSize {
    PixelSize::px(width.max(1) as u32, height.max(1) as u32)
}

/// Everything needed to open one popup surface.
///
/// exwlshell takes a grab for every popup it opens, so that is not a choice
/// here. It also has no corner-radius request, so a popup's outer edge is
/// square and rounding can only be drawn client-side inside the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PopupSettings {
    /// The id the popup surface will be given.
    pub id: window::Id,
    /// The surface the popup is a child of.
    pub parent: window::Id,
    /// Where it sits relative to that parent.
    pub positioner: Positioner,
    /// Whether this popup collapses into its anchor as it opens and closes.
    ///
    /// The shell cannot tell which widget built a popup, and only a context
    /// menu wants the genie: a dropdown being drawn into a corner is a
    /// different design question. Not forwarded by
    /// [`Self::to_exwlshell`] — it never reaches Wayland.
    pub animate: bool,
}

impl PopupSettings {
    /// The exwlshell request this describes.
    #[must_use]
    pub fn to_exwlshell(&self) -> IcedNewPopupSettings {
        let (width, height) = self.positioner.size.unwrap_or((1, 1));
        let rect = self.positioner.anchor_rect;

        IcedNewPopupSettings {
            size: pixel_size(width as i32, height as i32),
            parent: Some(self.parent),
            placement: PopupPlacement::Anchored {
                position: (rect.x, rect.y),
                size: pixel_size(rect.width, rect.height),
            },
            anchor: self.positioner.anchor,
            gravity: self.positioner.gravity,
            constraint_adjustment: self.positioner.constraint_adjustment,
        }
    }
}

/// Ignore this message in your application. It will be intercepted by the shell.
#[derive(Clone)]
pub enum Action<M> {
    /// Open a popup, rendering the given view into it.
    Popup(Settings, Option<View<M>>),
    /// Destroy a popup opened earlier.
    DestroyPopup {
        /// The popup surface to tear down.
        id: window::Id,
        /// Whether it may play an exit animation first.
        ///
        /// Only a popup that nothing is replacing may: the menu's widget
        /// tree is shared, and a popup that outlives its request while
        /// another takes its place leaves the two needing different trees.
        animate: bool,
    },
    /// Responsive menu bar measurement, published by `responsive_container`.
    ResponsiveMenuBar {
        /// Id of the menu bar.
        menu_bar: crate::ui::widget::Id,
        /// Limits the menu bar was measured under.
        limits: Limits,
        /// Size the expanded bar asked for.
        size: Size,
    },
}

impl<M: 'static> Action<M> {
    /// Re-type the action for a component whose messages are wrapped by `f`.
    ///
    /// A component that maps a widget's messages must map its surface actions
    /// too, or the popup it opens publishes the wrong message type.
    #[must_use]
    pub fn map<N: 'static>(self, f: impl Fn(M) -> N + Clone + Send + Sync + 'static) -> Action<N> {
        self.map_actions(move |action| action.map(f.clone()))
    }

    fn map_actions<N: 'static>(
        self,
        g: impl Fn(crate::ui::Action<M>) -> crate::ui::Action<N> + Clone + Send + Sync + 'static,
    ) -> Action<N> {
        match self {
            Action::Popup(settings, view) => Action::Popup(
                settings,
                view.map(|view| Arc::new(move || view().map(g.clone())) as View<N>),
            ),
            Action::DestroyPopup { id, animate } => Action::DestroyPopup { id, animate },
            Action::ResponsiveMenuBar {
                menu_bar,
                limits,
                size,
            } => Action::ResponsiveMenuBar {
                menu_bar,
                limits,
                size,
            },
        }
    }
}

impl<M: 'static> Action<crate::ui::Action<M>> {
    /// Collapse a doubly wrapped action into a single one.
    #[must_use]
    pub fn flatten(self) -> Action<M> {
        self.map_actions(crate::ui::Action::flatten)
    }
}

impl<M> std::fmt::Debug for Action<M> {
    #[cold]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Popup(_, view) => f
                .debug_tuple("Popup")
                .field(&view.as_ref().map(|_| "view"))
                .finish(),
            Self::DestroyPopup { id, animate } => f
                .debug_struct("DestroyPopup")
                .field("id", id)
                .field("animate", animate)
                .finish(),
            Self::ResponsiveMenuBar {
                menu_bar,
                limits,
                size,
            } => f
                .debug_struct("ResponsiveMenuBar")
                .field("menu_bar", menu_bar)
                .field("limits", limits)
                .field("size", size)
                .finish(),
        }
    }
}

/// Wrap a surface action into a task the shell will handle.
pub fn surface_task<M: Send + 'static>(action: Action<M>) -> iced::Task<crate::ui::Action<M>> {
    iced::Task::done(crate::ui::Action::Surface(action))
}

/// The corner a popup with this gravity collapses back into.
///
/// Gravity says which way a popup grows from its anchor point, so the
/// opposite corner is the anchor itself: a `BottomRight` popup hangs down
/// and to the right, so the pointer is at its top-left.
///
/// `Gravity` is generated from the xdg-shell protocol and has edge and
/// `None` variants as well as the four corners. No animated popup asks for
/// one, and `TopLeft` is the sane answer if one ever does: it is the corner
/// for the `BottomRight` gravity that every default uses.
#[must_use]
pub(crate) fn collapse_corner(gravity: PopupGravity) -> Corner {
    match gravity {
        PopupGravity::BottomRight => Corner::TopLeft,
        PopupGravity::BottomLeft => Corner::TopRight,
        PopupGravity::TopRight => Corner::BottomLeft,
        PopupGravity::TopLeft => Corner::BottomRight,
        _ => Corner::TopLeft,
    }
}

pub mod action {
    use super::{Action, PopupSettings, View};
    use crate::ui::Element;
    use iced_core::window;
    use std::sync::Arc;

    /// Used to produce a destroy-popup action from within a widget.
    #[must_use]
    pub fn destroy_popup<M>(id: window::Id) -> Action<M> {
        Action::DestroyPopup { id, animate: false }
    }

    /// A destroy that may play an exit animation first.
    ///
    /// Only for a popup that nothing is about to replace. A popup that
    /// animates outlives the request to destroy it, and two popups alive at
    /// once cannot share the one menu tree they both need.
    #[must_use]
    pub fn destroy_popup_animated<M>(id: window::Id) -> Action<M> {
        Action::DestroyPopup { id, animate: true }
    }

    /// Used to create a popup action from within a widget.
    #[must_use]
    pub fn simple_popup<M: 'static>(
        settings: impl Fn() -> PopupSettings + Send + Sync + 'static,
        view: Option<impl Fn() -> Element<'static, crate::ui::Action<M>> + Send + Sync + 'static>,
    ) -> Action<M> {
        Action::Popup(
            Arc::new(settings),
            view.map(|view| Arc::new(view) as View<M>),
        )
    }
}

/// Which popups the compositor has torn down.
///
/// iced's `window::Event::Closed` carries no id: it is delivered to the closed
/// window, so a widget running in the parent surface cannot see its own popup
/// die. The id has to get from the shell, which does see it
/// (`app::Action::SurfaceClosed`), to the widget, which needs it.
///
/// A thread-local does that. `update` for every widget and the shell's own
/// `update` run on the same thread in the same loop;
/// `widget::text_context_menu`'s popup queues already depend on exactly this.
///
/// Without it, a menu whose popup the compositor dismissed (a click outside,
/// the usual way menus close) would keep a stale `popup_id`, and the next click
/// on the menu would be spent tearing down a surface that no longer exists
/// instead of opening a new one.
pub(crate) mod dismissal {
    use iced_core::window;
    use std::cell::RefCell;

    /// An unclaimed id costs one `window::Id`; a widget that has gone away
    /// entirely never claims its own. Bounded so that cannot grow without end.
    const MAX_UNCLAIMED: usize = 64;

    thread_local! {
        static DISMISSED: RefCell<Vec<window::Id>> = const { RefCell::new(Vec::new()) };
    }

    /// The shell records a surface the compositor has closed.
    pub(crate) fn note(id: window::Id) {
        DISMISSED.with(|d| {
            let mut d = d.borrow_mut();
            if d.len() >= MAX_UNCLAIMED {
                d.remove(0);
            }
            if !d.contains(&id) {
                d.push(id);
            }
        });
    }

    /// A widget claims one of its own popups, if it was dismissed. Reports
    /// `true` once per dismissal.
    pub(crate) fn claim(id: window::Id) -> bool {
        DISMISSED.with(|d| {
            let mut d = d.borrow_mut();
            if let Some(i) = d.iter().position(|v| *v == id) {
                d.remove(i);
                true
            } else {
                false
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_popup_collapses_toward_the_corner_its_gravity_grew_from() {
        // A popup with `BottomRight` gravity extends down and to the right
        // of its anchor point, so that point is its top-left corner, and
        // that is where it has to collapse back into.
        assert_eq!(collapse_corner(PopupGravity::BottomRight), Corner::TopLeft);
        assert_eq!(collapse_corner(PopupGravity::BottomLeft), Corner::TopRight);
        assert_eq!(collapse_corner(PopupGravity::TopRight), Corner::BottomLeft);
        assert_eq!(collapse_corner(PopupGravity::TopLeft), Corner::BottomRight);
    }

    #[test]
    fn the_four_corners_are_distinct() {
        let corners = [
            collapse_corner(PopupGravity::BottomRight),
            collapse_corner(PopupGravity::BottomLeft),
            collapse_corner(PopupGravity::TopRight),
            collapse_corner(PopupGravity::TopLeft),
        ];

        for (i, a) in corners.iter().enumerate() {
            for b in &corners[i + 1..] {
                assert_ne!(a, b, "two gravities map to {a:?}");
            }
        }
    }

    #[test]
    fn a_gravity_without_a_corner_falls_back_to_the_usual_one() {
        // `Gravity` is generated from the xdg-shell protocol and carries
        // edge and `None` variants a context menu never asks for.
        assert_eq!(collapse_corner(PopupGravity::None), Corner::TopLeft);
    }
}
