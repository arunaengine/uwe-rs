//! The ops of operators whose compiled form needs more than one field: `branch` and `multiMap`,
//! methods of values, `transpose`, the splitter formats and the sort orders.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::split::Format;
use nextflow_runtime::export::Order;
use serde_json::{Map, Value as Json, json};

use crate::Sources;
use crate::groovy::{closure, literal, text, written};
use crate::operators::{Piece, edge, end, port, unary};

/// `branch` and `multiMap`: the criteria closure of the script gives the label and value of
/// each item, and a route or a multi-map sends it on by its label.
pub(crate) fn route(
    path: &str,
    labels: &[String],
    criteria: &Json,
    all: bool,
    stream: bool,
) -> Result<Piece, String> {
    let chosen = format!("{path}/choose");
    let mut piece = unary(path, "transform", json!({"body": criteria}), stream, stream);
    let kind = match all {
        false => {
            let cases: Vec<Json> = (labels.iter())
                .map(|label| {
                    let condition = written(format!("{{ it -> it[0] == {} }}", text(label)));
                    let value = written("{ it -> it[1] }".into());
                    json!({"output": label, "condition": condition, "body": value})
                })
                .collect();
            json!({"route": {"cases": cases}})
        }
        // A label that the criteria set gives its value, null too; one it did not set gives
        // nothing, so each value goes wrapped in a list that `<choose>.<index>` takes off.
        true => {
            let outputs: Vec<Json> = (labels.iter())
                .map(|label| {
                    let key = text(label);
                    let code = format!("{{ it -> it.containsKey({key}) ? [it[{key}]] : null }}");
                    json!({"output": label, "body": written(code), "nulls": "drop"})
                })
                .collect();
            json!({"multi_map": {"outputs": outputs}})
        }
    };
    let outputs: Vec<Json> = labels.iter().map(|label| port(label, true)).collect();
    piece.ops.push(json!({
        "path": chosen,
        "kind": kind,
        "inputs": [port("source", true)],
        "outputs": outputs,
        "values": "groovy",
    }));
    let source = match stream {
        true => (path.to_owned(), "out".to_owned()),
        // A branch of a value channel routes its one value.
        false => {
            let spread = format!("{path}/item");
            piece.ops.push(json!({
                "path": spread,
                "kind": {"emit": {}},
                "inputs": [port("source", false)],
                "outputs": [port("out", true)],
            }));
            piece.edges.push(edge(path, "out", &spread, "source"));
            (spread, "out".to_owned())
        }
    };
    piece
        .edges
        .push(edge(&source.0, &source.1, &chosen, "source"));
    piece.outputs = labels
        .iter()
        .map(|label| end(&chosen, label, true))
        .collect();
    if all {
        for (index, label) in labels.iter().enumerate() {
            let unwrap = format!("{chosen}.{index}");
            let body = written("{ it -> it[0] }".into());
            let mut taken = unary(
                &unwrap,
                "transform",
                json!({"body": body, "nulls": "keep"}),
                true,
                true,
            );
            piece.ops.append(&mut taken.ops);
            piece.edges.push(edge(&chosen, label, &unwrap, "source"));
            piece.outputs[index] = end(&unwrap, "out", true);
        }
    }
    Ok(piece)
}

/// A method or property of the value of a value channel, with value channel arguments.
pub(crate) fn invoke(
    path: &str,
    name: &str,
    arguments: &[Option<Json>],
    property: bool,
) -> Result<Piece, String> {
    let mut channels = 0;
    let mut parts = Vec::new();
    for argument in arguments {
        parts.push(match argument {
            Some(value) => literal(value)?,
            None => {
                channels += 1;
                format!("argument{channels}")
            }
        });
    }
    let code = match property {
        true => format!("{{ it -> it.{name} }}"),
        false => format!("{{ it -> it.{name}({}) }}", parts.join(", ")),
    };
    let mut body = written(code);
    let mut inputs = vec![port("source", false)];
    let mut ends = vec![end(path, "source", false)];
    let mut captures = Map::new();
    for index in 1..=channels {
        let name = format!("argument{index}");
        captures.insert(name.clone(), json!({"input": name}));
        inputs.push(port(&name, false));
        ends.push(end(path, &name, false));
    }
    if !captures.is_empty() {
        body["captures"] = Json::Object(captures);
    }
    let op = json!({
        "path": path,
        "kind": {"transform": {"body": body, "nulls": "keep"}},
        "inputs": inputs,
        "outputs": [port("out", false)],
        "values": "groovy",
    });
    Ok(Piece {
        ops: vec![op],
        inputs: ends,
        outputs: vec![end(path, "out", false)],
        ..Piece::default()
    })
}

/// The Groovy body of `transpose`, as the native operator does it: each list element, or the
/// elements at `by`, gives one tuple per position, up to the shortest or the longest list.
pub(crate) fn transpose(by: &str, remainder: bool) -> String {
    let count = if remainder { "max" } else { "min" };
    format!(
        "{{ item -> if (!(item instanceof List)) {{ return [item] }}; \
         def lists = {by} != null ? {by} : (0..<item.size()).findAll {{ item[it] instanceof List }}; \
         if (lists.isEmpty()) {{ return [item] }}; \
         def count = lists.collect {{ item[it].size() }}.{count}(); \
         (0..<count).collect {{ row -> def tuple = item.collect {{ it }}; \
         lists.each {{ tuple[it] = row < item[it].size() ? item[it][row] : null }}; tuple }} }}"
    )
}

/// The splitter format of a native splitter.
pub(crate) fn format(format: &Format) -> Result<Json, String> {
    Ok(match format {
        Format::Csv {
            separator,
            quote,
            header,
            skip,
            strip,
        } => {
            let mut csv = json!({});
            if *separator != ',' {
                csv["separator"] = json!(separator.to_string());
            }
            if let Some(quote) = quote.filter(|quote| *quote != '"') {
                csv["quote"] = json!(quote.to_string());
            }
            match header {
                None => {}
                Some(None) => csv["header"] = json!("read"),
                Some(Some(names)) => {
                    let names: Vec<String> = names.iter().map(ToString::to_string).collect();
                    csv["header"] = json!({"names": names});
                }
            }
            if *skip > 0 {
                csv["skip"] = json!(skip);
            }
            if *strip {
                csv["strip"] = json!(true);
            }
            json!({"csv": csv})
        }
        Format::Text { keep_header } => match keep_header {
            true => json!({"text": {"keep_header": true}}),
            false => json!({"text": {}}),
        },
        Format::Fasta(fields) => json!({"fasta": sequence(fields)}),
        Format::Fastq(fields) => json!({"fastq": sequence(fields)}),
        Format::Json { path } if path.is_empty() => json!({"json": {}}),
        Format::Json { path } => json!({"json": {"path": path}}),
    })
}

/// The record fields of a FASTA or FASTQ splitter, by their native names.
pub(crate) fn sequence(fields: &Option<Vec<String>>) -> Json {
    let Some(fields) = fields else {
        return json!({});
    };
    let fields: Vec<Json> = (fields.iter())
        .map(|name| {
            let field = match name.as_str() {
                "seqString" => "seq_string".to_owned(),
                "readHeader" => "read_header".to_owned(),
                "readString" => "read_string".to_owned(),
                "qualityHeader" => "quality_header".to_owned(),
                "qualityString" => "quality_string".to_owned(),
                other => other.to_owned(),
            };
            json!({"name": name, "field": field})
        })
        .collect();
    json!({"fields": fields})
}

/// The sort field of a collecting op, `None` for the default of no sort.
pub(crate) fn sorting(order: &Order, sources: &mut Sources) -> Option<Json> {
    Some(match order {
        Order::None => return None,
        Order::Natural => json!("natural"),
        Order::Key(callback) => json!({"key": closure(callback, sources)}),
        Order::Compare(callback) => json!({"compare": closure(callback, sources)}),
        Order::Hash { deep: false } => json!("hash"),
        Order::Hash { deep: true } => json!("deep"),
    })
}

/// The `by` field of a keyed op, left out for its default `[0]`.
pub(crate) fn keys(by: &[usize]) -> Json {
    match by {
        [0] => json!({}),
        by => json!({"by": by}),
    }
}
