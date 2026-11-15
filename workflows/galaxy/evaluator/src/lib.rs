//! Renders the Cheetah templates of Galaxy tools in Rust: no Python process. Python expressions
//! run on python-evaluator, and unsupported features fail with the tool and line.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod lexer;
pub mod parser;
pub mod sanitize;
pub mod values;

/// Why a template could not be rendered.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("line {line}: {message}")]
    Template { line: usize, message: String },
    #[error("line {line}: {source}")]
    Python {
        line: usize,
        #[source]
        source: python_evaluator::Error,
    },
    #[error("{0}")]
    Input(String),
    #[error("tool {tool}, {part}: {source}")]
    Tool {
        tool: String,
        part: String,
        #[source]
        source: Box<Error>,
    },
}

impl Error {
    pub(crate) fn template(line: usize, message: String) -> Error {
        Error::Template { line, message }
    }
}
