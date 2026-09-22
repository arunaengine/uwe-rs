//! Tests for JSONPath.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::get;
use crate::value::Value;

fn document() -> Value {
    Value::parse_json(
        r#"{"employees": [{"name": "a", "age": 43}, {"name": "b", "age": 30}],
            "samples": [{"files": {"bam": {"name": "x.bam"}}}], "0": ["p", "q"]}"#,
    )
    .expect("JSON")
}

fn path(text: &str) -> String {
    get(text, &document())
        .unwrap_or_else(|error| panic!("{text}: {error}"))
        .to_go_string()
}

#[test]
fn definite_paths_give_values() {
    assert_eq!(path("$.samples[0].files.bam.name"), "x.bam");
    assert_eq!(path("$['samples'][0]['files'].bam.name"), "x.bam");
    assert_eq!(path("$.employees[-1].age"), "30");
    assert_eq!(path("$.0"), "[p q]");
    assert!(get("$.missing", &document()).is_err());
    assert!(get("$.employees[5]", &document()).is_err());
}

#[test]
fn other_paths_give_lists() {
    assert_eq!(path("$.employees[*].name"), "[a b]");
    assert_eq!(path("$.employees[0:1].name"), "[a]");
    assert_eq!(path("$..name"), "[a b x.bam]");
    assert_eq!(path("$.employees[?(@.age>42 && @.age<44)].name"), "[a]");
    assert_eq!(path("$.employees[?(@.name == 'b')].age"), "[30]");
}

#[test]
fn large_slice_steps_do_not_overflow() {
    assert_eq!(path("$.employees[1::9223372036854775807].name"), "[b]");
    assert_eq!(path("$.employees[1::-9223372036854775808].name"), "[b]");
}
