//! Reads Snakefiles natively into a syntax tree, following Snakemake 9.27.0's parser.py.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod ast;
pub mod keyword;

pub use {ruff_python_ast, ruff_text_size};
