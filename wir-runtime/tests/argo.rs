//! Runs an Argo workflow natively and as the native op of a wir document, and compares the
//! tasks and the outputs; feeds its outputs to a task op and runs two blocks at once.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{
    Attempt, Driver, Executor, Launch, Outcome, OutputFile, Running, Simulator, Task, TaskId,
    execute,
};

/// The workflow of these tests.
fn workflow() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shout.yaml")
}

fn name_port() -> Json {
    json!([{"name": "name", "type": "string"}])
}

fn shout_ports() -> Json {
    json!([{"name": "line", "type": "string"}, {"name": "loud", "type": {"file": {}}}])
}

/// A block `path` that runs the workflow, with the outputs `outputs`.
fn block(path: &str, outputs: Json) -> Json {
    let native = json!({"source": "shout", "language": "argo"});
    json!({"path": path, "kind": {"native": native}, "inputs": name_port(), "outputs": outputs})
}

/// A wir document of `ops` and `edges` with the graph ports `ports`.
fn document(root: &Path, ops: Json, edges: Vec<Json>, ports: (Json, Json)) -> PathBuf {
    let document = json!({
        "wir_version": "0.3",
        "sources": [{"id": "shout", "language": "argo", "location": workflow()}],
        "graphs": [{
            "name": "main", "inputs": ports.0, "outputs": ports.1, "ops": ops, "edges": edges
        }],
        "entry": "main"
    });
    let path = root.join("run.wir.json");
    std::fs::write(&path, document.to_string()).unwrap();
    path
}

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

/// The inputs file of a wir run, and the parameter file of a native run, in `root`.
fn inputs(root: &Path, values: Json) -> PathBuf {
    let path = root.join("inputs.json");
    std::fs::write(&path, values.to_string()).unwrap();
    path
}

fn launch(root: &Path, target: PathBuf, inputs: PathBuf, simulated: bool) -> Launch {
    Launch {
        target,
        inputs: Some(inputs),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        simulated,
        ..Launch::default()
    }
}

fn settings() -> Settings {
    Settings {
        print: false,
        ..Settings::default()
    }
}

/// The workflow run alone with the parameter file `parameters`, delivering to the folder
/// that the block delivers to: the run and its driver.
fn native(
    root: &Path,
    parameters: PathBuf,
    executor: &mut dyn Executor,
    simulated: bool,
) -> (workflow_graph::Execution, argo_runtime::ArgoDriver) {
    let launch = Launch {
        target: workflow(),
        inputs: Some(parameters),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out/block")),
        simulated,
        ..Launch::default()
    };
    let services = argo_runtime::Services::default();
    let mut driver = argo_runtime::load_driver(&launch, services).unwrap();
    let execution = execute("native", &mut driver, executor);
    (execution, driver)
}

/// A task without what only names it in its run.
fn described(task: &Task) -> Task {
    Task {
        id: TaskId(0),
        name: String::new(),
        after: Vec::new(),
        ..task.clone()
    }
}

/// Runs each task by writing its command into each of its outputs.
struct Writer(PathBuf);

impl Executor for Writer {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        std::fs::create_dir_all(&self.0).unwrap();
        let files = (task.outputs.iter())
            .map(|output| {
                let name = output.trim_start_matches('/').replace('/', "-");
                let location = self.0.join(format!("{}-{name}", task.id));
                std::fs::write(&location, task.command.join(" ")).unwrap();
                OutputFile {
                    path: output.trim_start_matches('/').to_owned(),
                    location: location.to_string_lossy().into_owned(),
                    ..OutputFile::default()
                }
            })
            .collect();
        Running::ended(Ok(Outcome {
            id: task.id,
            files,
            ..Outcome::default()
        }))
    }
}

#[test]
fn matches_native_tasks() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let values = inputs(root, json!({"name": "bob"}));
    // Both runs share the folder of the simulator, so their task inputs are at equal paths.
    let mut simulator = Simulator::new(root.join("simulator"));
    let (alone, _) = native(root, values.clone(), &mut simulator, true);
    let alone = alone.into_result().unwrap();
    let cat = json!({
        "path": "cat",
        "kind": {"task": {
            "image": "debian:stable-slim",
            "command": ["cat", {"input": "file"}],
            "captures": [{"output": "all", "from": "stdout"}]
        }},
        "inputs": [{"name": "file", "type": {"file": {}}}],
        "outputs": [{"name": "all", "type": {"file": {}}}]
    });
    let edges = vec![
        edge((None, "name"), (Some("block"), "name")),
        edge((Some("block"), "loud"), (Some("cat"), "file")),
        edge((Some("cat"), "all"), (None, "all")),
    ];
    let ops = json!([block("block", shout_ports()), cat]);
    let ports = (name_port(), json!([{"name": "all", "type": {"file": {}}}]));
    let target = document(root, ops, edges, ports);
    let wir = launch(root, target, values, true);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut simulator = Simulator::new(root.join("simulator"));
    let through = execute("shout", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    assert_eq!(alone.nodes.len(), 2, "greet and shout");
    assert_eq!(through.nodes.len(), 3, "and the task op");
    for (one, other) in alone.nodes.iter().zip(&through.nodes) {
        assert_eq!(described(&one.task), described(&other.task));
        assert_eq!(other.task.name, format!("block: {}", one.task.name));
    }
    let cat = &through.nodes[2].task;
    assert_eq!(cat.step, "cat");
    assert_eq!(cat.inputs[0].path, "inputs/file/loud.txt");
}

#[test]
fn exports_block_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let values = inputs(root, json!({"name": "bob"}));
    let mut writer = Writer(root.join("native"));
    let (alone, driver) = native(root, values.clone(), &mut writer, false);
    alone.into_result().unwrap();
    let expected = driver.results().unwrap();
    let mut edges = vec![edge((None, "name"), (Some("block"), "name"))];
    for name in ["line", "loud"] {
        edges.push(edge((Some("block"), name), (None, name)));
    }
    let ops = json!([block("block", shout_ports())]);
    let target = document(root, ops, edges, (name_port(), shout_ports()));
    let wir = launch(root, target, values, false);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut writer = Writer(root.join("native"));
    execute("shout", &mut driver, &mut writer)
        .into_result()
        .unwrap();
    let results = driver.results().unwrap();
    assert_eq!(results["line"], expected["line"], "{results}");
    assert!(
        results["line"].as_str().unwrap().contains("hello bob"),
        "{results}"
    );
    assert_eq!(results["loud"]["basename"], "loud.txt");
    // The graph output reaches the output folder of the run by its basename.
    let delivered = root.join("out/loud.txt");
    assert_eq!(
        results["loud"]["location"],
        delivered.to_string_lossy().as_ref()
    );
}

#[test]
fn runs_two_blocks() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let ops = json!([block("one", shout_ports()), block("two", shout_ports())]);
    let mut edges = Vec::new();
    for (op, input, output) in [("one", "first", "ada"), ("two", "second", "bob")] {
        edges.push(edge((None, input), (Some(op), "name")));
        edges.push(edge((Some(op), "line"), (None, output)));
    }
    let taken = json!([{"name": "first", "type": "string"}, {"name": "second", "type": "string"}]);
    let given = json!([{"name": "ada", "type": "string"}, {"name": "bob", "type": "string"}]);
    let target = document(root, ops, edges, (taken, given));
    let values = inputs(root, json!({"first": "ada", "second": "bob"}));
    let wir = launch(root, target, values, false);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut writer = Writer(root.join("files"));
    let ran = execute("shout", &mut driver, &mut writer)
        .into_result()
        .unwrap();
    assert_eq!(ran.nodes.len(), 4, "each block runs its two steps");
    let results = driver.results().unwrap();
    for name in ["ada", "bob"] {
        let line = results[name].as_str().unwrap();
        assert!(line.contains(&format!("hello {name}")), "{results}");
    }
}

#[test]
fn checks_block_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let profiled = {
        let mut op = block("block", json!([]));
        op["kind"]["native"]["profiles"] = json!(["test"]);
        op
    };
    for (op, wanted) in [
        (
            block("block", json!([{"name": "line", "type": "int"}])),
            "output line does not fit",
        ),
        (
            block("block", json!([{"name": "other", "type": "string"}])),
            "output other does not fit",
        ),
        (profiled, "Argo blocks take no profiles"),
    ] {
        let edges = vec![edge((None, "name"), (Some("block"), "name"))];
        let target = document(root, json!([op]), edges, (name_port(), json!([])));
        let wir = launch(root, target, inputs(root, json!({"name": "ada"})), true);
        let mut driver = WirDriver::new(&wir, settings()).unwrap();
        let mut simulator = Simulator::new(root.join("simulator"));
        let error = execute("shout", &mut driver, &mut simulator)
            .into_result()
            .unwrap_err();
        assert!(error.to_string().contains(wanted), "{error}");
    }
}
