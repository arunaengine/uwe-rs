//! Runs process ops: base rounds of stream items with reused values, `each` choices, the one
//! round of value inputs, and rounds that run tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Attempt, Driver, Executor, Launch, Outcome, OutputFile, Running, execute};

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

/// Runs each task by writing `<last word>.out`, and records the task names.
struct Echoing {
    root: std::path::PathBuf,
    names: Vec<String>,
}

impl Executor for Echoing {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        self.names.push(task.name.clone());
        let work = self.root.join(format!("work-{}", task.id));
        std::fs::create_dir_all(&work).unwrap();
        let path = format!("{}.out", task.command.last().unwrap());
        std::fs::write(work.join(&path), "").unwrap();
        let file = OutputFile {
            location: work.join(&path).to_string_lossy().into_owned(),
            path,
            ..OutputFile::default()
        };
        Running::ended(Ok(Outcome {
            id: task.id,
            files: vec![file],
            ..Outcome::default()
        }))
    }
}

/// `main` runs the process `proc` with `spec` over its inputs, with the body `body`; the
/// output `ys` and the names of the tasks, or why the run failed.
fn run(
    spec: Json,
    inputs: Json,
    outputs: Json,
    body: Json,
    values: Json,
) -> Result<(Json, Vec<String>), String> {
    let folder = tempfile::tempdir().unwrap();
    let names: Vec<String> = inputs
        .as_array()
        .unwrap()
        .iter()
        .map(|port| port["name"].as_str().unwrap().to_owned())
        .collect();
    let mut edges: Vec<Json> = names
        .iter()
        .map(|name| edge((None, name), (Some("proc"), name)))
        .collect();
    edges.push(edge((Some("proc"), "out"), (None, "ys")));
    let mut ys = outputs.clone();
    ys["name"] = json!("ys");
    let op =
        json!({"path": "proc", "kind": {"process": spec}, "inputs": inputs, "outputs": [outputs]});
    let main =
        json!({"name": "main", "inputs": inputs, "outputs": [ys], "ops": [op], "edges": edges});
    let document = json!({"wir_version": "0.1", "graphs": [main, body], "entry": "main"});
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    std::fs::write(folder.path().join("inputs.json"), values.to_string()).unwrap();
    let launch = Launch {
        target: folder.path().join("run.wir.json"),
        inputs: Some(folder.path().join("inputs.json")),
        scratch: folder.path().join("scratch"),
        output_dir: Some(folder.path().join("out")),
        ..Launch::default()
    };
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).map_err(|error| error.message)?;
    let mut executor = Echoing {
        root: folder.path().to_owned(),
        names: Vec::new(),
    };
    execute("processes", &mut driver, &mut executor)
        .into_result()
        .map_err(|error| error.to_string())?;
    let ys = driver.results().unwrap()["ys"].clone();
    Ok((ys, executor.names))
}

fn port(name: &str, r#type: Json, flow: &str) -> Json {
    json!({"name": name, "type": r#type, "flow": flow})
}

/// The body `pair`, whose Groovy expr gives `[n, k]`.
fn pair() -> Json {
    let expr = json!({"path": "join", "kind": {"expr": {"language": "groovy", "code": "[inputs.n, inputs.k]"}},
        "inputs": [port("n", json!("dynamic"), "value"), port("k", json!("dynamic"), "value")],
        "outputs": [port("out", json!("dynamic"), "value")]});
    json!({"name": "pair", "inputs": [port("n", json!("dynamic"), "value"), port("k", json!("dynamic"), "value")],
        "outputs": [port("out", json!("dynamic"), "value")], "ops": [expr],
        "edges": [edge((None, "n"), (Some("join"), "n")), edge((None, "k"), (Some("join"), "k")), edge((Some("join"), "out"), (None, "out"))]})
}

#[test]
fn process_rounds() {
    let inputs = json!([
        port("n", json!("dynamic"), "stream"),
        port("k", json!("dynamic"), "value")
    ]);
    let out = port("out", json!("dynamic"), "stream");
    let (ys, _) = run(
        json!({"body": "pair"}),
        inputs,
        out,
        pair(),
        json!({"n": [1, 2], "k": "x"}),
    )
    .unwrap();
    assert_eq!(
        ys,
        json!([[1, "x"], [2, "x"]]),
        "an item of the stream and the value in every round"
    );
}

#[test]
fn process_each() {
    let inputs = json!([
        port("n", json!("dynamic"), "value"),
        port("k", json!("dynamic"), "value")
    ]);
    let out = port("out", json!("dynamic"), "stream");
    let spec = json!({"body": "pair", "each": ["n", "k"]});
    let (ys, _) = run(
        spec.clone(),
        inputs.clone(),
        out.clone(),
        pair(),
        json!({"n": [1, 2], "k": ["a", "b"]}),
    )
    .unwrap();
    assert_eq!(
        ys,
        json!([[1, "a"], [2, "a"], [1, "b"], [2, "b"]]),
        "the first each input varies fastest"
    );
    let (ys, _) = run(spec, inputs, out, pair(), json!({"n": [], "k": ["a"]})).unwrap();
    assert_eq!(ys, json!([]), "an empty choice starts no round");
}

#[test]
fn process_singleton() {
    let inputs = json!([
        port("n", json!("dynamic"), "value"),
        port("k", json!("dynamic"), "value")
    ]);
    let out = port("out", json!("dynamic"), "value");
    let (ys, _) = run(
        json!({"body": "pair"}),
        inputs,
        out,
        pair(),
        json!({"n": 1, "k": 2}),
    )
    .unwrap();
    assert_eq!(
        ys,
        json!([1, 2]),
        "value inputs run one round and give values"
    );
}

#[test]
fn process_tasks() {
    let file = json!({"file": {}});
    let task = json!({"path": "t", "kind": {"task": {"command": ["echo", {"input": "n"}],
        "captures": [{"output": "out", "from": {"glob": "*.out"}}]}},
        "inputs": [port("n", json!("string"), "value")], "outputs": [port("out", file.clone(), "value")]});
    let body = json!({"name": "body", "inputs": [port("n", json!("string"), "value")], "outputs": [port("out", file.clone(), "value")],
        "ops": [task], "edges": [edge((None, "n"), (Some("t"), "n")), edge((Some("t"), "out"), (None, "out"))]});
    let inputs = json!([port("n", json!("string"), "stream")]);
    let spec = json!({"body": "body", "parallelism": 1});
    let (ys, mut names) = run(
        spec,
        inputs,
        port("out", file, "stream"),
        body,
        json!({"n": ["a", "b"]}),
    )
    .unwrap();
    names.sort();
    assert_eq!(
        names,
        ["proc/0/0/t", "proc/1/0/t"],
        "each round runs its task at its path"
    );
    let basenames: Vec<&str> = ys
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["basename"].as_str().unwrap())
        .collect();
    assert_eq!(basenames.len(), 2);
}
