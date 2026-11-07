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
