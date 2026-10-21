//! Loads CWL documents into a typed model and reports errors with their file and position.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod error;
pub mod iri;
mod load;
pub mod model;
pub mod node;
pub mod salad;
pub mod source;
pub mod yaml;

pub use error::Error;
pub use load::{Document, Loader, load_file};
pub use node::{Entry, Location, Node, Value};
