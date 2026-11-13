//! The error of a Snakefile that cannot be read, with its position.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_text_size::TextRange;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{line}:{column}: {message}")]
pub struct Error {
    pub message: String,
    pub range: TextRange,
    /// 1-based line of the start of `range`.
    pub line: usize,
    /// 1-based column of the start of `range`, counted in characters.
    pub column: usize,
}

impl Error {
    pub(crate) fn new(source: &str, message: impl Into<String>, range: TextRange) -> Self {
        let mut start = range.start().to_usize().min(source.len());
        while !source.is_char_boundary(start) {
            start -= 1;
        }
        let before = &source[..start];
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        Self {
            message: message.into(),
            range,
            line: before.matches('\n').count() + 1,
            column: before[line_start..].chars().count() + 1,
        }
    }
}
