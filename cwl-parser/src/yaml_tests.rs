//! Tests reading YAML and JSON: scalar resolution, locations, keys, anchors and tags.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use crate::error::Error;
use crate::node::{Node, Value};
use crate::yaml::read;

fn file() -> Arc<str> {
    Arc::from("file:///work/tool.cwl")
}

fn parse(text: &str) -> Node {
    read(text, &file()).unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn failure(text: &str) -> Error {
    match read(text, &file()) {
        Ok(node) => panic!("{text}: read as {node:?}"),
        Err(error) => error,
    }
}

fn scalar(text: &str) -> Value {
    parse(&format!("value: {text}\n"))
        .get("value")
        .expect("the key is present")
        .value
        .clone()
}

#[test]
fn resolves_core_scalars() {
    assert_eq!(scalar("~"), Value::Null);
    assert_eq!(scalar(""), Value::Null);
    assert_eq!(scalar("True"), Value::Bool(true));
    assert_eq!(scalar("yes"), Value::String("yes".into()));
    assert_eq!(scalar("-12"), Value::Integer(-12));
    assert_eq!(scalar("0x1f"), Value::Integer(31));
    assert_eq!(scalar("0o17"), Value::Integer(15));
    assert_eq!(scalar("1.5"), Value::Float(1.5));
    assert_eq!(scalar("1e3"), Value::Float(1000.0));
    assert_eq!(scalar(".5"), Value::Float(0.5));
    assert_eq!(scalar("-.inf"), Value::Float(f64::NEG_INFINITY));
    assert_eq!(scalar("4.2e42"), Value::Float(4.2e42));
    assert_eq!(
        scalar("-004200000000000000000000000000000000000000000"),
        Value::BigInteger("-4200000000000000000000000000000000000000000".into())
    );
    assert_eq!(scalar("!!float 10000000000000000000"), Value::Float(1e19));
    assert_eq!(scalar("1.2.3"), Value::String("1.2.3".into()));
    assert_eq!(scalar("v1.0"), Value::String("v1.0".into()));
    assert_eq!(scalar("'12'"), Value::String("12".into()));
    assert_eq!(scalar("\"true\""), Value::String("true".into()));
}

#[test]
fn keeps_value_locations() {
    let node = parse("cwlVersion: v1.2\ninputs:\n  x:\n    type: File\n");
    let inputs = node.get("inputs").expect("inputs are present");
    assert_eq!((inputs.location.line, inputs.location.column), (3, 3));
    let entry = &inputs.as_map().expect("a map")[0];
    assert_eq!(entry.key, "x");
    assert_eq!((entry.location.line, entry.location.column), (3, 3));
    let kind = entry.value.get("type").expect("type is present");
    assert_eq!((kind.location.line, kind.location.column), (4, 11));
    assert_eq!(kind.location.file.as_ref(), "file:///work/tool.cwl");
}

#[test]
fn keeps_entry_order() {
    let node = parse("b: 1\na: 2\nc: 3\n");
    let keys: Vec<&str> = node
        .as_map()
        .expect("a map")
        .iter()
        .map(|entry| entry.key.as_str())
        .collect();
    assert_eq!(keys, ["b", "a", "c"]);
}

#[test]
fn rejects_duplicate_keys() {
    let error = failure("class: CommandLineTool\ninputs: {}\nclass: Workflow\n");
    assert_eq!(error.message, "duplicate key `class`");
    assert_eq!((error.location.line, error.location.column), (3, 1));
}

#[test]
fn reports_syntax_errors() {
    let error = failure("inputs:\n  - a\n b: c\n");
    assert_eq!(error.location.line, 3);
    assert!(!error.message.is_empty());
}

#[test]
fn reads_json_input() {
    let node =
        parse("{\"class\":\"CommandLineTool\",\n \"inputs\": [{\"id\": \"x\", \"type\": 3.5}]}");
    assert_eq!(
        node.get("class").and_then(Node::as_str),
        Some("CommandLineTool")
    );
    let input = &node.get("inputs").and_then(Node::as_list).expect("a list")[0];
    assert_eq!(
        input.get("type").map(|node| &node.value),
        Some(&Value::Float(3.5))
    );
    assert_eq!(input.location.line, 2);
}

#[test]
fn expands_aliases() {
    let node = parse("base: &image {dockerPull: alpine}\ncopy: *image\n");
    assert_eq!(
        node.get("copy").map(|node| &node.value),
        node.get("base").map(|node| &node.value)
    );
}

#[test]
fn applies_core_tags() {
    assert_eq!(scalar("!!str 12"), Value::String("12".into()));
    assert_eq!(scalar("!!float 2"), Value::Float(2.0));
    assert_eq!(
        failure("value: !custom 12\n").message,
        "unsupported YAML tag !custom"
    );
    assert!(
        failure("value: !!int twelve\n")
            .message
            .contains("does not fit")
    );
}

#[test]
fn reads_first_document() {
    let node = parse("class: Workflow\n---\n{ broken\n");
    assert_eq!(node.get("class").and_then(Node::as_str), Some("Workflow"));
    assert_eq!(parse("").value, Value::Null);
}

#[test]
fn skips_unicode_signature() {
    let node = parse("\u{feff}#!/usr/bin/env cwl-runner\ncwlVersion: v1.2\n");
    assert_eq!(node.get("cwlVersion").and_then(Node::as_str), Some("v1.2"));
}

#[test]
fn rejects_complex_keys() {
    assert_eq!(
        failure("? [a, b]\n: c\n").message,
        "a map key must be a string"
    );
    assert_eq!(failure("~: c\n").message, "a map key must not be null");
}

#[test]
fn accepts_indented_quotes() {
    let text = "steps:\n    a:\n        valueFrom: '${\n            return 1;\n        }'\n";
    let node = parse(text);
    let step = node.get("steps").and_then(|steps| steps.get("a"));
    let value = step
        .and_then(|step| step.get("valueFrom"))
        .expect("a value");
    assert_eq!(value.value, Value::String("${ return 1; }".into()));
    assert_eq!((value.location.line, value.location.column), (3, 20));
}

#[test]
fn accepts_flow_dash() {
    let node = parse("arguments: [odgi, build, -o, -, -t]\n");
    let arguments = node
        .get("arguments")
        .and_then(Node::as_list)
        .expect("a list");
    let words: Vec<&str> = arguments.iter().filter_map(Node::as_str).collect();
    assert_eq!(words, ["odgi", "build", "-o", "-", "-t"]);
}
