// SPDX-License-Identifier: GPL-3.0-only

//! The XDG base directories, read from the environment once, and the user
//! folders (Documents, Downloads, …) of `user-dirs.dirs`.

use std::path::PathBuf;
use std::sync::LazyLock;

use xdg_kit::basedir::{BaseDirs, UserDirKind, UserDirs};

/// `None` when neither `$HOME` nor the password database gives a home
/// folder, which is the one thing the base directories cannot do without.
pub static BASE_DIRS: LazyLock<Option<BaseDirs>> = LazyLock::new(|| {
    BaseDirs::from_env()
        .inspect_err(|err| log::warn!("no XDG base directories: {err}"))
        .ok()
});

/// The home folder.
pub fn home() -> Option<PathBuf> {
    BASE_DIRS.as_ref().map(|dirs| dirs.home().to_owned())
}

/// Where configuration is written: `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_home() -> Option<PathBuf> {
    BASE_DIRS.as_ref().map(|dirs| dirs.config_home().to_owned())
}

/// Where data is written: `$XDG_DATA_HOME`, else `~/.local/share`.
pub fn data_home() -> Option<PathBuf> {
    BASE_DIRS.as_ref().map(|dirs| dirs.data_home().to_owned())
}

/// The user folders, read from `user-dirs.dirs` now, so an edit made while
/// the app runs is seen.
pub fn user_dirs() -> Option<UserDirs> {
    BASE_DIRS.as_ref().map(UserDirs::load)
}

/// The user folder `kind`; `None` when it is not set, or set to the home
/// folder itself, which xdg-user-dirs takes to mean disabled.
pub fn user_dir(kind: &UserDirKind) -> Option<PathBuf> {
    user_dirs()?.get(kind).map(ToOwned::to_owned)
}
