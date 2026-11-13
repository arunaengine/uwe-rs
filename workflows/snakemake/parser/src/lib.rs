//! Reads Snakefiles natively into a syntax tree, following Snakemake 9.27.0's parser.py.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod ast;
mod automaton;
mod error;
pub mod keyword;
mod module;
mod python;
mod rule;
mod token;

pub use error::Error;
pub use {ruff_python_ast, ruff_text_size};

use crate::ast::{Snakefile, Statement};

/// Parses a Snakefile. Like Snakemake, it stops at the first error.
pub fn parse(source: &str) -> Result<Snakefile, Error> {
    let (statements, deferred) = automaton::statements(source)?;
    let ranges: Vec<_> = statements.iter().map(Statement::range).collect();
    // parser.py compiles its whole output at once, so the first Python error in the file wins.
    match (python::module(source, &ranges), deferred) {
        (Ok(python), None) => Ok(Snakefile { python, statements }),
        (Ok(_), Some(error)) | (Err(error), None) => Err(error),
        (Err(first), Some(second)) if second.range.start() < first.range.start() => Err(second),
        (Err(first), Some(_)) => Err(first),
    }
}
