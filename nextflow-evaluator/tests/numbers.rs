//! Checks number and unit methods, unit properties and `as` conversions against Nextflow 26.04.6
//! (Groovy spec, sections 4.5, 5.7, 8.9 and 10.8).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_parser::parse_script;

struct Silent;

impl Host for Silent {
    fn print(&mut self, _: &str) {}
}

/// The value of the body of a function, as its Groovy text, or the error.
fn eval(body: &str) -> Result<String, String> {
    let source = format!("def f() {{\n{body}\n}}");
    let parsed = parse_script(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
    let mut host = Silent;
    let mut interpreter = Interpreter::new(&mut host);
    interpreter
        .call_function(&module, "f", Vec::new())
        .map(|value| value.to_string())
        .map_err(|error| error.to_string())
}

fn value(body: &str) -> String {
    eval(body).expect("evaluates")
}

#[test]
fn unit_properties() {
    assert_eq!(
        value("[1.m, 1.b, 1.kb, 1.Hours, 1.5.GB, 2.GB, 90.min]"),
        "[1m, 1 B, 1 KB, 1h, 1.5 GB, 2 GB, 1h 30m]"
    );
    assert_eq!(
        eval("1.K").unwrap_err(),
        "MissingPropertyException: No such property: K for class: java.lang.Integer"
    );
    assert!(
        eval("1e3.GB")
            .unwrap_err()
            .starts_with("IllegalArgumentException: Not a valid FileSize value")
    );
    assert_eq!(
        value("[90.min.minutes, 2.GB.bytes, 6.GB.mega, 1.h.toMillis()]"),
        "[90, 2147483648, 6144, 3600000]"
    );
    assert_eq!(
        value("[6.GB.toGiga(), 6.GB.toUnit('MB'), (6.GB.mega * 0.8).intValue()]"),
        "[6, 6144, 4915]"
    );
    assert_eq!(
        value("[2.GB * 1.5, 2 * 1.h, 1.h / 7]"),
        "[3 GB, 2h, 8m 34s]"
    );
}

#[test]
fn rounding_and_conversion_methods() {
    assert_eq!(
        value(
            "[1.9.toInteger(), 2.5.round(), (-2.5).round(), 3.7d.round(), 2.5d.round(), (-2.5d).round()]"
        ),
        "[1, 3, -3, 4, 3, -2]"
    );
    assert_eq!(
        value("[(-7).intdiv(2), (-3).abs(), 1.5.toBigDecimal(), 2.toDouble()]"),
        "[-3, 3, 1.5, 2.0]"
    );
    assert_eq!(
        value("def seen = []\n3.times { seen << it }\nseen"),
        "[0, 1, 2]"
    );
}

#[test]
fn casts() {
    assert_eq!(
        value(
            "[3600000 as Duration, 1024 as MemoryUnit, '1e3' as BigDecimal, '2 GB' as MemoryUnit]"
        ),
        "[1h, 1 KB, 1E+3, 2 GB]"
    );
    assert_eq!(
        value("[' 42 ' as Integer, 3.7 as Integer, -3.7 as Integer, 5 as Long]"),
        "[42, 3, -3, 5]"
    );
    assert_eq!(
        eval("'4.5' as Integer").unwrap_err(),
        "NumberFormatException: For input string: \"4.5\""
    );
    assert_eq!(
        value("['false' as Boolean, '' as Boolean, 'abc' as List]"),
        "[true, false, [a, b, c]]"
    );
    assert_eq!(
        value("[[1, 2] as String, [a: 1] as String, 1 as Double]"),
        "[[1, 2], [a:1], 1.0]"
    );
    assert!(
        eval("-1 as Duration")
            .unwrap_err()
            .starts_with("AssertionError")
    );
}

#[test]
fn math_and_system_methods() {
    assert_eq!(
        value(
            "def values = [Math.max(1, 2), Math.max(1, 2L), Math.min(3, 2.0d), Math.round(2.5), \
             Math.round(2.5f), Math.ceil(1.2), Math.pow(2, 10), Math.abs(-3), Math.floor(2.7)]\n\
             [values, values[0] instanceof Integer, values[1] instanceof Long, \
             values[3] instanceof Long, values[4] instanceof Integer]"
        ),
        "[[2, 2, 2.0, 3, 3, 2.0, 1024.0, 3, 2.0], true, true, true, true]"
    );
    assert_eq!(value("System.getenv('IUNCTUS_NOT_SET')"), "null");
    assert_eq!(
        value("[true.toBoolean(), false.booleanValue()]"),
        "[true, false]"
    );
}

#[test]
fn dates_format_like_simple_date_format() {
    // The middle of a month is the same month in every time zone.
    let june: i64 = 1_592_222_400_000; // 2020-06-15T12:00:00Z
    assert_eq!(
        value(&format!("new Date({june}L).format(\"yyyy'T'MM''\")")),
        "2020T06'"
    );
    assert_eq!(
        value(&format!("new Date({june}L).getTime()")),
        "1592222400000"
    );
}
