//! Waiting on a child process with a deadline.

use std::process::{Child, ExitStatus};
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
}
