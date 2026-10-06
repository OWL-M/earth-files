// SPDX-License-Identifier: GPL-3.0-only

//! Doing an operation's steps as root, through `earth-files-helper`.
//!
//! The first time the user grants root in an operation, the helper is
//! started with `pkexec`, which asks polkit, which shows the password
//! prompt. It is told the operation's scope, then asked for one step at a
//! time over a pipe. When the operation ends the [`Helper`] is dropped, its
//! input closes, and the helper exits: root lasts that one operation.

pub mod proto;

use self::proto::{Entry, Reply, Request};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Why the helper did not start.
#[derive(Debug)]
pub enum StartError {
    /// The password prompt was dismissed or the password was wrong.
    NotGranted,
    /// Something else went wrong; the operation reports it.
    Failed(io::Error),
}

/// What an operation's steps as root may touch: what it copies or moves,
/// and the folders it puts things into.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    pub sources: Vec<PathBuf>,
    pub destinations: Vec<PathBuf>,
}

/// How a helper is started: with `pkexec` in the app, in this process in
/// tests.
pub type Start = fn(&Scope) -> Result<Helper, StartError>;

/// A running helper, there for one operation.
pub struct Helper {
    child: Option<Child>,
    to: Option<Box<dyn Write + Send>>,
    from: Box<dyn Read + Send>,
}

impl std::fmt::Debug for Helper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Helper").finish_non_exhaustive()
    }
}

/// Where the helper is: beside the app's own binary.
fn helper_path() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    Ok(dir.join("earth-files-helper"))
}

impl Helper {
    /// Starts the helper as root with `pkexec`, and tells it the scope.
    /// Blocking: it waits while polkit asks for the password.
    pub fn start(scope: &Scope) -> Result<Self, StartError> {
        let path = helper_path().map_err(StartError::Failed)?;
        let mut child = Command::new("pkexec")
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(StartError::Failed)?;
        let to = child
            .stdin
            .take()
            .map(|to| Box::new(to) as Box<dyn Write + Send>);
        let from = match child.stdout.take() {
            Some(from) => Box::new(from) as Box<dyn Read + Send>,
            None => return Err(StartError::Failed(io::Error::other("no helper output"))),
        };
        let mut helper = Self {
            child: Some(child),
            to,
            from,
        };
        match helper.scope(scope) {
            Ok(()) => Ok(helper),
            Err(err) => {
                // `pkexec` exits 126 when the prompt was dismissed and 127
                // when the password was refused, before the helper runs
                drop(helper.to.take());
                let status = helper.child.take().and_then(|mut child| child.wait().ok());
                match status.and_then(|status| status.code()) {
                    Some(126 | 127) => Err(StartError::NotGranted),
                    _ => Err(StartError::Failed(err)),
                }
            }
        }
    }

    /// Runs the helper's loop on a thread of this process, as the current
    /// user: for tests, where there is no `pkexec` and no root.
    pub fn in_process(scope: &Scope) -> Result<Self, StartError> {
        let (request_read, request_write) = io::pipe().map_err(StartError::Failed)?;
        let (reply_read, reply_write) = io::pipe().map_err(StartError::Failed)?;
        std::thread::spawn(move || {
            let _ = proto::serve(request_read, reply_write);
        });
        let mut helper = Self {
            child: None,
            to: Some(Box::new(request_write)),
            from: Box::new(reply_read),
        };
        helper.scope(scope).map_err(StartError::Failed)?;
        Ok(helper)
    }

    fn scope(&mut self, scope: &Scope) -> io::Result<()> {
        self.send(&Request::Scope {
            sources: scope.sources.clone(),
            destinations: scope.destinations.clone(),
        })?;
        match proto::read_reply(&mut self.from)? {
            Reply::Done => Ok(()),
            reply => Err(unexpected(&reply)),
        }
    }

    fn send(&mut self, request: &Request) -> io::Result<()> {
        let to = self
            .to
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        proto::write_request(to, request)
    }

    /// Carries out a step that answers Done.
    pub fn call(&mut self, request: &Request) -> io::Result<()> {
        self.send(request)?;
        match proto::read_reply(&mut self.from)? {
            Reply::Done => Ok(()),
            reply => Err(unexpected(&reply)),
        }
    }

    /// Copies a file as root, telling `progress` the bytes copied so far.
    /// When `progress` answers `false` the copy is asked to stop; one that
    /// stopped fails with `ECANCELED` and leaves nothing behind, one that
    /// had finished anyway counts.
    pub fn copy(
        &mut self,
        from: &Path,
        to: &Path,
        mut progress: impl FnMut(u64) -> bool,
    ) -> io::Result<u64> {
        self.send(&Request::Copy {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        })?;
        let mut stopping = false;
        loop {
            match proto::read_reply(&mut self.from)? {
                Reply::Progress(bytes) => {
                    if !progress(bytes) && !stopping {
                        stopping = true;
                        self.send(&Request::Stop)?;
                    }
                }
                Reply::Copied(bytes) => {
                    // The stop came after: it is answered on its own
                    if stopping {
                        self.expect_done()?;
                    }
                    return Ok(bytes);
                }
                // Failed for its own reason before it saw the stop: the stop
                // is answered on its own too, and that answer is read here,
                // not taken by the next request as its own
                Reply::Failed(errno) if stopping && errno != libc::ECANCELED => {
                    let _ = self.expect_done();
                    return Err(io::Error::from_raw_os_error(errno));
                }
                reply => return Err(unexpected(&reply)),
            }
        }
    }

    fn expect_done(&mut self) -> io::Result<()> {
        match proto::read_reply(&mut self.from)? {
            Reply::Done => Ok(()),
            reply => Err(unexpected(&reply)),
        }
    }

    /// Lists a folder as root.
    pub fn list(&mut self, dir: &Path) -> io::Result<Vec<Entry>> {
        self.send(&Request::List(dir.to_path_buf()))?;
        match proto::read_reply(&mut self.from)? {
            Reply::Listed(entries) => Ok(entries),
            reply => Err(unexpected(&reply)),
        }
    }
}

/// A reply that was not the one asked for: the helper's error, or a broken
/// exchange.
fn unexpected(reply: &Reply) -> io::Error {
    match reply {
        Reply::Failed(errno) => io::Error::from_raw_os_error(*errno),
        _ => io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected reply from the helper",
        ),
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        // Its input closing is what ends it; then it is waited for, so no
        // root process outlives the operation
        drop(self.to.take());
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_helper_in_process_does_steps_and_reports_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("f");
        fs::write(&file, b"abc").expect("write");
        let out = dir.path().join("out");
        fs::create_dir(&out).expect("mkdir");
        let scope = Scope {
            sources: vec![file.clone()],
            destinations: vec![out.clone()],
        };

        let mut helper = Helper::in_process(&scope).expect("start");
        let mut seen = 0;
        assert_eq!(
            helper
                .copy(&file, &out.join("f"), |bytes| {
                    seen = bytes;
                    true
                })
                .expect("copy"),
            3
        );
        assert_eq!(fs::read(out.join("f")).expect("read"), b"abc");
        let err = helper
            .call(&Request::Remove(dir.path().join("elsewhere")))
            .expect_err("outside the scope");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        drop(helper);
        let _ = seen;
    }

    /// A copy that fails for its own reason as the stop goes out: the
    /// stop's own answer is read with it, so the next request gets its own
    #[test]
    fn a_failure_racing_a_stop_keeps_the_answers_in_step() {
        let mut replies = Vec::new();
        for reply in [
            Reply::Progress(8),
            Reply::Failed(libc::EIO),
            // The stop's, sent after the failure
            Reply::Done,
            // The next request's
            Reply::Failed(libc::EACCES),
        ] {
            proto::write_reply(&mut replies, &reply).expect("reply");
        }
        let mut helper = Helper {
            child: None,
            to: Some(Box::new(Vec::new())),
            from: Box::new(io::Cursor::new(replies)),
        };
        let err = helper
            .copy(Path::new("/a"), Path::new("/b"), |_| false)
            .expect_err("failed");
        assert_eq!(err.raw_os_error(), Some(libc::EIO));
        let err = helper
            .call(&Request::Remove(PathBuf::from("/c")))
            .expect_err("the next request's own answer");
        assert_eq!(err.raw_os_error(), Some(libc::EACCES));
    }

    /// A copy told to stop at its first report stops, leaves nothing, and
    /// the helper goes on answering; a stop with no copy running is
    /// answered on its own
    #[test]
    fn a_copy_stops_when_asked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("big");
        // Larger than one progress report
        fs::write(&file, vec![0u8; 24 << 20]).expect("write");
        let out = dir.path().join("out");
        fs::create_dir(&out).expect("mkdir");
        let scope = Scope {
            sources: vec![file.clone()],
            destinations: vec![out.clone()],
        };
        let mut helper = Helper::in_process(&scope).expect("start");

        let err = helper
            .copy(&file, &out.join("big"), |_| false)
            .expect_err("stopped");
        assert_eq!(err.raw_os_error(), Some(libc::ECANCELED));
        assert!(!out.join("big").exists(), "nothing is left behind");

        helper
            .call(&Request::Stop)
            .expect("an idle stop is answered");
        helper
            .copy(&file, &out.join("big"), |_| true)
            .expect("the next copy runs");
        assert_eq!(
            fs::metadata(out.join("big")).expect("copied").len(),
            24 << 20
        );
    }
}
