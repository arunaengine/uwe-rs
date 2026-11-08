//! The wir ops of each node of the network: a process call becomes a process op with a
//! Nextflow dialect task, an operator the stream ops of its semantics.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_runtime::export::{Callback, Kind, Node};
use serde_json::{Value as Json, json};

use crate::groovy::{closure, test, written};
use crate::process::{Definition, process, source};
use crate::{Context, Sources};

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
    /// Whether each input and each output of the node is a value channel.
    pub values_in: Vec<bool>,
    pub values_out: Vec<bool>,
    pub context: &'a Context,
}

/// The ops of a node; an error says why the node has no lowering.
pub(crate) fn piece(
    node: &Node,
    place: &Place,
    sources: &mut Sources,
    bodies: &mut Vec<Json>,
) -> Result<Piece, String> {
    let path = place.path;
    let source_stream = !place.values_in.first().copied().unwrap_or(false);
    let same = |kind: &str, payload: Json| unary(path, kind, payload, source_stream, source_stream);
    let streams = |kind: &str, payload: Json| unary(path, kind, payload, true, true);
    let gather = |kind: &str, payload: Json| unary(path, kind, payload, true, false);
    let mut body = |callback: &Callback| closure(callback, sources);
    Ok(match &node.kind {
        Kind::Process {
            call,
            each,
            start,
            end,
            outputs,
        } => {
            let definition = Definition {
                call,
                each,
                span: (*start, *end),
                outputs,
            };
            process(&definition, place, sources, bodies)
        }
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
        Kind::Source { factory, arguments } => source(path, factory, arguments, place)?,
        Kind::Map(callback) => same("transform", json!({"body": body(callback)})),
        Kind::Filter(criteria) => same("filter", json!({"body": test(criteria, sources)?})),
        Kind::FlatMap(callback) => {
            streams("flat_map", optional("body", callback.as_ref().map(body)))
        }
        Kind::Flatten => streams("flatten", json!({"depth": "all", "drop_null": true})),
        Kind::Cast(class) => {
            let code = written(format!("{{ it -> it as {class} }}"));
            same("transform", json!({"body": code}))
        }
        Kind::View { callback, new_line } => {
            let mut payload = optional("body", callback.as_ref().map(body));
            if !new_line {
                payload["new_line"] = json!(false);
            }
            let mut piece = same("view", payload);
            piece.ops[0]["effects"] = json!(["log"]);
            piece
        }
        Kind::Unique(callback) => streams("unique", optional("key", callback.as_ref().map(body))),
        Kind::Distinct(callback) => {
            let mut payload = optional("key", callback.as_ref().map(body));
            payload["initial"] = json!({"value": null});
            streams("distinct", payload)
        }
        Kind::Take(count) => streams("take", json!({"count": count})),
        Kind::Until(callback) => streams("until", json!({"predicate": body(callback)})),
        Kind::Scan { .. } => return Err("nf-boost's `scan`, which no wir op does".into()),
        Kind::ToList => gather("to_list", json!({})),
        Kind::IfEmpty(criteria) => match criteria {
            nextflow_runtime::export::Criteria::Value(value) => {
                same("if_empty", json!({"value": {"value": value}}))
            }
            other => same("if_empty", json!({"body": test(other, sources)?})),
        },
        Kind::First(criteria) => {
            let predicate = criteria.as_ref().map(|criteria| test(criteria, sources));
            gather("first", optional("predicate", predicate.transpose()?))
        }
        Kind::ToSortedList(callback) => {
            let sort = match callback {
                Some(callback) if callback.parameters == 2 => json!({"compare": body(callback)}),
                Some(callback) => json!({"key": body(callback)}),
                None => json!("natural"),
            };
            let collect = json!({"flat": false, "sort": sort});
            let fallback = json!({"value": {"value": []}});
            chain(
                path,
                ("collect", collect, true, false),
                ("if_empty", fallback, false, false),
            )
        }
        Kind::Count(criteria) => {
            let predicate = criteria.as_ref().map(|criteria| test(criteria, sources));
            let mut payload = optional("body", predicate.transpose()?);
            payload["rule"] = json!("count");
            gather("reduce", payload)
        }
        Kind::Sum(callback) => {
            let mut payload = optional("body", callback.as_ref().map(body));
            payload["rule"] = json!("sum");
            gather("reduce", payload)
        }
        Kind::Min(callback) | Kind::Max(callback) => {
            let rule = if matches!(node.kind, Kind::Min(_)) {
                "min"
            } else {
                "max"
            };
            let mut payload = json!({"rule": rule});
            match callback {
                Some(callback) if callback.parameters == 2 => {
                    payload["compare"] = body(callback);
                }
                Some(callback) => payload["body"] = body(callback),
                None => {}
            }
            gather("reduce", payload)
        }
        Kind::Reduce { callback, seed } => {
            let mut payload =
                json!({"rule": "fold", "body": body(callback), "nulls": "keep_state"});
            // A null seed is no seed, as for legacy Nextflow reduce.
            if let Some(seed) = seed.as_ref().filter(|seed| !seed.is_null()) {
                payload["seed"] = json!({"value": seed});
            }
            gather("reduce", payload)
        }
        Kind::Last => gather("last", json!({})),
        Kind::Collate {
            size,
            step,
            remainder,
        } => {
            let mut payload = json!({"size": size});
            if step != size {
                payload["step"] = json!(step);
            }
            if !remainder {
                payload["remainder"] = json!(false);
            }
            streams("window", payload)
        }
        Kind::Buffer {
            open,
            close,
            size,
            skip,
            remainder,
        } => {
            let rule = match close {
                Some(close) => {
                    let open = open.as_ref().map(|open| test(open, sources)).transpose()?;
                    let mut rule = optional("open", open);
                    rule["close"] = test(close, sources)?;
                    json!({"match": rule})
                }
                None => json!({"size": {"size": size, "skip": skip}}),
            };
            let mut payload = json!({"rule": rule});
            if *remainder {
                payload["remainder"] = json!(true);
            }
            streams("buffer", payload)
        }
        Kind::Spread => unary(path, "emit", json!({"each": true}), false, true),
        Kind::Publish { name } => return Err(format!("the workflow output `{name}`")),
        Kind::Opaque(reason) => return Err(reason.clone()),
        _ => {
            return Err(format!(
                "the `{}` node, which the lowering does not cover yet",
                node.name
            ));
        }
    })
}

fn optional(name: &str, value: Option<Json>) -> Json {
    let mut payload = json!({});
    if let Some(value) = value {
        payload[name] = value;
    }
    payload
}

/// An op with the input `source` and the output `out`.
pub(crate) fn unary(path: &str, kind: &str, payload: Json, input: bool, output: bool) -> Piece {
    let op = json!({
        "path": path,
        "kind": {kind: payload},
        "inputs": [port("source", input)],
        "outputs": [port("out", output)],
        "values": "groovy",
    });
    Piece {
        ops: vec![op],
        inputs: vec![end(path, "source", input)],
        outputs: vec![end(path, "out", output)],
        ..Piece::default()
    }
}

/// Two unary ops in a row, the second at `<path>/then`.
fn chain(path: &str, first: (&str, Json, bool, bool), second: (&str, Json, bool, bool)) -> Piece {
    let then = format!("{path}/then");
    let mut piece = unary(path, first.0, first.1, first.2, first.3);
    let next = unary(&then, second.0, second.1, second.2, second.3);
    piece.ops.extend(next.ops);
    piece.edges.push(edge(path, "out", &then, "source"));
    piece.outputs = next.outputs;
    piece
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

pub(crate) fn edge(from: &str, port: &str, to: &str, input: &str) -> Json {
    json!({"from": {"op": from, "port": port}, "to": {"op": to, "port": input}})
}
