//! The dataflow network that a workflow builds, as plain data: nodes with their semantics,
//! channels and the workflow calls they belong to, for compiling the workflow into wir.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::split::Splitter;
use nextflow_evaluator::value::{Closure, Value};
use serde_json::Value as Json;

use crate::adapter::Call;
pub use nextflow_plugins::observer::Process;

/// The network of a run before it starts.
#[derive(Debug)]
pub struct Network {
    pub channels: Vec<Channel>,
    pub nodes: Vec<Node>,
    /// The workflow calls; the first is the entry workflow.
    pub scopes: Vec<Scope>,
    /// What the entry workflow emits, by emit name.
    pub emits: Vec<(String, usize)>,
    /// The params of the run, with those that the scripts set.
    pub params: Json,
    /// The main script.
    pub script: String,
    /// The config files of the run, in the order read, without the files they include.
    pub configs: Vec<String>,
    /// The resolved config in the flat form of `nextflow config -flat`, which the compiler
    /// pins by its digest. It may hold secret values, so it must not be written anywhere.
    pub config: String,
    /// The `nextflow_schema.json` of the pipeline, with its sample sheet schemas, if it has one.
    pub schema: Option<Json>,
    /// The launch folder, from which relative paths of params start.
    pub launch: String,
    /// The processes of the run as observers see them, with the processes that each feeds.
    pub processes: Vec<Process>,
    /// The files that the build read, as Nextflow reads them while it builds the workflow, by
    /// location with their `sha256:` digests; the compiler pins them as sources.
    pub reads: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct Channel {
    /// Whether it is a value channel, which holds at most one value.
    pub value: bool,
    /// The value of a value channel that the build bound, which no node writes.
    pub bound: Option<Json>,
}

/// A call of a workflow: its fully qualified name, the call it is in, and the channels of
/// its takes and emits.
#[derive(Debug)]
pub struct Scope {
    pub name: String,
    pub parent: Option<usize>,
    pub takes: Vec<usize>,
    pub emits: Vec<(Option<String>, usize)>,
}

#[derive(Debug)]
pub struct Node {
    pub name: String,
    /// The workflow call that made it.
    pub scope: usize,
    pub inputs: Vec<usize>,
    pub outputs: Vec<usize>,
    pub kind: Kind,
}

/// A closure of a script: where its code is and the values it captured, in slot order.
#[derive(Clone, Debug, PartialEq)]
pub struct Callback {
    pub script: String,
    pub start: u32,
    pub end: u32,
    pub code: String,
    pub parameters: usize,
    pub captures: Vec<Json>,
}

/// What a filter or a search tests an item against: a closure, or a value that Groovy's
/// `isCase` compares, like a pattern or a class.
#[derive(Clone, Debug, PartialEq)]
pub enum Criteria {
    Callback(Callback),
    Value(Json),
    Pattern(String),
    Class(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Order {
    None,
    Natural,
    Key(Callback),
    Compare(Callback),
    Hash { deep: bool },
}

/// One output of a process call.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub emit: Option<String>,
    pub topic: Option<String>,
    /// Whether the output only feeds its topic.
    pub hidden: bool,
}

/// What a node does, by the native operator or node that it is.
#[derive(Clone, Debug)]
pub enum Kind {
    Process {
        call: Call,
        /// The input declarations that are `each` inputs.
        each: Vec<usize>,
        /// The bytes of the process definition in its script.
        start: u32,
        end: u32,
        outputs: Vec<Output>,
    },
    /// A factory with the values that the build gave it.
    Values {
        values: Vec<Json>,
        close: bool,
    },
    /// A factory that reads its values when the run starts, like `channel.fromPath`; a
    /// factory or function of a plugin is named `<plugin>:<name>`.
    Source {
        factory: String,
        arguments: Vec<Json>,
    },
    Map(Callback),
    Filter(Criteria),
    FlatMap(Option<Callback>),
    Flatten,
    Cast(String),
    View {
        callback: Option<Callback>,
        new_line: bool,
    },
    Unique(Option<Callback>),
    Distinct(Option<Callback>),
    Take(i64),
    Until(Callback),
    Transpose {
        by: Option<Vec<usize>>,
        remainder: bool,
    },
    Scan {
        callback: Callback,
        seed: Json,
    },
    Collect {
        callback: Option<Callback>,
        flat: bool,
        sort: Order,
    },
    ToList,
    IfEmpty(Criteria),
    First(Option<Criteria>),
    ToSortedList(Option<Callback>),
    Count(Option<Criteria>),
    Sum(Option<Callback>),
    Min(Option<Callback>),
    Max(Option<Callback>),
    Reduce {
        callback: Callback,
        seed: Option<Json>,
    },
    Last,
    Collate {
        size: usize,
        step: usize,
        remainder: bool,
    },
    Buffer {
        open: Option<Criteria>,
        close: Option<Criteria>,
        size: usize,
        skip: usize,
        remainder: bool,
    },
    Mix,
    Concat,
    Merge(Option<Callback>),
    GroupTuple {
        by: Vec<usize>,
        size: Option<usize>,
        remainder: bool,
        sort: Order,
    },
    Join {
        by: Vec<usize>,
        remainder: bool,
        duplicate_error: bool,
        mismatch_error: bool,
    },
    Combine {
        by: Option<Vec<usize>>,
        /// The values of a right side that is no channel.
        right: Option<Vec<Json>>,
    },
    Cross(Option<Callback>),
    Route {
        labels: Vec<String>,
        callback: Callback,
        all: bool,
    },
    CollectFile {
        callback: Option<Callback>,
        name: Option<String>,
        directory: String,
        new_line: bool,
        sort: Order,
        skip: usize,
        keep_header: bool,
        seed: Option<String>,
    },
    Split {
        splitter: Splitter,
        elem: Option<usize>,
        each: Option<Callback>,
        decompress: bool,
        count: bool,
        files: bool,
    },
    Subscribe {
        next: Option<Callback>,
        complete: Option<Callback>,
        error: Option<Callback>,
    },
    /// A method or property of the value of a value channel, like `ch.size()`.
    Invoke {
        name: String,
        arguments: Vec<Option<Json>>,
        property: bool,
    },
    /// A workflow output of the `output { }` block.
    Publish {
        name: String,
    },
    /// A plugin function that checks the run when it starts, like `validateParameters()`.
    Check {
        plugin: String,
        function: String,
        arguments: Vec<Json>,
    },
    /// The items of the list that a value channel holds, as `channel.fromList` gives them.
    Spread,
    /// What the workflow printed while it was built.
    Print(String),
    /// A file that the workflow wrote while it was built.
    Write {
        path: String,
        text: String,
    },
    /// A node that does not compile into wir, with the reason.
    Opaque(String),
}

impl Callback {
    /// The code and captured values of a closure; it fails for a capture that has no JSON
    /// form, like a channel or another closure.
    pub(crate) fn of(closure: &Closure) -> Result<Callback, String> {
        let span = closure.code.span;
        let module = &closure.module;
        let source = module.source.borrow();
        let script = module
            .path
            .borrow()
            .as_deref()
            .unwrap_or_default()
            .to_owned();
        let code = (source.as_deref())
            .and_then(|text| text.get(span.start as usize..span.end as usize))
            .ok_or_else(|| format!("the code of a closure in {script}"))?;
        let captures = (closure.captured.iter())
            .map(|value| crate::bridge::to_json(&value.borrow()))
            .collect::<Result<_, _>>()
            .map_err(|message| format!("a closure captures {message}"))?;
        Ok(Callback {
            script,
            start: span.start,
            end: span.end,
            code: code.to_owned(),
            parameters: closure.code.parameters.len(),
            captures,
        })
    }
}

/// The callback of an optional closure.
pub(crate) fn callback(closure: &Option<std::rc::Rc<Closure>>) -> Result<Option<Callback>, String> {
    closure.as_deref().map(Callback::of).transpose()
}

impl Criteria {
    pub(crate) fn of(value: &Value) -> Result<Criteria, String> {
        Ok(match value {
            Value::Closure(closure) => Criteria::Callback(Callback::of(closure)?),
            Value::Pattern(pattern) => Criteria::Pattern(pattern.source.clone()),
            Value::Class(class) => Criteria::Class(class.to_string()),
            other => Criteria::Value(crate::bridge::to_json(other)?),
        })
    }
}

/// A plain value of a node as JSON, with the reason when it has none.
pub(crate) fn value(value: &Value) -> Result<Json, String> {
    crate::bridge::to_json(value)
}
