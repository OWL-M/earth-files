// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/keyboard_nav.rs
//!
//! Subscribe to common application keyboard shortcuts.

use iced::event::listen_raw;
use iced::{Event, Subscription, event, keyboard};
use iced_core::keyboard::key::Named;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Escape,
    FocusNext,
    FocusPrevious,
    Fullscreen,
    Search,
}

/// Common keyboard shortcuts, tagged with the window they came from.
///
/// The id matters because a nested shell, such as the embedded file chooser,
/// subscribes alongside its host: without it one Escape or Tab would be acted
/// on by both windows.
#[cold]
pub fn subscription() -> Subscription<(iced_core::window::Id, Action)> {
    listen_raw(|event, status, window_id| {
        if event::Status::Ignored != status {
            return None;
        }

        match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(key),
                modifiers,
                ..
            }) => match key {
                Named::Tab if !modifiers.control() => {
                    return Some((
                        window_id,
                        if modifiers.shift() {
                            Action::FocusPrevious
                        } else {
                            Action::FocusNext
                        },
                    ));
                }

                Named::Escape => {
                    return Some((window_id, Action::Escape));
                }

                Named::F11 => {
                    return Some((window_id, Action::Fullscreen));
                }

                _ => (),
            },
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Character(c),
                modifiers,
                ..
            }) if c == "f" && modifiers.control() => {
                return Some((window_id, Action::Search));
            }

            _ => (),
        }

        None
    })
}
