// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/icon_theme.rs
//!
//! Select the preferred icon theme.
//!
//! The theme is resolved once at startup from the first source that answers:
//!
//! 1. this app's own `Config::icon_theme`, when the user has set it;
//! 2. the XDG settings portal, `org.freedesktop.portal.Settings`, key
//!    `icon-theme` of `org.gnome.desktop.interface`, what GTK, Qt and every
//!    sandboxed app ask, and what a COSMIC session answers too;
//! 3. `icon-theme` as `gsettings` reports it, the one step that only answers
//!    with an installed theme;
//! 4. `gtk-icon-theme-name` from `gtk-4.0/settings.ini`, else
//!    `gtk-3.0/settings.ini`, for a session with no portal running;
//! 5. [`HICOLOR`], the freedesktop fallback theme every icon set is required to
//!    inherit from, so the worst case is the same icons GTK would show with no
//!    theme configured, not blank rows.
//!
//! It stays a process global because the only caller,
//! [`crate::ui::widget::icon::Named::path`], is on the draw path and takes no
//! arguments. Resolution happens once, in [`set_from_config`], because step 2
//! is a D-Bus round trip that costs the full [`PORTAL_TIMEOUT`] on a machine
//! with no portal. It is const-initialised to [`HICOLOR`], so a read before
//! that yields the final fallback rather than panicking.

use std::borrow::Cow;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use crate::config::Config;

/// The COSMIC desktop's icon theme, searched after the resolved theme as the
/// secondary theme of the lookup chain in
/// [`crate::ui::widget::icon::Named::path`].
pub const COSMIC: &str = "Cosmic";

/// The freedesktop fallback theme, and ours when nothing else answers.
pub const HICOLOR: &str = "hicolor";

/// How long the portal gets to answer. Same budget, and the same reasoning, as
/// `ui::theme`'s colour-scheme read: long enough for a local D-Bus round trip,
/// short enough to pass as startup jitter where no portal is running.
const PORTAL_TIMEOUT: Duration = Duration::from_millis(300);

static DEFAULT: Mutex<Cow<'static, str>> = Mutex::new(Cow::Borrowed(HICOLOR));

/// The fallback icon theme to search if no icon theme was specified.
#[must_use]
pub fn default() -> String {
    DEFAULT.lock().unwrap().to_string()
}

/// Set the fallback icon theme to search when loading system icons.
#[cold]
pub fn set_default(name: impl Into<Cow<'static, str>>) {
    *DEFAULT.lock().unwrap() = name.into();
}

/// Resolve the icon theme and point icon lookup at it.
///
/// Called where the app loads its config, before any window is built. Makes at
/// most one portal call for the life of the process.
pub fn set_from_config(config: &Config) {
    set_default(resolve(config.icon_theme.as_deref()));
}

/// The chain described in the module docs. `configured` is
/// `Config::icon_theme`; the later steps run only if it is unset.
fn resolve(configured: Option<&str>) -> String {
    configured
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .or_else(portal_icon_theme)
        .or_else(gsettings_icon_theme)
        .or_else(gtk_icon_theme)
        .unwrap_or_else(|| HICOLOR.to_owned())
}

/// `icon-theme` as `gsettings` reports it, via `freedesktop_icons`.
///
/// Sits between the portal and `settings.ini` because it is the only step that
/// validates: it returns a name only when that theme is actually installed,
/// having found it in the crate's own theme index. The other steps report
/// whatever the setting says, installed or not.
///
/// It shells out to `gsettings`, so it yields `None` wherever that is absent
/// (this machine among them) or the `org.gnome.desktop.interface` schema is not
/// installed, which is why it is a step in the chain rather than the whole of
/// it.
///
/// Note it returns the theme's `Name=` from `index.theme`, not its directory
/// name, "Gruvbox Plus Dark" rather than "Gruvbox-Plus-Dark". That is safe to
/// pass to `lookup(..).with_theme(..)`: the crate retries with spaces replaced
/// by hyphens before falling back to hicolor.
fn gsettings_icon_theme() -> Option<String> {
    freedesktop_icons::default_theme_gtk()
}

/// `icon-theme` as the XDG settings portal reports it.
///
/// The threading is `ui::theme::detect_system_prefers_dark`'s, for the reasons
/// given there: this can be called from inside the app's tokio executor, where
/// building a runtime panics, and zbus wants a live runtime to drop its D-Bus
/// stream on. A thread of its own, holding the runtime it drives, satisfies
/// both, and `_guard` is declared after `runtime` so it is released first.
fn portal_icon_theme() -> Option<String> {
    std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        let _guard = runtime.enter();
        // Everything the call allocates, the connection included, is dropped
        // inside `block_on`, while the runtime is still there to take it.
        runtime.block_on(async {
            tokio::time::timeout(PORTAL_TIMEOUT, read_portal_icon_theme())
                .await
                .ok()
                .flatten()
        })
    })
    .join()
    .ok()
    .flatten()
}

async fn read_portal_icon_theme() -> Option<String> {
    let connection = zbus::Connection::session().await.ok()?;
    let reply = connection
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Settings"),
            "ReadOne",
            &("org.gnome.desktop.interface", "icon-theme"),
        )
        .await
        .ok()?;
    // `ReadOne` replies with a bare variant. (`Read`, its deprecated
    // predecessor, nests it one deeper; portals old enough to have only that
    // are left to fall through to the gtk settings below.)
    let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    String::try_from(value).ok().filter(|name| !name.is_empty())
}

/// `gtk-icon-theme-name` from the GTK settings of the current user, preferring
/// GTK 4's file over GTK 3's.
fn gtk_icon_theme() -> Option<String> {
    // `dirs::config_dir` honours `XDG_CONFIG_HOME`, so this follows the same
    // config root `Config::load` does.
    let config_dir = dirs::config_dir()?;
    ["gtk-4.0", "gtk-3.0"]
        .into_iter()
        .find_map(|dir| gtk_icon_theme_in(&config_dir.join(dir).join("settings.ini")))
}

/// `gtk-icon-theme-name` in one `settings.ini`.
///
/// A key=value scan, not an ini parse: the file has a single `[Settings]`
/// section, and a commented-out line keeps the `#` in its key and so is
/// skipped.
fn gtk_icon_theme_in(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .filter_map(|line| line.split_once('='))
        .find(|(key, _)| key.trim() == "gtk-icon-theme-name")
        .map(|(_, value)| value.trim().to_owned())
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_applies_before_initialisation() {
        // Whatever order the tests in this binary run in, the theme is never
        // unset: reading it yields the fallback the resolver ends on.
        assert!(!default().is_empty());
    }

    #[test]
    fn configured_theme_wins() {
        // Set in our own config, so no portal call and no file read happen.
        assert_eq!(resolve(Some("Gruvbox-Plus-Dark")), "Gruvbox-Plus-Dark");
    }

    #[test]
    fn blank_configured_theme_is_treated_as_unset() {
        // An empty or whitespace name must not become the icon theme; it falls
        // through to the desktop, and in the worst case to `hicolor`.
        assert_ne!(resolve(Some("   ")), "   ");
        assert!(!resolve(Some("")).is_empty());
    }

    #[test]
    fn config_default_is_unset() {
        assert_eq!(Config::default().icon_theme, None);
    }

    #[test]
    fn gtk_settings_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.ini");

        // Nothing there at all.
        assert_eq!(gtk_icon_theme_in(&path), None);

        std::fs::write(
            &path,
            "[Settings]\n\
             #gtk-icon-theme-name=Commented-Out\n\
             gtk-cursor-theme-name=Some Cursors\n\
             gtk-icon-theme-name = Gruvbox-Plus-Dark \n",
        )
        .unwrap();
        assert_eq!(
            gtk_icon_theme_in(&path).as_deref(),
            Some("Gruvbox-Plus-Dark")
        );

        // A key with no value is not a theme name.
        std::fs::write(&path, "[Settings]\ngtk-icon-theme-name=\n").unwrap();
        assert_eq!(gtk_icon_theme_in(&path), None);
    }
}
