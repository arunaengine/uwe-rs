//! Tests for YAML 1.1 scalar resolution.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::resolve_plain;
use crate::node::Value;

fn text(value: &str) -> Value {
    Value::String(value.to_owned())
}

#[test]
fn resolves_yaml_booleans() {
    for word in ["y", "Yes", "true", "TRUE", "on", "ON"] {
        assert_eq!(resolve_plain(word), Value::Bool(true), "{word}");
    }
    for word in ["n", "No", "false", "off", "OFF"] {
        assert_eq!(resolve_plain(word), Value::Bool(false), "{word}");
    }
    assert_eq!(resolve_plain("tRUE"), text("tRUE"));
}

#[test]
fn resolves_nulls() {
    for word in ["", "~", "null", "Null", "NULL"] {
        assert_eq!(resolve_plain(word), Value::Null, "{word:?}");
    }
    assert_eq!(resolve_plain("nil"), text("nil"));
}

#[test]
fn resolves_integers() {
    let cases = [
        ("0", 0),
        ("42", 42),
        ("-7", -7),
        ("+5", 5),
        ("0755", 493),
        ("0o17", 15),
        ("0x1F", 31),
        ("-0x1F", -31),
        ("0b101", 5),
        ("1_000", 1000),
    ];
    for (source, expected) in cases {
        assert_eq!(resolve_plain(source), Value::Integer(expected), "{source}");
    }
    assert_eq!(
        resolve_plain("18446744073709551615"),
        Value::Unsigned(u64::MAX)
    );
}

#[test]
fn resolves_floats() {
    let cases = [
        ("1.5", 1.5),
        ("1e3", 1000.0),
        ("1.", 1.0),
        (".5", 0.5),
        ("-.5", -0.5),
        ("08", 8.0),
        ("-9223372036854775809", -9.223_372_036_854_776e18),
    ];
    for (source, expected) in cases {
        assert_eq!(resolve_plain(source), Value::Float(expected), "{source}");
    }
    assert_eq!(resolve_plain(".inf"), Value::Float(f64::INFINITY));
    assert_eq!(resolve_plain("-.Inf"), Value::Float(f64::NEG_INFINITY));
    assert!(matches!(resolve_plain(".nan"), Value::Float(number) if number.is_nan()));
}

#[test]
fn keeps_other_text() {
    for source in [
        "1.2.3",
        "0x",
        "1e",
        "2024-01-02",
        "v1",
        "-",
        "1_a",
        ".x",
        "infinity",
    ] {
        assert_eq!(resolve_plain(source), text(source), "{source}");
    }
}
