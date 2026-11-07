//! Runs documents with input defaults: of the entry graph, of an op, of a called graph and of
//! the body of a map, and a File default with a location relative to the document.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

/// An expr op `path` that gives the value of its input `name`.
fn echo(path: &str, input: Json, output: Json) -> Json {
    let name = input["name"].as_str().unwrap().to_owned();
    json!({
        "path": path,
        "kind": {"expr": {"language": "js", "code": format!("$(inputs.{name})")}},
        "inputs": [input],
        "outputs": [{"name": "out", "type": output}]
    })
}

fn document() -> Json {
    let optional = json!({"optional": "int"});
    let items = json!({"array": {"items": optional}});
    json!({
        "wir_version": wir::VERSION,
        "graphs": [{
            "name": "main",
            "inputs": [
                {"name": "count", "type": "int", "default": 3},
                {"name": "word", "type": {"optional": "string"}},
                {"name": "numbers", "type": items},
                {"name": "text", "type": {"file": {}}, "default": {"class": "File", "location": "a.txt"}}
            ],
            "outputs": [
                {"name": "count", "type": "int"},
                {"name": "word", "type": "string"},
                {"name": "inner", "type": "int"},
                {"name": "numbers", "type": {"array": {"items": "int"}}},
                {"name": "text", "type": "string"}
            ],
            "ops": [
                echo("count", json!({"name": "count", "type": "int"}), json!("int")),
                echo("word", json!({"name": "word", "type": "string", "default": "a"}), json!("string")),
                {
                    "path": "inner",
                    "kind": {"call": {"graph": "inner"}},
                    "inputs": [{"name": "n", "type": optional}],
                    "outputs": [{"name": "out", "type": "int"}]
                },
                {
                    "path": "each",
                    "kind": {"map": {"body": "inner", "over": ["n"]}},
                    "inputs": [{"name": "n", "type": items}],
                    "outputs": [{"name": "out", "type": {"array": {"items": "int"}}}]
                },
                echo("text", json!({"name": "text", "type": {"file": {}}}), json!("string"))
            ],
            "edges": [
                edge((None, "count"), (Some("count"), "count")),
                edge((None, "word"), (Some("word"), "word")),
                edge((None, "numbers"), (Some("each"), "n")),
                edge((None, "text"), (Some("text"), "text")),
                edge((Some("count"), "out"), (None, "count")),
                edge((Some("word"), "out"), (None, "word")),
                edge((Some("inner"), "out"), (None, "inner")),
                edge((Some("each"), "out"), (None, "numbers")),
                edge((Some("text"), "out"), (None, "text"))
            ]
        }, {
            "name": "inner",
            "inputs": [{"name": "n", "type": "int", "default": 7}],
            "outputs": [{"name": "out", "type": "int"}],
            "ops": [echo("n", json!({"name": "n", "type": "int"}), json!("int"))],
            "edges": [
                edge((None, "n"), (Some("n"), "n")),
                edge((Some("n"), "out"), (None, "out"))
            ]
        }],
        "entry": "main"
    })
}

#[test]
fn defaults_fill_nulls() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let mut text = document();
    text["graphs"][0]["ops"][4]["kind"]["expr"]["code"] = json!("$(inputs.text.location)");
    std::fs::write(root.join("defaults.wir.json"), text.to_string()).unwrap();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    let inputs = json!({"word": null, "numbers": [1, null]});
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    let launch = Launch {
        target: root.join("defaults.wir.json"),
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
    let execution = execute("defaults", &mut driver, &mut simulator);
    execution.into_result().unwrap();
    let results = driver.results().unwrap();
    assert_eq!(results["count"], 3, "the entry graph default");
    assert_eq!(results["word"], "a", "the op default takes null");
    assert_eq!(results["inner"], 7, "the called graph default");
    assert_eq!(
        results["numbers"],
        json!([1, 7]),
        "the body default per round"
    );
    let location = results["text"].as_str().unwrap();
    let place = root.join("a.txt").display().to_string();
    assert!(
        location.ends_with(&place),
        "{location} starts from the document"
    );
}
