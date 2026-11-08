//! Reads a text one line at a time, so that a large file is never whole in memory.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::BufRead;

/// The lines of a reader, each with its `\n`. Bytes that are not UTF-8 become U+FFFD as in
/// `String::from_utf8_lossy`; a `\n` never ends such bytes, so lines decode as the whole text.
pub struct Lines {
    reader: Box<dyn BufRead>,
    buffer: Vec<u8>,
}

impl Lines {
    pub fn new(reader: Box<dyn BufRead>) -> Lines {
        Lines {
            reader,
            buffer: Vec::new(),
        }
    }

    /// The lines of a text in memory.
    pub fn text(text: String) -> Lines {
        Lines::new(Box::new(std::io::Cursor::new(text.into_bytes())))
    }

    /// The next line, or `None` at the end of the text.
    pub fn next_line(&mut self) -> std::io::Result<Option<String>> {
        self.buffer.clear();
        if self.reader.read_until(b'\n', &mut self.buffer)? == 0 {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&self.buffer).into_owned()))
    }
}
