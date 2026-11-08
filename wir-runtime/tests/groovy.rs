//! Runs Groovy and Python expr ops: their inputs and captures, the source of their scope,
//! and scopes that do not fit their source.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Attempt, Driver, Executor, Launch, Running, execute};

/// Runs no task.
struct Idle;

impl Executor for Idle {
    fn run(&mut self, attempt: Attempt) -> Running {
        panic!("no task runs here: {}", attempt.task.name)
    }
}

/// The output `out` of a graph whose Groovy expr `code` reads the input `n`, or why it
/// failed; `main.nf` holds `source`.
fn run(expr: Json, source: &str) -> Result<Json, String> {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("main.nf"), source).unwrap();
    let edge = |from: (Option<&str>, &str), to: (Option<&str>, &str)| json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}});
    let op = json!({"path": "e", "kind": {"expr": expr}, "inputs": [{"name": "n", "type": "int"}],
        "outputs": [{"name": "out", "type": "dynamic"}]});
    let document = json!({"wir_version": "0.1",
        "sources": [{"id": "nf", "language": "nextflow", "location": "main.nf"},
            {"id": "smk", "language": "snakemake", "location": "main.nf"}],
        "graphs": [{"name": "main", "inputs": [{"name": "n", "type": "int"}],
            "outputs": [{"name": "out", "type": "dynamic"}], "ops": [op],
            "edges": [edge((None, "n"), (Some("e"), "n")), edge((Some("e"), "out"), (None, "out"))]}],
        "entry": "main"});
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    std::fs::write(
        folder.path().join("inputs.json"),
        json!({"n": 4}).to_string(),
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
    execute("groovy", &mut driver, &mut Idle)
        .into_result()
        .map_err(|error| error.to_string())?;
    Ok(driver.results().unwrap()["out"].clone())
}

#[test]
fn groovy_expressions() {
    let plain = json!({"language": "groovy", "code": "[inputs.n * k, 'n' + inputs.n]", "captures": {"k": {"value": 10}}});
    assert_eq!(run(plain, "").unwrap(), json!([40, "n4"]));
    let source = "def square(x) { x * x }\nworkflow { }\n";
    let scoped = json!({"language": "groovy", "code": "square(inputs.n)", "scope": {"source": "nf", "start": 0, "end": 24}});
    assert_eq!(
        run(scoped, source).unwrap(),
        json!(16),
        "the code sees the definitions of its source"
    );
    let thrown = json!({"language": "groovy", "code": "inputs.n.nothing()"});
    let error = run(thrown, "").unwrap_err();
    assert!(error.starts_with("op e: "), "{error}");
}

#[test]
fn scope_checked() {
    let source = "def ü() { 1 }\n";
    let inside =
        json!({"language": "groovy", "code": "1", "scope": {"source": "nf", "start": 5, "end": 6}});
    assert_eq!(
        run(inside, source).unwrap_err(),
        "the scope 5..6 is no range of characters of nf"
    );
    let past = json!({"language": "groovy", "code": "1", "scope": {"source": "nf", "start": 0, "end": 99}});
    assert_eq!(
        run(past, source).unwrap_err(),
        "the scope 0..99 is no range of characters of nf"
    );
}

#[test]
fn python_expressions() {
    let plain = json!({"language": "python", "code": "[inputs['n'] * k, f\"n{inputs['n']}\"]",
        "captures": {"k": {"value": 10}}});
    assert_eq!(run(plain, "").unwrap(), json!([40, "n4"]));
    let library = json!({"language": "python", "code": "square(inputs['n'])",
        "library": ["def square(x):\n    return x * x\n"]});
    assert_eq!(run(library, "").unwrap(), json!(16));
    let scoped = json!({"language": "python", "code": "1", "scope": {"source": "smk", "start": 0, "end": 0}});
    let error = run(scoped, "").unwrap_err();
    assert!(
        error.ends_with("uwe does not load Snakefile scopes for Python code yet"),
        "{error}"
    );
}
