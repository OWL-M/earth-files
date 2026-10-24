// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::mime_app::Mime;
use rustc_hash::FxHashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;
use std::{fs, process};
use xdg_kit::desktop_entry::KeyFile;

#[derive(Clone, Debug)]
pub struct Thumbnailer {
    pub exec: String,
}

impl Thumbnailer {
    pub fn command(
        &self,
        input: &Path,
        output: &Path,
        thumbnail_size: u32,
    ) -> Option<process::Command> {
        let args_vec: Vec<String> = shlex::split(&self.exec)?;
        let mut args = args_vec.iter();
        let mut command = process::Command::new(args.next()?);
        for arg in args {
            if arg.starts_with('%') {
                match arg.as_str() {
                    "%i" | "%u" => {
                        command.arg(input);
                    }
                    "%o" => {
                        command.arg(output);
                    }
                    "%s" => {
                        command.arg(format!("{thumbnail_size}"));
                    }
                    _ => {
                        log::warn!(
                            "unsupported thumbnailer Exec code {:?} in {:?}",
                            arg,
                            self.exec
                        );
                        return None;
                    }
                }
            } else {
                command.arg(arg);
            }
        }
        Some(command)
    }
}

pub struct ThumbnailerCache {
    cache: FxHashMap<Mime, Vec<Thumbnailer>>,
}

impl ThumbnailerCache {
    pub fn new() -> Self {
        let mut thumbnailer_cache = Self {
            cache: FxHashMap::default(),
        };
        thumbnailer_cache.reload();
        thumbnailer_cache
    }

    pub fn reload(&mut self) {
        let start = Instant::now();

        self.cache.clear();

        let mut search_dirs = Vec::new();
        if let Some(base_dirs) = crate::xdg_dirs::BASE_DIRS.as_ref() {
            search_dirs.push(base_dirs.data_home().join("thumbnailers"));
            search_dirs.extend(
                base_dirs
                    .data_dirs()
                    .iter()
                    .map(|data_dir| data_dir.join("thumbnailers")),
            );
        }

        // A thumbnailer file shadows any later directory's file of the same name,
        // so the user's data home overrides the system thumbnailers
        let mut seen_names = std::collections::HashSet::new();
        let mut thumbnailer_paths = Vec::new();
        for dir in search_dirs {
            log::trace!("looking for thumbnailers in {}", dir.display());
            match fs::read_dir(&dir) {
                Ok(entries) => {
                    thumbnailer_paths.extend(entries.filter_map(|entry_res| {
                        let entry = entry_res
                            .inspect_err(|err| {
                                log::warn!(
                                    "failed to read entry in directory {}: {}",
                                    dir.display(),
                                    err
                                )
                            })
                            .ok()?;
                        seen_names.insert(entry.file_name()).then(|| entry.path())
                    }));
                }
                Err(err) => {
                    log::warn!("failed to read directory {}: {}", dir.display(), err);
                }
            }
        }

        for path in thumbnailer_paths {
            let entry = match KeyFile::read(&path) {
                Ok(ok) => ok,
                Err(err) => {
                    log::warn!("failed to read {}: {}", path.display(), err);
                    continue;
                }
            };

            const GROUP: &str = "Thumbnailer Entry";
            if !entry.has_group(GROUP) {
                log::warn!(
                    "missing Thumbnailer Entry section for thumbnailer {}",
                    path.display()
                );
                continue;
            }
            if let Some(try_exec) = entry.string(GROUP, "TryExec")
                && !executable_exists(&try_exec)
            {
                log::debug!(
                    "skipping thumbnailer {}: TryExec {try_exec:?} not found",
                    path.display()
                );
                continue;
            }
            let Some(exec) = entry.string(GROUP, "Exec") else {
                log::warn!("missing Exec attribute for thumbnailer {}", path.display());
                continue;
            };
            let Some(mime_types) = entry.list(GROUP, "MimeType") else {
                log::warn!(
                    "missing MimeType attribute for thumbnailer {}",
                    path.display()
                );
                continue;
            };

            for mime_type in &mime_types {
                if let Ok(mime) = mime_type.parse::<Mime>() {
                    log::trace!("thumbnailer {}={}", mime, path.display());
                    let apps = self
                        .cache
                        .entry(mime)
                        .or_insert_with(|| Vec::with_capacity(1));
                    apps.push(Thumbnailer {
                        exec: exec.to_string(),
                    });
                }
            }
        }

        let elapsed = start.elapsed();
        log::info!("loaded thumbnailer cache in {elapsed:?}");
    }

    pub fn get(&self, key: &Mime) -> Vec<Thumbnailer> {
        self.cache.get(key).map_or_else(Vec::new, Vec::clone)
    }
}

/// Whether `program` is an executable file, searched on `PATH` when relative.
fn executable_exists(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let is_executable = |path: &Path| {
        fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    let program = Path::new(program);
    if program.is_absolute() {
        return is_executable(program);
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| is_executable(&dir.join(program)))
    })
}

static THUMBNAILER_CACHE: LazyLock<Mutex<ThumbnailerCache>> =
    LazyLock::new(|| Mutex::new(ThumbnailerCache::new()));

pub fn thumbnailer(mime: &Mime) -> Vec<Thumbnailer> {
    let thumbnailer_cache = THUMBNAILER_CACHE.lock().unwrap();
    thumbnailer_cache.get(mime)
}
