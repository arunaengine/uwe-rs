//! The tool definition: identity, ports, parameter schema, resources, tests and curation level.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// One version of a tool of the library. EDAM terms are short ids like `format_2572`; IRIs
/// and `edam:` names are accepted too.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub id: String,
    pub version: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
    /// An SPDX license expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<Author>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    /// The id of the tool in bio.tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub biotools: Option<String>,
    /// EDAM operations the tool does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<String>,
    /// EDAM topics of the tool.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    /// Free words that describe the tool, like the keywords of an nf-core module.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputPort>,
    /// The JSON Schema (draft 2020-12) of the parameter values, with their conditions such as
    /// `if`, `then`, `else` and `dependentRequired`; null for no parameters.
    #[serde(default, skip_serializing_if = "Json::is_null")]
    pub parameters: Json,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<OutputPort>,
    #[serde(default, skip_serializing_if = "Resources::is_empty")]
    pub resources: Resources,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<ToolTest>,
    #[serde(default)]
    pub curation: Curation,
}

/// Where the tool comes from, at an exact revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub kind: SourceKind,
    /// A repository, registry entry or file address.
    pub address: String,
    /// A commit, tag digest or file checksum.
    pub revision: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    NfCore,
    /// A Nextflow process outside an nf-core module.
    Nextflow,
    Snakemake,
    Cwl,
    Wdl,
    Galaxy,
    /// A container or script template of an Argo workflow or workflow template.
    Argo,
    Container,
    Other,
}

/// A container image, like `quay.io/biocontainers/samtools:1.21--h50ea8bc_0`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub name: String,
    /// The manifest digest, like `sha256:...`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Author {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orcid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Citation {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doi: Option<String>,
}

/// An input of the tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputPort {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: PortKind,
    #[serde(default)]
    pub cardinality: Cardinality,
    /// EDAM formats the port takes, any of them; empty takes every format.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default = "required")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Json>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<Json>,
    /// Data conditions the port needs, like sorting by coordinate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// A JSON Schema over the parameter values: the input is used only when they meet it, like a
    /// data input in one branch of a Galaxy conditional. None means always.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_when: Option<Json>,
}

/// An output of the tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputPort {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: PortKind,
    #[serde(default)]
    pub cardinality: Cardinality,
    #[serde(default)]
    pub format: OutputFormat,
    /// Data conditions every result of this output meets, like sorting by coordinate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guarantees: Vec<Condition>,
    /// A Python expression over the raw parameter values, as a Galaxy output filter: the output
    /// is created only when it is true. None means always.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_when: Option<String>,
    /// The output may be null, like a CWL output of type `File?`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// The JSON Schema of the values of a value output, like the fields of a record.
    #[serde(default, skip_serializing_if = "Json::is_null")]
    pub schema: Json,
}

/// What a port carries: `"file"`, `"directory"` or `{"value": "integer"}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortKind {
    File,
    Directory,
    Value(ValueType),
}

/// The JSON type of a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    String,
    Integer,
    Number,
    Boolean,
    Object,
    Array,
}

/// How many items a port carries. A list keeps a sample id per item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    #[default]
    One,
    List,
    /// Two files of one sample, like the reads of paired-end sequencing.
    Pair,
}

/// The format of an output: `{"terms": [...]}` or `{"follows": "<input name>"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// EDAM formats, one of which the output has; empty when not declared.
    Terms(Vec<String>),
    /// The format of the data at the named input, as Galaxy's `format_source`.
    Follows(String),
}

/// A data condition between ports: `"indexed"`, `{"sorted": "coordinate"}`,
/// `{"same_reference": "<input>"}`, `{"paired_with": "<input>"}` or `{"other": "<text>"}`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    Sorted(SortKey),
    /// Has its index beside it, like a `.bai` file beside a BAM file.
    Indexed,
    /// Uses the same reference as the named input of the consuming tool.
    SameReference(String),
    /// Belongs to the same samples as the named input of the consuming tool.
    PairedWith(String),
    Other(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    Coordinate,
    Name,
}

/// Resource hints of one run of the tool. Memory is in bytes, time in seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpus: Option<u32>,
}

/// A test of the tool with small data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolTest {
    pub name: String,
    /// Input values and files by input name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, TestValue>,
    /// Expected results by output name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, Expected>,
    /// The last result per backend kind, like `slurm` or `kubernetes`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub results: BTreeMap<String, TestResult>,
}

/// A test input: `{"value": ...}`, `{"file": "<address>"}` or `{"files": [...]}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestValue {
    Value(Json),
    File(String),
    Files(Vec<String>),
}

/// What a test expects of an output; every given field must hold.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    /// The SHA-256 checksum of the file, in hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Texts the file contains.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contains: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Json>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestResult {
    pub passed: bool,
    /// When the test ran, in RFC 3339.
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// How far the library trusts a tool definition, from most to least.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curation {
    /// Its tests pass on real backends in the checks of the library.
    Curated,
    /// Converted automatically from a source, and its tests pass.
    Imported,
    #[default]
    Unchecked,
}

impl Definition {
    pub fn input(&self, name: &str) -> Option<&InputPort> {
        self.inputs.iter().find(|port| port.name == name)
    }

    pub fn output(&self, name: &str) -> Option<&OutputPort> {
        self.outputs.iter().find(|port| port.name == name)
    }
}

impl Default for OutputFormat {
    fn default() -> OutputFormat {
        OutputFormat::Terms(Vec::new())
    }
}

impl Resources {
    pub fn is_empty(&self) -> bool {
        *self == Resources::default()
    }
}

fn required() -> bool {
    true
}
