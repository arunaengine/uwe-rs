//! Tests of how values flow between steps: merging, picking and nesting.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

fn values(text: &str) -> Vec<Value> {
    match Value::parse(text).expect("YAML") {
        Value::List(items) => items,
        other => vec![other],
    }
}

#[test]
fn picks_cwl_values() {
    let list = Value::List(values("[null, 1, null, 2]"));
    assert_eq!(
        pick(Some(PickValue::FirstNonNull), list.clone(), "#x"),
        Ok(Value::Integer(1))
    );
    assert_eq!(
        pick(Some(PickValue::AllNonNull), list.clone(), "#x"),
        Ok(Value::List(values("[1, 2]")))
    );
    assert!(pick(Some(PickValue::OnlyNonNull), list, "#x").is_err());
    let nulls = Value::List(values("[null, null]"));
    assert!(pick(Some(PickValue::FirstNonNull), nulls, "#x").is_err());
    assert_eq!(pick(None, Value::Integer(3), "#x"), Ok(Value::Integer(3)));
}

#[test]
fn nests_crossproduct_results() {
    let flat = values("[a, b, c, d, e, f]");
    assert_eq!(
        nest(&flat, &[2, 3]),
        Value::parse("[[a, b, c], [d, e, f]]").expect("YAML")
    );
    assert_eq!(nest(&[], &[2, 0]), Value::parse("[[], []]").expect("YAML"));
    assert_eq!(nest(&flat, &[6]), Value::List(flat.clone()));
}
