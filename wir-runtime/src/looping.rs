//! Loop ops: serial runs of the body graph, each with the carried outputs of the run before,
//! while the body's bool condition holds, at most `limit` runs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json};
use wir::{Document, Status};

use crate::flows::output_stream;
use crate::plan;
use crate::state::{Progress, State};
use crate::streams::StreamBatch;

/// A loop that runs, with the inputs of its next run.
pub struct Looping {
    spec: wir::reserved::Loop,
    values: Map<String, Json>,
    runs: u64,
    /// The node of the run that runs.
    active: Option<usize>,
}

impl Looping {
    pub fn new(spec: wir::reserved::Loop, bound: Map<String, Json>) -> Looping {
        Looping {
            spec,
            values: bound,
            runs: 0,
            active: None,
        }
    }

    /// Takes in the run that ended and starts the next; whether it did anything, or why the
    /// loop failed.
    fn step(
        &mut self,
        progress: &mut Progress,
        document: &Document,
        at: usize,
    ) -> Result<bool, String> {
        if let Some(run) = self.active {
            let State::Ended(status, values) = &progress.states[run] else {
                return Ok(false);
            };
            self.active = None;
            if *status != Status::Succeeded {
                let reasons = progress.reasons(run);
                return Err(match reasons.is_empty() {
                    true => format!("op {} failed", progress.node(run).path),
                    false => reasons,
                });
            }
            let values = values.clone();
            if self.next(progress, at, run, values)? {
                return Ok(true);
            }
        }
        let body = (document.graphs.iter())
            .find(|graph| graph.name == self.spec.body)
            .ok_or_else(|| format!("no graph {}", self.spec.body))?;
        // A run takes the inputs of the body; a missing one takes its default.
        let inputs = (self.values.iter())
            .filter(|(name, _)| body.inputs.iter().any(|port| port.name == **name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let start = progress.plan.nodes.len();
        let name = self.runs.to_string();
        let nodes = &mut progress.plan.nodes;
        let run = plan::round(document, nodes, (at, &name), &self.spec.body, inputs)
            .map_err(|error| error.message)?;
        progress.plan.nodes[run].ordinal = Some(self.runs);
        progress.adopt(start);
        self.active = Some(run);
        self.runs += 1;
        Ok(true)
    }

    /// Gives the stream outputs of a run that succeeded, then ends the loop when its
    /// condition is false; whether it ended.
    fn next(
        &mut self,
        progress: &mut Progress,
        at: usize,
        run: usize,
        values: Map<String, Json>,
    ) -> Result<bool, String> {
        let node = progress.node(at);
        let path = node.path.clone();
        let mut batch = StreamBatch::default();
        // An empty body output adds no item.
        for port in node.op.outputs.iter().filter(|port| !port.flow.is_value()) {
            if let Some(value) = values.get(&port.name) {
                let stream = output_stream(&path, &port.name);
                batch.items.push((stream, value.clone()));
            }
        }
        let condition = match values.get(&self.spec.condition) {
            Some(Json::Bool(holds)) => *holds,
            Some(other) => return Err(format!("the condition gave {other}, no bool")),
            None => return Err("the condition has no value".into()),
        };
        let ports: Vec<(String, bool)> = (node.op.outputs.iter())
            .map(|port| (port.name.clone(), port.flow.is_value()))
            .collect();
        (progress.streams)
            .commit(batch)
            .map_err(|error| error.message)?;
        if !condition {
            let (mut outputs, mut empty) = (Map::new(), Vec::new());
            for (port, _) in ports.iter().filter(|(_, value)| *value) {
                match values.get(port) {
                    Some(value) => _ = outputs.insert(port.clone(), value.clone()),
                    None if progress.closed_empty(run, port) => empty.push(port.clone()),
                    None => {}
                }
            }
            progress.end_with(at, Status::Succeeded, outputs, empty);
            return Ok(true);
        }
        if let Some(limit) = self.spec.limit.filter(|limit| self.runs >= *limit) {
            return Err(format!("the condition still holds after {limit} runs"));
        }
        for carry in &self.spec.carry {
            let value = values.get(&carry.output).cloned().unwrap_or_default();
            self.values.insert(carry.input.clone(), value);
        }
        Ok(false)
    }
}

/// The loops that run, by node.
pub type Loops = BTreeMap<usize, Looping>;

/// Steps every running loop once and ends those that ended or failed; whether any moved.
pub fn step_all(progress: &mut Progress, document: &Document, loops: &mut Loops) -> bool {
    let mut moved = false;
    for at in loops.keys().copied().collect::<Vec<usize>>() {
        let Some(looping) = loops.get_mut(&at) else {
            continue;
        };
        if matches!(progress.states[at], State::Ended(..)) {
            loops.remove(&at);
            continue;
        }
        match looping.step(progress, document, at) {
            Ok(step) => moved |= step,
            Err(why) => {
                if let Some(run) = looping.active.take() {
                    progress.cancel(run);
                }
                loops.remove(&at);
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
            loops.remove(&at);
        }
    }
    moved
}
