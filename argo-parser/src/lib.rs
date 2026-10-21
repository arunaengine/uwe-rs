//! Loads Argo Workflows documents (Workflow, WorkflowTemplate, ClusterWorkflowTemplate and
//! CronWorkflow) from YAML and JSON into a typed model, following Argo v4.1.4.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod decode;
pub mod model;
mod node;
mod scalar;
mod tree;

pub use decode::{DecodeError, UnknownField, decode};
pub use node::{Entry, Node, Position, Value};
pub use scalar::resolve_plain;
pub use tree::{SyntaxError, parse_documents, parse_yaml};
