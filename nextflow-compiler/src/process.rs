//! The process ops of process calls, each with a body graph whose task is a Nextflow dialect
//! task, and the ops of factories that read when the run starts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_runtime::export::Output;
use serde_json::{Value as Json, json};

use crate::Sources;
use crate::groovy::text;
use crate::operators::{Piece, Place, edge, end, port};

/// The call of a process, as the export describes it.
pub(crate) struct Definition<'a> {
    pub call: &'a nextflow_runtime::Call,
    pub each: &'a [usize],
    pub span: (u32, u32),
    pub outputs: &'a [Output],
}

/// A process op that runs its body graph per round, whose one task builds the native task
/// of the process call.
pub(crate) fn process(
    definition: &Definition,
    place: &Place,
    sources: &mut Sources,
    bodies: &mut Vec<Json>,
) -> Piece {
    let path = place.path;
    let call = definition.call;
    // A call of value channels reads them as streams, so that an empty one runs no task and
    // closes the outputs empty, as in Nextflow; it runs at most one round.
    let reads = |value: &bool| !value || call.singleton;
    let streams = place.values_in.iter().any(reads);
    let source = sources.script(&call.script);
    let inputs: Vec<String> = (0..place.values_in.len())
        .map(|index| format!("input.{index}"))
        .collect();
    let outputs: Vec<String> = (0..definition.outputs.len())
        .map(|index| format!("output.{index}"))
        .collect();
    let body = format!("task.{}", call.name.replace(':', "."));
    let configs: Vec<String> = (place.context.configs.iter())
        .map(|path| sources.config(path))
        .collect();
    let maps = |names: &[String], side: &str| -> Vec<Json> {
        (names.iter().enumerate())
            .map(|(index, name)| json!({"port": name, "native": format!("{side}/{index}")}))
            .collect()
    };
    let mut task = json!({
        "source": sources.main(),
        "entry": call.process,
        "call": call.name,
        "scope": {"source": source, "start": definition.span.0, "end": definition.span.1},
        "inputs": maps(&inputs, "input"),
        "outputs": maps(&outputs, "output"),
        "captures": {"singleton": {"value": call.singleton}},
    });
    // Observers of the run, like nf-prov, know the process by its id and the ids it feeds.
    if let Some(process) = place.observer {
        let observer = json!({"id": process.id, "feeds": process.feeds});
        task["captures"]["observer"] = json!({"value": observer});
    }
    if !place.context.profiles.is_empty() {
        task["profiles"] = json!(place.context.profiles);
    }
    if !configs.is_empty() {
        task["configs"] = json!(configs);
    }
    let value_ports =
        |names: &[String]| -> Vec<Json> { names.iter().map(|name| port(name, false)).collect() };
    let body_inputs = value_ports(&inputs);
    let task_op = json!({
        "path": "task",
        "kind": {"task": {"builder": {"nextflow_process": task}}},
        "inputs": body_inputs,
        "outputs": value_ports(&outputs),
    });
    let mut edges: Vec<Json> = (inputs.iter())
        .map(|name| json!({"from": {"port": name}, "to": {"op": "task", "port": name}}))
        .collect();
    edges.extend(
        (outputs.iter())
            .map(|name| json!({"from": {"op": "task", "port": name}, "to": {"port": name}})),
    );
    bodies.push(json!({
        "name": body,
        "inputs": body_inputs,
        "outputs": value_ports(&outputs),
        "ops": [task_op],
        "edges": edges,
    }));
    let each: Vec<&String> = definition
        .each
        .iter()
        .map(|index| &inputs[*index])
        .collect();
    let mut process = json!({"body": body});
    if !each.is_empty() {
        process["each"] = json!(each);
    }
    let op_inputs: Vec<Json> = (inputs.iter().zip(&place.values_in))
        .map(|(name, value)| port(name, reads(value)))
        .collect();
    let op_outputs: Vec<Json> = outputs.iter().map(|name| port(name, streams)).collect();
    let op = json!({
        "path": path,
        "kind": {"process": process},
        "inputs": op_inputs,
        "outputs": op_outputs,
        "values": "groovy",
        "origin": {"source": source},
    });
    Piece {
        ops: vec![op],
        edges: Vec::new(),
        inputs: (inputs.iter().zip(&place.values_in))
            .map(|(name, value)| end(path, name, reads(value)))
            .collect(),
        outputs: outputs
            .iter()
            .map(|name| end(path, name, streams))
            .collect(),
    }
}

/// A factory that reads when the run starts: a Groovy expression of the factory, whose list a
/// queue channel gets item by item.
pub(crate) fn source(
    path: &str,
    factory: &str,
    arguments: &[Json],
    place: &Place,
) -> Result<Piece, String> {
    let (plugin, name) = match factory.split_once(':') {
        Some((plugin, name)) => (Some(plugin), name),
        None => (None, factory),
    };
    let queue = !place.values_out[0];
    let code = match queue {
        true => format!("channel.{name}(*arguments)"),
        false => format!("{name}(*arguments)"),
    };
    let mut expr = json!({
        "language": "groovy",
        "code": code,
        "captures": {"arguments": {"value": arguments}},
    });
    if let Some(plugin) = plugin {
        expr["library"] = json!([format!(
            "include {{ {name} }} from {}",
            text(&format!("plugin/{plugin}"))
        )]);
    }
    let op = json!({
        "path": path,
        "kind": {"expr": expr},
        "outputs": [port("out", false)],
        "effects": ["read_files"],
    });
    let mut piece = Piece {
        ops: vec![op],
        outputs: vec![end(path, "out", false)],
        ..Piece::default()
    };
    if queue {
        let spread = format!("{path}/items");
        piece.ops.push(json!({
            "path": spread,
            "kind": {"emit": {"each": true}},
            "inputs": [port("source", false)],
            "outputs": [port("out", true)],
        }));
        piece.edges.push(edge(path, "out", &spread, "source"));
        piece.outputs = vec![end(&spread, "out", true)];
    }
    Ok(piece)
}
