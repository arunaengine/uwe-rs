//! Runs a Nextflow pipeline natively and as the native op of a wir document, and compares
//! the tasks and the outputs; feeds its outputs to a task op and runs two blocks at once.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_runtime::driver::{Nextflow, Settings as Host};
use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Execution, Launch, Simulator, Task, TaskId, execute};

/// The pipeline of these tests.
fn pipeline() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/greetings.nf")
}

/// The ports of a block that runs the workflow `greetings`, with the outputs `outputs`.
fn block(path: &str, outputs: Json) -> Json {
    let inputs = json!([
        {"name": "names", "type": {"array": {"items": "string"}}},
        {"name": "outdir", "type": "string"}
    ]);
    let native = json!({"source": "greetings", "language": "nextflow", "entry": "greetings"});
    json!({"path": path, "kind": {"native": native}, "inputs": inputs, "outputs": outputs})
}

/// A wir document of `ops` and `edges`, with the graph inputs `inputs` and outputs `outputs`.
fn document(root: &Path, ops: Json, edges: Json, ports: (Json, Json)) -> PathBuf {
    let document = json!({
        "wir_version": "0.2",
        "sources": [{"id": "greetings", "language": "nextflow", "location": pipeline()}],
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

/// A graph with the inputs `names` and `outdir`, which feed the block `op`.
fn feeding(op: &str) -> Vec<Json> {
    vec![
        edge((None, "names"), (Some(op), "names")),
        edge((None, "outdir"), (Some(op), "outdir")),
    ]
}

fn graph_inputs() -> Json {
    json!([
        {"name": "names", "type": {"array": {"items": "string"}}},
        {"name": "outdir", "type": "string"}
    ])
}

fn inputs(root: &Path, names: &[&str]) -> PathBuf {
    let path = root.join("inputs.json");
    let values = json!({"names": names, "outdir": "results"});
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

/// A host in `folder` that prints nothing.
struct Quiet(String);

impl nextflow_evaluator::interpreter::Host for Quiet {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        self.0.clone()
    }
}

/// The pipeline run alone with `names`, launched in `folder`: the run and its driver.
fn native(root: &Path, folder: &Path, names: &[&str], simulated: bool) -> (Execution, Nextflow) {
    let params = root.join("params.json");
    let values = json!({"names": names, "outdir": "results"});
    std::fs::write(&params, values.to_string()).unwrap();
    let launch = Launch {
        target: pipeline(),
        inputs: Some(params),
        targets: vec!["greetings".into()],
        scratch: root.join("native-scratch"),
        simulated,
        ..Launch::default()
    };
    let host = Host {
        host: Box::new(Quiet(folder.to_string_lossy().into_owned())),
        ..Host::default()
    };
    let mut driver = Nextflow::new(&launch, host).unwrap();
    let mut simulator = Simulator::new(root.join("native"));
    let execution = execute("greetings", &mut driver, &mut simulator);
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

/// Paths of the File values in an array, by basename.
fn basenames(value: &Json) -> Vec<String> {
    let mut names: Vec<String> = (value.as_array().unwrap().iter())
        .map(|file| file["basename"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn matches_native_tasks() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let names = ["ada", "bob"];
    let (alone, _) = native(root, &root.join("out/block"), &names, true);
    let alone = alone.into_result().unwrap();
    let outputs = json!([{"name": "files", "type": {"array": {"items": {"file": {}}}}}]);
    let cat = json!({
        "path": "cat",
        "kind": {"task": {
            "image": "debian:stable-slim",
            "command": ["cat", {"input": "files"}],
            "captures": [{"output": "all", "from": "stdout"}]
        }},
        "inputs": [{"name": "files", "type": {"array": {"items": {"file": {}}}}}],
        "outputs": [{"name": "all", "type": {"file": {}}}]
    });
    let mut edges = feeding("block");
    edges.push(edge((Some("block"), "files"), (Some("cat"), "files")));
    edges.push(edge((Some("cat"), "all"), (None, "all")));
    let ops = json!([block("block", outputs), cat]);
    let ports = (
        graph_inputs(),
        json!([{"name": "all", "type": {"file": {}}}]),
    );
    let target = document(root, ops, Json::from(edges), ports);
    let wir = launch(root, target, inputs(root, &names), true);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut simulator = Simulator::new(root.join("wir"));
    let through = execute("greetings", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    assert_eq!(alone.nodes.len(), 2);
    assert_eq!(through.nodes.len(), 3, "two greetings and the task op");
    for (one, other) in alone.nodes.iter().zip(&through.nodes) {
        assert_eq!(described(&one.task), described(&other.task));
        assert_eq!(other.task.name, format!("block: {}", one.task.name));
    }
    let cat = &through.nodes[2].task;
    assert_eq!(cat.step, "cat");
    let staged: Vec<&str> = cat.inputs.iter().map(|input| input.path.as_str()).collect();
    assert_eq!(
        staged.len(),
        2,
        "both greetings reach the task op: {staged:?}"
    );
}

#[test]
fn exports_block_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let names = ["ada", "bob"];
    let (alone, driver) = native(root, &root.join("native-launch"), &names, false);
    alone.into_result().unwrap();
    let expected = driver.results().unwrap();
    let outputs = json!([
        {"name": "files", "type": {"array": {"items": {"file": {}}}}},
        {"name": "count", "type": "int"},
        {"name": "outdir", "type": "directory"}
    ]);
    let mut edges = feeding("block");
    for name in ["files", "count", "outdir"] {
        edges.push(edge((Some("block"), name), (None, name)));
    }
    let ops = json!([block("block", outputs.clone())]);
    let target = document(root, ops, Json::from(edges), (graph_inputs(), outputs));
    let wir = launch(root, target, inputs(root, &names), false);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut simulator = Simulator::new(root.join("wir"));
    execute("greetings", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    let results = driver.results().unwrap();
    assert_eq!(
        basenames(&results["files"]),
        ["ada.txt", "bob.txt"],
        "{results}"
    );
    assert_eq!(placeless(&results["files"]), placeless(&expected["files"]));
    assert_eq!(results["count"], expected["count"]);
    assert_eq!(results["count"], 2);
    let published = root.join("out/block/results");
    assert_eq!(results["outdir"]["class"], "Directory");
    assert_eq!(
        results["outdir"]["location"],
        published.to_string_lossy().as_ref()
    );
}

#[test]
fn runs_two_blocks() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let outputs = json!([{"name": "outdir", "type": {"array": {"items": {"file": {}}}}}]);
    let ops = json!([block("one", outputs.clone()), block("two", outputs)]);
    let mut edges = feeding("one");
    edges.extend(feeding("two"));
    edges.push(edge((Some("one"), "outdir"), (None, "first")));
    edges.push(edge((Some("two"), "outdir"), (None, "second")));
    let files = json!({"array": {"items": {"file": {}}}});
    let given = json!([{"name": "first", "type": files}, {"name": "second", "type": files}]);
    let target = document(root, ops, Json::from(edges), (graph_inputs(), given));
    let wir = launch(root, target, inputs(root, &["ada", "bob"]), false);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut simulator = Simulator::new(root.join("wir"));
    let ran = execute("greetings", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    assert_eq!(ran.nodes.len(), 4);
    let results = driver.results().unwrap();
    for (port, op) in [("first", "one"), ("second", "two")] {
        let folder = root.join("out").join(op).join("results/greetings");
        let locations: Vec<&str> = (results[port].as_array().unwrap().iter())
            .map(|file| file["location"].as_str().unwrap())
            .collect();
        assert_eq!(locations.len(), 2, "{results}");
        for location in locations {
            assert!(Path::new(location).starts_with(&folder), "{location}");
        }
    }
}

#[test]
fn checks_block_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    for (outputs, wanted) in [
        (
            json!([{"name": "files", "type": "int"}]),
            "output files does not fit",
        ),
        (
            json!([{"name": "other", "type": "int"}]),
            "no emit of the entry workflow",
        ),
    ] {
        let ops = json!([block("block", outputs)]);
        let target = document(
            root,
            ops,
            Json::from(feeding("block")),
            (graph_inputs(), json!([])),
        );
        let wir = launch(root, target, inputs(root, &["ada"]), true);
        let mut driver = WirDriver::new(&wir, settings()).unwrap();
        let mut simulator = Simulator::new(root.join("simulator"));
        let error = execute("greetings", &mut driver, &mut simulator)
            .into_result()
            .unwrap_err();
        assert!(error.to_string().contains(wanted), "{error}");
    }
}
