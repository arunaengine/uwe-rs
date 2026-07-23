//! The inputs of a workflow as one JSON Schema (decision 24). Every driver describes the inputs
//! of its workflow as a [`workflow_graph::Inputs`]: a JSON Schema of draft 2020-12, the values
//! of the run, and the inputs that the workflow code reads. This crate checks the values
//! against the schema before any task starts ([`check`]), compares the schema with what the
//! code reads ([`compare`]), makes a schema from default values ([`infer`]), brings schemas of
//! older drafts to 2020-12 ([`upgrade`]), tells whether a schema is valid ([`valid`]), and writes
//! the inputs as text for help ([`text`]).
//!
//! Plain JSON Schema keywords describe types, enums, defaults, `title` and `description`. Files
//! and directories use the keywords of nf-schema, which nf-core pipelines use:
//!
//! - `format`: `file-path` for a file, `directory-path` for a directory, `path` for either, and
//!   `file-path-pattern` for a glob pattern of files;
//! - `exists`: `true` when the file or directory must exist before the run;
//! - `mimetype`: the media type of a file, like `text/csv`;
//! - `schema`: the schema of the content of a file, like the rows of a sample sheet: an array of
//!   objects with typed columns. nf-schema names a schema file there; iunctus puts the schema
//!   itself there, so that the schema of a workflow needs no other files.
//!
//! Two keywords are iunctus's own, for what CWL declares and JSON Schema has no word for:
//!
//! - `fileFormat`: the format of a file as the IRI of a concept, like
//!   `http://edamontology.org/format_1930`, or a list of such IRIs;
//! - `secondaryFiles`: the patterns of the files that come with a file, like `.bai`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod check;
mod compare;
mod counts;
mod infer;
mod paths;
mod properties;
mod text;
mod upgrade;
mod valid;
mod values;

pub use check::{Problem, Severity, check};
pub use compare::compare;
pub use counts::{Counts, OutputCounts};
pub use infer::{infer, value_schema};
pub use properties::{Property, properties};
pub use text::{outputs_text, text};
pub use upgrade::upgrade;
pub use valid::valid;

/// The draft of every schema that iunctus makes.
pub const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

/// The `format` of a text that names a file.
pub const FILE_PATH: &str = "file-path";

/// The `format` of a text that names a directory.
pub const DIRECTORY_PATH: &str = "directory-path";

/// The `format` of a text that names a file or a directory.
pub const PATH: &str = "path";

/// The `format` of a text that is a glob pattern of files.
pub const FILE_PATH_PATTERN: &str = "file-path-pattern";

/// Whether a text is an address of a remote file, like `https://...` or `s3://...`, or a
/// `data:` address that holds the content itself. The check does not look at such files yet.
pub fn is_remote(text: &str) -> bool {
    text.contains("://") || text.starts_with("data:")
}
