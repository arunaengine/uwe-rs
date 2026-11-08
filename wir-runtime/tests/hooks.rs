//! Runs the hooks of a document at the end of a run: `complete` hooks after every run with
//! the success of the run, `error` hooks after a failed one, and hooks that fail the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Attempt, Executor, Launch, Running, execute};

/// Runs no task: the runs here hold one expr.
struct Idle;

impl Executor for Idle {
    fn run(&mut self, attempt: Attempt) -> Running {
        panic!("no task runs here: {}", attempt.task.name)
    }
}

/// Runs the Groovy expr `code` over the input `n` with `hooks`; why the run failed, if it did.
fn run(code: &str, hooks: Json) -> Result<(), String> {
    let folder = tempfile::tempdir().unwrap();
    let int = |name: &str| json!({"name": name, "type": "int"});
    let edge = |from: (Option<&str>, &str), to: (Option<&str>, &str)| json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}});
    let op = json!({"path": "inc", "kind": {"expr": {"language": "groovy", "code": code}},
        "inputs": [int("n")], "outputs": [int("out")]});
    let main = json!({"name": "main", "inputs": [int("n")], "outputs": [int("out")], "ops": [op],
        "edges": [edge((None, "n"), (Some("inc"), "n")), edge((Some("inc"), "out"), (None, "out"))]});
    let document = json!({"wir_version": "0.1", "graphs": [main], "entry": "main", "hooks": hooks});
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    std::fs::write(
        folder.path().join("inputs.json"),
        json!({"n": 1}).to_string(),
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
