//! The graphs of a lowered network: one per workflow call, whose ports are the channels that
//! cross the call, with the process bodies, and the ops of the start of the run in `main`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_runtime::export::{Kind, Network};
use serde_json::{Value as Json, json};

use crate::operators::{End, Piece, Place, piece, port};
use crate::{Context, Kept, Sources};

/// What a channel without a node that writes it holds.
enum Literal {
    Value(Json),
    Empty,
}

/// The graphs of a network, `main` first; a node without a lowering is the error.
pub(crate) fn lower(
    network: &Network,
    _context: &Context,
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
            values_out: node
                .outputs
                .iter()
                .map(|channel| network.channels[*channel].value)
                .collect(),
        };
        let made = piece(node, &place, sources, &mut bodies).map_err(|reason| Kept {
            path: node.name.clone(),
            reason,
        })?;
        pieces.push(Some(made));
    }
    lowerer.pieces = pieces;
    if let Some(node) = network.nodes.iter().find(|node| node.scope != 0) {
        let reason = "a node of a workflow call, which the lowering does not cover yet";
        return Err(Kept {
            path: node.name.clone(),
            reason: reason.into(),
        });
    }
    let mut graphs = vec![lowerer.graph(0)?];
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

    fn graph(&self, scope: usize) -> Result<Json, Kept> {
        let network = self.network;
        let mut draft = Draft::default();
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
        // The readers of every channel: the inputs of the pieces, then those of the calls.
        for (index, node) in network.nodes.iter().enumerate() {
            let Some(piece) = self.pieces[index].as_ref().filter(|_| node.scope == scope) else {
                continue;
            };
            for (channel, reader) in node.inputs.iter().zip(&piece.inputs) {
                self.read(scope, *channel, reader, &mut draft)?;
            }
        }
        let mut inputs: Vec<Json> = Vec::new();
        let mut params = port("params", false);
        if scope == 0 {
            params["default"] = network.params.clone();
        }
        inputs.push(params);
        let mut outputs = Vec::new();
        let named = network.emits.clone();
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
        let Some((node, output)) = self.producer[channel] else {
            return Err(kept(
                &format!("channel.{channel}"),
                "a channel that no node writes",
            ));
        };
        if self.network.nodes[node].scope == scope {
            let piece = self.pieces[node]
                .as_ref()
                .ok_or_else(|| kept(&self.paths[node], "a node without ops"))?;
            return Ok(piece.outputs[output].clone());
        }
        Err(kept(
            &format!("channel.{channel}"),
            "a channel of a workflow call",
        ))
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
