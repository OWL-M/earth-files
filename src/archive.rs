use crate::mime_icon::mime_for_path;
use crate::operation::{Controller, OpReader, OperationError, OperationErrorType};
use crate::ui::iced::futures;
use jiff::Zoned;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;
use zip::result::ZipError;

pub const SUPPORTED_ARCHIVE_TYPES: &[&str] = &[
    "application/gzip",
    "application/x-compressed-tar",
    "application/x-tar",
    "application/zip",
    #[cfg(feature = "bzip2")]
    "application/x-bzip",
    #[cfg(feature = "bzip2")]
    "application/x-bzip-compressed-tar",
    #[cfg(feature = "bzip2")]
    "application/x-bzip2",
    #[cfg(feature = "bzip2")]
    "application/x-bzip2-compressed-tar",
    #[cfg(feature = "lzma-rust2")]
    "application/x-xz",
    #[cfg(feature = "lzma-rust2")]
    "application/x-xz-compressed-tar",
];

pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    ".tar.bz2",
    ".tar.gz",
    ".tar.lzma",
    ".tar.xz",
    ".tgz",
    ".tar",
    ".zip",
];

pub fn extract(
    path: &Path,
    new_dir: &Path,
    password: &Option<String>,
    controller: &Controller,
) -> Result<(Vec<PathBuf>, HashSet<PathBuf>), OperationError> {
    let mime = mime_for_path(path, None, false);
    let password = password.as_deref();
    match mime.essence_str() {
        "application/gzip" | "application/x-compressed-tar" => {
            OpReader::new(path, controller.clone())
                .map(io::BufReader::new)
                .map(flate2::read::GzDecoder::new)
                .map(tar::Archive::new)
                .and_then(|mut archive| archive.unpack(new_dir))
                .map_err(|e| OperationError::from_err(e, controller))
                .map(|_| Default::default())
        }
        "application/x-tar" => OpReader::new(path, controller.clone())
            .map(io::BufReader::new)
            .map(tar::Archive::new)
            .and_then(|mut archive| archive.unpack(new_dir))
            .map_err(|e| OperationError::from_err(e, controller))
            .map(|_| Default::default()),
        "application/zip" => fs::File::open(path)
            .map(io::BufReader::new)
            .map(zip::ZipArchive::new)
            .map_err(|e| OperationError::from_err(e, controller))?
            .and_then(move |mut archive| {
                zip_extract(&mut archive, new_dir, password, controller.clone())
            })
            .map_err(|e| match e {
                ZipError::UnsupportedArchive(ZipError::PASSWORD_REQUIRED)
                | ZipError::InvalidPassword => {
                    OperationError::from_kind(OperationErrorType::PasswordRequired, controller)
                }
                _ => OperationError::from_err(e, controller),
            }),
        #[cfg(feature = "bzip2")]
        "application/x-bzip"
        | "application/x-bzip-compressed-tar"
        | "application/x-bzip2"
        | "application/x-bzip2-compressed-tar" => OpReader::new(path, controller.clone())
            .map(io::BufReader::new)
            .map(bzip2::read::BzDecoder::new)
            .map(tar::Archive::new)
            .and_then(|mut archive| archive.unpack(new_dir))
            .map_err(|e| OperationError::from_err(e, controller))
            .map(|_| Default::default()),
        #[cfg(feature = "lzma-rust2")]
        "application/x-xz" | "application/x-xz-compressed-tar" => {
            OpReader::new(path, controller.clone())
                .map(io::BufReader::new)
                .map(|reader| lzma_rust2::XzReader::new(reader, true))
                .map(tar::Archive::new)
                .and_then(|mut archive| archive.unpack(new_dir))
                .map_err(|e| OperationError::from_err(e, controller))
                .map(|_| Default::default())
        }
        _ => Err(OperationError::from_err(
            format!("unsupported mime type {mime:?}"),
            controller,
        )),
    }
}

// From https://docs.rs/zip/latest/zip/read/struct.ZipArchive.html#method.extract, with cancellation and progress added
fn zip_extract<R: io::Read + io::Seek, P: AsRef<Path>>(
    archive: &mut zip::ZipArchive<R>,
    directory: P,
    password: Option<&str>,
    controller: Controller,
) -> zip::result::ZipResult<(Vec<PathBuf>, HashSet<PathBuf>)> {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::OpenOptionsExt;
    use zip::result::ZipError;

    fn make_writable_dir_all<T: AsRef<Path>>(
        outpath: T,
        target_dirs: &mut HashSet<PathBuf>,
    ) -> Result<(), ZipError> {
        let path = outpath.as_ref();
        if !path.exists() {
            fs::create_dir_all(path)?;
        }
        if !target_dirs.contains(path) {
            target_dirs.insert(path.to_path_buf());
        }

        // Dirs must be writable until all normal files are extracted
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(0o700 | fs::metadata(path)?.permissions().mode()),
        )?;
        Ok(())
    }

    /// Refuse an entry whose path reaches its place through a symlink.
    ///
    /// `enclosed_name` rejects `..` and absolute entry names, but an archive
    /// can hold a symlink entry and then a second entry underneath it: without
    /// this check the second entry is written through the link, outside the
    /// destination. An archive that legitimately wants to be unpacked over its
    /// own symlinked directory is refused too, which is the safe way round.
    fn reject_symlinked_path(root: &Path, outpath: &Path) -> Result<(), ZipError> {
        let Ok(relative) = outpath.strip_prefix(root) else {
            return Err(ZipError::InvalidArchive(
                "entry escapes the destination".into(),
            ));
        };
        let mut prefix = root.to_path_buf();
        for component in relative.components() {
            prefix.push(component);
            match fs::symlink_metadata(&prefix) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(ZipError::InvalidArchive(
                        "entry is placed through a symbolic link".into(),
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Whether a symlink entry at `link` pointing at `target` would resolve
    /// outside `root`. Resolved lexically: the link is not followed, so this
    /// holds even when the target does not exist yet.
    fn target_escapes(root: &Path, link: &Path, target: &Path) -> bool {
        let mut resolved = if target.is_absolute() {
            PathBuf::new()
        } else {
            match link.parent() {
                Some(parent) => parent.to_path_buf(),
                None => return true,
            }
        };
        for component in target.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !resolved.pop() {
                        return true;
                    }
                }
                other => resolved.push(other),
            }
        }
        !resolved.starts_with(root)
    }

    let root = directory.as_ref();
    let mut buffer = vec![0; 4 * 1024 * 1024];
    let total_files = archive.len();
    let mut written_files = Vec::with_capacity(total_files);
    let mut target_dirs = HashSet::new();
    let mut files_by_unix_mode = Vec::with_capacity(total_files);
    let mut files_by_last_modified = Vec::with_capacity(total_files);

    for i in 0..total_files {
        futures::executor::block_on(async {
            controller
                .check()
                .await
                .map_err(|s| io::Error::other(OperationError::from_state(s, &controller)))
        })?;

        controller.set_progress(i as f32 / total_files as f32);

        let mut file = match password {
            None => archive.by_index(i),
            Some(pwd) => archive.by_index_decrypt(i, pwd.as_bytes()),
        }?;

        let filepath = file
            .enclosed_name()
            .ok_or(ZipError::InvalidArchive("Invalid file path".into()))?;

        let outpath = root.join(filepath);
        reject_symlinked_path(root, &outpath)?;

        if let Some(last_modified) = file.last_modified() {
            files_by_last_modified.push((outpath.clone(), last_modified, file.is_symlink()));
        }

        if file.is_dir() {
            make_writable_dir_all(&outpath, &mut target_dirs)?;

            if let Some(mode) = file.unix_mode() {
                files_by_unix_mode.push((outpath, mode));
            }
            continue;
        }

        if let Some(parent) = outpath.parent() {
            make_writable_dir_all(parent, &mut target_dirs)?;
        }

        if file.is_symlink() {
            // The declared size comes from the archive, so never size a
            // buffer by it; a link target is at most PATH_MAX anyway, and
            // symlink(2) refuses one that long, so a cut-off target cannot
            // be created silently
            let mut target = Vec::new();
            (&mut file)
                .take(libc::PATH_MAX as u64)
                .read_to_end(&mut target)?;
            use std::os::unix::ffi::OsStringExt;
            let target = OsString::from_vec(target);
            if target_escapes(root, &outpath, Path::new(&target)) {
                return Err(ZipError::InvalidArchive(
                    "symbolic link points outside the destination".into(),
                ));
            }
            std::os::unix::fs::symlink(&target, outpath.as_path())?;

            written_files.push(outpath);
            continue;
        }

        let total = file.size();
        // O_NOFOLLOW so the final component cannot be a symlink planted by an
        // earlier entry, which `File::create` would happily write through
        let mut outfile = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&outpath)?;
        let mut current = 0;
        loop {
            futures::executor::block_on(async {
                controller
                    .check()
                    .await
                    .map_err(|s| io::Error::other(OperationError::from_state(s, &controller)))
            })?;

            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            outfile.write_all(&buffer[..count])?;
            current += count as u64;

            if current < total {
                let file_progress = current as f32 / total as f32;
                let total_progress = (i as f32 + file_progress) / total_files as f32;
                controller.set_progress(total_progress);
            }
        }

        // Check for real permissions, which we'll set in a second pass
        if let Some(mode) = file.unix_mode() {
            files_by_unix_mode.push((outpath.clone(), mode));
        }

        written_files.push(outpath);
    }
    use std::cmp::Reverse;
    use std::os::unix::fs::PermissionsExt;

    if files_by_unix_mode.len() > 1 {
        // Ensure we update children's permissions before making a parent unwritable
        files_by_unix_mode.sort_by_key(|(path, _)| Reverse(path.components().count()));
    }
    for (path, mode) in files_by_unix_mode {
        // Only the permission bits: the archive author does not get to hand
        // out setuid, setgid or sticky
        fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o777))?;
    }

    for (path, last_modified, is_symlink) in files_by_last_modified {
        if let Some(modified) = zip_date_time_to_system_time(last_modified) {
            let file_time = filetime::FileTime::from_system_time(modified);
            if is_symlink {
                // `set_file_mtime` follows the link, which would let a symlink
                // entry alone retimestamp a file outside the destination
                let accessed = fs::symlink_metadata(&path)
                    .map(|metadata| filetime::FileTime::from_last_access_time(&metadata))
                    .unwrap_or(file_time);
                filetime::set_symlink_file_times(&path, accessed, file_time)?;
            } else {
                filetime::set_file_mtime(&path, file_time)?;
            }
        }
    }

    Ok((written_files, target_dirs))
}

fn zip_date_time_to_system_time(date_time: zip::DateTime) -> Option<SystemTime> {
    let dt = DateTime::new(
        date_time.year() as i16,
        date_time.month() as i8,
        date_time.day() as i8,
        date_time.hour() as i8,
        date_time.minute() as i8,
        date_time.second() as i8,
        0,
    )
    .ok()?;
    TimeZone::system()
        .to_ambiguous_zoned(dt)
        .later()
        .ok()
        .map(SystemTime::from)
}

pub fn system_time_to_zip_date_time(system_time: SystemTime) -> Option<zip::DateTime> {
    let date_time = Zoned::try_from(system_time).ok()?;

    zip::DateTime::from_date_and_time(
        date_time.year() as u16,
        date_time.month() as u8,
        date_time.day() as u8,
        date_time.hour() as u8,
        date_time.minute() as u8,
        date_time.second() as u8,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_options() -> zip::write::FileOptions<'static, ()> {
        zip::write::FileOptions::default()
    }

    /// An archive holding a symlink out of the destination and a file beneath
    /// it must not write through the link
    #[test]
    fn zip_entry_under_a_symlink_cannot_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("victim.txt"), b"ORIGINAL").unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let zip_path = tmp.path().join("evil.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        writer
            .add_symlink("link", outside.to_str().unwrap(), zip_options())
            .unwrap();
        writer.start_file("link/victim.txt", zip_options()).unwrap();
        writer.write_all(b"PWNED").unwrap();
        writer.finish().unwrap();

        let result = extract(&zip_path, &dest, &None, &Controller::default());
        assert!(result.is_err(), "escaping archive must be refused");
        assert_eq!(
            fs::read_to_string(outside.join("victim.txt")).unwrap(),
            "ORIGINAL"
        );
    }

    /// A symlink entry on its own must not retimestamp its target either
    #[test]
    fn zip_symlink_entry_cannot_retimestamp_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, b"ORIGINAL").unwrap();
        let before = fs::metadata(&outside).unwrap().modified().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let zip_path = tmp.path().join("touch.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        writer
            .add_symlink(
                "link",
                outside.to_str().unwrap(),
                zip_options().last_modified_time(
                    zip::DateTime::from_date_and_time(1990, 1, 1, 0, 0, 0).unwrap(),
                ),
            )
            .unwrap();
        writer.finish().unwrap();

        let _ = extract(&zip_path, &dest, &None, &Controller::default());
        assert_eq!(fs::metadata(&outside).unwrap().modified().unwrap(), before);
        assert_eq!(fs::read_to_string(&outside).unwrap(), "ORIGINAL");
    }

    /// A symlink entry whose header claims an absurd uncompressed size must
    /// not make the extractor allocate that much before reading a byte
    #[test]
    fn zip_symlink_declared_size_is_not_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let zip_path = tmp.path().join("huge.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        writer
            .add_symlink("link", "a.txt", zip_options().large_file(true))
            .unwrap();
        writer.finish().unwrap();

        // `large_file` puts the sizes in zip64 extra fields, whose layout is
        // id 0x0001, length 16, uncompressed size, compressed size (u64 LE);
        // claim u64::MAX uncompressed in each, leaving the real data alone
        let mut bytes = fs::read(&zip_path).unwrap();
        let mut marker = vec![0x01, 0x00, 0x10, 0x00];
        marker.extend_from_slice(&("a.txt".len() as u64).to_le_bytes());
        let mut patched = 0;
        let mut i = 0;
        while i + marker.len() <= bytes.len() {
            if bytes[i..i + marker.len()] == marker[..] {
                bytes[i + 4..i + 12].copy_from_slice(&u64::MAX.to_le_bytes());
                patched += 1;
            }
            i += 1;
        }
        assert!(patched > 0, "zip64 size fields not found");
        fs::write(&zip_path, &bytes).unwrap();

        extract(&zip_path, &dest, &None, &Controller::default()).unwrap();
        assert_eq!(
            fs::read_link(dest.join("link")).unwrap(),
            Path::new("a.txt")
        );
    }

    /// An ordinary archive, including a symlink that stays inside, still works
    #[test]
    fn zip_ordinary_archive_still_extracts() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let zip_path = tmp.path().join("ok.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        writer.start_file("a.txt", zip_options()).unwrap();
        writer.write_all(b"hello").unwrap();
        writer.add_directory("sub", zip_options()).unwrap();
        writer.start_file("sub/b.txt", zip_options()).unwrap();
        writer.write_all(b"world").unwrap();
        writer
            .add_symlink("inside", "a.txt", zip_options())
            .unwrap();
        writer.finish().unwrap();

        extract(&zip_path, &dest, &None, &Controller::default()).unwrap();
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "hello");
        assert_eq!(fs::read_to_string(dest.join("sub/b.txt")).unwrap(), "world");
        assert_eq!(fs::read_to_string(dest.join("inside")).unwrap(), "hello");
    }

    /// A setuid bit chosen by the archive author must not survive extraction
    #[test]
    fn zip_setuid_bit_is_dropped() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let zip_path = tmp.path().join("suid.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        writer
            .start_file("tool", zip_options().unix_permissions(0o755))
            .unwrap();
        writer.write_all(b"#!/bin/sh").unwrap();
        writer.finish().unwrap();

        // The writer masks permissions to 0o777, so plant the setuid bit in the
        // central directory's external attributes by hand (offset 38 of the
        // 0x02014b50 header)
        let mut bytes = fs::read(&zip_path).unwrap();
        let header = bytes
            .windows(4)
            .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        let attrs = ((0o100000u32 | 0o4755) << 16).to_le_bytes();
        bytes[header + 38..header + 42].copy_from_slice(&attrs);
        fs::write(&zip_path, bytes).unwrap();

        extract(&zip_path, &dest, &None, &Controller::default()).unwrap();
        let mode = fs::metadata(dest.join("tool"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o755, "got {mode:o}");
    }
}
