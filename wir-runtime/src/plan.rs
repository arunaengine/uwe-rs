//! The op instances of a run: the ops of the entry graph and of the graphs they call, with
//! where each input gets its value.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use wir::{Document, Endpoint, Graph, Kind, Op, Status};
use workflow_graph::DriverError;

/// One op instance. Inside a call its path starts with the path of the call, like
/// `align/sort` for the op `sort` of the graph that the op `align` calls.
pub struct Node {
    pub path: String,
    pub op: Op,
    /// Where each input port gets its value.
    pub inputs: Vec<(String, Source)>,
    /// The nodes this one runs after, with the statuses that satisfy each.
    pub after: Vec<(usize, Vec<Status>)>,
    /// The call node whose graph holds this one; it starts only once that call started.
    pub parent: Option<usize>,
    /// For a call node: where each of its outputs comes from inside the called graph.
    pub exports: Vec<(String, Source)>,
}

/// Where a value comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// An input of the entry graph.
    Input(String),
    /// An output of a node.
    Output(usize, String),
    /// An input of a call node, read inside the called graph.
    Call(usize, String),
    /// No producer: an optional input, bound to null.
    Null,
}

/// The nodes of a run of the entry graph, and where each graph output comes from.
pub struct Plan {
    pub nodes: Vec<Node>,
    pub outputs: Vec<(String, Source)>,
}

/// Plans a verified document. Reserved op kinds parse and verify, but no runner of this
/// version runs them.
pub fn plan(document: &Document) -> Result<Plan, DriverError> {
    let mut nodes = Vec::new();
    let entry = graph(document, &document.entry)?;
    let outputs = instantiate(document, entry, "", None, &mut nodes)?;
    Ok(Plan { nodes, outputs })
}

fn graph<'d>(document: &'d Document, name: &str) -> Result<&'d Graph, DriverError> {
    (document.graphs.iter().find(|graph| graph.name == name))
        .ok_or_else(|| DriverError::new(format!("the document has no graph {name}")))
}

/// Adds the ops of `graph` to `nodes`, inside the call node `call` when there is one, and
/// gives where the outputs of the graph come from.
fn instantiate(
    document: &Document,
    graph: &Graph,
    prefix: &str,
    call: Option<usize>,
    nodes: &mut Vec<Node>,
) -> Result<Vec<(String, Source)>, DriverError> {
    let mut index = HashMap::new();
    for op in &graph.ops {
        if op.kind.is_reserved() {
            let message = format!("op {prefix}{}: uwe does not run this op kind yet", op.path);
            return Err(DriverError::unsupported(message));
        }
        index.insert(op.path.as_str(), nodes.len());
        nodes.push(Node {
            path: format!("{prefix}{}", op.path),
            op: op.clone(),
            inputs: Vec::new(),
            after: Vec::new(),
            parent: call,
            exports: Vec::new(),
        });
    }
    let source = |from: &Endpoint| match &from.op {
        Some(op) => Source::Output(index[op.as_str()], from.port.clone()),
        None => match call {
            Some(call) => Source::Call(call, from.port.clone()),
            None => Source::Input(from.port.clone()),
        },
    };
    let mut outputs = Vec::new();
    for op in &graph.ops {
        let at = index[op.path.as_str()];
        let inputs = (op.inputs.iter())
            .map(|port| {
                let edge = (graph.edges.iter()).find(|edge| {
                    edge.to.op.as_deref() == Some(op.path.as_str()) && edge.to.port == port.name
                });
                let from = edge.map_or(Source::Null, |edge| source(&edge.from));
                (port.name.clone(), from)
            })
            .collect();
        let after = (op.after.iter())
            .map(|dependency| (index[dependency.op.as_str()], dependency.on.clone()))
            .collect();
        nodes[at].inputs = inputs;
        nodes[at].after = after;
        if let Kind::Call(called) = &op.kind {
            let called = self::graph(document, &called.graph)?;
            let inner = format!("{prefix}{}/", op.path);
            nodes[at].exports = instantiate(document, called, &inner, Some(at), nodes)?;
        }
    }
    for port in &graph.outputs {
        let edge =
            (graph.edges.iter()).find(|edge| edge.to.op.is_none() && edge.to.port == port.name);
        outputs.push((
            port.name.clone(),
            edge.map_or(Source::Null, |edge| source(&edge.from)),
        ));
    }
    Ok(outputs)
}
