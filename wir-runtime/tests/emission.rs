//! Orders the outputs of process rounds: fair and completion order, rounds that settle
//! without outputs, and a failed round that cancels the rounds that still run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value as Json, json};
use wir_runtime::adapter::{CallId, Changes, Ending, TaskAdapter, TaskCall};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, DriverError, Launch, Outcome, OutputFile, Task, TaskId};

/// Runs each op as one task, and ends it by the exit code of its attempt: 0 succeeds, 1 is
/// ignored, 2 is skipped and 3 fails.
#[derive(Default)]
struct ExitCodes {
    calls: HashMap<TaskId, CallId>,
}

impl TaskAdapter for ExitCodes {
    fn start(&mut self, call: &TaskCall) -> Result<Changes, DriverError> {
        let task = Task {
            id: TaskId(self.calls.len() as u64 + 1),
            name: call.inputs["n"].as_str().unwrap().to_owned(),
            ..Task::default()
        };
        self.calls.insert(task.id, call.call);
        Ok(Changes {
            tasks: vec![(call.call, task)],
            ended: Vec::new(),
            ready: Vec::new(),
        })
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Changes, DriverError> {
        let ending = match outcome.exit_code {
            0 => {
                let file = json!({"class": "File", "location": outcome.files[0].location});
                Ending::Succeeded(Map::from_iter([("out".to_owned(), file)]))
            }
            1 => Ending::Ignored("ignored".into()),
            2 => Ending::Skipped,
            _ => Ending::Failed("the task failed".into()),
        };
        Ok(Changes {
            tasks: Vec::new(),
            ended: vec![(self.calls[&outcome.id], ending)],
            ready: Vec::new(),
        })
    }
}

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

fn port(name: &str, r#type: Json, flow: &str) -> Json {
    json!({"name": name, "type": r#type, "flow": flow})
}

/// A driver of the process `proc` in `order` over the stream `ns`, whose rounds run one
/// dialect task each.
fn driver(root: &Path, order: &str, ns: &[&str]) -> WirDriver {
    let file = json!({"file": {}});
    let builder = json!({"nextflow_process": {"source": "nf", "entry": "main", "call": "ECHO",
        "inputs": [{"port": "n", "native": "input/0"}], "outputs": [{"port": "out", "native": "output/0"}]}});
    let task = json!({"path": "t", "kind": {"task": {"builder": builder}},
        "inputs": [port("n", json!("string"), "value")], "outputs": [port("out", file.clone(), "value")]});
    let body = json!({"name": "body", "inputs": [port("n", json!("string"), "value")],
        "outputs": [port("out", file.clone(), "value")], "ops": [task],
        "edges": [edge((None, "n"), (Some("t"), "n")), edge((Some("t"), "out"), (None, "out"))]});
    let op = json!({"path": "proc", "kind": {"process": {"body": "body", "order": order}},
        "inputs": [port("n", json!("string"), "stream")], "outputs": [port("out", file.clone(), "stream")]});
    let main = json!({"name": "main", "inputs": [port("ns", json!("string"), "stream")],
        "outputs": [port("ys", file, "stream")], "ops": [op],
        "edges": [edge((None, "ns"), (Some("proc"), "n")), edge((Some("proc"), "out"), (None, "ys"))]});
    let source =
        json!({"id": "nf", "language": "nextflow", "location": "main.nf", "digest": "sha256:ab"});
    let document =
        json!({"wir_version": "0.1", "sources": [source], "graphs": [main, body], "entry": "main"});
    std::fs::write(root.join("run.wir.json"), document.to_string()).unwrap();
    std::fs::write(root.join("inputs.json"), json!({"ns": ns}).to_string()).unwrap();
    let launch = Launch {
        target: root.join("run.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    };
    let mut settings = Settings {
        print: false,
        ..Settings::default()
    };
    (settings.adapters).register("nextflow_process", |_| Ok(Box::new(ExitCodes::default())));
    WirDriver::new(&launch, settings).unwrap()
}

/// The outcome of a task that wrote `<name>.out`, with `exit_code`.
fn outcome(root: &Path, task: &Task, exit_code: i32) -> Outcome {
    // The run puts the op path before the name of the task.
    let path = format!("{}.out", task.name.rsplit(": ").next().unwrap());
    std::fs::write(root.join(&path), "").unwrap();
    Outcome {
        id: task.id,
        attempt: 1,
        exit_code,
        files: vec![OutputFile {
            location: root.join(&path).to_string_lossy().into_owned(),
            path,
            ..OutputFile::default()
        }],
        ..Outcome::default()
    }
}

/// Runs the rounds of `ns`, ending the task of each named round in the order and with the
/// exit code of `ends`; the names of the output files, or why the run failed.
fn run(order: &str, ns: &[&str], ends: &[(&str, i32)]) -> Result<Vec<String>, String> {
    let folder = tempfile::tempdir().unwrap();
    let mut driver = driver(folder.path(), order, ns);
    let tasks = driver.start().unwrap();
    assert_eq!(tasks.len(), ns.len(), "every round starts at once");
    for (name, exit_code) in ends {
        let task = tasks.iter().find(|task| task.name.ends_with(name)).unwrap();
        let more = driver.complete(outcome(folder.path(), task, *exit_code));
        assert!(more.unwrap().is_empty());
    }
    driver.finish().map_err(|error| error.message)?;
    let ys = driver.results().unwrap()["ys"].clone();
    let names = ys.as_array().unwrap().iter();
    Ok(names
        .map(|file| {
            file["location"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect())
}

#[test]
fn emission_order() {
    let ends = [("c", 0), ("a", 0), ("b", 0)];
    let fair = run("fair", &["a", "b", "c"], &ends).unwrap();
    assert_eq!(
        fair,
        ["a.out", "b.out", "c.out"],
        "fair order is round order"
    );
    let completion = run("completion", &["a", "b", "c"], &ends).unwrap();
    let expected = ["c.out", "a.out", "b.out"];
    assert_eq!(
        completion, expected,
        "completion order is the order rounds end"
    );
}

#[test]
fn settled_holes() {
    // The ignored round b and the skipped round c give no item, and fair order passes them.
    let ends = [("d", 0), ("b", 1), ("c", 2), ("a", 0)];
    let fair = run("fair", &["a", "b", "c", "d"], &ends).unwrap();
    assert_eq!(fair, ["a.out", "d.out"]);
}

#[test]
fn round_cancellation() {
    let folder = tempfile::tempdir().unwrap();
    let mut driver = driver(folder.path(), "fair", &["a", "b", "c"]);
    let tasks = driver.start().unwrap();
    let named = |name: &str| tasks.iter().find(|task| task.name.ends_with(name)).unwrap();
    driver
        .complete(outcome(folder.path(), named("b"), 3))
        .unwrap();
    let mut stopped = driver.cancellations();
    stopped.sort();
    assert_eq!(
        stopped,
        [named("a").id, named("c").id],
        "the other rounds stop"
    );
    assert!(
        driver.cancellations().is_empty(),
        "each task is stopped once"
    );
    // A late outcome of a cancelled round changes nothing.
    let late = driver.complete(outcome(folder.path(), named("a"), 0));
    assert!(late.unwrap().is_empty());
    let error = driver.finish().unwrap_err().message;
    assert!(error.contains("op proc/1/0/t: the task failed"), "{error}");
}
