//! Stream ops as state machines: each step reads what its readers have and commits one batch
//! of outputs, so that one op cannot hold the driver, and an op ends once its outputs did.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json};
use wir::reserved::Sort;
use wir::{Kind, Status};

use crate::collecting::Collecting;
use crate::engine::Context;
use crate::flows::output_stream;
use crate::mixing::{Mixing, RoundRule, Rounds};
use crate::pairing::{Combining, Joining};
use crate::plan::Node;
use crate::routing::Routing;
use crate::streams::{Failure, Read, ReaderId, StreamBatch, StreamEnd, Streams};
use crate::transforms::{Mapping, Rule};

/// The most items one step of an op commits.
pub const CHUNK: usize = 256;

/// What a step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing: it waits for items.
    Idle,
    /// It read or wrote, and may have more to do.
    Moved,
    /// It ended; its open streams close.
    Done,
}

/// What a stream op reads next from one input.
#[derive(Clone, Debug, PartialEq)]
pub enum Pull {
    Item(Json),
    Pending,
    Closed,
}

/// One step of one op: its readers, its value inputs, and what it commits.
pub struct Turn<'s> {
    pub path: String,
    /// The values of its value inputs.
    pub values: Map<String, Json>,
    streams: &'s mut Streams,
    readers: BTreeMap<String, ReaderId>,
    pub batch: StreamBatch,
    pub bound: Map<String, Json>,
    pub empty: Vec<String>,
}

impl<'s> Turn<'s> {
    pub fn new(node: &Node, values: Map<String, Json>, streams: &'s mut Streams) -> Turn<'s> {
        let readers = (node.op.inputs.iter())
            .filter(|port| !port.flow.is_value())
            .map(|port| {
                let reader = ReaderId {
                    owner: node.path.clone(),
                    input: port.name.clone(),
                };
                (port.name.clone(), reader)
            })
            .collect();
        Turn {
            path: node.path.clone(),
            values,
            streams,
            readers,
            batch: StreamBatch::default(),
            bound: Map::new(),
            empty: Vec::new(),
        }
    }

    /// The next item of an input, moving its reader. An error or a cancellation upstream
    /// fails this op too.
    pub fn pull(&mut self, input: &str) -> Result<Pull, Failure> {
        match self.peek(input)? {
            Pull::Item(_) => {
                let reader = self.reader(input)?;
                let taken = self.streams.consume(std::slice::from_ref(&reader));
                let mut values = taken
                    .map_err(|error| self.failed(error.message))?
                    .unwrap_or_default();
                Ok(values.pop().map_or(Pull::Pending, Pull::Item))
            }
            other => Ok(other),
        }
    }

    /// The next item of an input, without moving its reader.
    pub fn peek(&self, input: &str) -> Result<Pull, Failure> {
        let reader = self.reader(input)?;
        match self
            .streams
            .peek(&reader)
            .map_err(|error| self.failed(error.message))?
        {
            Read::Item(item) => Ok(Pull::Item(item.value)),
            Read::Pending => Ok(Pull::Pending),
            Read::End(terminal) => match terminal.end {
                StreamEnd::Close => Ok(Pull::Closed),
                StreamEnd::Error(failure) => Err(Failure {
                    source: self.path.clone(),
                    ..failure
                }),
                StreamEnd::Cancelled => Err(Failure {
                    status: Status::Cancelled,
                    message: format!("op {}: input {input} was cancelled", self.path),
                    source: self.path.clone(),
                }),
            },
        }
    }

    /// The event of the next item of an input, for an arrival order across inputs.
    pub fn next_event(&self, input: &str) -> Result<Option<u64>, Failure> {
        let reader = self.reader(input)?;
        match self
            .streams
            .peek(&reader)
            .map_err(|error| self.failed(error.message))?
        {
            Read::Item(item) => Ok(Some(item.event)),
            _ => Ok(None),
        }
    }

    /// The failure that an input ended with, also before its reader reached the end.
    pub fn input_failure(&self, input: &str) -> Option<Failure> {
        let reader = self.readers.get(input)?;
        match &self.streams.terminal_of(reader)?.end {
            StreamEnd::Error(failure) => Some(Failure {
                source: self.path.clone(),
                ..failure.clone()
            }),
            _ => None,
        }
    }

    /// The next items of all `inputs` at once, or none while one of them has no item.
    pub fn take(&mut self, inputs: &[String]) -> Result<Option<Vec<Json>>, Failure> {
        let readers = (inputs.iter())
            .map(|input| self.reader(input))
            .collect::<Result<Vec<_>, _>>()?;
        self.streams
            .consume(&readers)
            .map_err(|error| self.failed(error.message))
    }

    /// Stops reading an input; the producer and the other readers go on.
    pub fn detach(&mut self, input: &str) {
        if let Some(reader) = self.readers.get(input) {
            self.streams.detach(reader);
        }
    }

    /// Adds an item to a stream output.
    pub fn emit(&mut self, output: &str, value: Json) {
        self.batch
            .items
            .push((output_stream(&self.path, output), value));
    }

    /// Binds a value output.
    pub fn bind(&mut self, output: &str, value: Json) {
        self.bound.insert(output.to_owned(), value);
    }

    /// Closes a value output empty.
    pub fn close_empty(&mut self, output: &str) {
        self.empty.push(output.to_owned());
    }

    /// A failure of this op.
    pub fn failed(&self, message: impl Into<String>) -> Failure {
        Failure {
            status: Status::Failed,
            message: format!("op {}: {}", self.path, message.into()),
            source: self.path.clone(),
        }
    }

    fn reader(&self, input: &str) -> Result<ReaderId, Failure> {
        (self.readers.get(input).cloned())
            .ok_or_else(|| self.failed(format!("no stream input {input}")))
    }
}

/// A stream op that runs in the engine.
pub trait Operator {
    fn step(&mut self, turn: &mut Turn<'_>, context: &Context<'_>) -> Result<Step, Failure>;
}

/// The op of a node, when its kind runs as a stream op.
pub fn make(node: &Node) -> Option<Box<dyn Operator>> {
    let stream = node
        .op
        .inputs
        .iter()
        .any(|port| port.name == "source" && !port.flow.is_value());
    let mapping =
        |rule| -> Option<Box<dyn Operator>> { Some(Box::new(Mapping::new(rule, stream))) };
    let collecting = |body, flat, sort, list| -> Option<Box<dyn Operator>> {
        Some(Box::new(Collecting {
            body,
            flat,
            sort,
            list,
            items: Vec::new(),
            callbacks: Default::default(),
        }))
    };
    match &node.op.kind {
        Kind::Collect(spec) => collecting(spec.body.clone(), spec.flat, spec.sort.clone(), false),
        Kind::Join(spec) => {
            let width = |name: &str| {
                let port = node.op.inputs.iter().find(|port| port.name == name)?;
                let wir::Type::Tuple(items) = &port.r#type else {
                    return None;
                };
                Some(items.len().saturating_sub(spec.by.len()))
            };
            Some(Box::new(Joining::new(
                spec,
                [width("left"), width("right")],
            )))
        }
        Kind::Combine(spec) => Some(Box::new(Combining {
            by: spec.by.clone(),
            body: spec.body.clone(),
            callbacks: Default::default(),
            retained: Default::default(),
        })),
        Kind::ToList(_) => collecting(None, false, Sort::None, true),
        Kind::Transform(spec) => mapping(Rule::Transform(spec.body.clone(), spec.nulls)),
        Kind::Filter(spec) => mapping(Rule::Filter(spec.body.clone())),
        Kind::FlatMap(spec) => mapping(Rule::FlatMap(spec.body.clone())),
        Kind::Flatten(spec) if stream => mapping(Rule::Flatten(spec.depth, spec.drop_null)),
        Kind::Mix(_) | Kind::Concat(_) => {
            let inputs = node
                .op
                .inputs
                .iter()
                .map(|port| port.name.clone())
                .collect();
            let current = matches!(node.op.kind, Kind::Concat(_)).then_some(0);
            Some(Box::new(Mixing { inputs, current }))
        }
        Kind::Merge(merge) if merge.takes_rounds(streams(node)) => {
            rounds(node, RoundRule::Merge(merge.body.clone()))
        }
        Kind::Zip(zip) => rounds(
            node,
            RoundRule::Zip {
                remainder: zip.remainder,
            },
        ),
        Kind::Route(route) => Some(Box::new(Routing::Route(
            route.cases.clone(),
            Default::default(),
        ))),
        Kind::MultiMap(multi) => Some(Box::new(Routing::Multi(
            multi.outputs.clone(),
            Default::default(),
        ))),
        Kind::Emit(emit) => Some(Box::new(Emitting {
            each: emit.each,
            next: 0,
        })),
        _ => None,
    }
}

/// Whether a node has a stream port.
fn streams(node: &Node) -> bool {
    (node.op.inputs.iter().chain(&node.op.outputs)).any(|port| !port.flow.is_value())
}

/// The op that takes rounds of the inputs of a node.
fn rounds(node: &Node, rule: RoundRule) -> Option<Box<dyn Operator>> {
    let inputs = (node.op.inputs.iter())
        .map(|port| (port.name.clone(), !port.flow.is_value()))
        .collect();
    let single = node
        .op
        .outputs
        .first()
        .is_some_and(|port| port.flow.is_value());
    Some(Box::new(Rounds {
        inputs,
        rule,
        single,
        callbacks: Default::default(),
    }))
}

/// Gives its value input `source` as one item, or each of its items.
struct Emitting {
    each: bool,
    next: usize,
}

impl Operator for Emitting {
    fn step(&mut self, turn: &mut Turn<'_>, _context: &Context<'_>) -> Result<Step, Failure> {
        // An empty source closes the stream without items.
        let items = match (self.each, turn.values.get("source").cloned()) {
            (_, None) => Vec::new(),
            (true, Some(Json::Array(items))) => items,
            (true, Some(other)) => {
                return Err(turn.failed(format!("source {other} is not an array")));
            }
            (false, Some(value)) => vec![value],
        };
        let end = (self.next + CHUNK).min(items.len());
        for item in &items[self.next..end] {
            turn.emit("out", item.clone());
        }
        self.next = end;
        Ok(match end == items.len() {
            true => Step::Done,
            false => Step::Moved,
        })
    }
}

/// The stream ops that run, by node.
pub type Running = BTreeMap<usize, Box<dyn Operator>>;

/// Takes one step of every running stream op, in node order, and ends those that ended or
/// failed. Whether any of them moved.
pub fn step_all(
    progress: &mut crate::state::Progress,
    running: &mut Running,
    context: &Context<'_>,
) -> bool {
    let mut moved = false;
    let nodes: Vec<usize> = running.keys().copied().collect();
    for at in nodes {
        let values = match &progress.states[at] {
            crate::state::State::Running(values) => values.clone(),
            _ => continue,
        };
        let node = &progress.plan.nodes[at];
        let mut turn = Turn::new(node, values, &mut progress.streams);
        let step = running
            .get_mut(&at)
            .map(|operator| operator.step(&mut turn, context));
        let Turn {
            batch,
            bound,
            empty,
            path,
            ..
        } = turn;
        // What an op gave before it failed is a prefix of its outputs, before their error.
        let committed = progress
            .streams
            .commit(batch)
            .map_err(|error| format!("op {path}: {}", error.message));
        let step = match step {
            Some(Ok(step)) => committed.map(|()| step),
            Some(Err(failure)) => Err(failure.message),
            None => continue,
        };
        match step {
            Ok(Step::Idle) => {}
            Ok(Step::Moved) => moved = true,
            Ok(Step::Done) => {
                running.remove(&at);
                progress.end_with(at, Status::Succeeded, bound, empty);
                moved = true;
            }
            Err(why) => {
                running.remove(&at);
                progress.end(at, Status::Failed, Map::new(), Some(why));
                moved = true;
            }
        }
    }
    moved
}

/// Cancels the stream ops that still run once nothing else can happen: their inputs never
/// ended, which fails the run.
pub fn cancel_all(progress: &mut crate::state::Progress, running: &mut Running) {
    for at in std::mem::take(running).into_keys() {
        let path = progress.plan.nodes[at].path.clone();
        if let Err(error) = progress.streams.cancel(&path) {
            progress
                .failures
                .push(format!("op {path}: {}", error.message));
        }
        progress.end(at, Status::Cancelled, Map::new(), None);
        progress
            .failures
            .push(format!("op {path}: its inputs never ended"));
    }
}
