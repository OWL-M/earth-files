// SPDX-License-Identifier: GPL-3.0-only

//! What a copy or move checks over its whole selection before anything is
//! touched.
//!
//! One walk does it all: it plans the steps, adds up the size, and notes in
//! the order it finds them the problems the user is asked about before the
//! operation runs. Those are files that cannot be read, folders that cannot
//! be listed, and what the target filesystem cannot hold: files too large,
//! links, names. On a local solid-state drive the walk runs on several
//! threads; anywhere else on one, which is faster on spinning disks and
//! network shares.

use super::{Method, OpKind, PLAN_PAUSE_POLL, PlannedOp, unlisted_dir};
use crate::operation::{Blocked, Controller, ControllerState, OperationError};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The rules of the filesystem a copy or move writes to, as far as the
/// checks need them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Target {
    /// FAT, exFAT or NTFS, the ones with rules of their own; `None` for any
    /// other.
    pub(crate) fs: Option<&'static str>,
}

impl Target {
    /// The rules for the filesystem `dir` is on.
    pub(crate) fn of(dir: &Path) -> Self {
        Self {
            fs: crate::operation::filesystem_name(dir),
        }
    }

    /// The largest file it can hold: FAT keeps a file's size in 32 bits.
    fn max_file_size(self) -> Option<u64> {
        (self.fs == Some("FAT")).then_some(u64::from(u32::MAX))
    }

    /// Whether it can hold links at all.
    fn holds_links(self) -> bool {
        !matches!(self.fs, Some("FAT" | "exFAT"))
    }

    /// Whether it can hold an entry named `name`. A name that is not text
    /// is left to the copy itself: whether it fits depends on how the drive
    /// is mounted.
    fn allows_name(self, name: &OsStr) -> bool {
        let (Some(fs), Some(name)) = (self.fs, name.to_str()) else {
            return true;
        };
        let refused = |c: char| {
            c.is_control() || matches!(c, '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|')
        };
        if name.chars().any(refused) {
            return false;
        }
        // FAT drops a trailing dot or space, so two names could become one
        !(fs == "FAT" && (name.ends_with('.') || name.ends_with(' ')))
    }
}

/// What a walk found: the steps, the problems in the order found, and the
/// size of every file in it.
#[derive(Default)]
pub(crate) struct Checked {
    pub(crate) planned: Vec<PlannedOp>,
    pub(crate) problems: Vec<Blocked>,
    pub(crate) size: u64,
}

/// The walk's state, filled in by one thread or several.
#[derive(Default)]
struct Found {
    checked: Checked,
    /// Folders it could not list, whose own step comes out at the end.
    unlisted: Vec<PathBuf>,
}

impl Found {
    /// Adds what one entry came to.
    fn add(&mut self, visited: Visited) {
        self.checked.planned.extend(visited.planned);
        self.checked.problems.extend(visited.problems);
        self.checked.size += visited.size;
        self.unlisted.extend(visited.unlisted);
    }
}

/// What one entry came to, worked out without holding the walk's state, so
/// that threads only wait on each other to add it.
#[derive(Default)]
struct Visited {
    planned: Option<PlannedOp>,
    problems: Vec<Blocked>,
    size: u64,
    unlisted: Option<PathBuf>,
}

/// Plans copying or moving `from_parent` to `to_parent`, and checks every
/// entry on the way against what `target` can hold. See the
/// [module documentation](self).
///
/// Blocking by nature: it reads the whole tree and stats every entry. Run it
/// on a worker, never on the thread the operations themselves run on.
pub(crate) fn check_tree(
    from_parent: &Path,
    to_parent: &Path,
    method: Method,
    controller: &Controller,
    target: Target,
    parallel: bool,
) -> Result<Checked, OperationError> {
    let walk = Walk {
        from_parent,
        to_parent,
        method,
        controller,
        target,
    };

    // A root that is itself a link is one step, never a walk: the walker
    // follows a root link that points at a regular file, and would plan a
    // copy of the target's bytes where the user selected the link.
    let root = fs::symlink_metadata(from_parent).map_err(|err| {
        OperationError::from_err(
            format!("failed to stat {}: {}", from_parent.display(), err),
            controller,
        )
    })?;
    if root.file_type().is_symlink() {
        let mut found = Found::default();
        found.add(walk.link(from_parent.to_path_buf())?);
        return Ok(found.checked);
    }

    let mut builder = ignore::WalkBuilder::new(from_parent);
    builder.standard_filters(false);
    let mut found = if parallel {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
        let found = Mutex::new(Found::default());
        let failed = Mutex::new(None);
        builder.threads(threads).build_parallel().run(|| {
            Box::new(|entry| match walk.visit(entry) {
                Ok(visited) => {
                    found
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .add(visited);
                    ignore::WalkState::Continue
                }
                Err(err) => {
                    failed
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get_or_insert(err);
                    ignore::WalkState::Quit
                }
            })
        });
        if let Some(err) = failed
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            return Err(err);
        }
        let mut found = found
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Workers finish in no order; a folder still has to be made before
        // what goes in it, and a path sorts after its folder
        found
            .checked
            .planned
            .sort_by(|a: &PlannedOp, b: &PlannedOp| a.from.cmp(&b.from));
        found
    } else {
        let mut found = Found::default();
        for entry in builder.build() {
            found.add(walk.visit(entry)?);
        }
        found
    };

    // A folder that could not be listed is left out whole, its own step
    // included: nothing of it is made, not even empty
    let unlisted = std::mem::take(&mut found.unlisted);
    found
        .checked
        .planned
        .retain(|op| !unlisted.contains(&op.from));
    Ok(found.checked)
}

/// One walk's fixed inputs.
struct Walk<'a> {
    from_parent: &'a Path,
    to_parent: &'a Path,
    method: Method,
    controller: &'a Controller,
    target: Target,
}

impl Walk<'_> {
    /// Plans and checks one entry.
    fn visit(
        &self,
        entry: Result<ignore::DirEntry, ignore::Error>,
    ) -> Result<Visited, OperationError> {
        // Checked here, inside the work: nothing outside can interrupt a
        // blocking job, so a walk that is not watched from within would run to
        // the end of the tree after the user cancelled it.
        loop {
            match self.controller.state() {
                ControllerState::Running => break,
                ControllerState::Paused => std::thread::sleep(PLAN_PAUSE_POLL),
                state => return Err(OperationError::from_state(state, self.controller)),
            }
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                if let Some(dir) = unlisted_dir(&err) {
                    return Ok(Visited {
                        problems: vec![Blocked::List(dir.clone())],
                        unlisted: Some(dir),
                        ..Visited::default()
                    });
                }
                return Err(OperationError::from_err(
                    format!(
                        "failed to walk directory {}: {}",
                        self.from_parent.display(),
                        err
                    ),
                    self.controller,
                ));
            }
        };
        let file_type = entry.file_type();
        let size = entry.metadata().map_or(0, |meta| meta.len());
        let from = entry.into_path();
        if file_type.is_some_and(|t| t.is_symlink()) {
            return self.link(from);
        }
        let kind = if file_type.is_some_and(|t| t.is_dir()) {
            OpKind::Mkdir
        } else if file_type.is_some_and(|t| t.is_file()) {
            match self.method {
                Method::Copy => OpKind::Copy,
                Method::Move { cross_device_copy } => OpKind::Move { cross_device_copy },
            }
        } else {
            // Sockets, FIFOs and device nodes cannot be copied meaningfully
            log::warn!(
                "skipping {}: not a regular file, directory or symlink",
                from.display()
            );
            return Ok(Visited::default());
        };
        let to = self.to(&from)?;
        let mut visited = Visited::default();
        if !matches!(kind, OpKind::Mkdir) {
            self.controller.add_checked(1, size);
            visited.size = size;
            if crate::operation::cannot_read(&from) {
                visited.problems.push(Blocked::Read(from.clone()));
            }
            if let (Some(max), Some(fs)) = (self.target.max_file_size(), self.target.fs)
                && size > max
            {
                visited.problems.push(Blocked::TooBig {
                    path: from.clone(),
                    fs,
                });
            }
        }
        visited.problems.extend(self.name_problem(&from, &to));
        visited.planned = Some(PlannedOp { kind, from, to });
        Ok(visited)
    }

    /// Plans and checks a link, which is copied as a link.
    fn link(&self, from: PathBuf) -> Result<Visited, OperationError> {
        let target = fs::read_link(&from).map_err(|err| {
            OperationError::from_err(
                format!("failed to read link {}: {}", from.display(), err),
                self.controller,
            )
        })?;
        let to = self.to(&from)?;
        let mut visited = Visited::default();
        if !self.target.holds_links() {
            visited.problems.push(Blocked::Link {
                path: from.clone(),
                fs: self.target.fs,
            });
        }
        visited.problems.extend(self.name_problem(&from, &to));
        visited.planned = Some(PlannedOp {
            kind: OpKind::Symlink { target },
            from,
            to,
        });
        Ok(visited)
    }

    /// A name the target cannot hold, if `to` has one.
    fn name_problem(&self, from: &Path, to: &Path) -> Option<Blocked> {
        let (name, fs) = (to.file_name()?, self.target.fs?);
        (!self.target.allows_name(name)).then(|| Blocked::BadName {
            path: from.to_path_buf(),
            fs,
        })
    }

    /// Where `from` lands.
    fn to(&self, from: &Path) -> Result<PathBuf, OperationError> {
        if from == self.from_parent {
            // When copying a file, from matches from_parent, and to_parent must be used
            return Ok(self.to_parent.to_path_buf());
        }
        let relative = from.strip_prefix(self.from_parent).map_err(|err| {
            OperationError::from_err(
                format!(
                    "failed to remove prefix {} from {}: {}",
                    self.from_parent.display(),
                    from.display(),
                    err
                ),
                self.controller,
            )
        })?;
        Ok(self.to_parent.join(relative))
    }
}

/// Whether a walk of `path` is faster on several threads: only on a local
/// solid-state drive. Network and FUSE filesystems, spinning disks, and
/// anything that cannot be told apart are walked on one thread.
///
/// Blocking: it asks the filesystem and reads `/sys`.
pub(crate) fn walks_in_parallel(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    // From `linux/magic.h`: filesystems whose data is somewhere else
    const REMOTE: [i64; 9] = [
        0x6969,      // NFS
        0x517b,      // SMB
        0xff53_4d42, // CIFS
        0xfe53_4d42, // SMB2
        0x6573_5546, // FUSE, which gvfs and sshfs are
        0x5346_414f, // AFS
        0x00c3_6400, // Ceph
        0x7375_7245, // Coda
        0x0102_1997, // 9P
    ];
    match crate::operation::filesystem_magic(path) {
        Some(magic) if !REMOTE.contains(&magic) => {}
        _ => return false,
    }
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let dev = meta.dev();
    let (major, minor) = (libc::major(dev), libc::minor(dev));
    rotational(&PathBuf::from(format!("/sys/dev/block/{major}:{minor}")))
        .or_else(|| {
            // No block device of its own, as with btrfs: the device it is
            // mounted from says
            let source = mount_source(major, minor)?;
            let name = fs::canonicalize(source).ok()?;
            rotational(&Path::new("/sys/class/block").join(name.file_name()?))
        })
        .is_some_and(|rotational| !rotational)
}

/// Whether the block device at `dev` in `/sys` spins, asking its disk when
/// it is a partition. `None` when `/sys` does not say.
fn rotational(dev: &Path) -> Option<bool> {
    let dev = fs::canonicalize(dev).ok()?;
    let read = |dir: &Path| fs::read_to_string(dir.join("queue/rotational")).ok();
    let value = read(&dev).or_else(|| read(dev.parent()?))?;
    Some(value.trim() != "0")
}

/// The device a filesystem numbered `major:minor` is mounted from, from
/// `/proc/self/mountinfo`.
fn mount_source(major: u32, minor: u32) -> Option<PathBuf> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").ok()?;
    let wanted = format!("{major}:{minor}");
    mountinfo.lines().find_map(|line| {
        let mut fields = line.split(' ');
        if fields.nth(2)? != wanted {
            return None;
        }
        // After the separator: the filesystem type, then the source
        let mut after = line.split(" - ").nth(1)?.split(' ');
        let source = after.nth(1)?;
        source.starts_with("/dev/").then(|| PathBuf::from(source))
    })
}

#[cfg(test)]
mod tests {
    use super::{Checked, Target, check_tree, walks_in_parallel};
    use crate::operation::recursive::Method;
    use crate::operation::{Blocked, Controller};
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn check(from: &Path, to: &Path, target: Target, parallel: bool) -> Checked {
        check_tree(
            from,
            to,
            Method::Copy,
            &Controller::default(),
            target,
            parallel,
        )
        .expect("checking")
    }

    /// The steps as a set of paths, which the order of a parallel walk
    /// cannot change.
    fn steps(checked: &Checked) -> BTreeSet<String> {
        checked
            .planned
            .iter()
            .map(|op| op.from.display().to_string())
            .collect()
    }

    #[test]
    fn a_parallel_walk_finds_what_a_single_one_does() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("from");
        for sub in ["a", "a/b", "c"] {
            fs::create_dir_all(from.join(sub)).expect("mkdir");
            for file in ["1", "2", "3"] {
                fs::write(from.join(sub).join(file), b"12345").expect("write");
            }
        }
        let to = dir.path().join("to");

        let single = check(&from, &to, Target::default(), false);
        let parallel = check(&from, &to, Target::default(), true);
        assert_eq!(steps(&single), steps(&parallel));
        assert_eq!(single.size, 9 * 5);
        assert_eq!(parallel.size, single.size);
        // Parents come before what they hold either way
        for checked in [&single, &parallel] {
            for (index, op) in checked.planned.iter().enumerate() {
                if let Some(parent) = op.from.parent() {
                    assert!(
                        !checked.planned[index..]
                            .iter()
                            .any(|later| later.from == parent),
                        "{} comes before its folder",
                        op.from.display()
                    );
                }
            }
        }
    }

    #[test]
    fn unreadable_and_unlistable_are_found_and_left_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("from");
        let locked = from.join("locked");
        fs::create_dir_all(&locked).expect("mkdir");
        fs::write(locked.join("inside"), b"i").expect("write");
        let secret = from.join("secret");
        fs::write(&secret, b"s").expect("write");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).expect("chmod");
        if fs::read_dir(&locked).is_ok() {
            // Running with privileges that ignore the mode
            return;
        }

        for parallel in [false, true] {
            let checked = check(&from, &dir.path().join("to"), Target::default(), parallel);
            let problems: BTreeSet<_> = checked.problems.iter().map(|p| format!("{p:?}")).collect();
            let expected: BTreeSet<_> =
                [Blocked::List(locked.clone()), Blocked::Read(secret.clone())]
                    .into_iter()
                    .map(|p| format!("{p:?}"))
                    .collect();
            assert_eq!(problems, expected);
            assert!(
                !checked.planned.iter().any(|op| op.from == locked),
                "an unlisted folder is left out whole"
            );
        }
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[test]
    fn what_fat_cannot_hold_is_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("from");
        fs::create_dir(&from).expect("mkdir");
        // Sparse: 5 GB on paper, nothing on disk
        let big = from.join("big.iso");
        fs::File::create(&big)
            .and_then(|file| file.set_len(5 << 30))
            .expect("sparse file");
        let link = from.join("result");
        std::os::unix::fs::symlink("big.iso", &link).expect("symlink");
        let odd = from.join("a:b");
        fs::write(&odd, b"o").expect("write");
        let to = dir.path().join("to");

        let fat = check(&from, &to, Target { fs: Some("FAT") }, false);
        let found: BTreeSet<_> = fat.problems.iter().map(|p| format!("{p:?}")).collect();
        let expected: BTreeSet<_> = [
            Blocked::TooBig {
                path: big.clone(),
                fs: "FAT",
            },
            Blocked::Link {
                path: link.clone(),
                fs: Some("FAT"),
            },
            Blocked::BadName {
                path: odd.clone(),
                fs: "FAT",
            },
        ]
        .into_iter()
        .map(|p| format!("{p:?}"))
        .collect();
        assert_eq!(found, expected);

        // exFAT holds big files, but neither links nor that name
        let exfat = check(&from, &to, Target { fs: Some("exFAT") }, false);
        assert!(
            !exfat
                .problems
                .iter()
                .any(|p| matches!(p, Blocked::TooBig { .. }))
        );
        assert!(
            exfat
                .problems
                .iter()
                .any(|p| matches!(p, Blocked::Link { .. }))
        );
        assert!(
            exfat
                .problems
                .iter()
                .any(|p| matches!(p, Blocked::BadName { .. }))
        );

        // Anywhere else, nothing
        assert!(
            check(&from, &to, Target::default(), false)
                .problems
                .is_empty()
        );
    }

    #[test]
    fn fat_refuses_a_trailing_dot_and_others_do_not() {
        let fat = Target { fs: Some("FAT") };
        let ntfs = Target { fs: Some("NTFS") };
        assert!(!fat.allows_name("notes.".as_ref()));
        assert!(ntfs.allows_name("notes.".as_ref()));
        assert!(!ntfs.allows_name("what?".as_ref()));
        assert!(Target::default().allows_name("what?".as_ref()));
    }

    #[test]
    fn only_a_local_drive_is_walked_in_parallel() {
        // Neither has a block device to ask
        assert!(!walks_in_parallel(Path::new("/proc")));
        assert!(!walks_in_parallel(Path::new("/dev/shm")));
    }
}
