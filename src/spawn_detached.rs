use std::{io, process};

// This code is from the open crate and retains its MIT license.
pub fn spawn_detached(command: &mut process::Command) -> io::Result<()> {
    command
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null());

    unsafe {
        use std::os::unix::process::CommandExt as _;

        command
            .pre_exec(move || {
                match libc::fork() {
                    -1 => return Err(io::Error::last_os_error()),
                    0 => (),
                    _ => libc::_exit(0),
                }

                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }

                Ok(())
            })
            .spawn()?
            .wait()
            .map(|_| ())
    }
}
