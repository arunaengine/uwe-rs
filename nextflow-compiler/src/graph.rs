//! The graphs of a compiled network: one per workflow call, whose ports are the channels that
//! cross the call, with the process bodies, and the ops of the start of the run in `main`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use nextflow_runtime::export::{Kind, Network};
use serde_json::{Value as Json, json};

use crate::operators::{End, Piece, Place, edge, end, piece, port};
use crate::{Context, Kept, Sources};

/// What a channel without a node that writes it holds.
enum Literal {
    Value(Json),
    Empty,
}

/// The graphs of a network, `main` first; a node that does not compile is the error.
pub(crate) fn compile(
    network: &Network,
    context: &Context,
    sources: &mut Sources,
) -> Result<Vec<Json>, Kept> {
    let mut compiler = Compiler::new(network);
    let mut bodies = Vec::new();
    let mut pieces: Vec<Option<Piece>> = Vec::new();
    for (index, node) in network.nodes.iter().enumerate() {
        if compiler.start(index) || compiler.literal_node(index) {
            pieces.push(None);
            continue;
        }
        let place = Place {
            path: &compiler.paths[index],
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
            observer: (network.processes.iter()).find(|process| process.name == node.name),
        };
        let made = piece(node, &place, sources, &mut bodies).map_err(|reason| Kept {
            path: node.name.clone(),
            reason,
        })?;
        pieces.push(Some(made));
    }
    compiler.pieces = pieces;
    let mut graphs = Vec::new();
    for scope in 0..network.scopes.len() {
        graphs.push(compiler.graph(scope)?);
    }
    crate::start::ops(network, &mut graphs[0])?;
    graphs.extend(bodies);
    Ok(graphs)
}

struct Compiler<'a> {
    network: &'a Network,
    /// The node and output that writes each channel.
    producer: Vec<Option<(usize, usize)>>,
    paths: Vec<String>,
    pieces: Vec<Option<Piece>>,
    /// The workflow call whose graph holds each node.
    scopes: Vec<usize>,
}

/// The ops and edges of one graph while it is made.
#[derive(Default)]
struct Draft {
    ops: Vec<Json>,
    edges: Vec<Json>,
    /// The ops that turn a value channel into a stream, or a literal into one, by channel.
    streams: BTreeMap<usize, String>,
    /// The ops that turn a stream of a node into its value channel, by channel.
    values: BTreeMap<usize, String>,
}

impl<'a> Compiler<'a> {
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
        let mut compiler = Compiler {
            network,
            producer,
            paths,
            pieces: Vec::new(),
            scopes: network.nodes.iter().map(|node| node.scope).collect(),
        };
        for (index, node) in network.nodes.iter().enumerate() {
            if node.name.starts_with("topic ") {
                compiler.scopes[index] = compiler.common(index);
            }
        }
        compiler
    }

    /// The deepest workflow call that holds the readers of a node, else its writers, where the
    /// mix of a topic belongs, so that no call both feeds the mix and reads it.
    fn common(&self, node: usize) -> usize {
        let record = &self.network.nodes[node];
        let mut scopes = Vec::new();
        for (index, other) in self.network.nodes.iter().enumerate() {
            if other
                .inputs
                .iter()
                .any(|channel| record.outputs.contains(channel))
            {
                scopes.push(self.scopes[index]);
            }
        }
        if self
            .network
            .emits
            .iter()
            .any(|(_, channel)| record.outputs.contains(channel))
        {
            scopes.push(0);
        }
        if scopes.is_empty() {
            let writers = (record.inputs.iter()).filter_map(|channel| self.producer[*channel]);
            scopes = writers.map(|(writer, _)| self.scopes[writer]).collect();
        }
        let Some(first) = scopes.first().copied() else {
            return record.scope;
        };
        let mut common = first;
        while !scopes.iter().all(|scope| self.inside(common, *scope)) {
            common = self.network.scopes[common].parent.unwrap_or(0);
        }
        common
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
        for (index, node) in self.network.nodes.iter().enumerate() {
            if !self.inside(scope, self.scopes[index]) {
                continue;
            }
            for channel in &node.inputs {
                if let Some((writer, _)) = self.producer[*channel]
                    && self.literal(*channel).is_none()
                    && !self.inside(scope, self.scopes[writer])
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
            if !self.inside(scope, self.scopes[*writer]) || self.literal(channel).is_some() {
                continue;
            }
            let read_outside = (self.network.nodes.iter().enumerate()).any(|(index, node)| {
                node.inputs.contains(&channel) && !self.inside(scope, self.scopes[index])
            });
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
        for index in 0..network.nodes.len() {
            let Some(piece) = self.pieces[index]
                .as_ref()
                .filter(|_| self.scopes[index] == scope)
            else {
                continue;
            };
            draft.ops.extend(piece.ops.iter().cloned());
            draft.edges.extend(piece.edges.iter().cloned());
        }
        for (child, record) in network.scopes.iter().enumerate() {
            if record.parent != Some(scope) {
                continue;
            }
            let path = name(record.name.rsplit(':').next().unwrap_or_default());
            let inputs = self.inputs(child);
            let outputs = self.outputs(child);
            let ports: Vec<Json> = (inputs.iter())
                .map(|channel| {
                    port(
                        &format!("channel.{channel}"),
                        !network.channels[*channel].value,
                    )
                })
                .collect();
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
            calls.push((path, inputs));
        }
        // The readers of every channel: the inputs of the pieces, then those of the calls.
        for (index, node) in network.nodes.iter().enumerate() {
            let Some(piece) = self.pieces[index]
                .as_ref()
                .filter(|_| self.scopes[index] == scope)
            else {
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
        // The params of the run, which the Nextflow tasks of the document read.
        if scope == 0 {
            let schema = network.schema.as_ref();
            inputs.extend(crate::params::ports(&network.params, schema));
        }
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
                None => self.writer(scope, channel, &mut draft)?,
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
            Some(literal) => {
                let path = format!("literal.{channel}");
                if !draft.streams.contains_key(&channel) {
                    let (each, value) = match literal {
                        Literal::Value(value) => (false, value),
                        Literal::Empty => (true, json!([])),
                    };
                    let source = json!({"name": "source", "type": "dynamic", "default": value});
                    let mut kind = json!({});
                    if each {
                        kind["each"] = json!(true);
                    }
                    draft.ops.push(json!({
                        "path": path,
                        "kind": {"emit": kind},
                        "inputs": [source],
                        "outputs": [port("out", true)],
                    }));
                    draft.streams.insert(channel, path.clone());
                }
                end(&draft.streams[&channel], "out", true)
            }
            None => self.writer(scope, channel, draft)?,
        };
        let from = match (from.stream, reader.stream) {
            (false, true) => {
                let path = format!("emit.{channel}");
                if !draft.streams.contains_key(&channel) {
                    draft.ops.push(json!({
                        "path": path,
                        "kind": {"emit": {}},
                        "inputs": [port("source", false)],
                        "outputs": [port("out", true)],
                    }));
                    draft.edges.push(
                        json!({"from": endpoint(&from), "to": {"op": path, "port": "source"}}),
                    );
                    draft.streams.insert(channel, path.clone());
                }
                end(&draft.streams[&channel], "out", true)
            }
            (true, false) => return Err(kept(&reader.op, "a stream where the op reads a value")),
            _ => from,
        };
        draft
            .edges
            .push(json!({"from": endpoint(&from), "to": {"op": reader.op, "port": reader.port}}));
        Ok(())
    }

    /// Where a channel comes from in a graph: a piece of the graph, a call of a child graph
    /// that holds its writer, or an input of the graph.
    fn writer(&self, scope: usize, channel: usize, draft: &mut Draft) -> Result<End, Kept> {
        let value = self.network.channels[channel].value;
        let Some((node, output)) = self.producer[channel] else {
            return Err(kept(
                &format!("channel.{channel}"),
                "a channel that no node writes",
            ));
        };
        let writer_scope = self.scopes[node];
        if writer_scope == scope {
            let piece = self.pieces[node]
                .as_ref()
                .ok_or_else(|| kept(&self.paths[node], "a node without ops"))?;
            let from = piece.outputs[output].clone();
            if !(from.stream && value) {
                return Ok(from);
            }
            // A stream op that writes a value channel, like a branch of a value, gives its
            // first item.
            let path = format!("channel.{channel}.first");
            if !draft.values.contains_key(&channel) {
                draft.ops.push(json!({
                    "path": path,
                    "kind": {"first": {}},
                    "inputs": [port("source", true)],
                    "outputs": [port("out", false)],
                }));
                draft
                    .edges
                    .push(edge(&from.op, &from.port, &path, "source"));
                draft.values.insert(channel, path.clone());
            }
            return Ok(end(&draft.values[&channel], "out", false));
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
