//! Waiting on a child process with a deadline.

use std::io::{BufRead, BufReader};
use std::process::{Child, ExitStatus};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How often [`wait_with_timeout`] asks whether the child has exited.
///
/// Short enough that a child which finishes normally is not held up by the
/// polling, long enough that waiting costs nothing.
const POLL: Duration = Duration::from_millis(10);

/// Waits for `child`, killing it if it outlives `timeout`.
///
/// Returns `None` when the deadline was reached, having killed and reaped the
/// child. [`Child::wait`] has no deadline of its own, so this polls:
/// `try_wait` is a `waitpid` that does not block, and between tries this
/// thread sleeps rather than spins.
///
/// This does not drain the child's pipes, so it must not be used on a child
/// whose output can outgrow a pipe buffer: such a child blocks writing and
/// never exits, and the deadline turns what should be a wait into a kill.
pub fn wait_with_timeout(
    child: &mut Child,
    timeout: Duration,
) -> std::io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= deadline {
            child.kill()?;
            // Reap it. A killed child that is never waited for stays a zombie
            // for as long as this process lives, which is the leak the
            // deadline exists to avoid.
            child.wait()?;
            return Ok(None);
        }
        std::thread::sleep(POLL.min(deadline - now));
    }
}

/// Sends each line `child` writes to its stderr to the debug log, prefixed
/// with `label`. `child` must have been spawned with a piped stderr; without
/// one this does nothing and returns `None`.
///
/// The pipe is read on a thread of its own, so a chatty child can never fill
/// it and block while [`wait_with_timeout`] is waiting on it. The thread ends
/// when the pipe closes, i.e. once the child (and anything it passed the pipe
/// on to) has exited.
pub fn log_stderr(child: &mut Child, label: String) -> Option<JoinHandle<()>> {
    let stderr = child.stderr.take()?;
    let name = label.clone();
    thread::Builder::new()
        .name("child-stderr".into())
        .spawn(move || {
            // Bytes, decoded lossily, rather than `lines()`: that fails on a
            // line that is not UTF-8, and giving up there would close the
            // pipe and kill the child with SIGPIPE on its next write.
            let mut stderr = BufReader::new(stderr);
            let mut line = Vec::new();
            while stderr
                .read_until(b'\n', &mut line)
                .is_ok_and(|read| read > 0)
            {
                let text = String::from_utf8_lossy(&line);
                log::debug!("{label}: {}", text.trim_end());
                line.clear();
            }
        })
        .inspect_err(|err| log::warn!("failed to read stderr of {name}: {err}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_that_never_exits_is_killed_and_reaped() {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("sleep should be on PATH");

        let started = Instant::now();
        let status =
            wait_with_timeout(&mut child, Duration::from_millis(100)).expect("waiting should work");

        assert!(status.is_none(), "a killed child has no exit status");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the deadline did not end the wait"
        );
        // Already reaped, so this cannot be waiting on a live zombie.
        assert!(
            child.try_wait().is_ok(),
            "the killed child was left unreaped"
        );
    }

    #[test]
    fn a_child_that_exits_in_time_keeps_its_status() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("true should be on PATH");

        let status = wait_with_timeout(&mut child, Duration::from_secs(30))
            .expect("waiting should work")
            .expect("the child exited well inside the deadline");

        assert!(status.success(), "`true` exits successfully");
    }

    #[test]
    fn a_child_whose_stderr_outgrows_the_pipe_still_exits() {
        // Four times a Linux pipe's default 64 KiB buffer: left unread, the
        // child would block writing and only the deadline would end it.
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "head -c 262144 /dev/zero | tr '\\0' 'x' | fold -w 80 >&2",
            ])
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("sh should be on PATH");

        let reader = log_stderr(&mut child, "test".into()).expect("stderr was piped");
        let status = wait_with_timeout(&mut child, Duration::from_secs(10))
            .expect("waiting should work")
            .expect("the child was blocked on a full stderr pipe");

        assert!(status.success());
        reader.join().expect("the reader thread should not panic");
    }

    #[test]
    fn a_child_whose_stderr_is_not_utf8_still_exits_cleanly() {
        // A reader that gave up on the invalid line would close the pipe,
        // and the writes after it would kill the child with SIGPIPE.
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "printf 'bad \\377\\n' >&2; \
                 head -c 262144 /dev/zero | tr '\\0' 'x' | fold -w 80 >&2",
            ])
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("sh should be on PATH");

        let reader = log_stderr(&mut child, "test".into()).expect("stderr was piped");
        let status = wait_with_timeout(&mut child, Duration::from_secs(10))
            .expect("waiting should work")
            .expect("the child was blocked on a full stderr pipe");

        assert!(status.success(), "the child did not exit cleanly: {status}");
        reader.join().expect("the reader thread should not panic");
    }

    #[test]
    fn a_child_without_a_piped_stderr_has_nothing_to_log() {
        let mut child = std::process::Command::new("true")
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("true should be on PATH");

        assert!(log_stderr(&mut child, "test".into()).is_none());
        child.wait().expect("waiting should work");
    }
}
