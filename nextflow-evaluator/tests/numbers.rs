//! Checks number and unit methods, unit properties and `as` conversions against Nextflow 26.04.6
//! (Groovy spec, sections 4.5, 5.7, 8.9 and 10.8).
// Copyright (c) 2026 The uwe Contributors
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
    // Legacy configs compare units with `compareTo`.
    assert_eq!(
        value(
            "[2.GB.compareTo(1.GB), 1.GB.compareTo(1.GB), 1.GB.compareTo(3.GB), \
             2.h.compareTo(90.min), 2.GB.compareTo('6.GB' as nextflow.util.MemoryUnit)]"
        ),
        "[1, 0, -1, 1, -1]"
    );
}

#[test]
fn converts_numeric_methods() {
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
fn dispatches_system_methods() {
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
        value("[Arrays.asList(1, 2), Arrays.asList(['a'])]"),
        "[[1, 2], [a]]"
    );
    assert_eq!(
        value("[true.toBoolean(), false.booleanValue()]"),
        "[true, false]"
    );
}

#[test]
fn formats_java_dates() {
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

#[test]
fn class_values() {
    assert_eq!(
        value(
            "[Path, Path.simpleName, 'x'.getClass(), [1].getClass().name, \
             nextflow.script.types.VersionNumber, 1.getClass() == Integer, \
             [a: 1].getClass().simpleName, Integer.name, Integer.MAX_VALUE]"
        ),
        "[interface java.nio.file.Path, Path, class java.lang.String, java.util.ArrayList, \
         interface nextflow.script.types.VersionNumber, true, LinkedHashMap, java.lang.Integer, \
         2147483647]"
    );
}

#[test]
fn dispatches_scalar_parsers() {
    assert_eq!(
        value(
            "def x = null\n[x.getClass(), x.getClass() == nextflow.util.MemoryUnit, \
             Float.parseFloat('1.5'), Integer.parseInt('42'), Double.valueOf('2')]"
        ),
        "[class org.codehaus.groovy.runtime.NullObject, false, 1.5, 42, 2.0]"
    );
}

#[test]
fn defers_primitive_failures() {
    // Nextflow fails with "Missing process or function call([A])".
    assert_eq!(
        eval("(char)('A')").unwrap_err(),
        "MissingPropertyException: No such variable: char"
    );
}

#[test]
fn evaluates_primitive_casts() {
    assert_eq!(
        value(
            "[3.7 as int, 0 as boolean, Double.NEGATIVE_INFINITY, [1, 2, 3].countBy { v -> v % 2 }, \
             [a: 1].asImmutable()]"
        ),
        "[3, false, -Infinity, [1:2, 0:1], [a:1]]"
    );
}

#[test]
fn operator_methods() {
    assert_eq!(
        value(
            "[2.GB.multiply(1.5), 2.GB.div(2), 1.GB.plus(1.GB), 3.GB.minus(1.GB), 1.h.multiply(2), \
             6.multiply(7), 7.div(2), 1.plus(2), 5.minus(1), 2.power(3), 7.mod(3)]"
        ),
        "[3 GB, 1 GB, 2 GB, 2 GB, 2h, 42, 3.5, 3, 4, 8, 1]"
    );
}

/// Groovy's `upto` and `downto`, as nf-core/proteomicslfq reads lines with `1.upto(5) { ... }`;
/// Groovy 4 gives `[1, 2, 3, 5, 4, 3]` and fails for a range that goes the wrong way.
#[test]
fn upto_and_downto() {
    assert_eq!(
        value("def l = []\n1.upto(3) { l << it }\n5.downto(3) { l << it }\nl"),
        "[1, 2, 3, 5, 4, 3]"
    );
    let error = eval("3.downto(5) { }").expect_err("fails");
    assert!(
        error.contains(
            "GroovyRuntimeException: The argument (5) to downto() cannot be greater than the value (3) it's called on."
        ),
        "{error}"
    );
}

/// Groovy reads the private field `value` of a boxed Boolean or primitive number; Nextflow
/// 26.04.6 gives `[true, 3, 3, 1.5]`, and a `BigDecimal` has no such property.
#[test]
fn reads_scalar_values() {
    assert_eq!(
        value("[true.value, 3.value, 3L.value, 1.5d.value]"),
        "[true, 3, 3, 1.5]"
    );
    assert!(eval("1.5.value").is_err());
}

/// Static helpers of Java and Nextflow classes and `BigDecimal` scales, as Nextflow 26.04.6
/// prints them.
#[test]
fn converts_with_statics() {
    let cases = [
        (
            "[Boolean.parseBoolean('TRUE'), Boolean.parseBoolean('yes')]",
            "[true, false]",
        ),
        ("Boolean.valueOf('true')", "true"),
        (
            "[Integer.toString(5), Integer.toString(255, 16), Long.toString(-5)]",
            "[5, ff, -5]",
        ),
        (
            "[Integer.toHexString(255), Integer.toBinaryString(5)]",
            "[ff, 101]",
        ),
        ("Integer.toHexString(-1)", "ffffffff"),
        (
            "[String.join('-', ['a', 'b']), String.join('-', 'a', 'b')]",
            "[a-b, a-b]",
        ),
        ("new BigDecimal('1.50')", "1.50"),
        (
            "new BigDecimal('1.50').getClass().name",
            "java.math.BigDecimal",
        ),
        ("new BigDecimal(3)", "3"),
        (
            "new BigInteger('123456789012345678901234567890')",
            "123456789012345678901234567890",
        ),
        ("3.toBigDecimal().setScale(2)", "3.00"),
        ("1.555.setScale(2, java.math.RoundingMode.HALF_UP)", "1.56"),
        ("1.555.setScale(1, java.math.RoundingMode.DOWN)", "1.5"),
        ("2.5.setScale(0, java.math.RoundingMode.HALF_EVEN)", "2"),
        ("Duration.of('1h').toMinutes()", "60"),
        ("MemoryUnit.of('1 GB').toMega()", "1024"),
        (
            "[nextflow.util.Duration.of('90s'), nextflow.util.MemoryUnit.of('2 GB')]",
            "[1m 30s, 2 GB]",
        ),
        ("[MemoryUnit.of(1024), Duration.of(1000)]", "[1 KB, 1s]"),
    ];
    for (code, expected) in cases {
        assert_eq!(value(code), expected, "{code}");
    }
    let error = eval("1.555.setScale(2)").unwrap_err();
    assert!(
        error.ends_with("ArithmeticException: Rounding necessary"),
        "{error}"
    );
}
