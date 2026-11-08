//! Checks the execution rules of attributes and effects: placement, reuse and the settings
//! that a run refuses at planning.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{
    Attempt, ExecuteError, ExecutionGraph, Executor, Launch, Reuse, Running, Simulator, Task,
    execute,
};

/// The example `two-tools.json` in `root`, changed by `change`, with its inputs.
fn example(root: &Path, change: impl FnOnce(&mut Json)) -> Launch {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../wir/examples/two-tools.json");
    let mut document: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    change(&mut document);
    std::fs::write(root.join("two-tools.json"), document.to_string()).unwrap();
    std::fs::write(root.join("text.txt"), "b\na\n").unwrap();
    let format = "http://edamontology.org/format_2330";
    let text = json!({"class": "File", "location": "text.txt", "format": format});
    let inputs = json!({"text": text, "lines": 2});
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    Launch {
        target: root.join("two-tools.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    }
}

fn settings() -> Settings {
    Settings {
        print: false,
        ..Settings::default()
    }
}

/// The graph of a simulated run of the changed example.
fn simulate(change: impl FnOnce(&mut Json)) -> ExecutionGraph {
    let folder = tempfile::tempdir().unwrap();
    let launch = Launch {
        simulated: true,
        ..example(folder.path(), change)
    };
    let mut driver = WirDriver::new(&launch, settings()).unwrap();
    let mut simulator = Simulator::new(folder.path().join("simulator"));
    execute("rules", &mut driver, &mut simulator)
        .into_result()
        .unwrap()
}

/// Why the changed example cannot be planned.
fn refused(change: impl FnOnce(&mut Json)) -> String {
    let folder = tempfile::tempdir().unwrap();
    let launch = example(folder.path(), change);
    match WirDriver::new(&launch, settings()) {
        Ok(_) => panic!("the run was planned"),
        Err(error) => {
            assert!(error.unsupported, "{}", error.message);
            error.message
        }
    }
}

/// The op `index` of the main graph of a document.
fn op(document: &mut Json, index: usize) -> &mut Json {
    &mut document["graphs"][0]["ops"][index]
}

/// The tasks of a simulated run of a native op with `attributes` that runs a CWL scatter
/// demo, or why the run cannot be planned.
fn native(attributes: Json) -> Result<Vec<Task>, String> {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let workflow = Path::new(env!("CARGO_MANIFEST_DIR")).join("../demo/cwl/scatter-workflow.cwl");
    let port = json!([{"name": "message_array", "type": {"array": {"items": "string"}}}]);
    let document = json!({"wir_version": "0.1",
        "sources": [{"id": "block", "language": "cwl", "location": workflow}],
        "graphs": [{"name": "main", "inputs": port, "outputs": [],
            "ops": [{"path": "block", "kind": {"native": {"source": "block", "language": "cwl"}},
                "inputs": port, "attributes": attributes}],
            "edges": [{"from": {"port": "message_array"},
                "to": {"op": "block", "port": "message_array"}}]}],
        "entry": "main"});
    std::fs::write(root.join("run.wir.json"), document.to_string()).unwrap();
    let inputs = json!({"message_array": ["one", "two"]});
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    let launch = Launch {
        target: root.join("run.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = WirDriver::new(&launch, settings()).map_err(|error| error.message)?;
    let mut simulator = Simulator::new(root.join("simulator"));
    let graph = execute("native", &mut driver, &mut simulator).into_result();
    Ok(graph
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| node.task)
        .collect())
}

/// A pure op that gives its input `lines`, which has a default, as it is.
fn expr(attributes: Json) -> Json {
    json!({"path": "same", "kind": {"expr": {"language": "js", "code": "$(inputs.lines)"}},
        "inputs": [{"name": "lines", "type": "int", "default": 1}],
        "outputs": [{"name": "out", "type": "int"}], "attributes": attributes})
}

#[test]
fn places_task_ops() {
    let graph = simulate(|document| {
        op(document, 0)["attributes"]["place"] = json!("hamburg");
    });
    let sites: Vec<Option<&str>> = (graph.nodes.iter())
        .map(|node| node.task.site.as_deref())
        .collect();
    assert_eq!(sites, [Some("hamburg"), None], "only sort is placed");
}

/// Refuses every task as an executor does that lacks its site.
struct Siteless;

impl Executor for Siteless {
    fn run(&mut self, attempt: Attempt) -> Running {
        let site = attempt.task.site.clone().unwrap_or_default();
        Running::ended(Err(ExecuteError::Executor {
            task: attempt.task.name.clone(),
            message: format!("the task is placed at {site}, a site this run lacks"),
        }))
    }
}

#[test]
fn reports_missing_sites() {
    let folder = tempfile::tempdir().unwrap();
    let launch = example(folder.path(), |document| {
        op(document, 0)["attributes"] = json!({"place": "hamburg"});
    });
    let mut driver = WirDriver::new(&launch, settings()).unwrap();
    let execution = execute("rules", &mut driver, &mut Siteless);
    let error = execution.into_result().unwrap_err().to_string();
    let message = "op sort: task sort: the task is placed at hamburg, a site this run lacks";
    assert_eq!(error, message);
}

#[test]
fn refuses_engine_places() {
    let message = refused(|document| {
        let ops = document["graphs"][0]["ops"].as_array_mut().unwrap();
        ops.push(expr(json!({"place": "hamburg"})));
    });
    assert_eq!(
        message,
        "op main/same: only task and native ops run at a place"
    );
}

#[test]
fn places_native_tasks() {
    let tasks = native(json!({"place": "hamburg", "cache": "never"})).unwrap();
    assert_eq!(tasks.len(), 2, "one task per message");
    for task in &tasks {
        assert_eq!(task.site.as_deref(), Some("hamburg"), "{}", task.name);
        assert_eq!(task.reuse, Reuse::Disabled);
    }
}

#[test]
fn reuses_resumed_results() {
    let graph = simulate(|document| {
        op(document, 0)["attributes"]["cache"] = json!("resume");
    });
    let reuses: Vec<Reuse> = graph.nodes.iter().map(|node| node.task.reuse).collect();
    assert_eq!(reuses, [Reuse::Lineage, Reuse::Allowed]);
    let tasks = native(json!({"cache": "resume"})).unwrap();
    assert!(tasks.iter().all(|task| task.reuse == Reuse::Lineage));
}
