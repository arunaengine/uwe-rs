//! The inputs of a workflow as one JSON Schema, with the values of a run (decision 24).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

/// What a driver tells about the inputs of its workflow: their JSON Schema (draft 2020-12),
/// the values of this run, and the inputs that the workflow code reads. The `input-schema`
/// crate checks the values against the schema before any task starts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inputs {
    /// The JSON Schema of the inputs: an object with one property per input.
    pub schema: Value,
    /// The values of this run by input name, in the form that the schema describes: files and
    /// directories are paths or addresses. A language where `null` stands for no value leaves
    /// such values out.
    pub values: Value,
    /// The folder that relative paths in the values start from.
    pub base: PathBuf,
    /// The names of the inputs that the workflow code reads, as the driver finds them in the
    /// code; `None` when it cannot tell.
    pub read: Option<BTreeSet<String>>,
    /// Whether the run makes up a value for a required input that has none, as a simulated
    /// run of an Argo workflow or of a CWL document without a job file does. A missing value
    /// is then only a warning.
    pub assumes_values: bool,
    /// Whether the schema only describes the inputs, and the workflow itself does not hold its
    /// values to it: a schema made from default values, or one that no code of the workflow
    /// checks. A check then warns and never stops the run.
    pub advisory: bool,
    /// The inputs whose values the workflow leaves unchecked on purpose, like nf-schema's
    /// `validation.ignoreParams`: a value counts for a required input, but a check reports no
    /// problem of it.
    pub ignored: BTreeSet<String>,
}
