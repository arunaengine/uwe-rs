//! Validates, compares, infers, upgrades, and describes workflow input schemas.
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

pub use check::{Diagnostic, Problem, Severity, Validation, check, validate, validate_json};
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
