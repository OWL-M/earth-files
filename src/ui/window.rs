// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! The two window-id sentinels libcosmic's iced fork had as associated consts.
//!
//! The fork declared `Id::NONE = Id(0)` and `Id::RESERVED = Id(1)`, and taught
//! `Id::unique()` to skip both (`iced/core/src/window/id.rs:17-27`). Upstream
//! `iced_core 0.14` has neither, and its `unique()` starts at `1`, so the
//! first id it hands out is the fork's `RESERVED`. The field is private, so
//! the values cannot be restated; they have to be allocated.
//!
//! Allocating them from `unique()` gives the property both were for: an id no
//! real surface will ever be given. `LazyLock` makes the first touch anywhere
//! win, so it does not matter whether the shell or a widget asks first.
//!
//! [`reserved`] is additionally the id the main window is opened with, which is
//! why it must be a real, usable id rather than a niche value.

use crate::ui::iced::window::Id;
use std::sync::LazyLock;

static NONE: LazyLock<Id> = LazyLock::new(Id::unique);
static RESERVED: LazyLock<Id> = LazyLock::new(Id::unique);

/// An id no window will ever match. Was the fork's `window::Id::NONE`.
#[must_use]
pub fn none() -> Id {
    *NONE
}

/// The id the main window is opened with. Was the fork's `window::Id::RESERVED`.
#[must_use]
pub fn reserved() -> Id {
    *RESERVED
}
