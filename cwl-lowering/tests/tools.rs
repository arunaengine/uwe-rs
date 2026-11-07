//! Lowers CWL tools into wir task ops, and keeps tools with unknown requirements native.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use cwl_lowering::{Kept, Lowering, lower};
use serde_json::{Value as Json, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn lowered(name: &str) -> Lowering {
    let path = fixtures().join(name);
    let document = cwl_parser::load_file(&path, Default::default()).unwrap();
    lower(&document, &fixtures()).unwrap()
}

/// The only op of the entry graph, as JSON.
fn only_op(lowering: &Lowering) -> Json {
    let graph = &lowering.document.graphs[0];
    assert_eq!(graph.ops.len(), 1, "one op: {graph:?}");
    serde_json::to_value(&graph.ops[0]).unwrap()
}

#[test]
fn lowers_plain_tools() {
    let lowering = lowered("echo.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let op = only_op(&lowering);
    let task = json!({
        "image": "debian:stable-slim",
        "command": ["echo", "-n", {"input": "message"}, {"input": "words"}],
        "captures": [{"output": "out", "from": "stdout"}],
        "stdout": "out.txt"
    });
    assert_eq!(op["path"], "echo");
    assert_eq!(op["kind"]["task"], task);
    let inputs = json!([
        {"name": "message", "type": "string"},
        {"name": "unused", "type": {"optional": "int"}},
        {"name": "words", "type": {"optional": {"array": {"items": "string"}}}}
    ]);
    assert_eq!(op["inputs"], inputs);
    assert_eq!(op["origin"], json!({"source": "echo"}));
    let source = &lowering.document.sources[0];
    assert_eq!(
        (source.location.as_str(), source.version.as_deref()),
        ("echo.cwl", Some("v1.2"))
    );
}

#[test]
fn captures_stream_files() {
    let op = only_op(&lowered("count.cwl"));
    let task = &op["kind"]["task"];
    assert_eq!(task["image"], "docker.io/library/debian:12-slim");
    assert_eq!(task["command"], json!(["wc", "-l", {"input": "text"}]));
    assert_eq!(task["stdout"], "count.txt");
    let captures = json!([{"output": "count", "from": "stdout"}]);
    assert_eq!(
        task["captures"], captures,
        "the glob of the stdout file reads the stream"
    );
    let resources = json!({"cpus": 2, "memory": 67_108_864});
    assert_eq!(op["attributes"], json!({"resources": resources}));
}

#[test]
fn keeps_expressions_native() {
    let lowering = lowered("split.cwl");
    let reason = "output parts: an expression in a glob";
    assert_eq!(
        lowering.kept,
        [Kept {
            path: "split".into(),
            reason: reason.into()
        }]
    );
    let op = only_op(&lowering);
    let native = json!({"source": "split", "language": "cwl"});
    assert_eq!(op["kind"]["native"], native);
    assert_eq!(
        op["outputs"][0]["type"],
        json!({"array": {"items": {"file": {}}}})
    );
}

#[test]
fn keeps_requirements_native() {
    let lowering = lowered("environment.cwl");
    let reason = "the requirement EnvVarRequirement";
    assert_eq!(lowering.kept[0].reason, reason);
    assert!(wir::verify(&lowering.document).is_ok());
}

#[test]
fn lowers_expression_tools() {
    let lowering = lowered("sum.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let op = only_op(&lowering);
    assert_eq!(op["kind"]["call"]["graph"], "sum");
    let graph = serde_json::to_value(&lowering.document.graphs[1]).unwrap();
    assert_eq!(
        graph["inputs"][1],
        json!({"name": "b", "type": "int", "default": 2})
    );
    let ops = graph["ops"].as_array().unwrap();
    let paths: Vec<&str> = ops.iter().map(|op| op["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["both", "sum"], "an expr per output");
    let code = ops[1]["kind"]["expr"]["code"].as_str().unwrap();
    assert!(code.starts_with("${ var value = (function() {"), "{code}");
    assert!(code.ends_with(r#"})()["sum"]; return value === undefined ? null : value; }"#));
    assert_eq!(ops[1]["outputs"], json!([{"name": "out", "type": "int"}]));
    assert_eq!(
        ops[1]["inputs"][1],
        json!({"name": "b", "type": "int"}),
        "no default"
    );
}

#[test]
fn lowers_input_values() {
    let lowering = lowered("shout.cwl");
    assert!(lowering.full(), "{:?}", lowering.kept);
    let graph = serde_json::to_value(&lowering.document.graphs[0]).unwrap();
    let ops = graph["ops"].as_array().unwrap();
    let paths: Vec<&str> = ops.iter().map(|op| op["path"].as_str().unwrap()).collect();
    assert_eq!(
        paths,
        ["shout.count.valueFrom", "shout.message.valueFrom", "shout"]
    );
    let command = json!(["echo", {"input": "message.valueFrom"}, {"input": "count.valueFrom"}]);
    assert_eq!(ops[2]["kind"]["task"]["command"], command);
    let this = json!({"name": "self", "type": "int", "default": 2});
    assert_eq!(
        ops[0]["inputs"][2], this,
        "self is the input with its default"
    );
    let edge =
        json!({"from": {"port": "count"}, "to": {"op": "shout.count.valueFrom", "port": "self"}});
    assert!(graph["edges"].as_array().unwrap().contains(&edge));
}

#[test]
fn keeps_required_any() {
    let path = fixtures().join("twice.cwl");
    let document = cwl_parser::load_file(&path, Default::default()).unwrap();
    let error = lower(&document, &fixtures()).unwrap_err();
    assert!(error.contains("the Any input value"), "{error}");
}
