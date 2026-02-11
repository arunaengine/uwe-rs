//! Lexer, parser and syntax tree for Nextflow scripts and config files.
//!
//! The grammar follows the official strict syntax grammar in
//! `modules/nf-lang/src/main/antlr` of the Nextflow repository.

pub mod lexer;
mod span;
pub mod token;

pub use span::{Diagnostic, LineIndex, Located, Span};
