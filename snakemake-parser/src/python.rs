//! Parses the Python parts of a Snakefile with ruff, keeping their offsets in the Snakefile.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_python_ast::Suite;
use ruff_python_parser::{ParseError, parse_module};
use ruff_text_size::{TextRange, TextSize};

use crate::Error;

/// Parses the Python code around the Snakemake statements, which become `...` placeholders.
pub(crate) fn module(source: &str, statements: &[TextRange]) -> Result<Suite, Error> {
    let mut bytes = source.as_bytes().to_vec();
    for range in statements {
        let region = &mut bytes[range.start().to_usize()..range.end().to_usize()];
        // Every statement starts with an ASCII keyword of at least three letters.
        region[..3].copy_from_slice(b"...");
        for byte in &mut region[3..] {
            if !matches!(byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    }
    let text = String::from_utf8(bytes).expect("blanking keeps the text valid UTF-8");
    let whole = TextRange::up_to(TextSize::of(source));
    let parsed = parse_module(&text).map_err(|error| located(source, &error, whole))?;
    Ok(parsed.into_syntax().body)
}

/// Moves an error from the synthetic text into `range` of the Snakefile.
fn located(source: &str, error: &ParseError, range: TextRange) -> Error {
    let location = if range.contains_range(error.location) {
        error.location
    } else {
        TextRange::empty(error.location.start().clamp(range.start(), range.end()))
    };
    Error::new(source, error.error.to_string(), location)
}
