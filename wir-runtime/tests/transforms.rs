//! Runs the stream ops that map items: transform, filter, flat_map and flatten, on streams
//! and on values, with closure and graph bodies.
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

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

/// The output `ys` of a run of one op of `kind` from the input `xs` to `ys`, both of `flow`
/// with the types `from` and `to`, with the extra graphs; `None` when it closed empty.
fn run(
    kind: Json,
    flow: &str,
    (from, to): (Json, Json),
    graphs: Json,
    xs: Json,
) -> Result<Option<Json>, String> {
    let folder = tempfile::tempdir().unwrap();
    let port = |name: &str, r#type: &Json| json!({"name": name, "type": r#type, "flow": flow});
    let op = json!({"path": "op", "kind": kind, "inputs": [port("source", &from)], "outputs": [port("out", &to)]});
    let main = json!({"name": "main", "inputs": [port("xs", &from)], "outputs": [port("ys", &to)], "ops": [op],
        "edges": [edge((None, "xs"), (Some("op"), "source")), edge((Some("op"), "out"), (None, "ys"))]});
    let mut all = vec![main];
    all.extend(graphs.as_array().cloned().unwrap_or_default());
    let document = json!({"wir_version": "0.1", "graphs": all, "entry": "main"});
    std::fs::write(folder.path().join("run.wir.json"), document.to_string()).unwrap();
    std::fs::write(
        folder.path().join("inputs.json"),
        json!({"xs": xs}).to_string(),
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
    execute("transforms", &mut driver, &mut Idle)
        .into_result()
        .map_err(|error| error.to_string())?;
    Ok(driver.results().unwrap().get("ys").cloned())
}

fn groovy(code: &str) -> Json {
    json!({"language": "groovy", "code": code})
}

fn ints() -> (Json, Json) {
    (json!("int"), json!("int"))
}

#[test]
fn transform_nulls() {
    let tenfold = groovy("{ it % 2 == 0 ? null : it * 10 }");
    let dropped = run(
        json!({"transform": {"body": tenfold}}),
        "stream",
        ints(),
        json!([]),
        json!([1, 2, 3]),
    );
    assert_eq!(
        dropped.unwrap(),
        Some(json!([10, 30])),
        "nulls drop by default, in source order"
    );
    let optional = (json!("int"), json!({"optional": "int"}));
    let kept = run(
        json!({"transform": {"body": tenfold, "nulls": "keep"}}),
        "stream",
        optional,
        json!([]),
        json!([1, 2, 3]),
    );
    assert_eq!(kept.unwrap(), Some(json!([10, null, 30])));
    let failing = run(
        json!({"transform": {"body": groovy("{ it.nothing() }")}}),
        "stream",
        ints(),
        json!([]),
        json!([1]),
    );
    assert!(
        failing.unwrap_err().starts_with("op op: "),
        "a failing body fails the op"
    );
    let value = run(
        json!({"transform": {"body": groovy("{ it * 2 }")}}),
        "value",
        ints(),
        json!([]),
        json!(3),
    );
    assert_eq!(value.unwrap(), Some(json!(6)));
    let suppressed = run(
        json!({"transform": {"body": groovy("{ null }")}}),
        "value",
        ints(),
        json!([]),
        json!(3),
    );
    assert_eq!(
        suppressed.unwrap(),
        None,
        "a dropped null value closes empty"
    );
}

#[test]
fn filter_items() {
    let port = |name: &str, r#type: &str| json!({"name": name, "type": r#type});
    let test = json!({"path": "test", "kind": {"expr": {"language": "js", "code": "$(inputs.item > 1)"}},
        "inputs": [port("item", "int")], "outputs": [port("out", "bool")]});
    let big = json!({"name": "big", "inputs": [port("item", "int")], "outputs": [port("out", "bool")], "ops": [test],
        "edges": [edge((None, "item"), (Some("test"), "item")), edge((Some("test"), "out"), (None, "out"))]});
    let graph = run(
        json!({"filter": {"body": "big"}}),
        "stream",
        ints(),
        json!([big]),
        json!([1, 2, 3, 0]),
    );
    assert_eq!(graph.unwrap(), Some(json!([2, 3])));
    let truthy = run(
        json!({"filter": {"body": groovy("{ it % 2 }")}}),
        "stream",
        ints(),
        json!([]),
        json!([1, 2, 3]),
    );
    assert_eq!(truthy.unwrap(), Some(json!([1, 3])), "Groovy truth");
    let rejected = run(
        json!({"filter": {"body": groovy("{ false }")}}),
        "value",
        ints(),
        json!([]),
        json!(3),
    );
    assert_eq!(rejected.unwrap(), None, "a rejected value closes empty");
}

#[test]
fn flat_map_shapes() {
    let open = (json!("dynamic"), json!("dynamic"));
    let items = json!([[1, 2], {"a": 1}, null, 5, {"class": "Set", "items": [7]}, {"class": "MapEntry", "key": "k", "value": "v"}]);
    let flat = run(
        json!({"flat_map": {}}),
        "stream",
        open.clone(),
        json!([]),
        items,
    )
    .unwrap();
    let entry = json!({"class": "MapEntry", "key": "a", "value": 1});
    assert_eq!(flat, Some(json!([1, 2, entry, 5, 7, "k", "v"])));
    let mapped = run(
        json!({"flat_map": {"body": groovy("{ [it, it] }")}}),
        "stream",
        open,
        json!([]),
        json!([1, 2]),
    );
    assert_eq!(
        mapped.unwrap(),
        Some(json!([1, 1, 2, 2])),
        "all items of one item come first"
    );
}

#[test]
fn flatten_depth() {
    let open = (json!("dynamic"), json!("dynamic"));
    let items = json!([[1, [2]], null, 3, {"m": [4]}]);
    let one = run(
        json!({"flatten": {}}),
        "stream",
        open.clone(),
        json!([]),
        items.clone(),
    );
    assert_eq!(one.unwrap(), Some(json!([1, [2], null, 3, {"m": [4]}])));
    let all = run(
        json!({"flatten": {"depth": "all"}}),
        "stream",
        open.clone(),
        json!([]),
        items.clone(),
    );
    assert_eq!(
        all.unwrap(),
        Some(json!([1, 2, null, 3, {"m": [4]}])),
        "maps stay whole"
    );
    let nested = json!([[null], null]);
    let dropped = run(
        json!({"flatten": {"drop_null": true}}),
        "stream",
        open,
        json!([]),
        nested,
    );
    assert_eq!(
        dropped.unwrap(),
        Some(json!([null])),
        "only a null source item drops"
    );
}
