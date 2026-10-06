// SPDX-License-Identifier: GPL-3.0-only

//! The helper that does a copy or move's steps as root, started by the app
//! through `pkexec` for one operation. It includes only its own protocol
//! file; nothing else of the app runs as root. See
//! `src/operation/root/proto.rs`.

#[allow(dead_code)]
#[path = "../operation/root/proto.rs"]
mod proto;

use std::io;
use std::os::fd::AsFd;
use std::process::ExitCode;

fn main() -> ExitCode {
    if !proto::started_by_pkexec() {
        eprintln!("earth-files-helper: only the app starts this, through pkexec");
        return ExitCode::from(2);
    }
    // Unbuffered, so a copy can see a stop request the moment it comes
    let input = match io::stdin().as_fd().try_clone_to_owned() {
        Ok(fd) => std::fs::File::from(fd),
        Err(err) => {
            eprintln!("earth-files-helper: {err}");
            return ExitCode::FAILURE;
        }
    };
    match proto::serve(input, io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("earth-files-helper: {err}");
            ExitCode::FAILURE
        }
    }
}
