//! Runs stream ports: entry stream inputs, calls that forward streams, and stream results.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Attempt, Driver, Executor, Launch, Running, execute};

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

fn stream(name: &str) -> Json {
    json!({"name": name, "type": "int", "flow": "stream"})
}

/// Writes `graphs` with `main` as the entry and the inputs, and the launch of them.
fn launch(root: &Path, graphs: Json, inputs: Json) -> Launch {
    let document = json!({"wir_version": "0.1", "graphs": graphs, "entry": "main"});
    std::fs::write(root.join("run.wir.json"), document.to_string()).unwrap();
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    Launch {
        target: root.join("run.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    }
}

/// Runs no task: these graphs run in the engine.
struct Idle;

impl Executor for Idle {
    fn run(&mut self, attempt: Attempt) -> Running {
        panic!("no task runs here: {}", attempt.task.name)
    }
}

/// The results of a run of the graphs, or why it failed.
fn run(graphs: Json, inputs: Json) -> Result<Json, String> {
    let folder = tempfile::tempdir().unwrap();
    let launch = launch(folder.path(), graphs, inputs);
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).map_err(|error| error.message)?;
    execute("streams", &mut driver, &mut Idle)
        .into_result()
        .map_err(|error| error.to_string())?;
    Ok(driver.results().unwrap_or_default())
}

/// `main` passes its stream `xs` through two nested calls of graphs that pass it on.
fn passing() -> Json {
    let call = |path: &str, graph: &str| {
        json!({"path": path, "kind": {"call": {"graph": graph}},
        "inputs": [stream("x")], "outputs": [stream("y")]})
    };
    let main = json!({"name": "main", "inputs": [stream("xs")], "outputs": [stream("ys")], "ops": [call("outer", "wrap")],
        "edges": [edge((None, "xs"), (Some("outer"), "x")), edge((Some("outer"), "y"), (None, "ys"))]});
    let wrap = json!({"name": "wrap", "inputs": [stream("x")], "outputs": [stream("y")], "ops": [call("inner", "pass")],
        "edges": [edge((None, "x"), (Some("inner"), "x")), edge((Some("inner"), "y"), (None, "y"))]});
    let pass = json!({"name": "pass", "inputs": [stream("x")], "outputs": [stream("y")],
        "edges": [edge((None, "x"), (None, "y"))]});
    json!([main, wrap, pass])
}

#[test]
fn stream_boundaries() {
    let results = run(passing(), json!({"xs": [1, 2, 3]})).unwrap();
    assert_eq!(
        results["ys"],
        json!([1, 2, 3]),
        "calls forward every item, in order"
    );
    let empty = run(passing(), json!({"xs": []})).unwrap();
    assert_eq!(empty["ys"], json!([]), "an empty stream stays empty");
    let error = run(passing(), json!({"xs": [1, "two"]})).unwrap_err();
    assert_eq!(
        error, "the value of the input xs does not fit its type",
        "each item is checked"
    );
    let missing = run(passing(), json!({})).unwrap_err();
    assert_eq!(missing, "the input xs has no value");
}

/// `main` emits its value `xs` into the stream `ys`, as one item or each item.
fn emitting(each: bool, r#type: Json) -> Json {
    let item = if each { json!("int") } else { r#type.clone() };
    let emit = json!({"path": "emit", "kind": {"emit": {"each": each}},
        "inputs": [{"name": "source", "type": r#type.clone()}],
        "outputs": [{"name": "out", "type": item, "flow": "stream"}]});
    let output = json!({"name": "ys", "type": item, "flow": "stream"});
    json!([{"name": "main", "inputs": [{"name": "xs", "type": r#type}], "outputs": [output], "ops": [emit],
        "edges": [edge((None, "xs"), (Some("emit"), "source")), edge((Some("emit"), "out"), (None, "ys"))]}])
}

#[test]
fn emit_items() {
    let ints = json!({"array": {"items": "int"}});
    assert_eq!(
        run(emitting(true, ints.clone()), json!({"xs": [1, 2, 3]})).unwrap()["ys"],
        json!([1, 2, 3])
    );
    assert_eq!(
        run(emitting(true, ints.clone()), json!({"xs": []})).unwrap()["ys"],
        json!([])
    );
    assert_eq!(
        run(emitting(false, ints), json!({"xs": [1, 2]})).unwrap()["ys"],
        json!([[1, 2]]),
        "one item"
    );
    let maybe = json!({"optional": "int"});
    assert_eq!(
        run(emitting(false, maybe), json!({"xs": null})).unwrap()["ys"],
        json!([null]),
        "null is an item"
    );
}

#[test]
fn scheduler_yields() {
    let folder = tempfile::tempdir().unwrap();
    let items: Vec<u32> = (0..20_000).collect();
    let launch = launch(
        folder.path(),
        emitting(true, json!({"array": {"items": "int"}})),
        json!({"xs": items}),
    );
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    assert!(driver.start().unwrap().is_empty());
    let mut wakes = 0;
    while driver.wake_at().is_some() {
        assert!(driver.wake().unwrap().is_empty());
        wakes += 1;
    }
    assert!(
        wakes > 0,
        "a long stream yields to the engine and wakes again"
    );
    driver.finish().unwrap();
    assert_eq!(
        driver.results().unwrap()["ys"].as_array().unwrap().len(),
        20_000
    );
}

#[test]
fn capability_fence() {
    let mut graphs = emitting(true, json!({"array": {"items": "int"}}));
    let call = json!({"path": "later", "kind": {"call": {"graph": "inner"}}});
    graphs[0]["ops"].as_array_mut().unwrap().push(call);
    let region = json!({"path": "region", "kind": {"dynamic": {"plan": "plans"}},
        "inputs": [{"name": "plans", "type": "dynamic", "flow": "stream"}]});
    let inner = json!({"name": "inner", "inputs": [], "ops": [emitting(true, json!({"array": {"items": "int"}}))[0]["ops"][0], region],
        "edges": [edge((Some("emit"), "out"), (Some("region"), "plans"))]});
    graphs.as_array_mut().unwrap().push(inner);
    let mut graphs = graphs;
    graphs[1]["ops"][0]["inputs"][0]["default"] = json!([1]);
    let error = run(graphs, json!({"xs": [1]})).unwrap_err();
    assert_eq!(
        error, "op inner/region: uwe does not run stream ports yet",
        "nothing of the run starts"
    );
}
