//! Compiles one step or DAG task: a dialect task that the Argo runtime expands as its native run
//! does, or a call of the graph of a steps or DAG template, and a map for a loop.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use argo_parser::model::Template;
use serde_json::json;
use wir::{Binding, Graph, Kind, Op, Port, PortMap, TaskBuilder, TaskRef, Type};

use crate::graphs::Place;
use crate::{SOURCE, artifact, op, path_type, port, text};

/// The dialect task that calls the entrypoint, a leaf, for the workflow itself, and its outputs.
pub(crate) fn entry_task(
    graph: &mut Graph,
    path: &str,
    place: &Place,
    template: &Template,
) -> Vec<Port> {
    let path_port = crate::with_default(port("node.path", path_type()), json!([]));
    let inputs = vec![(path_port, "parameter/node.path".to_owned())];
    let task = task_op(path, place, "entrypoint", template, inputs);
    let outputs = task.outputs.clone();
    graph.ops.push(task);
    outputs
}

/// A dialect task of the Argo runtime: `entry` holds the step `call`, which runs `template`.
fn task_op(
    path: &str,
    holding: &Place,
    call: &str,
    template: &Template,
    inputs: Vec<(Port, String)>,
) -> Op {
    let maybe = |r#type: Type| Type::Optional(Box::new(r#type));
    let mut outputs: Vec<(Port, String)> = Vec::new();
    for parameter in &template.outputs.parameters {
        let name = format!("outputs.parameters.{}", parameter.name);
        outputs.push((
            port(name.clone(), maybe(text())),
            format!("parameter/{name}"),
        ));
    }
    for output in &template.outputs.artifacts {
        let name = format!("outputs.artifacts.{}", output.name);
        outputs.push((port(name.clone(), artifact()), format!("artifact/{name}")));
    }
    for name in ["outputs.result", "exitCode"] {
        outputs.push((port(name, maybe(text())), format!("parameter/{name}")));
    }
    let map = |ports: &[(Port, String)]| -> Vec<PortMap> {
        (ports.iter())
            .map(|(port, native)| PortMap {
                port: port.name.clone(),
                native: native.clone(),
            })
            .collect()
    };
    let mut captures = BTreeMap::new();
    if let Some((document, cluster_scope)) = &holding.reference {
        let reference = json!({"name": document, "cluster_scope": cluster_scope});
        captures.insert("template_ref".to_owned(), Binding::Value(reference));
    }
    let reference = TaskRef {
        source: SOURCE.into(),
        entry: holding.name.clone(),
        call: call.to_owned(),
        scope: None,
        inputs: map(&inputs),
        outputs: map(&outputs),
        profiles: Vec::new(),
        configs: Vec::new(),
        captures,
    };
    let kind = Kind::Task(wir::Task {
        builder: Some(TaskBuilder::ArgoTask(reference)),
        ..wir::Task::default()
    });
    let mut task = op(path, kind);
    task.inputs = inputs.into_iter().map(|(port, _)| port).collect();
    task.outputs = outputs.into_iter().map(|(port, _)| port).collect();
    task
}
