//! Reads Nextflow scripts and config files, following the official strict syntax grammar.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod ast;
pub mod lexer;
mod span;
pub mod token;

pub use span::{Diagnostic, LineIndex, Located, Span};
