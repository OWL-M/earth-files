// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::ui::iced_core::layout::Limits;
use crate::ui::shell::Settings;
use std::path::PathBuf;
use std::{env, fs, process};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::app::{App, Flags};
use crate::config::{Config, State};
use crate::tab::Location;

pub mod app;
mod archive;
mod batch_rename;
pub mod channel;
pub mod clipboard;
pub mod config;
mod context_action;
pub mod desktop_entry;
pub mod dialog;
pub mod file_category;
mod hex;
mod inhibit;
mod key_bind;
pub(crate) mod large_image;
pub(crate) mod load_image;
mod localize;
mod menu;
mod mime_app;
pub mod mime_icon;
mod mounter;
mod mouse_area;
pub mod operation;
mod spawn_detached;
pub mod tab;
mod thumbnail_cacher;
mod thumbnailer;
pub(crate) mod trash;
pub mod ui;
mod zoom;

pub(crate) type FxOrderMap<K, V> = ordermap::OrderMap<K, V, rustc_hash::FxBuildHasher>;

pub(crate) fn err_str<T: ToString>(err: T) -> String {
    err.to_string()
}

pub fn desktop_dir() -> PathBuf {
    if let Some(path) = dirs::desktop_dir() {
        path
    } else {
        let path = home_dir().join("Desktop");
        log::warn!(
            "failed to locate desktop directory, falling back to {}",
            path.display()
        );
        path
    }
}

pub fn home_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home
    } else {
        let path = PathBuf::from("/");
        log::warn!(
            "failed to locate home directory, falling back to {}",
            path.display()
        );
        path
    }
}

/// Runs application with these settings
#[rustfmt::skip]
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_format = tracing_subscriber::fmt::format()
        .pretty()
        .with_line_number(true)
        .with_file(true)
        .with_target(false)
        .with_thread_names(true);

    let log_layer = tracing_subscriber::fmt::Layer::default()
        .with_writer(std::io::stderr)
        .event_format(log_format);

    tracing_subscriber::registry()
        // `EnvFilter::from_default_env()` defaults to `error`, hiding warnings unless
        // `RUST_LOG` is set. Default to warnings so `ui::dnd` can explain failed tab
        // drags, copies and pastes. These warnings are rare and absent in a working
        // session. An explicit `RUST_LOG` still overrides the default.
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(
                    "earth_files::ui::dnd=warn"
                        .parse()
                        .expect("a literal directive that parses"),
                )
                // The runner reports Wayland connection failures because
                // `ui::dnd::init` is never called if the connection fails to open.
                .with_default_directive(
                    "earth_files::ui::shell::runner=warn"
                        .parse()
                        .expect("a literal directive that parses"),
                )
                .from_env_lossy(),
        )
        .with(log_layer)
        .init();

    localize::localize();

    let (config_handler, config) = Config::load();
    // Before any widget or window is built: `ui::font` is read argument-free
    // from every call site, so the configured families have to be in place
    // before the first one runs.
    crate::ui::font::set_families(&config);
    // Same reason: `ui::icon_theme` is read argument-free from the draw path.
    crate::ui::icon_theme::set_from_config(&config);
    let (state_handler, state) = State::load();

    let mut daemonize = true;
    let mut locations = Vec::new();
    let mut uris = Vec::new();
    for arg in env::args().skip(1) {
        let location = if &arg == "--no-daemon" {
            daemonize = false;
            continue;
        } else if &arg == "--trash" {
            Location::Trash
        } else if &arg == "--recents" {
            if config.show_recents {
                Location::Recents
            } else {
                log::warn!("recents feature is disabled in config");
                continue;
            }
        } else if &arg == "--network" {
            Location::Network("network:///".to_string(), fl!("networks"), None)
        } else {
            let path = match url::Url::parse(&arg) {
                Ok(url) if url.scheme() == "file" => if let Ok(path) = url.to_file_path() { path } else {
                    log::warn!("invalid argument {arg:?}");
                    continue;
                },
                Ok(url) if url.scheme() == "trash" => {
                    locations.push(Location::Trash);
                    continue;
                }
                Ok(url) if url.scheme() == "recent" => {
                    if config.show_recents {
                        locations.push(Location::Recents);
                    } else {
                        log::warn!("recents feature is disabled in config");
                    }
                    continue;
                }
                Ok(url) if url.scheme() == "network" => {
                    locations.push(Location::Network("network:///".to_string(), fl!("networks"), None));
                    continue;
                }
                // Any other scheme is a network location, mounted when its tab opens
                Ok(url) => {
                    uris.push(url);
                    continue;
                }
                _ => PathBuf::from(arg),
            };
            match fs::canonicalize(&path) {
                Ok(absolute) => Location::Path(absolute),
                Err(err) => {
                    log::warn!("failed to canonicalize {}: {}", path.display(), err);
                    continue;
                }
            }
        };
        locations.push(location);
    }

    if daemonize {
        match fork::daemon(true, true) {
            Ok(fork::Fork::Child) => (),
            Ok(fork::Fork::Parent(_child_pid)) => process::exit(0),
            Err(err) => {
                eprintln!("failed to daemonize: {err:?}");
                process::exit(1);
            }
        }
    }

    let mut settings = Settings::default();
    settings = settings.theme(config.app_theme.theme());
    settings = settings.size_limits(Limits::NONE.min_width(360.0).min_height(180.0));
    settings = settings.exit_on_close(false);

    #[cfg(feature = "jemalloc")]
    {
        settings = settings.default_mmap_threshold(None);
    }

    let flags = Flags {
        config_handler,
        config,
        state_handler,
        state,
        locations,
        uris
    };
    crate::ui::shell::run::<App>(settings, flags)?;

    Ok(())
}
