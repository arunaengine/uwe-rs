//! Loads CWL documents into a typed model and reports errors with their file and position.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub mod binding;
mod error;
pub mod expression;
pub mod iri;
pub mod node;
pub mod parameter;
pub mod process;
pub mod record;
pub mod requirement;
pub mod salad;
pub mod schema;
pub mod source;
pub mod workflow;
pub mod yaml;

pub use error::Error;
pub use node::{Entry, Location, Node, Value};
pub use process::{Class, Process, Version};
