// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Two window-id sentinels: an id no surface is ever given, and the id the
//! main window is opened with.
//!
//! `iced_core 0.14`'s `window::Id` has no such consts, and its field is
//! private, so the values cannot be restated; they have to be allocated from
//! `unique()`. `LazyLock` makes the first touch anywhere win, so it does not
//! matter whether the shell or a widget asks first.
//!
//! [`reserved`] is additionally the id the main window is opened with, which is
//! why it must be a real, usable id rather than a niche value.

use crate::ui::iced::window::Id;
use std::sync::LazyLock;

static NONE: LazyLock<Id> = LazyLock::new(Id::unique);
static RESERVED: LazyLock<Id> = LazyLock::new(Id::unique);

/// An id no window will ever match.
#[must_use]
pub fn none() -> Id {
    *NONE
}

/// The id the main window is opened with.
#[must_use]
pub fn reserved() -> Id {
    *RESERVED
}
