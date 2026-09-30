//! Checks number literals and Java number text against Nextflow 26.04.6 (Groovy spec, 4.1 and 6.2).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::number::Number;

fn integer(text: &str) -> (&'static str, String) {
    let number = Number::parse_integer(text).expect("valid integer literal");
    (number.class_name(), number.to_string())
}

fn decimal(text: &str) -> (&'static str, String) {
    let number = Number::parse_decimal(text).expect("valid decimal literal");
    (number.class_name(), number.to_string())
}

#[test]
fn narrows_integer_literals() {
    let cases = [
        ("1", "Integer", "1"),
        ("2147483647", "Integer", "2147483647"),
        ("2147483648", "Long", "2147483648"),
        ("9223372036854775807", "Long", "9223372036854775807"),
        ("9223372036854775808", "BigInteger", "9223372036854775808"),
        ("10L", "Long", "10"),
        ("10i", "Integer", "10"),
        ("10G", "BigInteger", "10"),
        ("0x1F", "Integer", "31"),
        ("0b101", "Integer", "5"),
        ("017", "Integer", "15"),
        ("0L", "Long", "0"),
        ("0", "Integer", "0"),
        ("1_000_000", "Integer", "1000000"),
        ("0xFFFFFFFFi", "Integer", "-1"),
        ("0xFFFFFFFFFFFFFFFFL", "Long", "-1"),
        ("0xFFFFFFFF", "Long", "4294967295"),
    ];
    for (text, class, value) in cases {
        assert_eq!(integer(text), (class, value.to_owned()), "{text}");
    }
    let error = Number::parse_integer("2147483648i").unwrap_err();
    assert_eq!(
        error.message,
        "Number of type Integer exceeds its limit: 2147483648i"
    );
    assert!(Number::parse_integer("9223372036854775808L").is_err());
}

#[test]
fn decimal_literals() {
    let cases = [
        ("1.5", "BigDecimal", "1.5"),
        ("1.50", "BigDecimal", "1.50"),
        ("1.5G", "BigDecimal", "1.5"),
        ("0.0", "BigDecimal", "0.0"),
        ("1e3", "BigDecimal", "1E+3"),
        ("1.0e3", "BigDecimal", "1.0E+3"),
        ("1e-3", "BigDecimal", "0.001"),
        ("1.5d", "Double", "1.5"),
        ("1d", "Double", "1.0"),
        ("1e3d", "Double", "1000.0"),
        ("1.5f", "Float", "1.5"),
        ("1f", "Float", "1.0"),
        ("1.1f", "Float", "1.1"),
        ("1e10f", "Float", "1.0E10"),
    ];
    for (text, class, value) in cases {
        assert_eq!(decimal(text), (class, value.to_owned()), "{text}");
    }
    assert!(Number::parse_decimal("1e400d").is_err());
    assert!(Number::parse_decimal("1e39f").is_err());
}

#[test]
fn formats_java_doubles() {
    let cases = [
        (1.0, "1.0"),
        (100.0, "100.0"),
        (1e7, "1.0E7"),
        (12345678.0, "1.2345678E7"),
        (9999999.0, "9999999.0"),
        (0.001, "0.001"),
        (1e-4, "1.0E-4"),
        (0.1 + 0.2, "0.30000000000000004"),
        (3.0 * 1.1, "3.3000000000000003"),
        (1e20, "1.0E20"),
        (-0.0, "-0.0"),
        (-2.5, "-2.5"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
        (f64::MAX, "1.7976931348623157E308"),
        (5e-324, "4.9E-324"),
        (f64::from(1.1f32), "1.100000023841858"),
    ];
    for (value, expected) in cases {
        assert_eq!(Number::Double(value).to_string(), expected, "{value:e}");
    }
    assert_eq!(Number::Float(1.4e-45).to_string(), "1.4E-45");
    assert_eq!(Number::Float(f32::MAX).to_string(), "3.4028235E38");
}
