//! Ops: the common op fields, the defined op kinds `task`, `call` and `native`, and attributes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::{Deserialize, Serialize};

use crate::document::{Language, Port};
use crate::reserved;

/// One op of a graph. It starts once every connected input is bound and every dependency in
/// `after` holds; an unconnected optional input is bound to null.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Op {
    /// Unique in its graph and stable across edits; segments of letters, digits, `_`, `-`, `.`
    /// joined by `/`. Run settings and provenance refer to the op by `<graph>/<path>`.
    pub path: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Port>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<Port>,
    /// Control and status dependencies on other ops of the same graph; all must hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<Dependency>,
    /// What the op does besides binding its outputs. An op with effects is a root for pruning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Attributes::is_empty")]
    pub attributes: Attributes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

/// What an op does, with the fields of its kind. `task`, `call`, `native`, `map`, `expr`,
/// `branch` and `select`, and `merge` and `flatten` on values, have defined semantics in this
/// version; the others are reserved, see [`reserved`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Kind {
    Task(Task),
    Call(Call),
    Native(Native),
    Expr(Expr),
    Map(Map),
    Collect(reserved::Collect),
    Filter(reserved::Filter),
    Flatten(reserved::Flatten),
    Merge(reserved::Merge),
    Zip(reserved::Zip),
    Cross(reserved::Cross),
    Group(reserved::Group),
    Join(reserved::Join),
    Branch(Branch),
    Select(reserved::Select),
    Loop(reserved::Loop),
    Dynamic(reserved::Dynamic),
}

impl Kind {
    /// Whether this kind is reserved: it parses and verifies, but no runner of this version
    /// runs it.
    pub fn is_reserved(&self) -> bool {
        !matches!(
            self,
            Kind::Task(_)
                | Kind::Call(_)
                | Kind::Native(_)
                | Kind::Map(_)
                | Kind::Expr(_)
                | Kind::Branch(_)
                | Kind::Select(_)
                | Kind::Merge(_)
                | Kind::Flatten(_)
        )
    }
}

/// One container command. It succeeds when its exit code is in `success_codes` and every
/// non-optional output was captured; otherwise it fails and binds no output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// The container image; none runs the command on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The argument vector, run without a shell, in a fresh working folder.
    pub command: Vec<Argument>,
    /// How each output is read once the command has ended. Every output has one capture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captures: Vec<Capture>,
    /// Exit codes that count as success; empty means only 0.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub success_codes: Vec<i32>,
    /// Whether the command may use the network; without it the task runs without network,
    /// so that runs can share its results. It is part of the task, unlike the attributes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub network: bool,
    /// The file in the working folder that standard output goes to, like CWL `stdout`; a
    /// `stdout` capture then reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<FileName>,
    /// The file that standard error goes to, like CWL `stderr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<FileName>,
    /// The absolute path of the working folder in the container, like the output folder of
    /// CWL; without it the runner picks one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
}

/// One command argument: literal text, or the value of a task input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Argument {
    Text(String),
    /// A scalar becomes its text, a File or Directory its path, an array one argument per
    /// item, and a null no argument. `separator` joins the items of an array into one.
    Input {
        input: String,
        /// Comes before the value; with it, false and an empty array give no argument and
        /// true only the prefix, as in CWL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        /// Whether the prefix is an argument of its own, else joined to the value.
        #[serde(default = "separate", skip_serializing_if = "Clone::clone")]
        separate: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        separator: Option<String>,
    },
}

/// The name of a file in the working folder: text, or the string value of a task input, where
/// null or an empty string names none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum FileName {
    Text(String),
    Input { input: String },
}

fn separate() -> bool {
    true
}

impl Argument {
    /// An argument of the plain value of an input.
    pub fn input(name: &str) -> Argument {
        Argument::Input {
            input: name.to_owned(),
            prefix: None,
            separate: true,
            separator: None,
        }
    }
}

/// How one task output is read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub output: String,
    pub from: CaptureRule,
    /// Whether each File it reads holds its text as `contents`, as CWL `loadContents`; a
    /// file over 64 KiB fails the task.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub load_contents: bool,
}

/// Where a captured output comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureRule {
    /// Paths in the working folder that match the pattern, sorted by path. No match binds null
    /// to an optional output and fails the task otherwise; several need an array output.
    Glob(String),
    /// Paths that match any pattern that a task input holds, a string or an array of strings,
    /// in the order of the patterns and sorted by path for each.
    Patterns(String),
    Stdout,
    Stderr,
}

/// A pure CWL expression over the op's inputs, which binds its one output.
/// Languages other than `js` stay reserved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expr {
    pub language: String,
    /// Text with `$(...)` parts, or a `${...}` function body, as a CWL expression.
    pub code: String,
    /// Code that runs before it, like CWL `expressionLib`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub library: Vec<String>,
}

/// Runs the `body` graph, like a call, when the bool input `condition` is true; otherwise the
/// op ends skipped with every output null.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    pub body: String,
    pub condition: String,
}

/// Runs the `body` graph once per round: each round takes items of the array inputs in `over`
/// and the other inputs as they are. Each output is the array of what the rounds gave, in
/// round order. `parallelism` and `min_success` stay reserved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Map {
    pub body: String,
    pub over: Vec<String>,
    #[serde(default, skip_serializing_if = "Scatter::is_dot")]
    pub mode: Scatter,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_success: Option<u32>,
}

/// How a `map` makes rounds of several inputs, as CWL `scatterMethod`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scatter {
    /// Item i of every input in round i; the inputs have equal lengths.
    #[default]
    Dot,
    /// Every combination, the first input varying slowest; flat outputs.
    FlatCross,
    /// Every combination, with outputs nested one level per input.
    NestedCross,
}

impl Scatter {
    pub fn is_dot(&self) -> bool {
        *self == Scatter::Dot
    }
}

/// Runs a named graph of the same document with the op's inputs. The op's ports have the
/// graph's port names; its outputs are bound when the whole graph has finished.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub graph: String,
}

/// A block that the language runtime runs as a whole, such as a complete CWL workflow. The
/// op's ports are its boundary: inputs go in by name, outputs are exported by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Native {
    /// The id of a document source; its language must equal `language`.
    pub source: String,
    pub language: Language,
    /// What to run inside the source: a workflow name, an entry point or a process id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// Profiles of the language, like Nextflow `-profile`, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
    /// Ids of document sources that configure the run, like Nextflow `-c` files, in the order
    /// read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configs: Vec<String>,
}

/// The op runs only after `op` has ended with one of the statuses in `on`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub op: String,
    pub on: Vec<Status>,
}

/// How an op ended, like the Argo task statuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Succeeded,
    /// It ran and was not accepted, such as an exit code outside `success_codes`.
    Failed,
    /// It could not run, such as a missing image or a lost node.
    Errored,
    /// Its own condition was false.
    Skipped,
    /// It did not run because a dependency did not hold.
    Omitted,
}

/// Something an op does that its outputs do not show. Effectful work stays in `task` or
/// `native` ops; `expr` is pure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Effect {
    /// Copies results to a place outside the run, such as a Nextflow `publishDir`.
    Publish(String),
    /// Changes state outside the run, such as a database or an upload target.
    External(String),
    /// Outputs may differ between runs with equal inputs.
    Nondeterministic,
}

/// Run settings that never enter a task cache key. A binding file per run may set them by
/// op path without changing the document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attributes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<Cache>,
    /// A site name or a placement rule; explicit placement never falls back silently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    /// How many more attempts after a failed or errored one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<u32>,
    /// The time limit of one attempt, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
}

/// Whether earlier results may be reused. No setting overrides a source's own veto on reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cache {
    /// Reuse a result with an equal task cache key.
    Default,
    Never,
    /// Reuse only results of the run that is resumed.
    Resume,
}

/// What one attempt needs. Memory and disk are in bytes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpus: Option<u32>,
}

/// Where an op came from, for provenance: a source id and a place inside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub source: String,
    /// A step id, rule name, process name or template name in the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

impl Attributes {
    pub fn is_empty(&self) -> bool {
        *self == Attributes::default()
    }
}
