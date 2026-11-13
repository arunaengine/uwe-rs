//! Source positions, error reports and line numbers.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

/// A byte range in the source text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }

    /// The smallest span that covers both spans.
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn text(self, source: &str) -> &str {
        &source[self.start as usize..self.end as usize]
    }
}

/// An error found while reading the source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }
}

/// Maps byte offsets to 1-based line and column numbers.
pub struct LineIndex {
    line_starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        Self { line_starts }
    }

    /// The 1-based line and column of a byte offset.
    pub fn line_column(&self, offset: u32) -> (usize, usize) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next) => next - 1,
        };
        (line + 1, (offset - self.line_starts[line]) as usize + 1)
    }
}

/// A diagnostic with its position, ready to print.
pub struct Located<'a> {
    pub diagnostic: &'a Diagnostic,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Located<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}",
            self.line, self.column, self.diagnostic.message
        )
    }
}

impl LineIndex {
    pub fn locate<'a>(&self, diagnostic: &'a Diagnostic) -> Located<'a> {
        let (line, column) = self.line_column(diagnostic.span.start);
        Located {
            diagnostic,
            line,
            column,
        }
    }
}
