//! Runs the hooks of a document at the end of a run: `complete` hooks after every run with
//! the success of the run, `error` hooks after a failed one, and hooks that fail the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Attempt, Driver, ExecuteError, Executor, Launch, Running, execute};

/// Runs no task: the runs here hold one expr.
struct Idle;

impl Executor for Idle {
    fn run(&mut self, attempt: Attempt) -> Running {
        panic!("no task runs here: {}", attempt.task.name)
    }
}

/// The launch of a run of the Groovy expr `code` over the input `n` with `hooks`, in `root`.
fn launch(root: &Path, code: &str, hooks: Json) -> Launch {
    let int = |name: &str| json!({"name": name, "type": "int"});
    let edge = |from: (Option<&str>, &str), to: (Option<&str>, &str)| json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}});
    let op = json!({"path": "inc", "kind": {"expr": {"language": "groovy", "code": code}},
        "inputs": [int("n")], "outputs": [int("out")]});
    let main = json!({"name": "main", "inputs": [int("n")], "outputs": [int("out")], "ops": [op],
        "edges": [edge((None, "n"), (Some("inc"), "n")), edge((Some("inc"), "out"), (None, "out"))]});
    let document = json!({"wir_version": "0.1", "graphs": [main], "entry": "main", "hooks": hooks});
    std::fs::write(root.join("run.wir.json"), document.to_string()).unwrap();
    std::fs::write(root.join("inputs.json"), json!({"n": 1}).to_string()).unwrap();
    Launch {
        target: root.join("run.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    }
}

/// Runs the Groovy expr `code` over the input `n` with `hooks`; why the run failed, if it did.
fn run(code: &str, hooks: Json) -> Result<(), String> {
    let folder = tempfile::tempdir().unwrap();
    let launch = launch(folder.path(), code, hooks);
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).map_err(|error| error.message)?;
    let execution = execute("hooks", &mut driver, &mut Idle);
    execution
        .into_result()
        .map(drop)
        .map_err(|error| error.to_string())
}

/// A Groovy hook closure with `code`, which sees the input `n` as `n`.
fn hook(code: &str) -> Json {
    json!({"language": "groovy", "code": code, "captures": {"n": {"input": "n"}}})
}

#[test]
fn run_hooks() {
    let good = "inputs.n + 1";
    // A hook that calls a missing method fails, which shows that it ran.
    let checked = hook("{ it.success && it.error == null && n == 1 ? 1 : it.nothing() }");
    assert_eq!(
        run(good, json!([{"complete": checked}])),
        Ok(()),
        "a complete hook sees success"
    );
    let failing = run(good, json!([{"complete": hook("{ it.nothing() }")}])).unwrap_err();
    assert!(
        failing.contains("hook 0:"),
        "a failing hook fails the run: {failing}"
    );
    let skipped = run(good, json!([{"error": hook("{ it.nothing() }")}]));
    assert_eq!(
        skipped,
        Ok(()),
        "an error hook runs only after a failed run"
    );
    let broken = "inputs.n.nothing()";
    let alone = run(broken, json!([])).unwrap_err();
    let seen = hook("{ !it.success && it.error.contains('inc') ? it.nothing() : 1 }");
    let heard = run(broken, json!([{"error": seen}])).unwrap_err();
    assert!(!alone.contains("more"), "{alone}");
    assert!(
        heard.contains("and 1 more"),
        "the error hook hears why the run failed: {heard}"
    );
}

/// A driver of the run of [`launch`] that started, which runs no task.
fn started(root: &Path, hooks: Json) -> WirDriver {
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch(root, "inputs.n + 1", hooks), settings).unwrap();
    assert!(driver.start().unwrap().is_empty());
    driver
}

#[test]
fn hook_results() {
    let folder = tempfile::tempdir().unwrap();
    let mut driver = started(
        folder.path(),
        json!([{"complete": hook("{ it.nothing() }")}]),
    );
    let error = driver.finish().unwrap_err().message;
    assert!(error.contains("hook 0:"), "{error}");
    assert_eq!(
        driver.results().unwrap()["out"],
        json!(2),
        "a failing hook keeps the results"
    );
}

#[test]
fn delivery_failures() {
    let folder = tempfile::tempdir().unwrap();
    let hooks = json!([{"complete": hook("{ 1 }")}, {"error": hook("{ it.nothing() }")}]);
    let mut driver = started(folder.path(), hooks);
    driver.finish().unwrap();
    let lost = ExecuteError::Delivery {
        location: "a".into(),
        target: "b".into(),
        message: "the disk is full".into(),
    };
    let error = driver.failed(&lost).unwrap_err().message;
    assert!(
        error.contains("hook 1:"),
        "the error hooks run after finish: {error}"
    );
    assert!(driver.failed(&lost).is_ok(), "they run once");
}

#[test]
fn hook_scopes() {
    // The helper comes from a Nextflow source that only the hook names.
    let folder = tempfile::tempdir().unwrap();
    let text = "def helper() { 1 }\n";
    std::fs::write(folder.path().join("lib.nf"), text).unwrap();
    let scope = json!({"source": "nf", "start": 0, "end": text.len()});
    let hook = json!({"language": "groovy", "code": "{ helper() == 1 ? 1 : it.nothing() }", "scope": scope});
    let launch = launch(folder.path(), "inputs.n + 1", json!([{"complete": hook}]));
    let mut document: Json =
        serde_json::from_str(&std::fs::read_to_string(&launch.target).unwrap()).unwrap();
    document["sources"] = json!([{"id": "nf", "language": "nextflow", "location": "lib.nf"}]);
    std::fs::write(&launch.target, document.to_string()).unwrap();
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let execution = execute("hooks", &mut driver, &mut Idle);
    assert!(
        execution.into_result().is_ok(),
        "the hook sees the helper of its scope"
    );
}

#[test]
fn hook_prints() {
    let folder = tempfile::tempdir().unwrap();
    let hook = json!({"language": "groovy", "code": "{ println 'done ' + it.success }"});
    let printed = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let sink = std::sync::Arc::clone(&printed);
    let settings = Settings {
        print: false,
        print_sink: wir_runtime::Printer::new(move |text| sink.lock().unwrap().push_str(text)),
        ..Settings::default()
    };
    let launch = launch(folder.path(), "inputs.n + 1", json!([{"complete": hook}]));
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    execute("hooks", &mut driver, &mut Idle)
        .into_result()
        .unwrap();
    assert_eq!(
        *printed.lock().unwrap(),
        "done true\n",
        "a hook prints to the print sink"
    );
}
