//! The graphs of a lowered network: one per workflow call, whose ports are the channels that
//! cross the call, with the process bodies, and the ops of the start of the run in `main`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use nextflow_runtime::export::{Kind, Network};
use serde_json::{Value as Json, json};

use crate::operators::{End, Piece, Place, end, piece, port};
use crate::{Context, Kept, Sources};

/// What a channel without a node that writes it holds.
enum Literal {
    Value(Json),
    Empty,
}

/// The graphs of a network, `main` first; a node without a lowering is the error.
pub(crate) fn lower(
    network: &Network,
    context: &Context,
    sources: &mut Sources,
) -> Result<Vec<Json>, Kept> {
    let mut lowerer = Lowerer::new(network);
    let mut bodies = Vec::new();
    let mut pieces: Vec<Option<Piece>> = Vec::new();
    for (index, node) in network.nodes.iter().enumerate() {
        if lowerer.start(index) || lowerer.literal_node(index) {
            pieces.push(None);
            continue;
        }
        let place = Place {
            path: &lowerer.paths[index],
            values_in: node
                .inputs
                .iter()
                .map(|channel| network.channels[*channel].value)
                .collect(),
            values_out: node
                .outputs
                .iter()
                .map(|channel| network.channels[*channel].value)
                .collect(),
            context,
        };
        let made = piece(node, &place, sources, &mut bodies).map_err(|reason| Kept {
            path: node.name.clone(),
            reason,
        })?;
        pieces.push(Some(made));
    }
    lowerer.pieces = pieces;
    let mut graphs = Vec::new();
    for scope in 0..network.scopes.len() {
        graphs.push(lowerer.graph(scope)?);
    }
    graphs.extend(bodies);
    Ok(graphs)
}

struct Lowerer<'a> {
    network: &'a Network,
    /// The node and output that writes each channel.
    producer: Vec<Option<(usize, usize)>>,
    paths: Vec<String>,
    pieces: Vec<Option<Piece>>,
}

/// The ops and edges of one graph while it is made.
#[derive(Default)]
struct Draft {
    ops: Vec<Json>,
    edges: Vec<Json>,
}

impl<'a> Lowerer<'a> {
    fn new(network: &'a Network) -> Self {
        let mut producer = vec![None; network.channels.len()];
        for (index, node) in network.nodes.iter().enumerate() {
            for (output, channel) in node.outputs.iter().enumerate() {
                producer[*channel] = Some((index, output));
            }
        }
        let paths = (network.nodes.iter().enumerate())
            .map(|(index, node)| match &node.kind {
                Kind::Process { call, .. } => {
                    call.name.rsplit(':').next().unwrap_or_default().to_owned()
                }
                _ => format!("{}.{index}", name(&node.name)),
            })
            .collect();
        Lowerer {
            network,
            producer,
            paths,
            pieces: Vec::new(),
        }
    }

    /// Whether a node belongs to the start of the run: a check, a print or a written file.
    fn start(&self, node: usize) -> bool {
        let kind = &self.network.nodes[node].kind;
        matches!(
            kind,
            Kind::Check { .. } | Kind::Print(_) | Kind::Write { .. }
        )
    }

    /// Whether a node only binds a value channel, which is then a literal of its readers.
    fn literal_node(&self, node: usize) -> bool {
        let node = &self.network.nodes[node];
        let value = node
            .outputs
            .first()
            .is_some_and(|channel| self.network.channels[*channel].value);
        value && matches!(&node.kind, Kind::Values { values, close: true } if values.len() == 1)
    }

    fn literal(&self, channel: usize) -> Option<Literal> {
        match self.producer[channel] {
            Some((node, _)) if self.literal_node(node) => {
                let Kind::Values { values, .. } = &self.network.nodes[node].kind else {
                    return None;
                };
                Some(Literal::Value(values[0].clone()))
            }
            Some(_) => None,
            None => match &self.network.channels[channel] {
                channel if channel.value => channel.bound.clone().map(Literal::Value),
                _ => Some(Literal::Empty),
            },
        }
    }

    /// Whether the scope `inner` is `outer` or inside it.
    fn inside(&self, outer: usize, inner: usize) -> bool {
        let mut scope = Some(inner);
        while let Some(current) = scope {
            if current == outer {
                return true;
            }
            scope = self.network.scopes[current].parent;
        }
        false
    }

    /// The child of `outer` that holds the scope `inner`.
    fn child(&self, outer: usize, inner: usize) -> usize {
        let mut scope = inner;
        while self.network.scopes[scope].parent != Some(outer) {
            scope = self.network.scopes[scope].parent.unwrap_or(outer);
        }
        scope
    }

    /// The channels that the nodes of a scope read and a node outside it writes.
    fn inputs(&self, scope: usize) -> BTreeSet<usize> {
        let mut inputs = BTreeSet::new();
        for node in self
            .network
            .nodes
            .iter()
            .filter(|node| self.inside(scope, node.scope))
        {
            for channel in &node.inputs {
                if let Some((writer, _)) = self.producer[*channel]
                    && self.literal(*channel).is_none()
                    && !self.inside(scope, self.network.nodes[writer].scope)
                {
                    inputs.insert(*channel);
                }
            }
        }
        inputs
    }

    /// The channels that the nodes of a call write and a node outside it reads, or that the
    /// entry workflow emits.
    fn outputs(&self, scope: usize) -> BTreeSet<usize> {
        let emitted: BTreeSet<usize> = self
            .network
            .emits
            .iter()
            .map(|(_, channel)| *channel)
            .collect();
        let mut outputs = BTreeSet::new();
        for (channel, writer) in self.producer.iter().enumerate() {
            let Some((writer, _)) = writer else {
                continue;
            };
            if !self.inside(scope, self.network.nodes[*writer].scope)
                || self.literal(channel).is_some()
            {
                continue;
            }
            let read_outside = (self.network.nodes.iter())
                .any(|node| node.inputs.contains(&channel) && !self.inside(scope, node.scope));
            if read_outside || emitted.contains(&channel) {
                outputs.insert(channel);
            }
        }
        outputs
    }

    fn graph(&self, scope: usize) -> Result<Json, Kept> {
        let network = self.network;
        let mut draft = Draft::default();
        let mut calls = Vec::new();
        for (index, node) in network.nodes.iter().enumerate() {
            let Some(piece) = self.pieces[index].as_ref().filter(|_| node.scope == scope) else {
                continue;
            };
            draft.ops.extend(piece.ops.iter().cloned());
            draft.edges.extend(piece.edges.iter().cloned());
            for op in &piece.params {
                draft
                    .edges
                    .push(json!({"from": {"port": "params"}, "to": {"op": op, "port": "params"}}));
            }
        }
        for (child, record) in network.scopes.iter().enumerate() {
            if record.parent != Some(scope) {
                continue;
            }
            let path = name(record.name.rsplit(':').next().unwrap_or_default());
            let inputs = self.inputs(child);
            let outputs = self.outputs(child);
            let mut ports: Vec<Json> = (inputs.iter())
                .map(|channel| {
                    port(
                        &format!("channel.{channel}"),
                        !network.channels[*channel].value,
                    )
                })
                .collect();
            ports.push(port("params", false));
            let outs: Vec<Json> = (outputs.iter())
                .map(|channel| {
                    port(
                        &format!("channel.{channel}"),
                        !network.channels[*channel].value,
                    )
                })
                .collect();
            draft.ops.push(json!({
                "path": path,
                "kind": {"call": {"graph": graph_name(&record.name)}},
                "inputs": ports,
                "outputs": outs,
            }));
            draft
                .edges
                .push(json!({"from": {"port": "params"}, "to": {"op": path, "port": "params"}}));
            calls.push((path, inputs));
        }
        // The readers of every channel: the inputs of the pieces, then those of the calls.
        for (index, node) in network.nodes.iter().enumerate() {
            let Some(piece) = self.pieces[index].as_ref().filter(|_| node.scope == scope) else {
                continue;
            };
            for (channel, reader) in node.inputs.iter().zip(&piece.inputs) {
                self.read(scope, *channel, reader, &mut draft)?;
            }
        }
        for (path, inputs) in &calls {
            for channel in inputs {
                let stream = !network.channels[*channel].value;
                let reader = end(path, &format!("channel.{channel}"), stream);
                self.read(scope, *channel, &reader, &mut draft)?;
            }
        }
        let mut inputs: Vec<Json> = Vec::new();
        if scope != 0 {
            for channel in self.inputs(scope) {
                inputs.push(port(
                    &format!("channel.{channel}"),
                    !network.channels[channel].value,
                ));
            }
        }
        let mut params = port("params", false);
        if scope == 0 {
            params["default"] = network.params.clone();
        }
        inputs.push(params);
        let mut outputs = Vec::new();
        let named: Vec<(String, usize)> = match scope {
            0 => network.emits.clone(),
            _ => (self.outputs(scope).into_iter())
                .map(|channel| (format!("channel.{channel}"), channel))
                .collect(),
        };
        for (output, channel) in named {
            let stream = !network.channels[channel].value;
            outputs.push(port(&output, stream));
            let from = match self.literal(channel) {
                Some(_) => return Err(kept(&output, "an output that the graph binds")),
                None => self.writer(scope, channel)?,
            };
            if from.stream != stream {
                return Err(kept(&output, "an output of the wrong flow"));
            }
            draft
                .edges
                .push(json!({"from": endpoint(&from), "to": {"port": output}}));
        }
        Ok(json!({
            "name": graph_name(&network.scopes[scope].name),
            "inputs": inputs,
            "outputs": outputs,
            "ops": draft.ops,
            "edges": draft.edges,
        }))
    }

    /// Connects a reader of a channel in a graph: by an edge from its writer, through an op
    /// that turns a value into a stream, or by a default for a literal value.
    fn read(
        &self,
        scope: usize,
        channel: usize,
        reader: &End,
        draft: &mut Draft,
    ) -> Result<(), Kept> {
        let from = match self.literal(channel) {
            Some(Literal::Value(value)) if !reader.stream => {
                default(draft, reader, value);
                return Ok(());
            }
            Some(_) => return Err(kept(&reader.op, "a literal where the op reads a stream")),
            None => self.writer(scope, channel)?,
        };
        if from.stream != reader.stream {
            return Err(kept(&reader.op, "a channel of the other flow"));
        }
        draft
            .edges
            .push(json!({"from": endpoint(&from), "to": {"op": reader.op, "port": reader.port}}));
        Ok(())
    }

    /// Where a channel comes from in a graph: a piece of the graph, a call of a child graph
    /// that holds its writer, or an input of the graph.
    fn writer(&self, scope: usize, channel: usize) -> Result<End, Kept> {
        let value = self.network.channels[channel].value;
        let Some((node, output)) = self.producer[channel] else {
            return Err(kept(
                &format!("channel.{channel}"),
                "a channel that no node writes",
            ));
        };
        let writer_scope = self.network.nodes[node].scope;
        if writer_scope == scope {
            let piece = self.pieces[node]
                .as_ref()
                .ok_or_else(|| kept(&self.paths[node], "a node without ops"))?;
            return Ok(piece.outputs[output].clone());
        }
        let port_name = format!("channel.{channel}");
        if self.inside(scope, writer_scope) {
            let child = self.child(scope, writer_scope);
            let path = name(
                self.network.scopes[child]
                    .name
                    .rsplit(':')
                    .next()
                    .unwrap_or_default(),
            );
            return Ok(end(&path, &port_name, !value));
        }
        Ok(End {
            op: String::new(),
            port: port_name,
            stream: !value,
        })
    }
}

/// Sets the default of an op input that reads a literal value.
fn default(draft: &mut Draft, reader: &End, value: Json) {
    let op = draft
        .ops
        .iter_mut()
        .find(|op| op["path"] == reader.op.as_str());
    let inputs = op.and_then(|op| op["inputs"].as_array_mut());
    if let Some(input) = inputs.and_then(|inputs| {
        inputs
            .iter_mut()
            .find(|input| input["name"] == reader.port.as_str())
    }) {
        input["default"] = value;
    }
}

fn endpoint(end: &End) -> Json {
    match end.op.is_empty() {
        true => json!({"port": end.port}),
        false => json!({"op": end.op, "port": end.port}),
    }
}

/// The graph of a workflow call, by its fully qualified name; the entry workflow is `main`.
fn graph_name(qualified: &str) -> String {
    match qualified {
        "" => "main".into(),
        qualified => format!("workflow.{}", name(&qualified.replace(':', "."))),
    }
}

/// A name of letters, digits, `_`, `-` and `.`.
fn name(text: &str) -> String {
    (text.chars())
        .map(|letter| match letter {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.' => letter,
            _ => '-',
        })
        .collect()
}

fn kept(path: &str, reason: &str) -> Kept {
    Kept {
        path: path.into(),
        reason: reason.into(),
    }
}
