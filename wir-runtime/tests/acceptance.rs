//! Checks the attempts of task ops: optional captures that stay empty, capture arity, the
//! acceptance callback, cached outcomes that go through the same checks, and retry rules.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Outcome, OutputFile, Task};

/// A driver of the one task op `make` with `capture` for its output `out`, the task fields
/// `rules`, like `accept`, and the `retry` attribute when it is not 0.
fn task_driver(root: &Path, capture: Json, rules: Option<Json>, retry: u32) -> WirDriver {
    let out = json!({"name": "out", "type": capture["type"].clone()});
    let mut task = json!({"command": ["make"], "captures": [capture["capture"].clone()]});
    for (name, value) in rules.iter().flat_map(|rules| rules.as_object().unwrap()) {
        task[name] = value.clone();
    }
    let attributes = match retry {
        0 => json!({}),
        retry => json!({"retry": retry}),
    };
    let op = json!({"path": "make", "kind": {"task": task}, "outputs": [out],
        "attributes": attributes});
    let edge = json!({"from": {"op": "make", "port": "out"}, "to": {"port": "ys"}});
    let main = json!({"name": "main", "outputs": [{"name": "ys", "type": capture["type"].clone()}],
        "ops": [op], "edges": [edge]});
    let document = json!({"wir_version": "0.1", "graphs": [main], "entry": "main"});
    std::fs::write(root.join("run.wir.json"), document.to_string()).unwrap();
    let launch = Launch {
        target: root.join("run.wir.json"),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    };
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    WirDriver::new(&launch, settings).unwrap()
}

/// The outcome of an attempt of `task` that wrote the files `names`.
fn outcome(root: &Path, task: &Task, attempt: u32, names: &[&str]) -> Outcome {
    let files = (names.iter())
        .map(|name| {
            std::fs::write(root.join(name), "").unwrap();
            OutputFile {
                path: (*name).to_owned(),
                location: root.join(name).to_string_lossy().into_owned(),
                ..OutputFile::default()
            }
        })
        .collect();
    Outcome {
        id: task.id,
        attempt,
        files,
        ..Outcome::default()
    }
}

/// A capture of `*.out` with `fields`, for an output of `type`.
fn glob(fields: Json, r#type: Json) -> Json {
    let mut capture = json!({"output": "out", "from": {"glob": "*.out"}});
    for (name, value) in fields.as_object().unwrap() {
        capture[name] = value.clone();
    }
    json!({"capture": capture, "type": r#type})
}

#[test]
fn optional_captures() {
    let folder = tempfile::tempdir().unwrap();
    let capture = glob(json!({"optional": true}), json!({"file": {}}));
    let mut driver = task_driver(folder.path(), capture, None, 0);
    let tasks = driver.start().unwrap();
    driver
        .complete(outcome(folder.path(), &tasks[0], 1, &[]))
        .unwrap();
    assert!(driver.take_acceptance(tasks[0].id, 1));
    driver.finish().unwrap();
    let results = driver.results().unwrap();
    assert_eq!(
        results,
        json!({}),
        "an optional output that matched nothing is empty"
    );
}

#[test]
fn capture_arity() {
    let folder = tempfile::tempdir().unwrap();
    let files = json!({"array": {"items": {"file": {}}}});
    let capture = glob(json!({"arity": {"min": 2, "max": 3}}), files);
    let mut driver = task_driver(folder.path(), capture.clone(), None, 0);
    let tasks = driver.start().unwrap();
    driver
        .complete(outcome(folder.path(), &tasks[0], 1, &["a.out"]))
        .unwrap();
    assert!(!driver.take_acceptance(tasks[0].id, 1));
    let error = driver.finish().unwrap_err().message;
    assert_eq!(error, "op make: output out: 1 paths matched, not 2 to 3");
    let mut driver = task_driver(folder.path(), capture, None, 0);
    let tasks = driver.start().unwrap();
    let made = outcome(folder.path(), &tasks[0], 1, &["a.out", "b.out"]);
    driver.complete(made).unwrap();
    driver.finish().unwrap();
    assert_eq!(
        driver.results().unwrap()["ys"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn acceptance_callback() {
    let folder = tempfile::tempdir().unwrap();
    let capture = glob(json!({}), json!({"file": {}}));
    let accept = groovy("{ it.attempt > 1 && it.outputs.out != null }");
    let mut driver = task_driver(
        folder.path(),
        capture.clone(),
        Some(json!({"accept": accept})),
        1,
    );
    let tasks = driver.start().unwrap();
    let again = driver.complete(outcome(folder.path(), &tasks[0], 1, &["a.out"]));
    assert_eq!(again.unwrap(), tasks, "a rejected attempt runs again");
    assert!(!driver.take_acceptance(tasks[0].id, 1));
    driver
        .complete(outcome(folder.path(), &tasks[0], 2, &["a.out"]))
        .unwrap();
    assert!(driver.take_acceptance(tasks[0].id, 2));
    driver.finish().unwrap();
    let mut driver = task_driver(folder.path(), capture, Some(json!({"accept": accept})), 0);
    let tasks = driver.start().unwrap();
    driver
        .complete(outcome(folder.path(), &tasks[0], 1, &["a.out"]))
        .unwrap();
    let error = driver.finish().unwrap_err().message;
    assert_eq!(error, "op make: the acceptance check does not hold");
}

#[test]
fn cached_rejection() {
    let folder = tempfile::tempdir().unwrap();
    let capture = glob(json!({}), json!({"file": {}}));
    let mut driver = task_driver(folder.path(), capture, None, 1);
    let tasks = driver.start().unwrap();
    let cached = Outcome {
        cached: true,
        exit_code: 1,
        ..outcome(folder.path(), &tasks[0], 1, &["a.out"])
    };
    let again = driver.complete(cached).unwrap();
    assert_eq!(again, tasks, "a rejected cached outcome runs again");
    assert!(!driver.take_acceptance(tasks[0].id, 1));
}

fn groovy(code: &str) -> Json {
    json!({"language": "groovy", "code": code})
}

/// The outcome of a rejected attempt of `task` with `exit_code`.
fn rejected(root: &Path, task: &Task, attempt: u32, exit_code: i32) -> Outcome {
    Outcome {
        exit_code,
        ..outcome(root, task, attempt, &["a.out"])
    }
}

#[test]
fn retry_directives() {
    let folder = tempfile::tempdir().unwrap();
    let capture = glob(json!({}), json!({"file": {}}));
    let code = "{ [image: 'img:' + it.attempt, resources: [memory: 1024 * it.attempt]] + \
        (it.attempt == 2 ? [command: ['again', it.previous.exit_code as String]] : [:]) }";
    let rules = json!({"retry": {"limit": 2, "directives": groovy(code)}});
    let mut driver = task_driver(folder.path(), capture, Some(rules), 0);
    let first = driver.start().unwrap().remove(0);
    assert_eq!(
        first.image.as_deref(),
        Some("img:1"),
        "the first attempt is patched too"
    );
    let second = driver
        .complete(rejected(folder.path(), &first, 1, 3))
        .unwrap()
        .remove(0);
    assert_eq!(
        (second.id, second.image.as_deref()),
        (first.id, Some("img:2"))
    );
    assert_eq!(second.resources.memory_bytes, Some(2048));
    assert_eq!(second.command, ["again", "3"], "the patch of attempt two");
    let third = driver
        .complete(rejected(folder.path(), &second, 2, 1))
        .unwrap()
        .remove(0);
    assert_eq!(
        third.command, first.command,
        "each attempt starts from the base task"
    );
    let none = driver
        .complete(rejected(folder.path(), &third, 3, 1))
        .unwrap();
    assert!(none.is_empty(), "the limit allows two more attempts");
    let error = driver.finish().unwrap_err().message;
    assert_eq!(error, "op make: the command ended with exit code 1");
}

#[test]
fn retry_decisions() {
    let folder = tempfile::tempdir().unwrap();
    let capture = glob(json!({}), json!({"file": {}}));
    let test = groovy("{ it.previous.exit_code == 3 }");
    let rules = json!({"retry": {"limit": 5, "retry_if": test}});
    let mut driver = task_driver(folder.path(), capture.clone(), Some(rules), 0);
    let task = driver.start().unwrap().remove(0);
    let again = driver
        .complete(rejected(folder.path(), &task, 1, 3))
        .unwrap();
    assert_eq!(again.len(), 1, "retry_if holds for exit code 3");
    let none = driver
        .complete(rejected(folder.path(), &task, 2, 4))
        .unwrap();
    assert!(none.is_empty(), "retry_if does not hold for exit code 4");
    assert!(driver.finish().is_err());
    // A cancelled attempt never runs again.
    let rules = json!({"retry": {"limit": 5}});
    let mut driver = task_driver(folder.path(), capture.clone(), Some(rules), 0);
    let task = driver.start().unwrap().remove(0);
    let cancelled = Outcome {
        error: Some(workflow_graph::TaskError::Cancelled),
        ..rejected(folder.path(), &task, 1, 1)
    };
    assert!(driver.complete(cancelled).unwrap().is_empty());
    // An ignored failure ends the op with its outputs empty.
    let rules = json!({"retry": {"failure": "ignore"}});
    let mut driver = task_driver(folder.path(), capture, Some(rules), 0);
    let task = driver.start().unwrap().remove(0);
    driver
        .complete(rejected(folder.path(), &task, 1, 1))
        .unwrap();
    driver.finish().unwrap();
    assert_eq!(driver.results().unwrap(), json!({}));
}
