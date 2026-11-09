//! Process ops: base rounds of one item of every stream input with the value inputs, child
//! rounds for each choice of the `each` inputs, and the round graphs that run them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json};
use wir::{Document, Emission, Status};

use crate::flows::output_stream;
use crate::plan;
use crate::state::{Progress, State};
use crate::streams::{Read, ReaderId, StreamBatch};

/// The values and the place of a child round.
type NextRound = (Map<String, Json>, String);

/// The choices of the `each` inputs of one base round.
struct Choices {
    base: u64,
    values: Map<String, Json>,
    lists: Vec<(String, Vec<Json>)>,
    next: usize,
    total: usize,
}

/// A process that runs, with the rounds it started.
pub struct Rounding {
    pub spec: wir::Process,
    /// Its stream inputs, in order.
    pub streams: Vec<String>,
    /// Its outputs are values: no stream input and no `each`.
    pub single: bool,
    base: u64,
    exhausted: bool,
    choices: Option<Choices>,
    ordinal: u64,
    /// The node of each round that runs, with its ordinal.
    active: BTreeMap<usize, u64>,
    /// With fair order: the outputs of settled rounds by ordinal, and the next to give.
    held: BTreeMap<u64, Map<String, Json>>,
    given: u64,
    /// The values of the one round of a process with value outputs.
    result: Option<(Map<String, Json>, Vec<String>)>,
}

impl Rounding {
    pub fn new(spec: wir::Process, node: &plan::Node, bound: &Map<String, Json>) -> Rounding {
        let streams: Vec<String> = (node.op.inputs.iter())
            .filter(|port| !port.flow.is_value())
            .map(|port| port.name.clone())
            .collect();
        let single = streams.is_empty() && spec.each.is_empty();
        // An empty value input runs no round, and every output closes empty.
        let empty = (node.op.inputs.iter())
            .any(|port| port.flow.is_value() && !bound.contains_key(&port.name));
        let outputs = node
            .op
            .outputs
            .iter()
            .map(|port| port.name.clone())
            .collect();
        Rounding {
            spec,
            streams,
            single,
            base: 0,
            exhausted: empty,
            choices: None,
            ordinal: 0,
            active: BTreeMap::new(),
            held: BTreeMap::new(),
            given: 0,
            result: empty.then(|| (Map::new(), outputs)),
        }
    }

    /// The values and the place of the next child round, if one can start now.
    fn next_round(
        &mut self,
        progress: &mut Progress,
        at: usize,
    ) -> Result<Option<NextRound>, String> {
        loop {
            if let Some(choices) = &mut self.choices {
                if choices.next < choices.total {
                    // The first `each` input varies fastest.
                    let mut rest = choices.next;
                    let mut values = choices.values.clone();
                    for (name, list) in &choices.lists {
                        values.insert(name.clone(), list[rest % list.len()].clone());
                        rest /= list.len();
                    }
                    let place = format!("{}/{}", choices.base, choices.next);
                    choices.next += 1;
                    return Ok(Some((values, place)));
                }
                self.choices = None;
            }
            if self.exhausted {
                return Ok(None);
            }
            let mut values = match &progress.states[at] {
                State::Running(values) => values.clone(),
                _ => return Ok(None),
            };
            if self.streams.is_empty() {
                self.exhausted = true;
            } else {
                let path = progress.node(at).path.clone();
                let readers: Vec<ReaderId> = (self.streams.iter())
                    .map(|input| ReaderId {
                        owner: path.clone(),
                        input: input.clone(),
                    })
                    .collect();
                // Every stream input is read before any item is taken.
                for reader in &readers {
                    match progress
                        .streams
                        .peek(reader)
                        .map_err(|error| error.message)?
                    {
                        Read::Item(_) => {}
                        Read::Pending => return Ok(None),
                        Read::End(terminal) => match terminal.end {
                            crate::streams::StreamEnd::Close => {
                                self.exhausted = true;
                                return Ok(None);
                            }
                            crate::streams::StreamEnd::Error(failure) => {
                                return Err(failure.message);
                            }
                            crate::streams::StreamEnd::Cancelled => {
                                return Err(format!("op {path}: an input was cancelled"));
                            }
                        },
                    }
                }
                let items = progress
                    .streams
                    .consume(&readers)
                    .map_err(|error| error.message)?
                    .unwrap_or_default();
                values.extend(self.streams.iter().cloned().zip(items));
            }
            let base = self.base;
            self.base += 1;
            if self.spec.each.is_empty() {
                return Ok(Some((values, format!("{base}/0"))));
            }
            let lists: Vec<(String, Vec<Json>)> = (self.spec.each.iter())
                .map(|name| {
                    (
                        name.clone(),
                        choices(values.get(name).cloned().unwrap_or_default()),
                    )
                })
                .collect();
            let total = lists.iter().map(|(_, list)| list.len()).product();
            self.choices = Some(Choices {
                base,
                values,
                lists,
                next: 0,
                total,
            });
        }
    }

    /// Takes in the rounds that ended, gives their outputs, and starts new rounds; whether it
    /// did anything, or why the process failed.
    fn step(
        &mut self,
        progress: &mut Progress,
        document: &Document,
        at: usize,
    ) -> Result<bool, String> {
        let mut moved = false;
        let path = progress.node(at).path.clone();
        let ended: Vec<(usize, u64)> = (self.active.iter())
            .filter(|(node, _)| matches!(progress.states[**node], State::Ended(..)))
            .map(|(node, ordinal)| (*node, *ordinal))
            .collect();
        let mut batch = StreamBatch::default();
        for (node, ordinal) in ended {
            self.active.remove(&node);
            moved = true;
            let State::Ended(status, values) = &progress.states[node] else {
                continue;
            };
            if *status != Status::Succeeded {
                let round = &progress.node(node).path;
                let reasons = progress.reasons(node);
                return Err(match reasons.is_empty() {
                    true => format!("op {round} failed"),
                    false => reasons,
                });
            }
            let ports: Vec<String> = progress
                .node(at)
                .op
                .outputs
                .iter()
                .map(|port| port.name.clone())
                .collect();
            if self.single {
                let empty = ports
                    .iter()
                    .filter(|port| progress.closed_empty(node, port))
                    .cloned()
                    .collect();
                self.result = Some((values.clone(), empty));
                continue;
            }
            // A settled round without outputs is a hole that fair order passes.
            self.held.insert(ordinal, values.clone());
            while let Some(values) = match self.spec.order {
                Emission::Fair => self.held.remove(&self.given),
                Emission::Completion => self.held.pop_first().map(|(_, values)| values),
            } {
                self.given += 1;
                // An empty output gives no item; a null is an item.
                for port in &ports {
                    if let Some(value) = values.get(port) {
                        let stream = output_stream(&path, port);
                        batch.items.push((stream, value.clone()));
                    }
                }
            }
        }
        progress
            .streams
            .commit(batch)
            .map_err(|error| error.message)?;
        while self
            .spec
            .parallelism
            .is_none_or(|most| self.active.len() < most as usize)
        {
            let Some((values, place)) = self.next_round(progress, at)? else {
                break;
            };
            let start = progress.plan.nodes.len();
            let node = plan::round(
                document,
                &mut progress.plan.nodes,
                (at, &place, None, &[]),
                &self.spec.body,
                values,
            )
            .map_err(|error| error.message)?;
            progress.plan.nodes[node].ordinal = Some(self.ordinal);
            progress.adopt(start);
            self.active.insert(node, self.ordinal);
            self.ordinal += 1;
            moved = true;
        }
        if self.exhausted && self.choices.is_none() && self.active.is_empty() {
            let (values, empty) = self.result.take().unwrap_or_default();
            progress.end_with(at, Status::Succeeded, values, empty);
            return Ok(true);
        }
        Ok(moved)
    }
}

/// The choices of an `each` input: the items of an array or a set, or the value itself.
fn choices(value: Json) -> Vec<Json> {
    match value {
        Json::Array(items) => items,
        Json::Object(object) if object.get("class").and_then(Json::as_str) == Some("Set") => object
            .get("items")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default(),
        other => vec![other],
    }
}

/// The processes that run, by node.
pub type Processes = BTreeMap<usize, Rounding>;

/// Steps every running process once and ends those that ended or failed; whether any moved.
pub fn step_all(progress: &mut Progress, document: &Document, processes: &mut Processes) -> bool {
    let mut moved = false;
    let nodes: Vec<usize> = processes.keys().copied().collect();
    for at in nodes {
        let Some(rounding) = processes.get_mut(&at) else {
            continue;
        };
        // A process that was cancelled from outside starts no more rounds.
        if matches!(progress.states[at], State::Ended(..)) {
            processes.remove(&at);
            continue;
        }
        match rounding.step(progress, document, at) {
            Ok(step) => moved |= step,
            Err(why) => {
                // A failed round cancels the rounds that still run.
                for round in std::mem::take(&mut rounding.active).into_keys() {
                    progress.cancel(round);
                }
                processes.remove(&at);
                let path = progress.node(at).path.clone();
                let why = match why.starts_with("op ") {
                    true => why,
                    false => format!("op {path}: {why}"),
                };
                progress.end(at, Status::Failed, Map::new(), Some(why));
                moved = true;
                continue;
            }
        }
        if matches!(progress.states[at], State::Ended(..)) {
            processes.remove(&at);
        }
    }
    moved
}
