use std::path::Path;
use std::{fs, io};

use crate::operation::OperationError;

use super::Controller;

// Special reader just for operations, handling cancel and progress
pub struct OpReader {
    file: fs::File,
    len: u64,
    current: u64,
    controller: Controller,
    /// Where this file's progress starts, and how much of the whole it is
    share: (f32, f32),
}

impl OpReader {
    pub fn new<P: AsRef<Path>>(path: P, controller: Controller) -> io::Result<Self> {
        let file = fs::File::open(&path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            file,
            len,
            current: 0,
            controller,
            share: (0.0, 1.0),
        })
    }

    /// Reads `file`, `len` long, as one of `count` parts of the operation,
    /// the `index`th.
    pub fn part(
        file: fs::File,
        len: u64,
        controller: Controller,
        index: usize,
        count: usize,
    ) -> Self {
        Self {
            file,
            len,
            current: 0,
            controller,
            share: (index as f32 / count as f32, 1.0 / count as f32),
        }
    }
}

impl io::Read for OpReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        crate::ui::iced::futures::executor::block_on(async {
            self.controller
                .check()
                .await
                .map_err(|s| io::Error::other(OperationError::from_state(s)))
        })?;

        let count = self.file.read(buf)?;
        self.current += count as u64;

        let progress = self.current as f32 / self.len as f32;
        let (start, share) = self.share;
        self.controller.set_progress(start + progress * share);

        Ok(count)
    }
}
