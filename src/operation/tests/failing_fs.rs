//! Operations on filesystems that fail as real ones do: a drive that fills
//! up, one mounted read-only, one that goes away while an operation runs.
//!
//! Each test runs again in a child process with a user and mount namespace
//! of its own, where this user is root and may mount a small `tmpfs`
//! without touching the system's mounts. Where the system does not allow
//! that, as in some build sandboxes, the test says so and passes.

use std::ffi::CString;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use test_log::test;

use super::super::{Blocked, BlockedAnswer, Operation};
use super::perform_blocked;

/// Set in the child that runs a test inside its namespace.
const IN_NAMESPACE: &str = "EARTH_FILES_TEST_IN_NAMESPACE";

/// Added to the error number of a step that sets up the namespace, which is
/// all the child can send back before it runs the test. The parent then
/// tells a refused namespace, which skips the test, from a failure to start
/// the child at all, which fails it.
const SETUP_FAILED: i32 = 0x4e53_0000;

/// Why setting up the namespace may be refused where the system does not
/// allow one: unprivileged user namespaces switched off (`EPERM`, `EACCES`),
/// none left (`ENOSPC`, `EUSERS`), or not built in (`EINVAL`, `ENOSYS`).
const REFUSED: [i32; 6] = [
    libc::EPERM,
    libc::EACCES,
    libc::ENOSPC,
    libc::EUSERS,
    libc::EINVAL,
    libc::ENOSYS,
];

/// Whether this process is the one to run the test `name`. The first time,
/// it runs `name` again in a child with a namespace of its own and answers
/// `false`, having checked the child passed; in that child it answers
/// `true`. Also `false` when the system allows no such namespace: the test
/// is skipped, and says so.
fn in_namespace(name: &str) -> bool {
    if std::env::var_os(IN_NAMESPACE).is_some() {
        return true;
    }
    // Made before the fork: only system calls may run between it and exec
    // SAFETY: neither call can fail
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let uid_map = CString::new(format!("0 {uid} 1")).unwrap();
    let gid_map = CString::new(format!("0 {gid} 1")).unwrap();
    let deny = CString::new("deny").unwrap();
    let mut command = Command::new(std::env::current_exe().expect("the test binary"));
    command
        .args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(IN_NAMESPACE, "1");
    // SAFETY: the hook makes system calls only, on memory made before the
    // fork
    unsafe {
        command.pre_exec(move || {
            let check = |status: libc::c_int| {
                if status < 0 {
                    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                    Err(std::io::Error::from_raw_os_error(SETUP_FAILED + errno))
                } else {
                    Ok(())
                }
            };
            let write = |path: &[u8], text: &CString| {
                let fd = libc::open(path.as_ptr().cast(), libc::O_WRONLY);
                check(fd)?;
                let bytes = text.as_bytes();
                let written = libc::write(fd, bytes.as_ptr().cast(), bytes.len());
                libc::close(fd);
                check(i32::from(written >= 0) - 1)
            };
            check(libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS))?;
            write(b"/proc/self/setgroups\0", &deny)?;
            write(b"/proc/self/uid_map\0", &uid_map)?;
            write(b"/proc/self/gid_map\0", &gid_map)?;
            // Nothing mounted here reaches the system's mount table
            check(libc::mount(
                c"none".as_ptr(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ))
        });
    }
    match command.output() {
        Ok(output) => {
            // Shown as the test's own, captured or not as it is
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            assert!(
                output.status.success(),
                "{name} failed in its namespace:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(err) => {
            let setup = err
                .raw_os_error()
                .and_then(|code| code.checked_sub(SETUP_FAILED))
                .filter(|errno| (0..0x1_0000).contains(errno));
            match setup {
                Some(errno) if REFUSED.contains(&errno) => eprintln!(
                    "skipped {name}: no user and mount namespace here ({})",
                    std::io::Error::from_raw_os_error(errno)
                ),
                Some(errno) => panic!(
                    "{name}: setting up its namespace failed: {}",
                    std::io::Error::from_raw_os_error(errno)
                ),
                None => panic!("{name}: its child could not be started: {err}"),
            }
        }
    }
    false
}

/// Mounts a `tmpfs` of `size` bytes at `at`, read-only with `read_only`.
fn mount_tmpfs(at: &Path, size: usize, read_only: bool) {
    let at = CString::new(at.as_os_str().as_encoded_bytes()).unwrap();
    let options = CString::new(format!("size={size}")).unwrap();
    let flags = if read_only { libc::MS_RDONLY } else { 0 };
    // SAFETY: CStrings that outlive the call
    let status = unsafe {
        libc::mount(
            c"none".as_ptr(),
            at.as_ptr(),
            c"tmpfs".as_ptr(),
            flags,
            options.as_ptr().cast(),
        )
    };
    assert_eq!(status, 0, "mount: {}", std::io::Error::last_os_error());
}

/// Lets go of the mount at `at` at once, as a drive pulled out: its paths
/// are gone, while files already open on it stay usable.
fn detach(at: &Path) {
    let at = CString::new(at.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a CString that outlives the call
    let status = unsafe { libc::umount2(at.as_ptr(), libc::MNT_DETACH) };
    assert_eq!(status, 0, "umount: {}", std::io::Error::last_os_error());
}

/// A file of `len` bytes at `path`.
fn file(path: &Path, len: usize) -> PathBuf {
    fs::write(path, vec![7u8; len]).expect("write");
    path.to_path_buf()
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read_dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// A copy that cannot fit is refused before anything is written, saying
/// how much it needs and how much is free
#[test(compio::test)]
async fn a_copy_too_big_for_the_drive_is_refused_up_front() {
    if !in_namespace(
        "operation::tests::failing_fs::a_copy_too_big_for_the_drive_is_refused_up_front",
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (src, drive) = (dir.path().join("src"), dir.path().join("drive"));
    fs::create_dir(&src).expect("mkdir");
    fs::create_dir(&drive).expect("mkdir");
    mount_tmpfs(&drive, 64 << 10, false);
    let small = file(&src.join("a small"), 1 << 10);
    let big = file(&src.join("b big"), 256 << 10);

    let (result, asked) = perform_blocked(
        Operation::Copy {
            paths: vec![small, big],
            to: drive.clone(),
        },
        |_, _, _| BlockedAnswer::Cancel,
    )
    .await;
    assert!(asked.is_empty(), "asked {asked:?}");
    let err = result.expect_err("refused");
    assert!(names(&drive).is_empty(), "written: {:?}", names(&drive));
    assert!(err.partial.created.is_empty());
    detach(&drive);
}

/// The files of `count` copied into a 64 KiB drive, each `len` bytes: they
/// fit by their sizes, but each takes a page of its own on the drive, so it
/// fills part-way through, as block rounding does on a real one.
const MANY: usize = 40;

/// A drive that fills part-way through a copy asks about the file it could
/// not finish, naming the full drive, and leaves no half of it. Cancelled
/// there, everything copied goes again; aborted, what was copied before
/// stays, to undo; skipped, each later file is asked about in turn and left
/// out
#[test(compio::test)]
async fn a_drive_that_fills_mid_copy_asks_and_leaves_no_half_file() {
    if !in_namespace(
        "operation::tests::failing_fs::a_drive_that_fills_mid_copy_asks_and_leaves_no_half_file",
    ) {
        return;
    }
    for answer in [
        BlockedAnswer::Cancel,
        BlockedAnswer::Abort,
        BlockedAnswer::Skip(false),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let (src, drive) = (dir.path().join("src"), dir.path().join("drive"));
        fs::create_dir(&src).expect("mkdir");
        fs::create_dir(&drive).expect("mkdir");
        mount_tmpfs(&drive, 64 << 10, false);
        let files: Vec<PathBuf> = (0..MANY)
            .map(|i| file(&src.join(format!("f{i:02}")), 512))
            .collect();

        let (result, asked) = perform_blocked(
            Operation::Copy {
                paths: files.clone(),
                to: drive.clone(),
            },
            |_, _, _| answer,
        )
        .await;
        assert!(!asked.is_empty(), "the drive never filled: {result:?}");
        for question in &asked {
            let Blocked::Failed { reason, .. } = question else {
                panic!("asked {question:?}");
            };
            assert!(
                reason.contains(&crate::fl!("reason-drive-full")),
                "{reason}"
            );
        }
        let Blocked::Failed { path: first, .. } = &asked[0] else {
            unreachable!()
        };
        let stopped = files
            .iter()
            .position(|file| file == first)
            .expect("a copied file");
        let before: Vec<String> = (0..stopped).map(|i| format!("f{i:02}")).collect();
        // Every file on the drive is whole
        for name in names(&drive) {
            assert_eq!(
                fs::metadata(drive.join(&name)).expect("stat").len(),
                512,
                "{name}"
            );
        }
        match answer {
            BlockedAnswer::Cancel => {
                result.expect_err("cancelled");
                assert!(
                    names(&drive).is_empty(),
                    "not put back: {:?}",
                    names(&drive)
                );
            }
            BlockedAnswer::Abort => {
                let err = result.expect_err("aborted");
                assert_eq!(names(&drive), before, "a half file was left");
                let created: Vec<PathBuf> = before.iter().map(|name| drive.join(name)).collect();
                assert_eq!(err.partial.created, created);
            }
            _ => {
                let done = result.expect("skipped past it");
                assert_eq!(asked.len(), MANY - names(&drive).len(), "{asked:?}");
                assert_eq!(done.skipped.len(), asked.len());
            }
        }
        detach(&drive);
    }
}

/// Remounts the `tmpfs` at `at` read-only, keeping what is on it.
fn remount_read_only(at: &Path) {
    let at = CString::new(at.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a CString that outlives the call
    let status = unsafe {
        libc::mount(
            std::ptr::null(),
            at.as_ptr(),
            std::ptr::null(),
            libc::MS_REMOUNT | libc::MS_RDONLY,
            std::ptr::null(),
        )
    };
    assert_eq!(status, 0, "remount: {}", std::io::Error::last_os_error());
}

/// A copy onto a drive mounted read-only is refused before anything is
/// written, saying so
#[test(compio::test)]
async fn a_copy_onto_a_read_only_drive_is_refused_up_front() {
    if !in_namespace(
        "operation::tests::failing_fs::a_copy_onto_a_read_only_drive_is_refused_up_front",
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (src, drive) = (dir.path().join("src"), dir.path().join("drive"));
    fs::create_dir(&src).expect("mkdir");
    fs::create_dir(&drive).expect("mkdir");
    mount_tmpfs(&drive, 1 << 20, true);
    let a = file(&src.join("a"), 100);

    let (result, asked) = perform_blocked(
        Operation::Copy {
            paths: vec![a],
            to: drive.clone(),
        },
        |_, _, _| BlockedAnswer::Cancel,
    )
    .await;
    assert!(asked.is_empty(), "asked {asked:?}");
    let err = result.expect_err("refused");
    assert!(err.to_string().contains("read-only"), "{err}");
    assert!(names(&drive).is_empty());
    detach(&drive);
}

/// A move off a drive mounted read-only, whose originals cannot be removed,
/// asks first; skipped, the original stays and nothing is copied
#[test(compio::test)]
async fn a_move_off_a_read_only_drive_asks_before_copying() {
    if !in_namespace(
        "operation::tests::failing_fs::a_move_off_a_read_only_drive_asks_before_copying",
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (drive, dest) = (dir.path().join("drive"), dir.path().join("dest"));
    fs::create_dir(&drive).expect("mkdir");
    fs::create_dir(&dest).expect("mkdir");
    mount_tmpfs(&drive, 1 << 20, false);
    let b = file(&drive.join("b"), 100);
    remount_read_only(&drive);

    let (result, asked) = perform_blocked(
        Operation::Move {
            paths: vec![b.clone()],
            to: dest.clone(),
            cross_device_copy: false,
        },
        |_, _, _| BlockedAnswer::Skip(false),
    )
    .await;
    assert!(
        matches!(
            asked.as_slice(),
            [Blocked::Move {
                read_only: true,
                ..
            }]
        ),
        "asked {asked:?}"
    );
    assert_eq!(result.expect("skipped").skipped, [b]);
    assert!(names(&dest).is_empty(), "copied: {:?}", names(&dest));
    assert_eq!(names(&drive), ["b"]);
    detach(&drive);
}

/// A drive pulled out while a copy waits on a question: tried again, the
/// step is asked about once more, as gone, not taken as done; aborted, the
/// copies made before are listed, and their undo, with them gone too, is
/// not one the app would run
#[test(compio::test)]
async fn a_drive_pulled_out_mid_copy_is_asked_about_as_gone() {
    if !in_namespace(
        "operation::tests::failing_fs::a_drive_pulled_out_mid_copy_is_asked_about_as_gone",
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (src, drive) = (dir.path().join("src"), dir.path().join("drive"));
    fs::create_dir(&src).expect("mkdir");
    fs::create_dir(&drive).expect("mkdir");
    mount_tmpfs(&drive, 64 << 10, false);
    // Into a folder on the drive, which goes with it: the mount point left
    // behind is an empty folder that could still be written to
    let target = drive.join("target");
    fs::create_dir(&target).expect("mkdir");
    let files: Vec<PathBuf> = (0..MANY)
        .map(|i| file(&src.join(format!("f{i:02}")), 512))
        .collect();

    // The drive fills, and while that is asked about it is pulled out
    let mut asked_before = 0;
    let operation = Operation::Copy {
        paths: files.clone(),
        to: target.clone(),
    };
    let (result, asked) = perform_blocked(operation.clone(), |_, _, _| {
        asked_before += 1;
        if asked_before == 1 {
            detach(&drive);
            BlockedAnswer::Retry
        } else {
            BlockedAnswer::Abort
        }
    })
    .await;
    let reasons: Vec<&str> = asked
        .iter()
        .map(|question| match question {
            Blocked::Failed { reason, .. } => reason.as_str(),
            other => panic!("asked {other:?}"),
        })
        .collect();
    let [full, gone] = reasons.as_slice() else {
        panic!("asked {asked:?}");
    };
    assert!(full.contains(&crate::fl!("reason-drive-full")), "{full}");
    assert!(gone.contains(&crate::fl!("reason-gone")), "{gone}");

    let err = result.expect_err("aborted");
    assert!(!err.partial.created.is_empty(), "{err:?}");
    assert!(
        err.partial
            .created
            .iter()
            .all(|path| path.starts_with(&target))
    );
    let undo = operation.undo(&err.partial);
    assert!(!undo.is_empty());
    assert!(
        !undo.iter().all(Operation::is_applicable),
        "an undo of what went with the drive would run: {undo:?}"
    );
}

/// An archive that fills the drive while it is extracted into a folder of
/// its own fails, and takes the half-made folder with it
#[test(compio::test)]
async fn an_extract_that_fills_the_drive_leaves_no_half_folder() {
    if !in_namespace(
        "operation::tests::failing_fs::an_extract_that_fills_the_drive_leaves_no_half_folder",
    ) {
        return;
    }
    use std::io::Write;
    let dir = tempfile::tempdir().expect("tempdir");
    let drive = dir.path().join("drive");
    fs::create_dir(&drive).expect("mkdir");
    mount_tmpfs(&drive, 64 << 10, false);
    // Small as an archive, far bigger than the drive once extracted
    let archive = dir.path().join("big.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).expect("create"));
    let options = zip::write::SimpleFileOptions::default();
    for i in 0..4 {
        zip.start_file(format!("part{i}"), options).expect("entry");
        zip.write_all(&vec![0u8; 128 << 10]).expect("write");
    }
    zip.finish().expect("finish");

    let (result, _) = perform_blocked(
        Operation::Extract {
            paths: vec![archive].into(),
            to: drive.clone(),
            password: None,
            as_folder: true,
        },
        |_, _, _| BlockedAnswer::Cancel,
    )
    .await;
    let err = result.expect_err("the drive filled");
    assert!(names(&drive).is_empty(), "left behind: {:?}", names(&drive));
    assert!(err.partial.created.is_empty(), "{err:?}");
    detach(&drive);
}
