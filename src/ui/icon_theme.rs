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
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use rustc_hash::FxHashMap;
use xdg_kit::icon_theme::{Formats, Icon, IconFormat, IconRequest, IconThemes};

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

/// Each icon theme looked up so far, with what it inherits, by name: indexed
/// once, on its first lookup, since a lookup touches no files.
static LOADED: LazyLock<Mutex<FxHashMap<String, Arc<IconThemes>>>> = LazyLock::new(Mutex::default);

/// The icon theme `theme` with what it inherits, indexed on first use.
fn loaded(theme: &str) -> Option<Arc<IconThemes>> {
    let base_dirs = crate::xdg_dirs::BASE_DIRS.as_ref()?;
    let mut loaded = LOADED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Some(Arc::clone(loaded.entry(theme.to_owned()).or_insert_with(
        || {
            let themes = IconThemes::load(base_dirs, theme);
            for diagnostic in themes.diagnostics() {
                log::debug!("icon theme {theme}: {diagnostic:?}");
            }
            Arc::new(themes)
        },
    )))
}

/// The file of icon `name` in `theme`, or a theme it inherits, closest to
/// `size` at `scale`, as the Icon Theme spec finds it.
///
/// With `prefer_svg`, an SVG of the same theme is taken over a raster file:
/// one that fits the size exactly, or any one when the raster file does not
/// fit it either. So a scalable SVG beats a 16-pixel PNG, which matters for
/// symbolic icons, recoloured only as SVG; but an SVG in a theme further up
/// the inheritance does not beat the theme's own PNG.
pub fn lookup_in(
    theme: &str,
    name: &str,
    size: u16,
    scale: u16,
    prefer_svg: bool,
) -> Option<PathBuf> {
    find(&*loaded(theme)?, name, size, scale, prefer_svg)
}

/// [`lookup_in`] in `themes`.
fn find(
    themes: &IconThemes,
    name: &str,
    size: u16,
    scale: u16,
    prefer_svg: bool,
) -> Option<PathBuf> {
    let (size, scale) = (u32::from(size), u32::from(scale));
    let request = IconRequest {
        size,
        scale,
        formats: Formats::default(),
    };
    let icon = themes.lookup(name, &request)?;
    if !prefer_svg || icon.format() == IconFormat::Svg {
        return Some(icon.path().to_path_buf());
    }
    let svg_only = IconRequest {
        formats: Formats {
            png: false,
            svg: true,
            xpm: false,
        },
        ..request
    };
    let exact = |found: &Icon| found.sizing().is_some_and(|rule| rule.matches(size, scale));
    match themes.lookup(name, &svg_only) {
        Some(svg) if svg.theme() == icon.theme() && (exact(&svg) || !exact(&icon)) => {
            Some(svg.path().to_path_buf())
        }
        _ => Some(icon.path().to_path_buf()),
    }
}

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

/// Indexes the resolved theme and [`COSMIC`] on a thread of their own.
///
/// Indexing a theme takes a while, and the first icon drawn would otherwise
/// wait for it on the event loop; started early, it is mostly done by then.
/// A lookup that comes first waits for it, as it would have anyway. It
/// starts a thread, so it runs after the process has detached from its
/// terminal, which needs the process to have only the one.
pub fn warm() {
    let theme = default();
    let _ = std::thread::Builder::new()
        .name("icon-themes".to_owned())
        .spawn(move || {
            loaded(&theme);
            loaded(COSMIC);
        });
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

/// `icon-theme` as `gsettings` reports it.
///
/// Sits between the portal and `settings.ini` because it is the only step that
/// validates: it returns a name only when a theme by that folder name is
/// installed. The other steps report whatever the setting says, installed or
/// not.
///
/// It shells out to `gsettings`, so it yields `None` wherever that is absent
/// (this machine among them) or the `org.gnome.desktop.interface` schema is not
/// installed, which is why it is a step in the chain rather than the whole of
/// it.
fn gsettings_icon_theme() -> Option<String> {
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8(output.stdout).ok()?;
    let name = name.trim().trim_matches('\'');
    let base_dirs = crate::xdg_dirs::BASE_DIRS.as_ref()?;
    xdg_kit::icon_theme::themes(base_dirs, &xdg_kit::locale::Locales::untranslated())
        .iter()
        .any(|theme| theme.id == name)
        .then(|| name.to_owned())
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
    let value: zbus::OwnedVariant = reply.body().deserialize().ok()?;
    String::try_from(value).ok().filter(|name| !name.is_empty())
}

/// `gtk-icon-theme-name` from the GTK settings of the current user, preferring
/// GTK 4's file over GTK 3's.
fn gtk_icon_theme() -> Option<String> {
    // The same config root `Config::load` uses, `XDG_CONFIG_HOME` included
    let config_dir = crate::xdg_dirs::config_home()?;
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

    /// Writes the theme `name`, inheriting from `inherits`, holding `files`
    /// (relative to the theme), into the icon folder `icons`. A folder
    /// starting with `scalable` takes any size; every other is 16 pixels.
    fn write_theme(icons: &Path, name: &str, inherits: &str, files: &[&str]) {
        let theme = icons.join(name);
        let mut dirs: Vec<&str> = files
            .iter()
            .filter_map(|file| file.rsplit_once('/').map(|(dir, _)| dir))
            .collect();
        dirs.dedup();
        let mut index = format!(
            "[Icon Theme]\nName={name}\nInherits={inherits}\nDirectories={}\n",
            dirs.join(",")
        );
        for dir in &dirs {
            if dir.starts_with("scalable") {
                index.push_str(&format!(
                    "\n[{dir}]\nSize=16\nMinSize=8\nMaxSize=512\nType=Scalable\n"
                ));
            } else {
                index.push_str(&format!("\n[{dir}]\nSize=16\nType=Fixed\n"));
            }
        }
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(theme.join("index.theme"), index).unwrap();
        for file in files {
            let path = theme.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
    }

    /// A home with the theme `Test`, inheriting from `Parent`, which holds
    /// `parent_files`; `Test` holds `files`.
    fn themes_with(files: &[&str], parent_files: &[&str]) -> (tempfile::TempDir, IconThemes) {
        let home = tempfile::tempdir().unwrap();
        let icons = home.path().join("data/icons");
        write_theme(&icons, "Test", "Parent", files);
        write_theme(&icons, "Parent", "hicolor", parent_files);
        let base_dirs = xdg_kit::basedir::BaseDirs::from_vars(|name| {
            let path = match name {
                "HOME" => home.path().to_owned(),
                "XDG_DATA_HOME" => home.path().join("data"),
                "XDG_DATA_DIRS" => home.path().join("none"),
                _ => return None,
            };
            Some(path.into())
        })
        .unwrap();
        let themes = IconThemes::load(&base_dirs, "Test");
        (home, themes)
    }

    /// [`themes_with`] with an empty parent.
    fn theme_with(files: &[&str]) -> (tempfile::TempDir, IconThemes) {
        themes_with(files, &[])
    }

    #[test]
    fn an_icon_is_found_in_its_theme() {
        let (home, themes) = theme_with(&["16x16/apps/foo.png"]);
        assert_eq!(
            find(&themes, "foo", 16, 1, false),
            Some(home.path().join("data/icons/Test/16x16/apps/foo.png"))
        );
        assert_eq!(find(&themes, "bar", 16, 1, false), None);
    }

    /// Within one folder PNG wins, as the spec orders them, unless an SVG
    /// is preferred: then the SVG beside it is taken.
    #[test]
    fn an_svg_beside_the_png_is_taken_when_preferred() {
        let (home, themes) = theme_with(&["16x16/apps/foo.png", "16x16/apps/foo.svg"]);
        let dir = home.path().join("data/icons/Test/16x16/apps");
        assert_eq!(
            find(&themes, "foo", 16, 1, false),
            Some(dir.join("foo.png"))
        );
        assert_eq!(find(&themes, "foo", 16, 1, true), Some(dir.join("foo.svg")));
    }

    /// A scalable SVG fits any size, so it is preferred over a PNG of the
    /// exact size in another folder.
    #[test]
    fn a_scalable_svg_is_preferred_over_a_sized_png_elsewhere() {
        let (home, themes) = theme_with(&[
            "16x16/actions/foo-symbolic.png",
            "scalable/actions/foo-symbolic.svg",
        ]);
        let theme = home.path().join("data/icons/Test");
        assert_eq!(
            find(&themes, "foo-symbolic", 16, 1, true),
            Some(theme.join("scalable/actions/foo-symbolic.svg"))
        );
        assert_eq!(
            find(&themes, "foo-symbolic", 16, 1, false),
            Some(theme.join("16x16/actions/foo-symbolic.png"))
        );
    }

    /// An SVG further up the inheritance does not beat the theme's own PNG.
    #[test]
    fn the_themes_own_png_beats_an_inherited_svg() {
        let (home, themes) = themes_with(&["16x16/apps/foo.png"], &["scalable/apps/foo.svg"]);
        assert_eq!(
            find(&themes, "foo", 16, 1, true),
            Some(home.path().join("data/icons/Test/16x16/apps/foo.png"))
        );
    }

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
