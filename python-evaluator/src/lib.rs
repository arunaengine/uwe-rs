//! An interpreter for the Python subset that workflow languages embed, like the Python code of
//! Snakefiles. It walks the syntax tree of ruff's parser, and gives native stand-ins for the
//! library modules that such code calls, `pandas.read_csv` included. It needs no Python
//! install. Code that uses a feature outside the subset fails with an unsupported error.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod builtins;
pub mod complex;
pub mod error;
pub mod format;
pub mod host;
pub mod interpreter;
pub mod methods;
pub mod modules;
pub mod number;
pub mod packages;
pub mod pandas;
pub mod types;
pub mod value;

pub use error::{Error, ErrorKind, Location};
pub use host::{Command, CommandOutcome, FileKind, Host, LocalHost, Stat};
pub use interpreter::{AttributeHook, Frame, FrameKind, Interpreter, Source, StatementHook};
pub use value::{Args, Dict, Key, Name, Native, Scope, Value};
pub use {ruff_python_ast, ruff_text_size};
