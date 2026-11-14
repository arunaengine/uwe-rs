//! Tool definitions of a tool library and the checks of how their ports connect.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod conditions;
pub mod connection;
pub mod definition;
pub mod edam;
pub mod finding;

pub use conditions::{Evidence, Fact};
pub use connection::{Check, FormatMatch, Shape, Supply};
pub use definition::{
    Author, Cardinality, Citation, Condition, Curation, Definition, Expected, Image, InputPort,
    OutputFormat, OutputPort, PortKind, Resources, SortKey, Source, SourceKind, TestResult,
    TestValue, ToolTest, ValueType,
};
pub use finding::{Aspect, Finding, Fit};
