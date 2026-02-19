//! Tests for typed decoding from value trees.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde::Deserialize;

use super::decode;
use crate::node::{Node, Position, Value};
use crate::tree::parse_yaml;

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
struct Outer {
    name: String,
    count: i64,
    ratio: f64,
    enabled: bool,
    inner: Option<Inner>,
    items: Vec<Inner>,
    extra: Option<Node>,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
struct Inner {
    label: String,
}

fn node(text: &str) -> Node {
    parse_yaml(text).expect("valid YAML").expect("one document")
}

#[test]
fn decodes_typed_fields() {
    let source =
        "name: a\ncount: 3\nratio: 2\nenabled: yes\ninner: {label: x}\nitems: [{label: b}]\n";
    let (outer, unknown) = decode::<Outer>(&node(source)).expect("decodes");
    assert!(unknown.is_empty());
    assert_eq!(outer.name, "a");
    assert_eq!(outer.count, 3);
    assert_eq!(outer.ratio, 2.0);
    assert!(outer.enabled);
    assert_eq!(outer.inner.map(|inner| inner.label), Some("x".to_owned()));
    assert_eq!(outer.items[0].label, "b");
}

#[test]
fn keeps_raw_nodes() {
    let (outer, _) = decode::<Outer>(&node("extra:\n  deep: [1, true]\n")).expect("decodes");
    let extra = outer.extra.expect("raw value");
    assert_eq!(extra.position, Position { line: 2, column: 3 });
    let deep = extra.get("deep").expect("deep");
    let Value::Sequence(items) = &deep.value else {
        panic!("expected a sequence");
    };
    assert_eq!(items[1].value, Value::Bool(true));
}

#[test]
fn lists_unknown_fields() {
    let source = "name: a\nsurprise: 1\nitems:\n  - label: b\n    other: 2\n";
    let (_, unknown) = decode::<Outer>(&node(source)).expect("decodes");
    let paths: Vec<&str> = unknown.iter().map(|field| field.path.as_str()).collect();
    assert_eq!(paths, ["surprise", "items[0].other"]);
    assert_eq!(unknown[1].position, Position { line: 5, column: 5 });
}

#[test]
fn locates_type_errors() {
    let source = "items:\n  - label: ok\n  - label: [1]\n";
    let error = decode::<Outer>(&node(source)).expect_err("wrong type");
    assert_eq!(error.path, "items[1].label");
    assert_eq!(
        error.position,
        Some(Position {
            line: 3,
            column: 12
        })
    );
    assert!(error.message.contains("sequence"), "{}", error.message);
}

#[test]
fn null_means_absent() {
    let source = "name: null\ncount: ~\ninner: null\nitems: null\n";
    let (outer, _) = decode::<Outer>(&node(source)).expect("decodes");
    assert_eq!(outer, Outer::default());
}

#[test]
fn rejects_fractional_integers() {
    assert_eq!(
        decode::<Outer>(&node("count: 2.0\n")).map(|(outer, _)| outer.count),
        Ok(2)
    );
    let error = decode::<Outer>(&node("count: 2.5\n")).expect_err("fraction");
    assert_eq!(error.path, "count");
}
