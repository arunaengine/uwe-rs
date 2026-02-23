//! Loads CWL documents into a typed model and reports errors with their file and position.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod error;
pub mod node;
pub mod yaml;

pub use error::Error;
pub use node::{Entry, Location, Node, Value};
