//! Tests for Go's formatting of values.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use super::{Value, go_float, go_quote};

#[test]
fn floats_print_like_go() {
    assert_eq!(go_float(4.0), "4");
    assert_eq!(go_float(0.5), "0.5");
    assert_eq!(go_float(123456.0), "123456");
    assert_eq!(go_float(1e6), "1e+06");
    assert_eq!(go_float(1234567.0), "1.234567e+06");
    assert_eq!(go_float(18446744073709551616.0), "1.8446744073709552e+19");
    assert_eq!(go_float(0.0001), "0.0001");
    assert_eq!(go_float(0.00001), "1e-05");
    assert_eq!(go_float(-2.5), "-2.5");
}

#[test]
fn collections_print_like_go() {
    let map: BTreeMap<String, Value> = [
        ("b".to_owned(), Value::Int(2)),
        ("a".to_owned(), Value::string("x")),
    ]
    .into_iter()
    .collect();
    let value = Value::Array(vec![Value::Map(map), Value::Nil, Value::Bool(true)]);
    assert_eq!(value.to_go_string(), "[map[a:x b:2] <nil> true]");
    assert_eq!(value.to_json(), r#"[{"a":"x","b":2},null,true]"#);
}

#[test]
fn json_escapes_like_go() {
    let escaped = Value::string("a<b>&\"\n").to_json();
    let backslash = '\\';
    assert_eq!(
        escaped,
        format!("\"a{backslash}u003cb{backslash}u003e{backslash}u0026{backslash}\"{backslash}n\"")
    );
    assert_eq!(Value::Float(1.0).to_json(), "1");
}

#[test]
fn quotes_like_go() {
    let backslash = '\\';
    assert_eq!(
        go_quote("[{\"a\":\"b\n\"}]"),
        format!("\"[{{{backslash}\"a{backslash}\":{backslash}\"b{backslash}n{backslash}\"}}]\"")
    );
    assert_eq!(
        go_quote("\u{1}\u{a0}é"),
        format!("\"{backslash}x01{backslash}u00a0é\"")
    );
}

#[test]
fn json_numbers_are_floats() {
    let value = Value::parse_json(r#"{"n": 3, "l": [1, "x"]}"#).expect("JSON");
    let Value::Map(map) = value else {
        panic!("a map");
    };
    assert_eq!(map["n"], Value::Float(3.0));
    assert_eq!(map["l"].to_go_string(), "[1 x]");
}
