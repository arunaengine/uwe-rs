//! wir, the workflow intermediate representation: one typed graph format for workflow languages.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod document;
mod flow;
mod op;
pub mod reserved;
mod types;

pub use document::{
    Document, Edge, Endpoint, Graph, Language, ParseError, Port, Source, VERSION, parse,
};
pub use flow::Flow;
pub use op::{
    Argument, Attributes, Cache, Call, Capture, CaptureRule, Dependency, Effect, Kind, Native, Op,
    Origin, Resources, Status, Task,
};
pub use types::{
    ArrayType, Field, FileType, KeyType, MapType, SecondaryFile, Type, TypeDef, resolve,
};
