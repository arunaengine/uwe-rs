//! Reads Nextflow scripts and config files, following the official strict syntax grammar.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod ast;
pub mod lexer;
mod parser;
mod span;
pub mod token;

pub use parser::{Parsed, parse_config, parse_script};

pub use span::{Diagnostic, LineIndex, Located, Span};
