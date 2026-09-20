// SPDX-License-Identifier: GPL-3.0-only

use std::num::NonZeroU16;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::Action;

use crate::FxOrderMap;
use crate::tab::{HeadingOptions, Location, View};
use crate::ui::theme::{self, Density};

pub use crate::context_action::{ContextActionPreset, ContextActionSelection};
pub use store::Store;

pub mod store;

// Default icon sizes
pub const ICON_SIZE_LIST: u16 = 32;
pub const ICON_SIZE_LIST_CONDENSED: u16 = 48;
pub const ICON_SIZE_GRID: u16 = 64;
pub const ICON_SCALE_MAX: u16 = 5;

// Default font families.
pub const INTERFACE_FONT_DEFAULT: &str = "Open Sans";
pub const MONOSPACE_FONT_DEFAULT: &str = "Noto Sans Mono";

macro_rules! percent {
    ($perc:expr, $pixel:ident) => {
        (($perc.get() as f32 * $pixel as f32) / 100.).clamp(1., ($pixel * ICON_SCALE_MAX) as _)
    };
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AppTheme {
    Dark,
    Light,
    System,
}

impl AppTheme {
    /// The theme to render with.
    pub fn theme(&self) -> theme::Theme {
        let dark = match self {
            Self::Dark => true,
            Self::Light => false,
            Self::System => crate::ui::theme::system_prefers_dark(),
        };
        if dark {
            theme::Theme::dark()
        } else {
            theme::Theme::light()
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Favorite {
    Home,
    Documents,
    Downloads,
    Music,
    Pictures,
    Videos,
    Path(PathBuf),
    Network {
        uri: String,
        name: String,
        path: PathBuf,
    },
    /// A path with a custom name chosen by the user
    Named {
        path: PathBuf,
        name: String,
    },
}

impl Favorite {
    pub fn from_path(path: PathBuf) -> Self {
        // Ensure that special folders are handled properly
        [
            Self::Home,
            Self::Documents,
            Self::Downloads,
            Self::Music,
            Self::Pictures,
            Self::Videos,
        ]
        .into_iter()
        .find(|fav| fav.path_opt().as_ref() == Some(&path))
        .unwrap_or(Self::Path(path))
    }

    pub fn path_opt(&self) -> Option<PathBuf> {
        match self {
            Self::Home => dirs::home_dir(),
            Self::Documents => dirs::document_dir(),
            Self::Downloads => dirs::download_dir(),
            Self::Music => dirs::audio_dir(),
            Self::Pictures => dirs::picture_dir(),
            Self::Videos => dirs::video_dir(),
            Self::Path(path) => Some(path.clone()),
            Self::Network { path, .. } => Some(path.clone()),
            Self::Named { path, .. } => Some(path.clone()),
        }
    }

    /// Name shown in the sidebar, or `None` if the path has no usable file name
    pub fn display_name(&self) -> Option<String> {
        match self {
            Self::Home => Some(crate::fl!("home")),
            Self::Named { name, .. } | Self::Network { name, .. } => Some(name.clone()),
            _ => self
                .path_opt()?
                .file_name()
                .and_then(|x| x.to_str())
                .map(ToString::to_string),
        }
    }

    /// Return this favorite with a custom sidebar label chosen by the user
    pub fn with_label(&self, name: &str) -> Self {
        match self {
            Self::Network { uri, path, .. } => Self::Network {
                uri: uri.clone(),
                name: name.to_string(),
                path: path.clone(),
            },
            other => match other.path_opt() {
                Some(path) => Self::Named {
                    path,
                    name: name.to_string(),
                },
                None => other.clone(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TypeToSearch {
    Recursive,
    EnterPath,
    SelectByPrefix,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct State {
    pub sort_names: FxOrderMap<String, (HeadingOptions, bool)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            sort_names: FxOrderMap::from_iter(dirs::download_dir().into_iter().map(|dir| {
                (
                    Location::Path(dir).normalize().to_string(),
                    (HeadingOptions::Modified, false),
                )
            })),
        }
    }
}

impl State {
    pub fn load() -> (Store, Self) {
        let store = Store::named("state");
        let config = store.load();
        (store, config)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Config {
    pub app_theme: AppTheme,
    pub dialog: DialogConfig,
    pub context_actions: Vec<ContextActionPreset>,
    pub density: Density,
    /// Header-bar size, kept separate from interface density: the header bar
    /// reads `header_size` while everything else reads `density`, and a
    /// desktop can legitimately set them differently.
    pub header_size: Density,
    pub thumb_cfg: ThumbCfg,
    pub favorites: Vec<Favorite>,
    /// Name of the XDG icon theme to look icons up in, used by
    /// `ui::icon_theme`. `None` means "follow the desktop", which is what an
    /// unset key and a config file written before this key existed both give.
    pub icon_theme: Option<String>,
    /// Family of the interface font, used by `ui::font`.
    pub interface_font: String,
    /// Family of the monospace font, used by `ui::font`.
    pub monospace_font: String,
    pub show_details: bool,
    pub show_recents: bool,
    pub tab: TabConfig,
    pub type_to_search: TypeToSearch,
    /// Keyboard shortcut overrides, applied on top of the built-in ones.
    /// Keys are shortcuts such as `"Ctrl+Shift+N"` or `"F5"`, values are
    /// action names, e.g. `key_binds: { "Ctrl+Shift+H": ToggleShowHidden }`.
    pub key_binds: FxOrderMap<String, Action>,
}

impl Config {
    pub fn load() -> (Store, Self) {
        let store = Store::named("config");
        let config = store.load();
        (store, config)
    }

    /// Construct tab config for dialog
    pub const fn dialog_tab(&self) -> TabConfig {
        TabConfig {
            folders_first: self.dialog.folders_first,
            icon_sizes: self.dialog.icon_sizes,
            show_hidden: self.dialog.show_hidden,
            show_type_column: false,
            max_search_results: DEFAULT_MAX_SEARCH_RESULTS,
            single_click: false,
            view: self.dialog.view,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            app_theme: AppTheme::System,
            dialog: DialogConfig::default(),
            context_actions: Vec::new(),
            density: Density::Standard,
            header_size: Density::Standard,
            thumb_cfg: ThumbCfg::default(),
            favorites: vec![
                Favorite::Home,
                Favorite::Documents,
                Favorite::Downloads,
                Favorite::Music,
                Favorite::Pictures,
                Favorite::Videos,
            ],
            icon_theme: None,
            interface_font: INTERFACE_FONT_DEFAULT.to_string(),
            monospace_font: MONOSPACE_FONT_DEFAULT.to_string(),
            show_details: false,
            show_recents: true,
            tab: TabConfig::default(),
            type_to_search: TypeToSearch::Recursive,
            key_binds: FxOrderMap::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct DialogConfig {
    /// Show folders before files
    pub folders_first: bool,
    /// Icon zoom
    pub icon_sizes: IconSizes,
    /// Show details sidebar
    pub show_details: bool,
    /// Show hidden files and folders
    pub show_hidden: bool,
    /// Selected view, grid or list
    pub view: View,
}

impl Default for DialogConfig {
    fn default() -> Self {
        Self {
            folders_first: false,
            icon_sizes: IconSizes::default(),
            show_details: true,
            show_hidden: false,
            view: View::List,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct ThumbCfg {
    pub jobs: NonZeroU16,
    pub max_mem_mb: NonZeroU16,
    pub max_size_mb: NonZeroU16,
}

impl Default for ThumbCfg {
    fn default() -> Self {
        Self {
            jobs: 4.try_into().unwrap(),
            max_mem_mb: 2000.try_into().unwrap(),
            max_size_mb: 64.try_into().unwrap(),
        }
    }
}

/// Global and local [`crate::tab::Tab`] config.
///
/// [`TabConfig`] contains options that are passed to each instance of [`crate::tab::Tab`].
const DEFAULT_MAX_SEARCH_RESULTS: NonZeroU16 = NonZeroU16::new(200).unwrap();

/// These options are set globally through the main config, but each tab may change options
/// locally. Local changes aren't saved to the main config.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct TabConfig {
    /// Show folders before files
    pub folders_first: bool,
    /// Icon zoom
    pub icon_sizes: IconSizes,
    /// Show hidden files and folders
    pub show_hidden: bool,
    /// Show the Type column in list view
    pub show_type_column: bool,
    /// Maximum number of search results kept
    pub max_search_results: NonZeroU16,
    /// Single click to open
    pub single_click: bool,
    /// Selected view, grid or list
    pub view: View,
}

impl Default for TabConfig {
    fn default() -> Self {
        Self {
            folders_first: true,
            icon_sizes: IconSizes::default(),
            show_hidden: false,
            show_type_column: false,
            max_search_results: DEFAULT_MAX_SEARCH_RESULTS,
            single_click: false,
            view: View::List,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct IconSizes {
    pub list: NonZeroU16,
    pub grid: NonZeroU16,
}

impl Default for IconSizes {
    fn default() -> Self {
        Self {
            list: 100.try_into().unwrap(),
            grid: 100.try_into().unwrap(),
        }
    }
}

impl IconSizes {
    pub fn list(&self) -> u16 {
        percent!(self.list, ICON_SIZE_LIST) as _
    }

    pub fn list_condensed(&self) -> u16 {
        percent!(self.list, ICON_SIZE_LIST_CONDENSED) as _
    }

    pub fn grid(&self) -> u16 {
        percent!(self.grid, ICON_SIZE_GRID) as _
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favorite_with_label_converts_path_to_named() {
        let favorite = Favorite::Path(PathBuf::from("/some/dir"));
        assert_eq!(
            favorite.with_label("Custom"),
            Favorite::Named {
                path: PathBuf::from("/some/dir"),
                name: "Custom".to_string(),
            }
        );
    }

    #[test]
    fn favorite_with_label_updates_network_in_place() {
        let favorite = Favorite::Network {
            uri: "sftp://example.com/".to_string(),
            name: "example.com".to_string(),
            path: PathBuf::from("/run/mount/example"),
        };
        assert_eq!(
            favorite.with_label("Custom"),
            Favorite::Network {
                uri: "sftp://example.com/".to_string(),
                name: "Custom".to_string(),
                path: PathBuf::from("/run/mount/example"),
            }
        );
    }

    #[test]
    fn favorite_with_label_converts_special_folder_to_named() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            Favorite::Home.with_label("Custom"),
            Favorite::Named {
                path: home,
                name: "Custom".to_string(),
            }
        );
    }

    #[test]
    fn favorite_display_name() {
        assert_eq!(
            Favorite::Path(PathBuf::from("/some/dir")).display_name(),
            Some("dir".to_string())
        );
        assert_eq!(Favorite::Path(PathBuf::from("/")).display_name(), None);
        assert_eq!(
            Favorite::Named {
                path: PathBuf::from("/some/dir"),
                name: "Custom".to_string(),
            }
            .display_name(),
            Some("Custom".to_string())
        );
    }
}
