//! The events file of nf-biffix: one JSON event per line, which biffix replays later.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

/// The events file of a run, truncated by its first write.
pub(super) struct Journal {
    path: PathBuf,
    file: Option<File>,
    /// A write failed, so no more are tried.
    failed: bool,
}

impl Journal {
    pub(super) fn new(path: PathBuf) -> Journal {
        Journal {
            path,
            file: None,
            failed: false,
        }
    }

    /// Appends one event and flushes it.
    pub(super) fn write(&mut self, line: &str) {
        if self.failed {
            return;
        }
        let written = match &mut self.file {
            Some(file) => writeln!(file, "{line}").and_then(|()| file.flush()),
            None => File::create(&self.path).and_then(|mut file| {
                writeln!(file, "{line}")?;
                file.flush()?;
                self.file = Some(file);
                Ok(())
            }),
        };
        if let Err(error) = written {
            self.failed = true;
            tracing::warn!(
                "nf-biffix: writing the events file {} failed; it is disabled for the rest of \
                 the run: {error}",
                self.path.display()
            );
        }
    }
}
