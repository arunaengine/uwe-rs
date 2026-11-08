//! The ops of the start of a compiled run in `main`: the checks, prints and written files of
//! the build, in the order the build met them, before every other op of `main`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_runtime::export::{Kind, Network};
use serde_json::{Value as Json, json};

use crate::Kept;
use crate::groovy::text;
use crate::operators::{edge, port};

/// The ops of the start of the run in `main`, in the order the build met them: checks,
/// prints and written files. Every other op of `main` waits for them.
pub(crate) fn ops(network: &Network, main: &mut Json) -> Result<(), Kept> {
    let mut ops = Vec::new();
    let mut edges = Vec::new();
    let mut last: Option<String> = None;
    for (index, node) in network.nodes.iter().enumerate() {
        let path = format!("start.{index}");
        let mut made = match &node.kind {
            Kind::Check {
                plugin,
                function,
                arguments,
            } => {
                let include = format!(
                    "include {{ {function} }} from {}",
                    text(&format!("plugin/{plugin}"))
                );
                vec![json!({
                    "path": path,
                    "kind": {"expr": {
                        "language": "groovy",
                        "code": format!("{function}(*arguments)"),
                        "library": [include],
                        "captures": {"arguments": {"value": arguments}},
                    }},
                    "outputs": [port("out", false)],
                    "effects": ["log", "read_files"],
                })]
            }
            Kind::Print(printed) => {
                let view = format!("{path}/view");
                edges.push(edge(&path, "out", &view, "source"));
                vec![
                    emit(&path, json!([printed])),
                    json!({
                        "path": view,
                        "kind": {"view": {"new_line": false}},
                        "inputs": [port("source", true)],
                        "outputs": [port("out", true)],
                        "effects": ["log"],
                    }),
                ]
            }
            Kind::Write { path: target, text } => {
                let (folder, file) = target.rsplit_once('/').unwrap_or((".", target));
                let collect = format!("{path}/file");
                edges.push(edge(&path, "out", &collect, "source"));
                vec![
                    emit(&path, json!([text])),
                    json!({
                        "path": collect,
                        "kind": {"collect_file": {"name": file, "directory": folder, "sort": "none"}},
                        "inputs": [port("source", true)],
                        "outputs": [port("out", true)],
                        "effects": ["write_files", {"publish": folder}],
                    }),
                ]
            }
            _ => continue,
        };
        if let Some(previous) = &last {
            made[0]["after"] = json!([{"op": previous, "on": ["succeeded"]}]);
        }
        last = Some(
            made.last()
                .and_then(|op| op["path"].as_str())
                .unwrap_or_default()
                .to_owned(),
        );
        ops.extend(made);
    }
    let Some(last) = last else {
        return Ok(());
    };
    let barrier = json!([{"op": last, "on": ["succeeded"]}]);
    let main_ops = main["ops"]
        .as_array_mut()
        .ok_or_else(|| kept("main", "no ops"))?;
    for op in main_ops.iter_mut() {
        if op.get("after").is_none() {
            op["after"] = barrier.clone();
        }
    }
    main_ops.extend(ops);
    let main_edges = main["edges"]
        .as_array_mut()
        .ok_or_else(|| kept("main", "no edges"))?;
    main_edges.extend(edges);
    Ok(())
}

/// An emit of a literal array, item by item.
fn emit(path: &str, items: Json) -> Json {
    json!({
        "path": path,
        "kind": {"emit": {"each": true}},
        "inputs": [{"name": "source", "type": "dynamic", "default": items}],
        "outputs": [port("out", true)],
    })
}

fn kept(path: &str, reason: &str) -> Kept {
    Kept {
        path: path.into(),
        reason: reason.into(),
    }
}
