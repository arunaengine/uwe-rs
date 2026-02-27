//! Checks `BigDecimal` against the values Nextflow 26.04.6 printed (Groovy spec, sections 4 and 5).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;

use nextflow_evaluator::number::{BigDecimal, DivideError};

fn decimal(text: &str) -> BigDecimal {
    BigDecimal::parse(text).expect("valid decimal")
}

fn divide(left: &str, right: &str) -> String {
    decimal(left)
        .groovy_divide(&decimal(right))
        .expect("divisor is not zero")
        .to_string()
}

#[test]
fn literal_text() {
    let cases = [
        ("1.5", "1.5"),
        ("1.50", "1.50"),
        ("0.0", "0.0"),
        ("100.0", "100.0"),
        ("1e3", "1E+3"),
        ("1.0e3", "1.0E+3"),
        ("1.59e7", "1.59E+7"),
        ("1e-3", "0.001"),
        ("1e-7", "1E-7"),
        ("0.0000001", "1E-7"),
        ("0.000001", "0.000001"),
        ("-1.25", "-1.25"),
        ("0e5", "0E+5"),
        ("12", "12"),
    ];
    for (text, expected) in cases {
        assert_eq!(decimal(text).to_string(), expected, "{text}");
    }
    assert_eq!(decimal("1.50").scale(), 2);
    assert_eq!(BigDecimal::parse("1.5.2"), None);
    assert_eq!(BigDecimal::parse(""), None);
    assert_eq!(BigDecimal::parse("."), None);
}

#[test]
fn add_subtract_multiply() {
    assert_eq!(decimal("1.10").add(&decimal("1.20")).to_string(), "2.30");
    assert_eq!(decimal("0.1").add(&decimal("0.2")).to_string(), "0.3");
    assert_eq!(decimal("1.5").subtract(&decimal("1.5")).to_string(), "0.0");
    assert_eq!(decimal("1.5").multiply(&decimal("2")).to_string(), "3.0");
    assert_eq!(decimal("2.50").multiply(&decimal("2")).to_string(), "5.00");
    assert_eq!(decimal("1.0").multiply(&decimal("1.0")).to_string(), "1.00");
    assert_eq!(decimal("-1.50").negate().to_string(), "1.50");
    assert_eq!(decimal("1.5").pow(2).to_string(), "2.25");
}

#[test]
fn groovy_division() {
    let cases = [
        ("7", "2", "3.5"),
        ("6", "2", "3"),
        ("6", "2.0", "3"),
        ("6", "3", "2"),
        ("10", "4", "2.5"),
        ("1", "8", "0.125"),
        ("1", "1024", "0.0009765625"),
        ("1", "3", "0.3333333333"),
        ("2", "3", "0.6666666667"),
        ("10", "3", "3.3333333333"),
        ("100", "3", "33.3333333333"),
        ("123456789012", "7", "17636684144.5714285714"),
        ("1.00000000000", "3", "0.33333333333"),
        ("10", "0.5", "2E+1"),
        ("100", "0.5", "2.0E+2"),
        ("-7", "2", "-3.5"),
        ("-2", "3", "-0.6666666667"),
        ("0", "3", "0"),
    ];
    for (left, right, expected) in cases {
        assert_eq!(divide(left, right), expected, "{left} / {right}");
    }
    let zero = decimal("0");
    assert_eq!(decimal("1").groovy_divide(&zero), Err(DivideError::ByZero));
    assert_eq!(zero.groovy_divide(&zero), Err(DivideError::Undefined));
    assert_eq!(DivideError::ByZero.to_string(), "Division by zero");
}

#[test]
fn exact_and_rounded_division() {
    assert_eq!(decimal("1").divide_exact(&decimal("3")), Ok(None));
    let exact = decimal("1.00")
        .divide_exact(&decimal("4"))
        .unwrap()
        .unwrap();
    assert_eq!(exact.to_string(), "0.25");
    let rounded = decimal("2").divide_rounded(&decimal("3"), 5).unwrap();
    assert_eq!(rounded.to_string(), "0.66667");
    let carried = decimal("999").divide_rounded(&decimal("1"), 2).unwrap();
    assert_eq!(carried.to_string(), "1.0E+3");
}

#[test]
fn remainder_keeps_the_sign_of_the_dividend() {
    let remainder = |left: &str, right: &str| {
        decimal(left)
            .remainder(&decimal(right))
            .unwrap()
            .to_string()
    };
    assert_eq!(remainder("7.5", "2"), "1.5");
    assert_eq!(remainder("-7.5", "2"), "-1.5");
    assert_eq!(remainder("7", "0.25"), "0.00");
    assert_eq!(remainder("5", "0.25"), "0.0");
    assert_eq!(remainder("1", "3"), "1");
}

#[test]
fn rounding_comparing_and_converting() {
    assert_eq!(decimal("2.5").set_scale_half_up(0).to_string(), "3");
    assert_eq!(decimal("-2.5").set_scale_half_up(0).to_string(), "-3");
    assert_eq!(decimal("2.4").set_scale_half_up(0).to_string(), "2");
    assert_eq!(decimal("2").set_scale_half_up(2).to_string(), "2.00");
    assert_eq!(decimal("1.0").compare(&decimal("1.00")), Ordering::Equal);
    assert_ne!(decimal("1.0"), decimal("1.00"));
    assert_eq!(decimal("-3.7").to_bigint(), (-3).into());
    assert_eq!(decimal("1E+3").to_bigint(), 1000.into());
    assert_eq!(decimal("1.5").to_f64(), 1.5);
    assert_eq!(decimal("12.340").precision(), 5);
}
