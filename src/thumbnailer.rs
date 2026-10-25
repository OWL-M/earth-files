// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! The thumbnailer programs installed for each file type, as the
//! freedesktop.org Thumbnail Managing Standard finds them: read once.

use std::path::Path;
use std::process;
use std::sync::LazyLock;
use std::time::Instant;

use xdg_kit::thumbnails::{ThumbnailRequest, ThumbnailSource, Thumbnailer, Thumbnailers};

use crate::mime_app::Mime;

/// Every `.thumbnailer` file in the data folders; none without base
/// directories.
static THUMBNAILERS: LazyLock<Option<Thumbnailers>> = LazyLock::new(|| {
    let start = Instant::now();
    let thumbnailers = crate::xdg_dirs::BASE_DIRS.as_ref().map(Thumbnailers::load);
    if let Some(thumbnailers) = &thumbnailers {
        for diagnostic in thumbnailers.diagnostics() {
            log::debug!("thumbnailers: {diagnostic:?}");
        }
    }
    log::info!("loaded thumbnailers in {:?}", start.elapsed());
    thumbnailers
});

/// The thumbnailers for `mime` whose program is installed, those for the
/// type itself first, then those for an alias or a type it is a kind of.
pub fn thumbnailers(mime: &Mime) -> Vec<&'static Thumbnailer> {
    let Some(all) = THUMBNAILERS.as_ref() else {
        return Vec::new();
    };
    let path = std::env::var_os("PATH");
    all.for_type(&crate::mime_icon::SHARED_MIME_INFO, mime)
        .into_iter()
        .filter(|thumbnailer| thumbnailer.available(path.as_deref()))
        .collect()
}

/// The command that has `thumbnailer` write a thumbnail of `input`, `size`
/// pixels across, to `output`; `None`, logged, when its `Exec` cannot make
/// one.
pub fn command(
    thumbnailer: &Thumbnailer,
    input: &Path,
    output: &Path,
    size: u32,
) -> Option<process::Command> {
    let source = ThumbnailSource::local(input).ok()?;
    match thumbnailer.command(&ThumbnailRequest {
        source: &source,
        output,
        size,
    }) {
        Ok(command) => Some(command.to_command()),
        Err(err) => {
            log::warn!("thumbnailer {}: {err}", thumbnailer.path().display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A thumbnailer file's `Exec`, with the standard's `%i %o %s`, becomes
    /// the command for one file.
    #[test]
    fn a_thumbnailer_file_gives_the_command_for_a_file() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("data/thumbnailers");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("shot.thumbnailer"),
            "[Thumbnailer Entry]\nExec=/usr/bin/shot -s %s %i %o\nMimeType=image/x-shot;\n",
        )
        .unwrap();
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
        let all = Thumbnailers::load(&base_dirs);
        let shot = all.iter().next().expect("the thumbnailer is found");

        let command = command(
            shot,
            Path::new("/pics/a b.shot"),
            Path::new("/tmp/out.png"),
            256,
        )
        .expect("a command");
        assert_eq!(command.get_program(), "/usr/bin/shot");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["-s", "256", "/pics/a b.shot", "/tmp/out.png"]);
    }
}
