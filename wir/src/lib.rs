//! wir, the workflow intermediate representation: one typed graph format for workflow languages.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod document;
mod flow;
mod op;
pub mod reserved;
mod types;
mod value;
mod verify;

pub use document::{
    Document, Edge, Endpoint, Graph, Language, ParseError, Port, SCHEMA, Source, VERSION, parse,
};
pub use flow::{Flow, Subscription};
pub use op::{
    Argument, Attributes, Branch, Cache, Call, Capture, CaptureRule, Dependency, Effect, Expr,
    Kind, Map, Native, Op, Origin, Resources, Scatter, Status, Task,
};
pub use types::{
    ArrayType, Field, FileType, KeyType, MapType, SecondaryFile, Type, TypeDef, resolve,
};
pub use value::{DirectoryValue, Entry, FileValue, fits, secondary_name};
pub use verify::{Problem, VerifyError, verify};
