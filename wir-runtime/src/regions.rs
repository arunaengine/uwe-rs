//! Dynamic regions: plan updates that are checked, then reconciled with the instances of the
//! region, keeping started ones and replacing the rest, until the final plan settles.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value as Json, json};
use wir::reserved::{Dynamic, Identity, PlanUpdate};
use wir::{Document, Status};

use crate::adapter::Adapters;
use crate::plan::{self, Source};
use crate::state::{Progress, State};
use crate::streams::{Read, ReaderId, StreamEnd};

/// An instance of a region: its node, its definition, and whether it was retired.
struct Instance {
    node: usize,
    identity: Identity,
    definition: String,
    retired: bool,
    /// The driver call in which it was seen to end, and whether the planner heard of it.
    seen: Option<u64>,
    heard: bool,
}

/// A producer that a stream output of a region forwards, attached once: the output, the
/// producer's identity and port, its node, the reader, and whether its stream ended.
struct Attachment {
    output: String,
    key: String,
    port: String,
    node: usize,
    reader: ReaderId,
    ended: bool,
}

/// A region that runs, with the instances of its plans by canonical identity.
pub struct Region {
    pub spec: Dynamic,
    pub bound: Map<String, Json>,
    revision: Option<u64>,
    last: Option<PlanUpdate>,
    pub final_plan: bool,
    instances: BTreeMap<String, Instance>,
    /// Whether the planner gave its first plan, or the value plan input was read.
    pub planned: bool,
    /// Whether no plan can come any more: the plan input closed, or a planner plans.
    plan_closed: bool,
    /// The producers that its stream outputs forward, in the order they were attached, and
    /// how many readers it installed for them.
    attached: Vec<Attachment>,
    readers: usize,
}

impl Region {
    pub fn new(spec: Dynamic, bound: Map<String, Json>) -> Region {
        Region {
            plan_closed: spec.plan.is_none(),
            spec,
            bound,
            revision: None,
            last: None,
            final_plan: false,
            instances: BTreeMap::new(),
            planned: false,
            attached: Vec::new(),
            readers: 0,
        }
    }

    /// The plan updates that the `plan` input gave since the last call: its value once, or
    /// the new items of its stream; an input that ended without a final plan fails.
    pub fn given(&mut self, progress: &mut Progress, at: usize) -> Result<Vec<Json>, String> {
        let Some(input) = self.spec.plan.clone() else {
            return Ok(Vec::new());
        };
        let streamed = (progress.node(at).op.inputs.iter())
            .any(|port| port.name == input && !port.flow.is_value());
        if !streamed {
            self.plan_closed = true;
            if std::mem::replace(&mut self.planned, true) {
                return Ok(Vec::new());
            }
            return Ok(vec![self.bound.get(&input).cloned().unwrap_or_default()]);
        }
        let reader = ReaderId {
            owner: progress.node(at).path.clone(),
            input,
        };
        let mut updates = Vec::new();
        loop {
            match progress
                .streams
                .peek(&reader)
                .map_err(|error| error.message)?
            {
                Read::Item(_) => {
                    let items = progress.streams.consume(std::slice::from_ref(&reader));
                    let items = items.map_err(|error| error.message)?.unwrap_or_default();
                    updates.extend(items);
                }
                Read::Pending => return Ok(updates),
                Read::End(terminal) => match terminal.end {
                    StreamEnd::Close if self.final_plan || !updates.is_empty() => {
                        self.plan_closed = true;
                        return Ok(updates);
                    }
                    StreamEnd::Close => {
                        return Err("the plan input ended before a final plan".into());
                    }
                    StreamEnd::Error(failure) => return Err(failure.message),
                    StreamEnd::Cancelled => return Err("the plan input was cancelled".into()),
                },
            }
        }
    }

    /// Checks a plan update, then reconciles the region with it: started instances stay and
    /// must keep their definition, the others are retired, and the rest of the plan is added.
    pub fn adopt(
        &mut self,
        progress: &mut Progress,
        (document, adapters): (&Document, &Adapters),
        at: usize,
        update: Json,
    ) -> Result<(), String> {
        let update: PlanUpdate = serde_json::from_value(update)
            .map_err(|error| format!("a plan update does not parse: {error}"))?;
        let revision = update.revision;
        match self.revision {
            _ if self.final_plan && self.last.as_ref() != Some(&update) => {
                return Err(format!("revision {revision} came after the final plan"));
            }
            None if revision != 0 => return Err(format!("the first plan has revision {revision}")),
            Some(last) if revision == last => {
                return match self.last.as_ref() == Some(&update) {
                    true => Ok(()),
                    false => Err(format!("revision {revision} came twice, changed")),
                };
            }
            Some(last) if revision != last + 1 => {
                return Err(format!("revision {revision} came after {last}"));
            }
            _ => {}
        }
        let (checked, name) = checked(document, adapters, &update)?;
        let mut reuse = HashMap::new();
        let mut keys = HashMap::new();
        let mut kept = Vec::new();
        let mut added = Vec::new();
        for op in &update.graph.ops {
            let identity = (update.keys.iter())
                .find(|key| key.op == op.path)
                .map_or_else(
                    || Identity::Named(op.path.clone()),
                    |key| key.identity.clone(),
                );
            let key = serde_json::to_string(&identity).unwrap_or_default();
            keys.insert(op.path.clone(), key.clone());
            let edges: Vec<&wir::Edge> = (update.graph.edges.iter())
                .filter(|edge| edge.to.op.as_deref() == Some(op.path.as_str()))
                .collect();
            let definition = json!({"op": op, "edges": edges}).to_string();
            match self.instances.get(&key) {
                Some(instance) if !instance.retired && started(progress, instance.node) => {
                    if instance.definition != definition {
                        return Err(format!("the started instance {key} changed"));
                    }
                    reuse.insert(op.path.clone(), instance.node);
                    kept.push(key);
                }
                _ => added.push((op.path.clone(), key, identity, definition)),
            }
        }
        // Unstarted instances are replaced; started ones the plan dropped finish.
        for instance in self.instances.values_mut() {
            if !instance.retired && !started(progress, instance.node) {
                instance.retired = true;
                progress.cancel(instance.node);
            }
        }
        let graph = (checked.graphs.iter()).find(|graph| graph.name == name);
        let graph = graph.ok_or_else(|| format!("no graph {name}"))?;
        let start = progress.plan.nodes.len();
        let path = progress.node(at).path.clone();
        let (inner, name) = (&progress.node(at).inner, &progress.node(at).name);
        // New instances live below the revision that added them, so that a replacement never
        // takes the streams of the instance it replaced.
        let prefixes = (
            format!("{path}/{revision}/"),
            inner.clone(),
            format!("{}{revision}/", plan::under(name)),
        );
        let exports = plan::extend(
            &checked,
            graph,
            (&prefixes.0, &prefixes.1, &prefixes.2),
            (Some(at), &reuse),
            &mut progress.plan.nodes,
        )
        .map_err(|error| error.message)?;
        progress.plan.nodes[at].exports = exports;
        progress.adopt(start);
        for (op, key, identity, definition) in added {
            let node = (start..progress.plan.nodes.len())
                .find(|node| progress.node(*node).path == format!("{path}/{revision}/{op}"))
                .ok_or_else(|| format!("instance {key} has no node"))?;
            let instance = Instance {
                node,
                identity,
                definition,
                retired: false,
                seen: None,
                heard: false,
            };
            self.instances.insert(key, instance);
        }
        // New instances open their streams and install their readers before any starts.
        let added: Vec<usize> = (start..progress.plan.nodes.len())
            .filter(|node| progress.node(*node).parent == Some(at))
            .collect();
        progress.install(&added).map_err(|error| error.message)?;
        self.attach(progress, at, &update, &keys)?;
        self.revision = Some(revision);
        self.final_plan = update.final_plan;
        self.last = Some(update);
        Ok(())
    }

    /// Attaches each producer that a stream output of the plan names, once by its identity
    /// and port; a producer that the plan replaced before it started is attached anew.
    fn attach(
        &mut self,
        progress: &mut Progress,
        at: usize,
        update: &PlanUpdate,
        keys: &HashMap<String, String>,
    ) -> Result<(), String> {
        let path = progress.node(at).path.clone();
        let outputs: Vec<String> = (progress.node(at).op.outputs.iter())
            .filter(|port| !port.flow.is_value())
            .map(|port| port.name.clone())
            .collect();
        for edge in update
            .graph
            .edges
            .iter()
            .filter(|edge| edge.to.op.is_none())
        {
            let output = &edge.to.port;
            if !outputs.contains(output) {
                continue;
            }
            let port = edge.from.port.clone();
            // An output comes from an instance, or straight from an input of the region.
            let (key, node, source) = match &edge.from.op {
                Some(op) => {
                    let Some(key) = keys.get(op) else {
                        return Err(format!(
                            "output {output} comes from no instance of the plan"
                        ));
                    };
                    let node = self.instances[key].node;
                    (key.clone(), node, Source::Output(node, port.clone()))
                }
                None => (format!("input {port}"), at, Source::Call(at, port.clone())),
            };
            let key = &key;
            let same =
                |item: &Attachment| item.output == *output && item.key == *key && item.port == port;
            let found = self.attached.iter().position(same);
            if found.is_some_and(|index| self.attached[index].node == node) {
                continue;
            }
            self.readers += 1;
            let reader = ReaderId {
                owner: path.clone(),
                input: format!("export {output} {}", self.readers),
            };
            let stream = progress.stream_of(&source).map_err(|error| error.message)?;
            let mode = wir::Subscription::All;
            (progress.streams)
                .subscribe(&stream, reader.clone(), mode)
                .map_err(|e| e.message)?;
            match found.map(|index| &mut self.attached[index]) {
                Some(item) => {
                    progress.streams.detach(&item.reader);
                    (item.node, item.reader, item.ended) = (node, reader, false);
                }
                None => self.attached.push(Attachment {
                    output: output.clone(),
                    key: key.clone(),
                    port,
                    node,
                    reader,
                    ended: false,
                }),
            }
        }
        Ok(())
    }

    /// Forwards the items of the attached producers to the stream outputs: in fair order one
    /// producer after the other by attachment, else as they come; whether any moved.
    pub fn forward(&mut self, progress: &mut Progress, at: usize) -> Result<bool, String> {
        let path = progress.node(at).path.clone();
        let fair = self.spec.order == wir::Emission::Fair;
        let mut batch = crate::streams::StreamBatch::default();
        let mut moved = false;
        let mut waiting: Vec<String> = Vec::new();
        for item in self.attached.iter_mut().filter(|item| !item.ended) {
            if fair && waiting.contains(&item.output) {
                continue;
            }
            let stream = crate::flows::output_stream(&path, &item.output);
            loop {
                match progress
                    .streams
                    .peek(&item.reader)
                    .map_err(|error| error.message)?
                {
                    Read::Item(_) => {
                        let reader = std::slice::from_ref(&item.reader);
                        let items = progress.streams.consume(reader).map_err(|e| e.message)?;
                        let items = items.unwrap_or_default().into_iter();
                        batch
                            .items
                            .extend(items.map(|value| (stream.clone(), value)));
                        moved = true;
                    }
                    Read::Pending => break,
                    Read::End(terminal) => {
                        // The prefix of an attached producer leaves before its error.
                        if let StreamEnd::Error(failure) = terminal.end {
                            let prefix = std::mem::take(&mut batch);
                            progress
                                .streams
                                .commit(prefix)
                                .map_err(|error| error.message)?;
                            return Err(failure.message);
                        }
                        item.ended = true;
                        moved = true;
                        break;
                    }
                }
            }
            if !item.ended {
                waiting.push(item.output.clone());
            }
        }
        progress
            .streams
            .commit(batch)
            .map_err(|error| error.message)?;
        Ok(moved)
    }

    /// Whether every attached producer ended its stream.
    pub fn exported(&self) -> bool {
        self.attached.iter().all(|item| item.ended)
    }

    /// The instances that ended before the driver call `call`, with their identity and
    /// status, once each: the files of an instance are delivered after the call it ended in.
    pub fn ended(&mut self, progress: &Progress, call: u64) -> Vec<(Identity, Status)> {
        let mut ended = Vec::new();
        for instance in self.instances.values_mut() {
            let State::Ended(status, _) = &progress.states[instance.node] else {
                continue;
            };
            let seen = *instance.seen.get_or_insert(call);
            if !instance.retired && seen < call && !std::mem::replace(&mut instance.heard, true) {
                ended.push((instance.identity.clone(), *status));
            }
        }
        ended
    }

    /// Whether an instance ended that the planner has not heard of yet.
    pub fn unheard(&self, progress: &Progress) -> bool {
        (self.instances.values()).any(|instance| {
            !instance.heard
                && !instance.retired
                && matches!(progress.states[instance.node], State::Ended(..))
        })
    }

    /// The nodes of instances that run.
    pub fn running(&self, progress: &Progress) -> Vec<usize> {
        (self.instances.values())
            .filter(|instance| !matches!(progress.states[instance.node], State::Ended(..)))
            .map(|instance| instance.node)
            .collect()
    }
}

/// Whether a node started.
fn started(progress: &Progress, node: usize) -> bool {
    !matches!(progress.states[node], State::Waiting)
}

/// The document with the graph of a plan update, once it verifies and this runner runs it,
/// with the name the graph has in it.
fn checked(
    document: &Document,
    adapters: &Adapters,
    update: &PlanUpdate,
) -> Result<(Document, String), String> {
    let mut checked = document.clone();
    let mut graph = update.graph.clone();
    let taken = |name: &str| checked.graphs.iter().any(|graph| graph.name == name);
    let mut number = 0;
    while taken(&graph.name) {
        number += 1;
        graph.name = format!("{}-{number}", update.graph.name);
    }
    let name = graph.name.clone();
    checked.graphs.push(graph);
    if let Err(problems) = wir::verify(&checked) {
        let problems: Vec<String> = problems.iter().map(ToString::to_string).collect();
        return Err(format!(
            "a plan update does not verify: {}",
            problems.join("; ")
        ));
    }
    plan::refuse_from(&checked, adapters, &name).map_err(|error| error.message)?;
    Ok((checked, name))
}

/// The regions that run, by node.
pub type Regions = BTreeMap<usize, Region>;

/// Fails a region: its instances that run are cancelled.
pub fn fail(progress: &mut Progress, regions: &mut Regions, at: usize, why: String) {
    if let Some(region) = regions.remove(&at) {
        for node in region.running(progress) {
            progress.cancel(node);
        }
    }
    let path = progress.node(at).path.clone();
    let why = match why.starts_with("op ") {
        true => why,
        false => format!("op {path}: {why}"),
    };
    progress.end(at, Status::Failed, Map::new(), Some(why));
}

/// Ends the regions whose final plan settled: every instance ended.
pub fn settle(progress: &mut Progress, regions: &mut Regions) -> bool {
    let mut moved = false;
    for at in regions.keys().copied().collect::<Vec<usize>>() {
        let region = &regions[&at];
        if matches!(progress.states[at], State::Ended(..)) {
            regions.remove(&at);
            continue;
        }
        let settled = region.final_plan
            && region.plan_closed
            && region.running(progress).is_empty()
            && region.exported();
        let heard = region.spec.planner.is_none() || !region.unheard(progress);
        if settled && heard && progress.end_call(at) {
            regions.remove(&at);
            moved = true;
        }
    }
    moved
}

#[cfg(test)]
#[path = "regions_tests.rs"]
mod tests;
