//! Runs loop ops: serial runs with carried values, the condition after each run, the stream
//! of every run's value, and the limit of runs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Attempt, Driver, Executor, Launch, Running, execute};

/// Runs no task: loops of exprs make none.
struct Idle;

impl Executor for Idle {
    fn run(&mut self, attempt: Attempt) -> Running {
        panic!("no task runs here: {}", attempt.task.name)
    }
}

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

fn port(name: &str, r#type: &str, flow: &str) -> Json {
    json!({"name": name, "type": r#type, "flow": flow})
}

/// A Groovy expr op from the int input `n` to the output `out` of `type`.
fn expr(path: &str, code: &str, r#type: &str) -> Json {
    json!({"path": path, "kind": {"expr": {"language": "groovy", "code": code}},
        "inputs": [port("n", "int", "value")], "outputs": [port("out", r#type, "value")]})
}

/// Loops the body `count`, which gives `n + 1` as `next` and `seen`, while `n + 1 < 3`,
/// from `n`, with `limit`; the outputs `next` and `seen`, or why the run failed.
fn run(n: i64, limit: Option<u64>) -> Result<Json, String> {
    let folder = tempfile::tempdir().unwrap();
    let count = json!({"name": "count", "inputs": [port("n", "int", "value")],
        "outputs": [port("next", "int", "value"), port("seen", "int", "value"), port("more", "bool", "value")],
        "ops": [expr("inc", "inputs.n + 1", "int"), expr("check", "inputs.n + 1 < 3", "bool")],
        "edges": [edge((None, "n"), (Some("inc"), "n")), edge((None, "n"), (Some("check"), "n")),
            edge((Some("inc"), "out"), (None, "next")), edge((Some("inc"), "out"), (None, "seen")),
            edge((Some("check"), "out"), (None, "more"))]});
    let mut spec = json!({"body": "count", "condition": "more",
        "carry": [{"output": "next", "input": "n"}]});
    if let Some(limit) = limit {
        spec["limit"] = json!(limit);
    }
    let op = json!({"path": "again", "kind": {"loop": spec}, "inputs": [port("n", "int", "value")],
        "outputs": [port("next", "int", "value"), port("seen", "int", "stream")]});
    let main = json!({"name": "main", "inputs": [port("n", "int", "value")],
        "outputs": [port("next", "int", "value"), port("seen", "int", "stream")], "ops": [op],
        "edges": [edge((None, "n"), (Some("again"), "n")), edge((Some("again"), "next"), (None, "next")),
            edge((Some("again"), "seen"), (None, "seen"))]});
    let document = json!({"wir_version": "0.1", "graphs": [main, count], "entry": "main"});
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    std::fs::write(
        folder.path().join("inputs.json"),
        json!({"n": n}).to_string(),
    )
    .unwrap();
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
    execute("loops", &mut driver, &mut Idle)
        .into_result()
        .map_err(|error| error.to_string())?;
    Ok(driver.results().unwrap())
}

#[test]
fn loop_runs() {
    let results = run(0, None).unwrap();
    assert_eq!(
        results,
        json!({"next": 3, "seen": [1, 2, 3]}),
        "three runs carry n"
    );
    let once = run(5, None).unwrap();
    assert_eq!(
        once,
        json!({"next": 6, "seen": [6]}),
        "the body runs before the first check"
    );
}

#[test]
fn loop_limit() {
    let error = run(0, Some(2)).unwrap_err();
    assert!(
        error.contains("the condition still holds after 2 runs"),
        "{error}"
    );
    let results = run(0, Some(3)).unwrap();
    assert_eq!(
        results["next"],
        json!(3),
        "the last run may end at the limit"
    );
}
