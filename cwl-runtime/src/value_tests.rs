//! Tests of values: exact numbers, JSON forms and Python's text forms.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

#[test]
fn keeps_large_integers_exact() {
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
fn writes_floats_as_python_and_javascript_do() {
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
fn sorts_keys_and_escapes_like_python() {
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
fn compares_numbers_as_python_does() {
    assert!(python_equal(&Value::Integer(1), &Value::Float(1.0)));
    assert!(python_equal(&Value::Bool(true), &Value::Integer(1)));
    assert!(!python_equal(&Value::string("1"), &Value::Integer(1)));
}
