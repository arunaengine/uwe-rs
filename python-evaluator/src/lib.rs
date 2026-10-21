//! Interprets workflow Python and library stand-ins without Python; unsupported features fail
//! explicitly.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod builtins;
pub mod complex;
pub mod entries;
pub mod error;
pub mod format;
pub mod hashing;
pub mod host;
pub mod interpreter;
pub mod methods;
pub mod modules;
pub mod number;
pub mod packages;
pub mod pandas;
mod set_table;
pub mod types;
pub mod value;
pub mod views;

pub use error::{Error, ErrorKind, Location};
pub use host::{Command, CommandOutcome, FileKind, Host, LocalHost, Stat};
pub use interpreter::{AttributeHook, Frame, FrameKind, Interpreter, Source, StatementHook};
pub use value::{Args, Dict, Key, Name, Native, Scope, Value};
pub use {ruff_python_ast, ruff_text_size};
