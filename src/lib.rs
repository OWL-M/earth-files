// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::ui::iced_core::layout::Limits;
use crate::ui::shell::Settings;
use std::path::{Path, PathBuf};
use std::{env, fs, process};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use xdg_kit::basedir::UserDirKind;

use crate::app::{App, Flags};
use crate::config::{Config, State};
use crate::tab::Location;

mod action_card;
pub mod app;
mod archive;
mod batch_rename;
pub mod channel;
mod child;
pub mod clipboard;
pub mod config;
mod context_action;
pub mod desktop_entry;
pub mod dialog;
mod exit_gate;
pub mod file_category;
mod fs_at;
mod handoff;
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
pub mod portal;
pub mod progress;
pub mod recents;
mod service_menu;
mod spawn_detached;
pub mod tab;
mod thumbnail_cacher;
mod thumbnailer;
pub(crate) mod trash;
pub(crate) mod trashing;
pub mod type_check;
pub mod ui;
mod xdg_dirs;
mod zoom;

pub(crate) type FxOrderMap<K, V> = ordermap::OrderMap<K, V, rustc_hash::FxBuildHasher>;

pub(crate) fn err_str<T: ToString>(err: T) -> String {
    err.to_string()
}

pub fn desktop_dir() -> PathBuf {
    if let Some(path) = xdg_dirs::user_dir(&UserDirKind::Desktop) {
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
    if let Some(home) = xdg_dirs::home() {
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
/// Set up logging, once, however the process was started.
///
/// Both the file manager and the portal backend go through this, so they
/// report the same way and the default filter is stated in one place.
pub fn init_logging() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(install_logging);
}

#[rustfmt::skip]
fn install_logging() {
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
        .with({
            // The builder holds one default directive, so asking for two
            // would keep only the last. Both go into one default filter
            // instead, used only when `RUST_LOG` is unset: adding directives
            // after reading the environment would override what the user
            // asked for. The runner is included because a Wayland connection
            // that fails to open means `ui::dnd::init` is never reached.
            const DEFAULT_FILTER: &str =
                "earth_files::ui::dnd=warn,earth_files::ui::shell::runner=warn";
            match std::env::var(tracing_subscriber::EnvFilter::DEFAULT_ENV) {
                Ok(filter) if !filter.trim().is_empty() => {
                    tracing_subscriber::EnvFilter::new(filter)
                }
                _ => tracing_subscriber::EnvFilter::new(DEFAULT_FILTER),
            }
        })
        .with(log_layer)
        .init();
}

#[rustfmt::skip]
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    localize::localize();

    // Before the first palette is handed out: theme files are read once and
    // the result is what `Theme::cosmic()` returns from then on.
    crate::ui::theme::custom::load();

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
        if single_threaded() {
            if let Err(err) = detach() {
                eprintln!("failed to daemonize: {err}");
                process::exit(1);
            }
        } else {
            log::warn!(
                "not detaching from the terminal: other threads may already be running"
            );
        }
    }
    crate::ui::icon_theme::warm();

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
    let result = crate::ui::shell::run::<App>(settings, flags);

    // Reached when the loop returns rather than when the window is closed;
    // that path exits the process directly and drains through `shut_down`
    // itself. Done even when the run failed: the settings from before it did
    // are still the user's.
    shut_down();

    result?;

    Ok(())
}

/// Wait for everything written in the background to reach the disk.
///
/// Settings and recent files are written by worker threads, which the process
/// would otherwise take down with it along with whatever is still queued.
/// Anything that ends the process must call this first.
/// Leaves the terminal Earth Files was started from, as `daemon(7)`
/// describes: the shell gets its prompt back, and closing the terminal or
/// pressing Ctrl+C there no longer ends it. Returns only in the detached
/// process; the others end here.
///
/// It forks twice. After the first fork and `setsid`, the process leads a new
/// session with no terminal. The second fork leaves a process that does not
/// lead it, which can never take a terminal back: a session leader opening a
/// terminal device without `O_NOCTTY`, as `File::open` does, would make it
/// its controlling terminal, and be ended with `SIGHUP` when it hangs up.
///
/// Only the thread that forks exists in the child, so this runs while there
/// is just the one; see [`single_threaded`].
fn detach() -> std::io::Result<()> {
    // SAFETY: the process has a single thread (checked by the caller), so
    // nothing in the children waits on a lock or a thread that is not there.
    // The parents end with `_exit`, which runs no destructors or exit
    // handlers that the child, a copy of the same state, is still using.
    unsafe {
        match libc::fork() {
            -1 => return Err(std::io::Error::last_os_error()),
            0 => {}
            _ => libc::_exit(0),
        }
        if libc::setsid() == -1 {
            return Err(std::io::Error::last_os_error());
        }
        match libc::fork() {
            -1 => Err(std::io::Error::last_os_error()),
            0 => Ok(()),
            _ => libc::_exit(0),
        }
    }
}

/// Whether this process is known to run only the one thread, as [`detach`]
/// needs. Not when `/proc` cannot tell: forking then is not known to be safe.
fn single_threaded() -> bool {
    thread_count(Path::new("/proc/self/task")) == Some(1)
}

/// How many threads `tasks`, a `/proc/<pid>/task` folder, lists, or `None`
/// when it cannot be read whole.
fn thread_count(tasks: &Path) -> Option<usize> {
    fs::read_dir(tasks)
        .ok()?
        .try_fold(0, |count, task| task.ok().map(|_| count + 1))
}

pub fn shut_down() {
    config::store::flush();
    recents::flush();
    mime_app::flush_associations();
}

#[cfg(test)]
mod tests {
    use super::{single_threaded, thread_count};

    #[test]
    fn another_thread_is_seen() {
        let (stop, wait) = std::sync::mpsc::channel::<()>();
        let other = std::thread::spawn(move || {
            let _ = wait.recv();
        });
        assert!(!single_threaded());
        drop(stop);
        other.join().unwrap();
    }

    #[test]
    fn a_folder_that_cannot_be_read_counts_nothing() {
        let gone = tempfile::tempdir().unwrap().path().join("task");
        assert_eq!(thread_count(&gone), None);
    }

    #[test]
    fn each_task_listed_is_counted() {
        let tasks = tempfile::tempdir().unwrap();
        for id in ["1", "2", "3"] {
            std::fs::create_dir(tasks.path().join(id)).unwrap();
        }
        assert_eq!(thread_count(tasks.path()), Some(3));
    }
}
