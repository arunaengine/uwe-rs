//! Checks `MemoryUnit` and `Duration` against Nextflow 26.04.6 (Groovy spec, 4.5, 5.6 and 6.4).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::number::Number;
use nextflow_evaluator::units::{
    divide_count, duration_text, java_round, memory_text, multiply_count, parse_duration,
    parse_memory,
};

fn memory(text: &str) -> String {
    memory_text(parse_memory(text).expect("valid memory"))
}

fn duration(text: &str) -> String {
    duration_text(parse_duration(text).expect("valid duration"))
}

fn decimal(text: &str) -> Number {
    Number::parse_decimal(text).expect("valid decimal")
}

#[test]
fn parses_memory_text() {
    let cases = [
        ("2GB", "2 GB"),
        ("1536MB", "1.5 GB"),
        ("1.25GB", "1.2 GB"),
        ("1.35GB", "1.3 GB"),
        ("1.95GB", "2 GB"),
        ("1000B", "1000 B"),
        ("1024B", "1 KB"),
        ("1023MB", "1023 MB"),
        ("0B", "0"),
        ("1EB", "1 EB"),
        ("1ZB", "8 EB"),
        ("2g", "2 GB"),
        ("2 GB", "2 GB"),
        ("1.5 GB", "1.5 GB"),
        ("2", "2 B"),
    ];
    for (text, expected) in cases {
        assert_eq!(memory(text), expected, "{text}");
    }
    for (power, unit) in ["B", "KB", "MB", "GB", "TB", "PB", "EB"].iter().enumerate() {
        assert_eq!(memory_text(1i64 << (10 * power)), format!("1 {unit}"));
    }
    for text in ["2gb", "2 kb", "2 GiB", "GB", "2GB "] {
        let error = parse_memory(text).unwrap_err();
        assert_eq!(error.class, "IllegalArgumentException", "{text}");
    }
    assert_eq!(
        parse_memory("1.5B").unwrap_err().class,
        "NumberFormatException"
    );
}

#[test]
fn memory_arithmetic() {
    let gb = parse_memory("1GB").unwrap();
    let text = |count| memory_text(count);
    assert_eq!(
        text(divide_count(2 * gb, &Number::Integer(3), "Memory").unwrap()),
        "682.7 MB"
    );
    assert_eq!(
        text(multiply_count(2 * gb, &decimal("1.5"), "Memory").unwrap()),
        "3 GB"
    );
    assert_eq!(
        text(multiply_count(gb, &decimal("0.5"), "Memory").unwrap()),
        "512 MB"
    );
    let error = multiply_count(gb, &Number::Integer(-1), "Memory").unwrap_err();
    assert_eq!(error.class, "AssertionError");
}

#[test]
fn parses_duration_text() {
    let cases = [
        ("1h", "1h"),
        ("90min", "1h 30m"),
        ("1500ms", "1.5s"),
        ("2s", "2s"),
        ("1.25s", "1.3s"),
        ("59999ms", "60s"),
        ("59.5s", "59.5s"),
        ("61s", "1m 1s"),
        ("90.5s", "1m 31s"),
        ("119999ms", "2m"),
        ("60000ms", "1m"),
        ("25h", "1d 1h"),
        ("36.5h", "1d 12h 30m"),
        ("0ms", "0ms"),
        ("1.5ms", "2ms"),
        ("1day 6hours 3minutes 30seconds", "1d 6h 3m 30s"),
        ("2h30m", "2h 30m"),
        ("1.5h", "1h 30m"),
        ("1 hour", "1h"),
        ("01:30:00", "1h 30m"),
        ("2 days", "2d"),
    ];
    for (text, expected) in cases {
        assert_eq!(duration(text), expected, "{text}");
    }
    for text in ["1H", "", "1h x", "1:2:3:4", "123:00:00"] {
        let error = parse_duration(text).unwrap_err();
        assert_eq!(error.class, "IllegalArgumentException", "{text}");
        assert_eq!(error.message, format!("Not a valid duration value: {text}"));
    }
}

#[test]
fn duration_arithmetic() {
    let hour = parse_duration("1h").unwrap();
    let text = |count| duration_text(count);
    assert_eq!(
        text(multiply_count(hour, &decimal("1.5"), "Duration").unwrap()),
        "1h 30m"
    );
    assert_eq!(
        text(divide_count(hour, &Number::Integer(7), "Duration").unwrap()),
        "8m 34s"
    );
}

#[test]
fn java_rounding() {
    assert_eq!(java_round(2.5), 3);
    assert_eq!(java_round(-2.5), -2);
    assert_eq!(java_round(-0.5), 0);
    assert_eq!(java_round(3.7), 4);
    assert_eq!(java_round(0.49999999999999994), 0);
    assert_eq!(java_round(f64::NAN), 0);
    assert_eq!(java_round(1e300), i64::MAX);
    assert_eq!(java_round(-1e300), i64::MIN);
}
