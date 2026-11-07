//! Runs merge, flatten and select ops: the items in port order, one level of flattening, and
//! the errors of `first` and `only` when not one item is set.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

/// The output of a merge of the inputs `a` and `b`, then `last`, an op over the merged array.
fn run(root: &Path, inputs: Json, types: (Json, Json), last: Json) -> Result<Json, String> {
    let (a, b) = types;
    let ports = json!([{"name": "a", "type": a}, {"name": "b", "type": b}]);
    let output = last["outputs"][0]["type"].clone();
    let document = json!({
        "wir_version": wir::VERSION,
        "graphs": [{
            "name": "main",
            "inputs": ports,
            "outputs": [{"name": "out", "type": output}],
            "ops": [{
                "path": "both",
                "kind": {"merge": {}},
                "inputs": ports,
                "outputs": [{"name": "out", "type": {"array": {"items": "dynamic"}}}]
            }, last],
            "edges": [
                edge((None, "a"), (Some("both"), "a")),
                edge((None, "b"), (Some("both"), "b")),
                edge((Some("both"), "out"), (Some("last"), "in")),
                edge((Some("last"), "out"), (None, "out"))
            ]
        }],
        "entry": "main"
    });
    std::fs::write(root.join("select.wir.json"), document.to_string()).unwrap();
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    let launch = Launch {
        target: root.join("select.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    };
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let mut simulator = Simulator::new(root.join("simulator"));
    let execution = execute("select", &mut driver, &mut simulator);
    execution.into_result().map_err(|error| error.to_string())?;
    Ok(driver.results().unwrap()["out"].clone())
}

/// A select op `last` with `rule` and the output type `output`.
fn select(rule: &str, output: Json) -> Json {
    json!({
        "path": "last",
        "kind": {"select": {"rule": rule}},
        "inputs": [{"name": "in", "type": {"array": {"items": "dynamic"}}}],
        "outputs": [{"name": "out", "type": output}]
    })
}

#[test]
fn selects_set_items() {
    let folder = tempfile::tempdir().unwrap();
    let optional = || (json!({"optional": "int"}), json!({"optional": "int"}));
    let picked = |inputs: Json, rule: &str, output: Json| {
        run(folder.path(), inputs, optional(), select(rule, output))
    };
    let one = json!({"a": null, "b": 2});
    assert_eq!(picked(one.clone(), "first", json!("int")), Ok(json!(2)));
    assert_eq!(picked(one.clone(), "only", json!("int")), Ok(json!(2)));
    let all = json!({"array": {"items": "int"}});
    assert_eq!(picked(one, "all", all.clone()), Ok(json!([2])));
    let two = json!({"a": 1, "b": 2});
    assert_eq!(picked(two.clone(), "first", json!("int")), Ok(json!(1)));
    let error = picked(two, "only", json!("int")).unwrap_err();
    assert!(
        error.contains("op last: 2 items are set, not one"),
        "{error}"
    );
    let none = json!({"a": null, "b": null});
    assert_eq!(picked(none.clone(), "all", all), Ok(json!([])));
    let error = picked(none, "first", json!("int")).unwrap_err();
    assert!(error.contains("op last: no item is set"), "{error}");
}

#[test]
fn flattens_one_level() {
    let folder = tempfile::tempdir().unwrap();
    let list = json!({"array": {"items": {"array": {"items": "int"}}}});
    let types = (list.clone(), json!("int"));
    let flatten = json!({
        "path": "last",
        "kind": {"flatten": {}},
        "inputs": [{"name": "in", "type": {"array": {"items": "dynamic"}}}],
        "outputs": [{"name": "out", "type": {"array": {"items": "dynamic"}}}]
    });
    let inputs = json!({"a": [[1], [], [2, 3]], "b": 4});
    let out = run(folder.path(), inputs, types, flatten.clone()).unwrap();
    assert_eq!(
        out,
        json!([[1], [], [2, 3], 4]),
        "an array item gives its items"
    );
    let mut typed = flatten;
    typed["outputs"][0]["type"] = json!({"array": {"items": "int"}});
    let inputs = json!({"a": [[1], [2]], "b": 4});
    let error = run(folder.path(), inputs, (list, json!("int")), typed).unwrap_err();
    assert!(
        error.contains("op last: the value does not fit output out"),
        "{error}"
    );
}
