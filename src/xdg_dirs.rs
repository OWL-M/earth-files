// SPDX-License-Identifier: GPL-3.0-only

//! The XDG base directories, read from the environment once.

use std::sync::LazyLock;

use xdg_kit::basedir::BaseDirs;

/// `None` when neither `$HOME` nor the password database gives a home
/// folder, which is the one thing the base directories cannot do without.
pub static BASE_DIRS: LazyLock<Option<BaseDirs>> = LazyLock::new(|| {
    BaseDirs::from_env()
        .inspect_err(|err| log::warn!("no XDG base directories: {err}"))
        .ok()
});
