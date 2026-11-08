//! The wir ops of each node of the network: a process call becomes a process op with a
//! Nextflow dialect task, an operator the stream ops of its semantics.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_runtime::export::{Kind, Node};
use serde_json::{Value as Json, json};

use crate::Sources;

/// The ops of one node, with the op ports that its input and output channels connect to.
#[derive(Debug, Default)]
pub(crate) struct Piece {
    pub ops: Vec<Json>,
    pub edges: Vec<Json>,
    pub inputs: Vec<End>,
    pub outputs: Vec<End>,
    /// The ops of the piece that read the params of the run, at their port `params`.
    pub params: Vec<String>,
}

/// An op port of a piece, and whether it carries a stream.
#[derive(Clone, Debug)]
pub(crate) struct End {
    pub op: String,
    pub port: String,
    pub stream: bool,
}

/// What a piece needs to know of the channels of its node and its place in the document.
pub(crate) struct Place<'a> {
    pub path: &'a str,
    /// Whether each output of the node is a value channel.
    pub values_out: Vec<bool>,
}

/// The ops of a node; an error says why the node has no lowering.
pub(crate) fn piece(
    node: &Node,
    place: &Place,
    _sources: &mut Sources,
    _bodies: &mut Vec<Json>,
) -> Result<Piece, String> {
    let path = place.path;
    Ok(match &node.kind {
        Kind::Values {
            values,
            close: true,
        } if !place.values_out[0] => {
            let source = json!({"name": "source", "type": "dynamic", "default": values});
            let op = json!({
                "path": path,
                "kind": {"emit": {"each": true}},
                "inputs": [source],
                "outputs": [port("out", true)],
            });
            Piece {
                ops: vec![op],
                outputs: vec![end(path, "out", true)],
                ..Piece::default()
            }
        }
        Kind::Values { .. } => return Err("a value channel that the graph binds".into()),
        Kind::Opaque(reason) => return Err(reason.clone()),
        _ => {
            return Err(format!(
                "the `{}` node, which the lowering does not cover yet",
                node.name
            ));
        }
    })
}

pub(crate) fn port(name: &str, stream: bool) -> Json {
    match stream {
        true => json!({"name": name, "type": "dynamic", "flow": "stream"}),
        false => json!({"name": name, "type": "dynamic"}),
    }
}

pub(crate) fn end(op: &str, port: &str, stream: bool) -> End {
    End {
        op: op.into(),
        port: port.into(),
        stream,
    }
}
