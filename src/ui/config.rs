// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Toolkit preferences this app's chrome reads.
//!
//! These were `cosmic::config::show_maximize()` / `show_minimize()`, backed by
//! `COSMIC_TK`, a `LazyLock` reading cosmic-config on first access, kept live
//! by the `CosmicTk` watcher in the shell's subscription. Both are gone with
//! libcosmic, and the watcher with them.
//!
//! They stay as functions rather than becoming a literal `true` at the two call
//! sites, so that when this app grows a config of its own they have somewhere
//! to move to. Both defaulted to `true` upstream.
//!
//! Note the buttons they gate are currently inert under `iced_exwlshell`; see
//! [`crate::ui::command`].

/// Whether the header bar should offer a maximize button.
#[must_use]
pub const fn show_maximize() -> bool {
    true
}

/// Whether the header bar should offer a minimize button.
#[must_use]
pub const fn show_minimize() -> bool {
    true
}
