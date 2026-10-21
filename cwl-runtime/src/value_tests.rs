//! Tests of values: exact numbers, JSON forms and Python's text forms.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

#[test]
fn preserves_large_integers() {
    let value = Value::parse("{\"a\": 4200000000000000000000000000000000000000000, \"b\": 4.2}")
        .expect("JSON");
    assert_eq!(
        value.to_python_json(true),
        "{\"a\": 4200000000000000000000000000000000000000000, \"b\": 4.2}"
    );
    assert_eq!(
        value.to_json(),
        "{\"a\":4200000000000000000000000000000000000000000,\"b\":4.2}"
    );
}

#[test]
fn formats_python_floats() {
    assert_eq!(python_float(4.2), "4.2");
    assert_eq!(python_float(100.0), "100.0");
    assert_eq!(python_float(1e-7), "1e-07");
    assert_eq!(python_float(0.0001), "0.0001");
    assert_eq!(python_float(1e16), "1e+16");
    assert_eq!(python_float(1e15), "1000000000000000.0");
    assert_eq!(python_float(-1.5e300), "-1.5e+300");
    assert_eq!(javascript_float(1e21), "1e+21");
    assert_eq!(javascript_float(1e-7), "1e-7");
    assert_eq!(javascript_float(0.000001), "0.000001");
    assert_eq!(javascript_float(100.0), "100");
}

#[test]
fn sorts_python_keys() {
    let value = Value::parse("{b: [1, 'x'], a: null, c: 'é'}").expect("YAML");
    assert_eq!(
        value.to_python_json(true),
        "{\"a\": null, \"b\": [1, \"x\"], \"c\": \"\\u00e9\"}"
    );
    assert_eq!(
        Value::parse("[true, null, 'it''s']")
            .expect("YAML")
            .to_python_text(),
        "[True, None, \"it's\"]"
    );
}

#[test]
fn compares_python_numbers() {
    assert!(python_equal(&Value::Integer(1), &Value::Float(1.0)));
    assert!(python_equal(&Value::Bool(true), &Value::Integer(1)));
    assert!(!python_equal(&Value::string("1"), &Value::Integer(1)));
}

#[test]
fn summarizes_message_values() {
    let summary = |text: &str| Value::parse(text).expect("YAML").summary();
    assert_eq!(
        summary("{class: File, location: 'file:///d/a.vcf'}"),
        "a File `a.vcf`"
    );
    assert_eq!(
        summary("{class: Directory, path: /d/out/}"),
        "a Directory `out`"
    );
    assert_eq!(summary("'12'"), "the string \"12\"");
    assert_eq!(summary("0.05"), "the number 0.05");
    assert_eq!(summary("{a: 1}"), "an object");
    assert_eq!(
        summary("[x, y]"),
        "a list of 2 items (the first is the string \"x\")"
    );
    assert_eq!(
        Value::string("x".repeat(50)).summary(),
        format!("the string \"{}\"...", "x".repeat(40))
    );
}

#[test]
fn formats_output_objects() {
    let value = Value::parse(
        "{b: [1, 2], a: {}, c: [], d: 1000000000000000000000000000000000000000000, e: 4.2, \
         f: 1.0e42, g: é}",
    )
    .expect("YAML");
    // Python's json.dumps(indent=4, ensure_ascii=False) of the same object.
    let expected = "{\n    \"b\": [\n        1,\n        2\n    ],\n    \"a\": {},\n    \
        \"c\": [],\n    \"d\": 1000000000000000000000000000000000000000000,\n    \
        \"e\": 4.2,\n    \"f\": 1e+42,\n    \"g\": \"é\"\n}";
    assert_eq!(value.to_pretty_json(), expected);
}
