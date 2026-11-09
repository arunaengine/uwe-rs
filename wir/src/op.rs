//! Ops: the common op fields, the defined op kinds `task`, `call` and `native`, and attributes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::closure::{Binding, Closure, ExprLanguage, Scope, ValueRules};
use crate::control::{Control, StatusGroup};
use crate::dialect::TaskBuilder;
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
    /// A boolean tree of status tests that must hold too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends: Option<Control>,
    /// The failures of other ops that this op handles once it succeeds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handles: Vec<Dependency>,
    /// What the op does besides binding its outputs. An op with effects is a root for pruning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Attributes::is_empty")]
    pub attributes: Attributes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// The rules the op compares, orders and adds values by.
    #[serde(default, skip_serializing_if = "ValueRules::is_wir")]
    pub values: ValueRules,
}

/// What an op does, with the fields of its kind. `task`, `call`, `native`, `map`, `expr`,
/// `branch` and `select`, and `merge` and `flatten` on values, have defined semantics in this
/// version; the others are reserved, see [`reserved`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::large_enum_variant, reason = "a parsed document holds few ops")]
pub enum Kind {
    Task(Task),
    Call(Call),
    Native(Native),
    Expr(Expr),
    Load(Load),
    Map(Map),
    Emit(reserved::Emit),
    Transform(reserved::Transform),
    FlatMap(reserved::FlatMap),
    Collect(reserved::Collect),
    ToList(reserved::ToList),
    Reduce(reserved::Reduce),
    First(reserved::First),
    Last(reserved::Last),
    Filter(reserved::Filter),
    Flatten(reserved::Flatten),
    Mix(reserved::Mix),
    Concat(reserved::Concat),
    Merge(reserved::Merge),
    Zip(reserved::Zip),
    Cross(reserved::Cross),
    Group(reserved::GroupTuple),
    GroupTuple(reserved::GroupTuple),
    Join(reserved::Join),
    Combine(reserved::Combine),
    Buffer(reserved::Buffer),
    Window(reserved::Window),
    Take(reserved::Take),
    Unique(reserved::Unique),
    Distinct(reserved::Distinct),
    Until(reserved::Until),
    IfEmpty(reserved::IfEmpty),
    Split(reserved::Split),
    CollectFile(reserved::CollectFile),
    View(reserved::View),
    Observe(reserved::Observe),
    Route(reserved::Route),
    MultiMap(reserved::MultiMap),
    Branch(Branch),
    Process(Process),
    StatusGroup(StatusGroup),
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
                | Kind::Load(_)
                | Kind::Branch(_)
                | Kind::Select(_)
                | Kind::Merge(_)
                | Kind::Flatten(_)
        )
    }
}

/// One container command. It succeeds when its exit code is in `success_codes`, every
/// capture worked and `accept` holds; otherwise it fails and binds no output.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// The container image; none runs the command on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The argument vector, run without a shell, in a fresh working folder, unless `shell`; a
    /// task with a builder has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<Argument>,
    /// Whether the arguments form one `/bin/sh` command line, each quoted apart from raw text
    /// and input values that are not quoted, like CWL `ShellCommandRequirement`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shell: bool,
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
    /// The absolute folder that the Files and Directories of the task inputs go to first, in
    /// the order of the op's inputs, each top-level one in a numbered folder of its own, like
    /// the input folder of CWL; without it, each goes below `inputs/<port>` when used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging: Option<String>,
    /// The task inputs that go to `staging`, in this order; without it every input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged: Option<Vec<String>>,
    /// What the working folder holds before the command starts, like CWL
    /// `InitialWorkDirRequirement`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Staged>,
    /// The environment variables of the command, by name; null sets none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, Content>,
    /// What standard input reads: a path, or a task input whose File gives its path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<Content>,
    /// The file whose JSON object, when the command writes it, gives the outputs in place of
    /// the captures, like CWL `cwl.output.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<String>,
    /// The task input whose object sets `cpus`, `memory` and `disk` in bytes, `gpus` and
    /// `time` in seconds over the attributes; a field that is null or missing keeps them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<String>,
    /// The JavaScript library that `code` arguments see, like the library of an expr.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub library: Vec<String>,
    /// The language runtime that builds the task, instead of the command form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder: Option<TaskBuilder>,
    /// A check of the outputs of an attempt that must hold for the task to succeed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept: Option<Closure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<Retry>,
}

/// When an attempt that is not accepted runs again, and how each attempt changes the task.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    /// More attempts after the first; the `retry` attribute wins when it is set.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub limit: u32,
    /// Whether to try again after a rejected attempt; true when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_if: Option<Closure>,
    /// The [`TaskPatch`] of each attempt, the first included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directives: Option<Closure>,
    #[serde(default, skip_serializing_if = "FailureRule::is_fail")]
    pub failure: FailureRule,
}

/// What a task does after its last rejected attempt: fail, or end with empty outputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureRule {
    #[default]
    Fail,
    Ignore,
}

impl FailureRule {
    pub fn is_fail(&self) -> bool {
        *self == FailureRule::Fail
    }
}

/// The changes of one attempt to the base settings of a task; a field left out keeps them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<Argument>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_codes: Option<Vec<i32>>,
}

/// One entry of the working folder: a value placed at a path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Staged {
    pub value: Content,
    /// The path in the working folder; without it, a File or Directory keeps its basename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<FileName>,
    /// Whether the command may change it, in a copy of its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub writable: bool,
}

/// The value of an entry of the working folder: text, or the value of a task input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Content {
    Text(String),
    Input { input: String },
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
        /// In a shell command, whether the words of the value and prefix are quoted.
        #[serde(default = "separate", skip_serializing_if = "Clone::clone")]
        quote: bool,
    },
    /// Text that a shell command holds as it is, unquoted; otherwise like text.
    Raw {
        raw: String,
    },
    /// Code like that of a `js` expr, which the runner evaluates as it builds the task, over
    /// the inputs with their Files staged; its value binds like the value of an input.
    Code {
        code: String,
        /// The task input that the code sees as `self`.
        #[serde(default, rename = "self", skip_serializing_if = "Option::is_none")]
        this: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        #[serde(default = "separate", skip_serializing_if = "Clone::clone")]
        separate: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        separator: Option<String>,
        #[serde(default = "separate", skip_serializing_if = "Clone::clone")]
        quote: bool,
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
            quote: true,
        }
    }
}

/// How one task output is read. An `optional` capture that matches nothing leaves the
/// output empty, which is not null; `arity` bounds the number of matched paths.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub output: String,
    pub from: CaptureRule,
    /// Whether each File it reads holds its text as `contents`, as CWL `loadContents`; a
    /// file over 64 KiB fails the task.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub load_contents: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arity: Option<Arity>,
    /// How deep each Directory it reads holds its listing, like CWL `loadListing`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<Listing>,
    /// Code like that of a `code` argument whose value is the output, over the matched paths
    /// as an array in `self`; see the README.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Code like that of a `code` argument whose value is the format of each File it reads,
    /// with that File as `self`, like a CWL `format` expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

/// The least and the most paths a capture may match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arity {
    pub min: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u64>,
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
    /// The field of the output in the task's `results`; null without that file.
    Results,
    /// No paths, for a capture whose code gives the output.
    Empty,
}

/// A pure expression over the op's inputs, which binds its one output. `js` and `javascript`
/// are CWL expressions; the other languages use their own evaluator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expr {
    pub language: ExprLanguage,
    /// For CWL, text with `$(...)` parts or a `${...}` function body.
    pub code: String,
    /// Code that runs before it, like CWL `expressionLib`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub library: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub captures: BTreeMap<String, Binding>,
    /// What an Argo text gives once its tags are substituted; the text when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<ExprForm>,
}

/// What an `argo` expr gives: its text, whether it holds as a `when`, the items of a loop, or
/// the labels that those items give the node names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExprForm {
    Text,
    Condition,
    Items,
    Labels,
}

/// Runs the `body` graph, like a call, when the bool input `condition` is true; otherwise the
/// op ends skipped with every output null.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    pub body: String,
    pub condition: String,
}

/// Gives the value of its one input with files read into it: the text of each File, alone or
/// in arrays, as `contents`, like CWL `loadContents`, and the listing of each Directory.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Load {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub contents: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<Listing>,
}

/// How deep a load reads the listing of a Directory: its entries, or all below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Listing {
    Shallow,
    Deep,
}

/// Runs the `body` graph once per round over items of the array inputs in `over`; other inputs
/// stay as they are. Outputs are arrays of the round results, in round order. `parallelism`
/// limits rounds at once; `min_success` lets the map succeed with failed rounds, which give null.
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

/// Runs the `body` graph once per round: each base round takes an item of every stream input
/// and every value input as it is, and each choice of the `each` inputs makes a child round.
/// With a stream input or an `each` input, every output is a stream of the rounds' values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Process {
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub each: Vec<String>,
    #[serde(default, skip_serializing_if = "Emission::is_completion")]
    pub order: Emission,
    /// The most child rounds that run at once, retries included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<u32>,
}

/// The order in which rounds give their outputs: as they end, or in round order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Emission {
    #[default]
    Completion,
    Fair,
}

impl Emission {
    pub fn is_completion(&self) -> bool {
        *self == Emission::Completion
    }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// It was stopped.
    Cancelled,
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
    /// Prints through the print sink of the host.
    Log,
    /// Reads the files of its items.
    ReadFiles,
    /// Writes files in the file area of the run.
    WriteFiles,
}

impl Effect {
    /// Whether this publishes to the folder `directory`: its target is that folder or one that
    /// holds it.
    pub fn publishes(&self, directory: &str) -> bool {
        let Effect::Publish(target) = self else {
            return false;
        };
        let (target, directory) = (
            target.trim_end_matches('/'),
            directory.trim_end_matches('/'),
        );
        let inside = |rest: &str| rest.is_empty() || rest.starts_with('/');
        (plain_folder(target) && plain_folder(directory))
            && directory.strip_prefix(target).is_some_and(inside)
    }
}

/// Whether a folder outside the run names its place plainly: a path or an address whose
/// parts are not empty, `.` or `..`.
pub fn plain_folder(folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    let scheme = |name: &str| {
        name.starts_with(|c: char| c.is_ascii_alphabetic())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    };
    let rest = match folder.split_once("://") {
        Some((name, rest)) if scheme(name) => rest,
        _ => folder,
    };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let plain = |part: &str| !part.is_empty() && part != "." && part != "..";
    !rest.is_empty() && rest.split('/').all(plain)
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

fn is_zero(value: &u32) -> bool {
    *value == 0
}
