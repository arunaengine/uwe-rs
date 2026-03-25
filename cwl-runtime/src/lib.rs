//! Runs CWL documents as a driver of the execution graph: values with exact numbers, File and
//! Directory objects, parameter references and JavaScript expressions, the command line of
//! tools, and workflows with scatter, conditions and subworkflows.
//!
//! [`driver`] is the entry point: it loads the document of a [`workflow_graph::Launch`], takes
//! its input object from the job file (or placeholders from the input types when there is
//! none), and asks for one task per command line tool run. Expression tools run inside the
//! driver. Features iunctus does not support fail with [`workflow_graph::DriverError`]s that
//! are marked unsupported, which gives exit code 33.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod binding;
pub mod expression;
pub mod files;
mod javascript;
pub mod types;
pub mod value;

pub use expression::{Evaluator, Variables};
pub use value::{Map, Value};
