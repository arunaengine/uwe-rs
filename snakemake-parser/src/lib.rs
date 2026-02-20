//! Reads Snakefiles natively into a syntax tree, following Snakemake 9.27.0's parser.py.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod ast;
mod automaton;
mod error;
pub mod keyword;
mod python;
mod token;

pub use error::Error;
pub use {ruff_python_ast, ruff_text_size};

use crate::ast::{Snakefile, Statement};

/// Parses a Snakefile. Like Snakemake, it stops at the first error.
pub fn parse(source: &str) -> Result<Snakefile, Error> {
    let statements = automaton::statements(source)?;
    let ranges: Vec<_> = statements.iter().map(Statement::range).collect();
    let python = python::module(source, &ranges)?;
    Ok(Snakefile { python, statements })
}
