//! Runs dynamic regions through the driver: a plan given as a value input, its instances,
//! and the outputs of the final plan.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Attempt, Driver, Executor, Launch, Running, execute};

/// Runs no task: the plans here hold exprs only.
struct Idle;

impl Executor for Idle {
    fn run(&mut self, attempt: Attempt) -> Running {
        panic!("no task runs here: {}", attempt.task.name)
    }
}

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

/// Runs the region `r` with the plan `plan` and the input `n`; its output `out`, or why the
/// run failed.
fn run(plan: Json, n: i64) -> Result<Json, String> {
    let folder = tempfile::tempdir().unwrap();
    let int = |name: &str| json!({"name": name, "type": "int"});
    let op = json!({"path": "r", "kind": {"dynamic": {"plan": "plan"}},
        "inputs": [{"name": "plan", "type": "dynamic"}, int("n")], "outputs": [int("out")]});
    let main = json!({"name": "main", "inputs": [{"name": "plan", "type": "dynamic"}, int("n")],
        "outputs": [int("out")], "ops": [op],
        "edges": [edge((None, "plan"), (Some("r"), "plan")), edge((None, "n"), (Some("r"), "n")),
            edge((Some("r"), "out"), (None, "out"))]});
    let document = json!({"wir_version": "0.1", "graphs": [main], "entry": "main"});
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    let inputs = json!({"plan": plan, "n": n});
    std::fs::write(folder.path().join("inputs.json"), inputs.to_string()).unwrap();
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
    execute("regions", &mut driver, &mut Idle)
        .into_result()
        .map_err(|error| error.to_string())?;
    Ok(driver.results().unwrap())
}

/// A plan of `revision` whose op `inc` gives `n + 1` as `out`.
fn plan(revision: u64, final_plan: bool) -> Json {
    let int = |name: &str| json!({"name": name, "type": "int"});
    let inc = json!({"path": "inc", "kind": {"expr": {"language": "groovy", "code": "inputs.n + 1"}},
        "inputs": [int("n")], "outputs": [int("out")]});
    let graph = json!({"name": "plan", "inputs": [int("n")], "outputs": [int("out")], "ops": [inc],
        "edges": [edge((None, "n"), (Some("inc"), "n")), edge((Some("inc"), "out"), (None, "out"))]});
    json!({"revision": revision, "graph": graph, "final_plan": final_plan})
}

#[test]
fn region_plans() {
    assert_eq!(run(plan(0, true), 4).unwrap(), json!({"out": 5}));
    let open = run(plan(0, false), 4).unwrap_err();
    assert!(open.contains("its plan never settled"), "{open}");
    let late = run(plan(1, true), 4).unwrap_err();
    assert!(late.contains("the first plan has revision 1"), "{late}");
}

/// A plan of `revision` with emitters of `xs` named `ops`, the last of which exports to `ys`.
fn emitters(revision: u64, ops: &[&str], final_plan: bool) -> Json {
    let ints = json!({"array": {"items": "int"}});
    let ops_json: Vec<Json> = (ops.iter())
        .map(|op| {
            json!({"path": op, "kind": {"emit": {"each": true}},
            "inputs": [{"name": "source", "type": ints}],
            "outputs": [{"name": "out", "type": "int", "flow": "stream"}]})
        })
        .collect();
    let mut edges: Vec<Json> = ops
        .iter()
        .map(|op| edge((None, "xs"), (Some(op), "source")))
        .collect();
    edges.push(edge((Some(ops[ops.len() - 1]), "out"), (None, "ys")));
    let graph = json!({"name": "plan", "inputs": [{"name": "xs", "type": ints}],
        "outputs": [{"name": "ys", "type": "int", "flow": "stream"}], "ops": ops_json, "edges": edges});
    json!({"revision": revision, "graph": graph, "final_plan": final_plan})
}

/// Runs the region `r` with the stream of plans `plans` over `xs`; its stream output `ys`.
fn streamed(plans: Json, xs: Json) -> Result<Json, String> {
    let folder = tempfile::tempdir().unwrap();
    let ints = json!({"array": {"items": "int"}});
    let plan = json!({"name": "plans", "type": "dynamic", "flow": "stream"});
    let ys = json!({"name": "ys", "type": "int", "flow": "stream"});
    let op = json!({"path": "r", "kind": {"dynamic": {"plan": "plans", "order": "fair"}},
        "inputs": [plan, {"name": "xs", "type": ints}], "outputs": [ys]});
    let main = json!({"name": "main", "inputs": [plan, {"name": "xs", "type": ints}],
        "outputs": [ys], "ops": [op],
        "edges": [edge((None, "plans"), (Some("r"), "plans")), edge((None, "xs"), (Some("r"), "xs")),
            edge((Some("r"), "ys"), (None, "ys"))]});
    let document = json!({"wir_version": "0.1", "graphs": [main], "entry": "main"});
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    let inputs = json!({"plans": plans, "xs": xs});
    std::fs::write(folder.path().join("inputs.json"), inputs.to_string()).unwrap();
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
    execute("regions", &mut driver, &mut Idle)
        .into_result()
        .map_err(|error| error.to_string())?;
    Ok(driver.results().unwrap()["ys"].clone())
}

#[test]
fn region_streams() {
    let once = streamed(json!([emitters(0, &["a"], true)]), json!([1, 2])).unwrap();
    assert_eq!(once, json!([1, 2]), "the producer's items leave the region");
    let again = json!([emitters(0, &["a"], false), emitters(1, &["a"], true)]);
    let replaced = streamed(again, json!([1, 2])).unwrap();
    assert_eq!(
        replaced,
        json!([1, 2]),
        "an export attached again gives its items once"
    );
    let moved = json!([emitters(0, &["a"], false), emitters(1, &["a", "b"], true)]);
    assert_eq!(streamed(moved, json!([3])).unwrap(), json!([3]));
}

#[test]
fn region_inner_streams() {
    // The plan's instances emit a stream and gather it, and the region exports only a value.
    let folder = tempfile::tempdir().unwrap();
    let ints = json!({"array": {"items": "int"}});
    let emit = json!({"path": "e", "kind": {"emit": {"each": true}}, "inputs": [{"name": "source", "type": ints}],
        "outputs": [{"name": "out", "type": "int", "flow": "stream"}]});
    let gather = json!({"path": "l", "kind": {"to_list": {}},
        "inputs": [{"name": "source", "type": "int", "flow": "stream"}], "outputs": [{"name": "out", "type": ints}]});
    let graph = json!({"name": "plan", "inputs": [{"name": "xs", "type": ints}], "outputs": [{"name": "ys", "type": ints}],
        "ops": [emit, gather], "edges": [edge((None, "xs"), (Some("e"), "source")),
            edge((Some("e"), "out"), (Some("l"), "source")), edge((Some("l"), "out"), (None, "ys"))]});
    let plan = json!({"revision": 0, "graph": graph, "final_plan": true});
    let op = json!({"path": "r", "kind": {"dynamic": {"plan": "plan"}},
        "inputs": [{"name": "plan", "type": "dynamic"}, {"name": "xs", "type": ints}],
        "outputs": [{"name": "ys", "type": ints}]});
    let main = json!({"name": "main", "inputs": [{"name": "plan", "type": "dynamic"}, {"name": "xs", "type": ints}],
        "outputs": [{"name": "ys", "type": ints}], "ops": [op],
        "edges": [edge((None, "plan"), (Some("r"), "plan")), edge((None, "xs"), (Some("r"), "xs")),
            edge((Some("r"), "ys"), (None, "ys"))]});
    let document = json!({"wir_version": "0.1", "graphs": [main], "entry": "main"});
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    let inputs = json!({"plan": plan, "xs": [4, 5]});
    std::fs::write(folder.path().join("inputs.json"), inputs.to_string()).unwrap();
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
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    execute("regions", &mut driver, &mut Idle)
        .into_result()
        .unwrap();
    assert_eq!(driver.results().unwrap()["ys"], json!([4, 5]));
}
