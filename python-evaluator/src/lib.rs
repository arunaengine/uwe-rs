//! An interpreter for the Python subset that workflow languages embed, like the Python code of
//! Snakefiles. It walks the syntax tree of ruff's parser, and gives native stand-ins for the
//! library modules that such code calls, `pandas.read_csv` included. It needs no Python
//! install. Code that uses a feature outside the subset fails with an unsupported error.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod host;

pub use host::{FileKind, Host, LocalHost, Stat};
