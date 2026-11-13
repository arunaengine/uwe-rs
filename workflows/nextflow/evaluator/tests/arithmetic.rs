//! Checks Groovy arithmetic against Nextflow 26.04.6 (Groovy spec, section 5).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;

use nextflow_evaluator::number::{ArithmeticError, Number};

/// A number from literal text; a leading `-` is part of the value here.
fn n(text: &str) -> Number {
    Number::parse_integer(text)
        .or_else(|_| Number::parse_decimal(text))
        .expect("valid literal")
}

fn show(number: Number) -> (&'static str, String) {
    (number.class_name(), number.to_string())
}

fn expect(class: &'static str, text: &str) -> (&'static str, String) {
    (class, text.to_owned())
}

fn unsupported(message: &str) -> ArithmeticError {
    ArithmeticError::Unsupported(message.to_owned())
}

#[test]
fn promotes_wrapping_arithmetic() {
    assert_eq!(
        show(n("2147483647").add(&n("1"))),
        expect("Integer", "-2147483648")
    );
    assert_eq!(
        show(n("2147483647").multiply(&n("2"))),
        expect("Integer", "-2")
    );
    let long_max = n("9223372036854775807");
    assert_eq!(
        show(long_max.add(&n("1"))),
        expect("Long", "-9223372036854775808")
    );
    assert_eq!(
        show(n("2147483647").add(&n("1L"))),
        expect("Long", "2147483648")
    );
    assert_eq!(show(n("1.5f").add(&n("1"))), expect("Double", "2.5"));
    assert_eq!(
        show(n("1.1f").add(&n("0"))),
        expect("Double", "1.100000023841858")
    );
    assert_eq!(
        show(n("1.10").add(&n("1.20"))),
        expect("BigDecimal", "2.30")
    );
    assert_eq!(
        show(n("0.1d").add(&n("0.2d"))),
        expect("Double", "0.30000000000000004")
    );
    assert_eq!(
        show(n("3").multiply(&n("1.1d"))),
        expect("Double", "3.3000000000000003")
    );
    assert_eq!(
        show(n("1.5").multiply(&n("2"))),
        expect("BigDecimal", "3.0")
    );
    assert_eq!(show(n("5G").subtract(&n("7"))), expect("BigInteger", "-2"));
}

#[test]
fn division() {
    let divide = |left: &str, right: &str| show(n(left).divide(&n(right)).unwrap());
    assert_eq!(divide("7", "2"), expect("BigDecimal", "3.5"));
    assert_eq!(divide("6", "3"), expect("BigDecimal", "2"));
    assert_eq!(divide("7L", "2"), expect("BigDecimal", "3.5"));
    assert_eq!(divide("7d", "2"), expect("Double", "3.5"));
    assert_eq!(divide("7", "2d"), expect("Double", "3.5"));
    assert_eq!(divide("7f", "2"), expect("Double", "3.5"));
    assert_eq!(divide("1d", "0"), expect("Double", "Infinity"));
    assert_eq!(divide("0d", "0"), expect("Double", "NaN"));
    let error = n("1").divide(&n("0")).unwrap_err();
    assert_eq!(
        error,
        ArithmeticError::Arithmetic("Division by zero".into())
    );
}

#[test]
fn divides_integer_values() {
    let intdiv = |left: &str, right: &str| n(left).intdiv(&n(right)).map(show);
    assert_eq!(intdiv("-7", "2"), Ok(expect("Integer", "-3")));
    assert_eq!(intdiv("7L", "2"), Ok(expect("Long", "3")));
    assert_eq!(intdiv("-2147483648", "-1").unwrap().1, "-2147483648");
    let message = "Cannot use intdiv() on this number type: java.math.BigDecimal with value: 7.5";
    assert_eq!(intdiv("7.5", "2"), Err(unsupported(message)));
    let message = "Cannot use intdiv() on this number type: java.lang.Double with value: 7.0";
    assert_eq!(intdiv("7d", "2"), Err(unsupported(message)));
    assert_eq!(
        intdiv("1", "0"),
        Err(ArithmeticError::Arithmetic("/ by zero".into()))
    );

    let modulo = |left: &str, right: &str| n(left).modulo(&n(right)).map(show);
    assert_eq!(modulo("-7", "3"), Ok(expect("Integer", "-1")));
    assert_eq!(modulo("7", "-3"), Ok(expect("Integer", "1")));
    assert_eq!(modulo("-7G", "3"), Ok(expect("BigInteger", "2")));
    assert_eq!(modulo("7.5", "2"), Ok(expect("BigDecimal", "1.5")));
    assert_eq!(modulo("7.5d", "2"), Ok(expect("Double", "1.5")));
    assert!(modulo("1", "0").is_err());
    assert!(modulo("1G", "-2").is_err());
}

#[test]
fn power() {
    let power = |left: &str, right: &str| show(n(left).power(&n(right)));
    assert_eq!(power("2", "3"), expect("Integer", "8"));
    assert_eq!(power("2", "31"), expect("BigInteger", "2147483648"));
    assert_eq!(power("2L", "40"), expect("Long", "1099511627776"));
    assert_eq!(
        power("2L", "63"),
        expect("BigInteger", "9223372036854775808")
    );
    assert_eq!(power("1.5", "2"), expect("BigDecimal", "2.25"));
    assert_eq!(power("2G", "2G"), expect("BigInteger", "4"));
    assert_eq!(power("2", "-1"), expect("Double", "0.5"));
    assert_eq!(power("4", "0.5"), expect("Integer", "2"));
    assert_eq!(power("2d", "2"), expect("Integer", "4"));
    assert_eq!(power("2", "3L"), expect("Integer", "8"));
    assert_eq!(power("10", "10L"), expect("Long", "10000000000"));
}

#[test]
fn applies_bitwise_operations() {
    assert_eq!(show(n("1.50").negate()), expect("BigDecimal", "-1.50"));
    assert_eq!(
        show(n("-2147483648").negate()),
        expect("Integer", "-2147483648")
    );
    assert_eq!(n("5").and(&n("3")).map(show), Ok(expect("Integer", "1")));
    assert_eq!(n("5").or(&n("2L")).map(show), Ok(expect("Long", "7")));
    assert_eq!(
        n("5G").xor(&n("1")).map(show),
        Ok(expect("BigInteger", "4"))
    );
    let message = "Cannot use and() on this number type: java.math.BigDecimal with value: 1.5";
    assert_eq!(n("1.5").and(&n("1")).map(show), Err(unsupported(message)));
    assert_eq!(n("5").bitwise_not().map(show), Ok(expect("Integer", "-6")));
    assert!(n("1.5").bitwise_not().is_err());

    let shift = |result: Result<Number, ArithmeticError>| result.map(show);
    assert_eq!(
        shift(n("1").shift_left(&n("32"))),
        Ok(expect("Integer", "1"))
    );
    assert_eq!(
        shift(n("1L").shift_left(&n("32"))),
        Ok(expect("Long", "4294967296"))
    );
    assert_eq!(
        shift(n("-8").shift_right(&n("1"))),
        Ok(expect("Integer", "-4"))
    );
    assert_eq!(
        shift(n("-8").shift_right_unsigned(&n("28"))),
        Ok(expect("Integer", "15"))
    );
    assert_eq!(
        shift(n("1G").shift_left(&n("-1"))),
        Ok(expect("BigInteger", "0"))
    );
    assert_eq!(
        shift(n("-3G").shift_right(&n("1"))),
        Ok(expect("BigInteger", "-2"))
    );
    assert!(n("8G").shift_right_unsigned(&n("1")).is_err());
    let message =
        "Shift distance must be an integral type, but 2.0 (java.lang.Double) was supplied";
    assert_eq!(
        shift(n("1").shift_left(&n("2d"))),
        Err(unsupported(message))
    );
}

#[test]
fn compares_promoted_numbers() {
    assert_eq!(n("1").compare(&n("1.0")), Ordering::Equal);
    assert_eq!(n("1").compare(&n("1L")), Ordering::Equal);
    assert_eq!(n("1.5").compare(&n("1.49d")), Ordering::Greater);
    assert_eq!(n("0.0d").compare(&n("-0.0d")), Ordering::Greater);
    let nan = n("0d").divide(&n("0")).unwrap();
    assert_eq!(nan.compare(&nan), Ordering::Equal);
    assert_eq!(
        nan.compare(&n("1d").divide(&n("0")).unwrap()),
        Ordering::Greater
    );
    assert_eq!(n("-0.1").compare(&n("0")), Ordering::Less);
}
