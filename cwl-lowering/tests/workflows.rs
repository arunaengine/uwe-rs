//! Lowers CWL workflows into wir graphs: steps, plain links and subworkflow calls.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use cwl_lowering::{Lowering, lower};
use serde_json::{Value as Json, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn lowered(name: &str) -> Lowering {
    let path = fixtures().join(name);
    let document = cwl_parser::load_file(&path, Default::default()).unwrap();
    lower(&document, &fixtures()).unwrap()
}

fn graph(lowering: &Lowering, name: &str) -> Json {
    let graph = lowering
        .document
        .graphs
        .iter()
        .find(|graph| graph.name == name);
    serde_json::to_value(graph.unwrap()).unwrap()
}

fn kinds(graph: &Json) -> Vec<(String, String)> {
    let ops = graph["ops"].as_array().unwrap();
    (ops.iter())
        .map(|op| {
            let kind = op["kind"].as_object().unwrap().keys().next().unwrap();
            (op["path"].as_str().unwrap().to_owned(), kind.clone())
        })
        .collect()
}

#[test]
fn lowers_steps_calls() {
    let lowering = lowered("pipeline.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let main = graph(&lowering, "main");
    let ops = [
        ("inner".into(), "call".into()),
        ("say".into(), "task".into()),
    ];
    assert_eq!(kinds(&main), ops, "the steps in the order of their ids");
    assert_eq!(main["ops"][0]["kind"]["call"]["graph"], "counting");
    let edges = json!([
        {"from": {"op": "say", "port": "out"}, "to": {"op": "inner", "port": "text"}},
        {"from": {"port": "message"}, "to": {"op": "say", "port": "message"}},
        {"from": {"op": "inner", "port": "count"}, "to": {"port": "count"}},
        {"from": {"op": "say", "port": "out"}, "to": {"port": "said"}}
    ]);
    assert_eq!(main["edges"], edges);
    let counting = graph(&lowering, "counting");
    assert_eq!(kinds(&counting), [("wc".to_owned(), "task".to_owned())]);
    let locations: Vec<&str> = (lowering.document.sources.iter())
        .map(|source| source.location.as_str())
        .collect();
    assert_eq!(locations, ["counting.cwl", "count.cwl", "echo.cwl"]);
}

#[test]
fn lowers_scatter_maps() {
    let lowering = lowered("scatter.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let main = graph(&lowering, "main");
    assert_eq!(kinds(&main), [("say".to_owned(), "map".to_owned())]);
    let map = json!({"body": "say", "over": ["message"]});
    assert_eq!(main["ops"][0]["kind"]["map"], map);
    let message = &main["ops"][0]["inputs"][0];
    assert_eq!(message["type"], json!({"array": {"items": "string"}}));
    assert_eq!(
        kinds(&graph(&lowering, "say")),
        [("say".to_owned(), "task".to_owned())]
    );
    let lowering = lowered("cross.cwl");
    let main = graph(&lowering, "main");
    assert_eq!(main["ops"][0]["kind"]["map"]["mode"], "nested_cross");
    let nested = json!({"array": {"items": {"array": {"items": {"file": {}}}}}});
    assert_eq!(main["ops"][0]["outputs"][0]["type"], nested);
}

#[test]
fn lowers_when_branches() {
    let lowering = lowered("gate.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let main = graph(&lowering, "main");
    let ops = [
        ("say.when".into(), "expr".into()),
        ("say".into(), "branch".into()),
    ];
    assert_eq!(kinds(&main), ops);
    assert_eq!(main["ops"][0]["kind"]["expr"]["code"], "$(inputs.go)");
    let branch = json!({"body": "say.when", "condition": "when"});
    assert_eq!(main["ops"][1]["kind"]["branch"], branch);
    let gate = json!({"from": {"port": "go"}, "to": {"op": "say.when", "port": "go"}});
    assert!(
        main["edges"].as_array().unwrap().contains(&gate),
        "only the condition takes go"
    );
    let lowering = lowered("when.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let round = graph(&lowering, "say");
    assert_eq!(kinds(&round), ops, "the condition holds per round");
    assert_eq!(round["ops"][0]["kind"]["expr"]["library"], Json::Null);
    let optional = json!({"array": {"items": {"optional": {"file": {}}}}});
    assert_eq!(
        graph(&lowering, "main")["ops"][0]["outputs"][0]["type"],
        optional
    );
}

#[test]
fn lowers_input_defaults() {
    let lowering = lowered("defaults.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let main = graph(&lowering, "main");
    let name = json!({"name": "name", "type": "string", "default": "world"});
    assert_eq!(main["inputs"][0], name, "a graph input keeps its type");
    let text = json!({"class": "File", "location": "echo.cwl"});
    assert_eq!(main["inputs"][2]["default"], text, "relative to the base");
    let ops = main["ops"].as_array().unwrap();
    let op = |path: &str| ops.iter().find(|op| op["path"] == path).unwrap();
    let again = json!([
        {"name": "greeting", "type": "string", "default": "hi"},
        {"name": "name", "type": "string", "default": "there"}
    ]);
    assert_eq!(op("again")["inputs"], again, "the step defaults win");
    let say = json!({"name": "greeting", "type": "string", "default": "hello"});
    assert_eq!(op("say")["inputs"][0], say, "the tool default");
}

#[test]
fn lowers_step_values() {
    let lowering = lowered("values.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let main = graph(&lowering, "main");
    let ops = [
        ("each".into(), "map".into()),
        ("say.message".into(), "expr".into()),
        ("say".into(), "task".into()),
    ];
    assert_eq!(kinds(&main), ops);
    let value = &main["ops"][1];
    assert_eq!(value["kind"]["expr"]["code"], "hello $(self)");
    let names: Vec<&str> = (value["inputs"].as_array().unwrap().iter())
        .map(|port| port["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["message", "self"], "the step inputs and self");
    assert_eq!(
        value["outputs"][0]["type"], "string",
        "the type of the tool input"
    );
    let edges = main["edges"].as_array().unwrap();
    for port in ["message", "self"] {
        let edge = json!({"from": {"port": "name"}, "to": {"op": "say.message", "port": port}});
        assert!(edges.contains(&edge), "{port} takes the source");
    }
    let round = graph(&lowering, "each");
    let ops = [
        ("each.message".into(), "expr".into()),
        ("each".into(), "task".into()),
    ];
    assert_eq!(kinds(&round), ops, "valueFrom runs per round");
    let greeting = json!({"name": "greeting", "type": "dynamic", "default": "hi"});
    let map = main["ops"][0]["inputs"].as_array().unwrap();
    assert!(
        map.contains(&greeting),
        "the map takes the default: {map:?}"
    );
}
