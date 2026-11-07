//! Runs Snakefiles natively and as the native op of a wir document, and compares the tasks
//! and the outputs; feeds its outputs to a task op, runs two blocks and replans checkpoints.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use snakemake_runtime::{RunSettings, SnakemakeDriver};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{
    Attempt, Execution, Executor, Launch, Outcome, OutputFile, Running, Simulator, Task, TaskId,
    execute,
};

/// A fixture Snakefile of these tests.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The ports of a block that runs the source `source` with `inputs` and `outputs`.
fn block(path: &str, source: &str, inputs: Json, outputs: Json) -> Json {
    let native = json!({"source": source, "language": "snakemake"});
    json!({"path": path, "kind": {"native": native}, "inputs": inputs, "outputs": outputs})
}

fn names_port() -> Json {
    json!([{"name": "names", "type": {"array": {"items": "string"}}}])
}

fn greetings_ports() -> Json {
    json!([
        {"name": "greetings", "type": {"array": {"items": {"file": {}}}}},
        {"name": "total", "type": {"file": {}}}
    ])
}

/// A wir document of `ops` and `edges` with the graph ports `ports`, and the sources of both
/// fixtures and of `configs`.
fn document(root: &Path, ops: Json, edges: Vec<Json>, ports: (Json, Json)) -> PathBuf {
    let document = json!({
        "wir_version": "0.2",
        "sources": [
            {"id": "greetings", "language": "snakemake", "location": fixture("greetings.smk")},
            {"id": "checkpoint", "language": "snakemake", "location": fixture("checkpoint.smk")},
            {"id": "words", "language": "snakemake", "location": root.join("words.yaml")}
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

/// The inputs file of a wir run and the config file of a native run, with `names`.
fn inputs(root: &Path, names: &[&str]) -> PathBuf {
    let path = root.join("inputs.json");
    std::fs::write(&path, json!({"names": names}).to_string()).unwrap();
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

/// `snakefile` run alone with the config file `config`, started in `folder`, with the
/// scratch folder of the wir runs: the run and its driver.
fn native(
    root: &Path,
    snakefile: PathBuf,
    folder: &Path,
    config: PathBuf,
    executor: &mut dyn Executor,
    simulated: bool,
) -> (Execution, SnakemakeDriver) {
    std::fs::create_dir_all(folder).unwrap();
    let launch = Launch {
        target: snakefile,
        inputs: Some(config),
        scratch: root.join("scratch"),
        simulated,
        ..Launch::default()
    };
    let options = RunSettings {
        workdir: Some(folder.to_owned()),
        ..RunSettings::default()
    };
    let settings = snakemake_runtime::Settings::default();
    let mut driver = snakemake_runtime::spawn_driver(&launch, settings, options).unwrap();
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

/// The task op `cat`, which writes its files one after the other.
fn cat() -> Json {
    json!({
        "path": "cat",
        "kind": {"task": {
            "image": "debian:stable-slim",
            "command": ["cat", {"input": "files"}],
            "captures": [{"output": "all", "from": "stdout"}]
        }},
        "inputs": [{"name": "files", "type": {"array": {"items": {"file": {}}}}}],
        "outputs": [{"name": "all", "type": {"file": {}}}]
    })
}

#[test]
fn matches_native_tasks() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let names = ["ada", "bob"];
    let config = inputs(root, &names);
    // Both runs share the folder of the simulator, so their task inputs are at equal paths.
    let mut simulator = Simulator::new(root.join("simulator"));
    let snakefile = fixture("greetings.smk");
    let started = root.join("out/block");
    let (alone, _) = native(root, snakefile, &started, config, &mut simulator, true);
    let alone = alone.into_result().unwrap();
    let edges = vec![
        edge((None, "names"), (Some("block"), "names")),
        edge((Some("block"), "greetings"), (Some("cat"), "files")),
        edge((Some("cat"), "all"), (None, "all")),
    ];
    let ops = json!([
        block("block", "greetings", names_port(), greetings_ports()),
        cat()
    ]);
    let ports = (names_port(), json!([{"name": "all", "type": {"file": {}}}]));
    let target = document(root, ops, edges, ports);
    let wir = launch(root, target, inputs(root, &names), true);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut simulator = Simulator::new(root.join("simulator"));
    let through = execute("greetings", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    assert_eq!(alone.nodes.len(), 4, "two greetings, the total and all");
    assert_eq!(through.nodes.len(), 5, "and the task op");
    for (one, other) in alone.nodes.iter().zip(&through.nodes) {
        assert_eq!(described(&one.task), described(&other.task));
        assert_eq!(other.task.name, format!("block: {}", one.task.name));
    }
    let cat = &through.nodes[4].task;
    assert_eq!(cat.step, "cat");
    assert_eq!(cat.inputs.len(), 2, "both greetings reach the task op");
}

#[test]
fn exports_block_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let names = ["ada", "bob"];
    let config = inputs(root, &names);
    let mut simulator = Simulator::new(root.join("native"));
    let snakefile = fixture("greetings.smk");
    let started = root.join("alone");
    let (alone, driver) = native(root, snakefile, &started, config, &mut simulator, false);
    alone.into_result().unwrap();
    let expected = workflow_graph::Driver::results(&driver).unwrap();
    let mut edges = vec![edge((None, "names"), (Some("block"), "names"))];
    for name in ["greetings", "total"] {
        edges.push(edge((Some("block"), name), (None, name)));
    }
    let ops = json!([block("block", "greetings", names_port(), greetings_ports())]);
    let target = document(root, ops, edges, (names_port(), greetings_ports()));
    let wir = launch(root, target, inputs(root, &names), false);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut simulator = Simulator::new(root.join("wir"));
    execute("greetings", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    let results = workflow_graph::Driver::results(&driver).unwrap();
    assert_eq!(placeless(&results), placeless(&expected), "{results}");
    let started = root.join("out/block");
    let at = |path: &str| started.join(path).to_string_lossy().into_owned();
    assert_eq!(results["greetings"][1]["location"], at("greetings/bob.txt"));
    assert_eq!(results["total"]["location"], at("total.txt"));
}

#[test]
fn runs_two_blocks() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    std::fs::write(root.join("words.yaml"), "greeting: hi\n").unwrap();
    let mut configured = block("two", "greetings", names_port(), greetings_ports());
    configured["kind"]["native"]["configs"] = json!(["words"]);
    let ops = json!([
        block("one", "greetings", names_port(), greetings_ports()),
        configured
    ]);
    let mut edges = Vec::new();
    for (op, port) in [("one", "first"), ("two", "second")] {
        edges.push(edge((None, "names"), (Some(op), "names")));
        edges.push(edge((Some(op), "total"), (None, port)));
    }
    let file = json!({"file": {}});
    let given = json!([{"name": "first", "type": file}, {"name": "second", "type": file}]);
    let target = document(root, ops, edges, (names_port(), given));
    let wir = launch(root, target, inputs(root, &["ada", "bob"]), true);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut simulator = Simulator::new(root.join("wir"));
    let ran = execute("greetings", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    assert_eq!(ran.nodes.len(), 8, "each block runs its four jobs");
    for node in &ran.nodes {
        let task = &node.task;
        let started = task.name.split(':').next().unwrap();
        let folder = root.join("out").join(started);
        assert!(
            (task.inputs.iter()).all(|input| !input.source.contains("/out/")
                || Path::new(&input.source).starts_with(&folder)),
            "{} reads only from its own folder",
            task.name
        );
        if task.step == "greet" {
            let script = task.script.clone().unwrap_or_default();
            let word = if started == "two" { "'hi " } else { "'hello " };
            assert!(
                script.contains(word),
                "a config source sets the word: {script}"
            );
        }
    }
}

/// Runs each task by writing the name of the file that the checkpoint picks into each of
/// its outputs, as `checkpoint.smk` expects; it records the steps it ran.
struct Picker {
    root: PathBuf,
    steps: Vec<String>,
}

impl Executor for Picker {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        self.steps.push(task.step.clone());
        std::fs::create_dir_all(&self.root).unwrap();
        let files = (task.outputs.iter())
            .map(|output| {
                let location = self
                    .root
                    .join(format!("{}-{}", task.id, output.replace('/', "-")));
                std::fs::write(&location, "made/chosen.txt\n").unwrap();
                OutputFile {
                    path: output.clone(),
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
fn replans_checkpoints() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let config = inputs(root, &[]);
    let mut picker = Picker {
        root: root.join("native"),
        steps: Vec::new(),
    };
    let snakefile = fixture("checkpoint.smk");
    let started = root.join("alone");
    let (alone, _) = native(root, snakefile, &started, config, &mut picker, false);
    alone.into_result().unwrap();
    assert_eq!(picker.steps, ["pick", "generate", "finish", "all"]);
    let outputs = json!([{"name": "final", "type": {"file": {}}}]);
    let ops = json!([block("block", "checkpoint", json!([]), outputs.clone())]);
    let edges = vec![edge((Some("block"), "final"), (None, "final"))];
    let target = document(root, ops, edges, (json!([]), outputs));
    let empty = root.join("empty.json");
    std::fs::write(&empty, "{}").unwrap();
    let wir = launch(root, target, empty, false);
    let mut driver = WirDriver::new(&wir, settings()).unwrap();
    let mut through = Picker {
        root: root.join("wir"),
        steps: Vec::new(),
    };
    execute("checkpoint", &mut driver, &mut through)
        .into_result()
        .unwrap();
    assert_eq!(
        through.steps, picker.steps,
        "the block replans after the checkpoint"
    );
    let results = workflow_graph::Driver::results(&driver).unwrap();
    let made = root.join("out/block/final.txt");
    assert_eq!(
        results["final"]["location"],
        made.to_string_lossy().as_ref()
    );
}

#[test]
fn checks_block_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    for (outputs, wanted) in [
        (
            json!([{"name": "total", "type": "int"}]),
            "output total does not fit",
        ),
        (
            json!([{"name": "other", "type": {"file": {}}}]),
            "output other does not fit",
        ),
    ] {
        let ops = json!([block("block", "greetings", names_port(), outputs)]);
        let edges = vec![edge((None, "names"), (Some("block"), "names"))];
        let target = document(root, ops, edges, (names_port(), json!([])));
        let wir = launch(root, target, inputs(root, &["ada"]), true);
        let mut driver = WirDriver::new(&wir, settings()).unwrap();
        let mut simulator = Simulator::new(root.join("simulator"));
        let error = execute("greetings", &mut driver, &mut simulator)
            .into_result()
            .unwrap_err();
        assert!(error.to_string().contains(wanted), "{error}");
    }
}
