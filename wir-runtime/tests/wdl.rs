//! Runs a WDL workflow natively and as the native op of a wir document, and compares the
//! tasks and the outputs; feeds its outputs to a task op and runs two blocks at once.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use wdl_runtime::WdlDriver;
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Execution, Launch, Simulator, Task, TaskId, execute};

/// The scatter demo of WDL, which greets each name and collects the greetings.
fn workflow() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../demo/wdl/scatter.wdl")
}

fn names_port() -> Json {
    json!([{"name": "names", "type": {"array": {"items": "string"}}}])
}

fn collected_ports() -> Json {
    json!([{"name": "all", "type": {"file": {}}}, {"name": "count", "type": "int"}])
}

/// A block `path` that runs the workflow, with the outputs `outputs`.
fn block(path: &str, outputs: Json) -> Json {
    let native = json!({"source": "scatter", "language": "wdl"});
    json!({"path": path, "kind": {"native": native}, "inputs": names_port(), "outputs": outputs})
}

/// A wir document of `ops` and `edges` with the graph ports `ports`.
fn document(root: &Path, ops: Json, edges: Vec<Json>, ports: (Json, Json)) -> PathBuf {
    let document = json!({
        "wir_version": "0.1",
        "sources": [
            {"id": "scatter", "language": "wdl", "location": workflow()},
            {"id": "other", "language": "wdl", "location": workflow()}
        ],
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

/// The inputs file of a wir run and of a native run, in `root`.
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

/// The workflow run alone with the inputs `values`, delivering to the folder that the block
/// delivers to: the run and its driver.
fn native(root: &Path, values: PathBuf, simulated: bool) -> (Execution, WdlDriver) {
    let launch = Launch {
        target: workflow(),
        inputs: Some(values),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out/block")),
        simulated,
        ..Launch::default()
    };
    let options = wdl_runtime::Options {
        print: false,
        ..wdl_runtime::Options::default()
    };
    let mut driver = WdlDriver::new(&launch, options).unwrap();
    // Both runs share the folder of the simulator, so their task inputs are at equal paths.
    let mut simulator = Simulator::new(root.join("simulator"));
    let execution = execute("native", &mut driver, &mut simulator);
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

/// The results without where their files are.
fn placeless(value: &Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.iter().map(placeless).collect()),
        Json::Object(object) => (object.iter())
            .filter(|(key, _)| *key != "location")
            .map(|(key, item)| (key.clone(), placeless(item)))
            .collect(),
        other => other.clone(),
    }
}

#[test]
fn matches_native_tasks() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let values = inputs(root, json!({"names": ["Ada", "Bob"]}));
    let (alone, _) = native(root, values.clone(), true);
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
        edge((None, "names"), (Some("block"), "names")),
        edge((Some("block"), "all"), (Some("cat"), "file")),
        edge((Some("cat"), "all"), (None, "all")),
    ];
    let ops = json!([block("block", collected_ports()), cat]);
    let ports = (names_port(), json!([{"name": "all", "type": {"file": {}}}]));
    let target = document(root, ops, edges, ports);
    let wir = launch(root, target, values, true);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut simulator = Simulator::new(root.join("simulator"));
    let through = execute("scatter", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    assert_eq!(alone.nodes.len(), 3, "two greetings and the collection");
    assert_eq!(through.nodes.len(), 4, "and the task op");
    for (one, other) in alone.nodes.iter().zip(&through.nodes) {
        assert_eq!(described(&one.task), described(&other.task));
        assert_eq!(other.task.name, format!("block: {}", one.task.name));
    }
    let cat = &through.nodes[3].task;
    assert_eq!(cat.step, "cat");
    assert_eq!(cat.inputs[0].path, "inputs/file/all.txt");
}

#[test]
fn exports_block_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let values = inputs(root, json!({"names": ["Ada", "Bob"]}));
    let (alone, driver) = native(root, values.clone(), false);
    alone.into_result().unwrap();
    let expected = driver.exported().unwrap();
    let mut edges = vec![edge((None, "names"), (Some("block"), "names"))];
    for name in ["all", "count"] {
        edges.push(edge((Some("block"), name), (None, name)));
    }
    let ops = json!([block("block", collected_ports())]);
    let target = document(root, ops, edges, (names_port(), collected_ports()));
    let wir = launch(root, target, values, false);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut simulator = Simulator::new(root.join("simulator"));
    execute("scatter", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    let results = driver.results().unwrap();
    assert_eq!(placeless(&results), placeless(&expected), "{results}");
    let delivered = root.join("out/block/all/all.txt");
    assert_eq!(
        results["all"]["location"],
        delivered.to_string_lossy().as_ref()
    );
    assert_eq!(results["all"]["location"], expected["all"]["location"]);
}

#[test]
fn runs_two_blocks() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let ops = json!([
        block("one", collected_ports()),
        block("two", collected_ports())
    ]);
    let mut edges = Vec::new();
    for (op, port) in [("one", "first"), ("two", "second")] {
        edges.push(edge((None, "names"), (Some(op), "names")));
        edges.push(edge((Some(op), "all"), (None, port)));
    }
    let file = json!({"file": {}});
    let given = json!([{"name": "first", "type": file}, {"name": "second", "type": file}]);
    let target = document(root, ops, edges, (names_port(), given));
    let values = inputs(root, json!({"names": ["Ada"]}));
    let wir = launch(root, target, values, false);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut simulator = Simulator::new(root.join("simulator"));
    let ran = execute("scatter", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    assert_eq!(ran.nodes.len(), 4, "each block greets once and collects");
    let results = driver.results().unwrap();
    for (port, op) in [("first", "one"), ("second", "two")] {
        let delivered = root.join("out").join(op).join("all/all.txt");
        assert_eq!(
            results[port]["location"],
            delivered.to_string_lossy().as_ref()
        );
    }
}

#[test]
fn checks_block_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let configured = {
        let mut op = block("block", json!([]));
        op["kind"]["native"]["configs"] = json!(["other"]);
        op
    };
    for (op, wanted) in [
        (
            block("block", json!([{"name": "all", "type": "int"}])),
            "output all does not fit",
        ),
        (
            block("block", json!([{"name": "other", "type": "int"}])),
            "output other does not fit",
        ),
        (configured, "WDL blocks take no profiles or configs"),
    ] {
        let edges = vec![edge((None, "names"), (Some("block"), "names"))];
        let target = document(root, json!([op]), edges, (names_port(), json!([])));
        let wir = launch(root, target, inputs(root, json!({"names": ["Ada"]})), true);
        let mut driver = WirDriver::new(&wir, settings()).unwrap();
        let mut simulator = Simulator::new(root.join("simulator"));
        let error = execute("scatter", &mut driver, &mut simulator)
            .into_result()
            .unwrap_err();
        assert!(error.to_string().contains(wanted), "{error}");
    }
}
