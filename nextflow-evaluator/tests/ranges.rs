//! Checks ranges against Nextflow 26.04.6 (Groovy spec, sections 4.4, 6.1, 8.5 and 8.11).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_parser::parse_script;

#[derive(Default)]
struct Output(String);

impl Host for Output {
    fn print(&mut self, text: &str) {
        self.0.push_str(text);
    }
}

/// The value of the body of a function as its Groovy text, or the error; and what it printed.
fn run(body: &str) -> (Result<String, String>, String) {
    let source = format!("def f() {{\n{body}\n}}");
    let parsed = parse_script(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
    let mut output = Output::default();
    let mut interpreter = Interpreter::new(&mut output);
    let result = interpreter
        .call_function(&module, "f", Vec::new())
        .map(|value| value.to_string())
        .map_err(|error| error.to_string());
    (result, output.0)
}

fn value(body: &str) -> String {
    run(body).0.expect("evaluates")
}

#[test]
fn elements() {
    assert_eq!(value("\"${1..3}\""), "[1, 2, 3]");
    assert_eq!(
        value("\"${3..1} ${0..<-2} ${1..<1}\""),
        "[3, 2, 1] [0, -1] []"
    );
    assert_eq!(
        value("\"${1.5..3} ${'a'..'c'} ${1L..3}\""),
        "[1.5, 2.5] [a, b, c] [1, 2, 3]"
    );
    assert_eq!(
        value("[(1..3).collect { it * 2 }, (1..3).sum(), (1..3).size()]"),
        "[[2, 4, 6], 6, 3]"
    );
    assert_eq!(
        value("[(1..3) == [1, 2, 3], (1..3) instanceof List, 1 + 1..3]"),
        "[true, true, 2..3]"
    );
}

#[test]
fn text() {
    let (result, output) = run("println(1..3)\n'x' + (1..3)");
    assert_eq!(
        (result.as_deref(), output.as_str()),
        (Ok("x1..3"), "1..3\n")
    );
    assert_eq!(value("\"${[1..3, 'a'..'b']}\""), "[1..3, a..b]");
    assert_eq!(value("(1..<1).toString()"), "1..<1");
}

#[test]
fn bounds_inclusive_ranges() {
    assert_eq!(value("(2147483647..2147483647).size()"), "1");
    assert_eq!(value("(2147483646..2147483647).size()"), "2");
    assert_eq!(value("(-2147483647..Integer.MIN_VALUE).size()"), "2");
    assert_eq!(value("(Long.MAX_VALUE..Long.MAX_VALUE).size()"), "1");
    assert_eq!(value("((Long.MIN_VALUE + 1L)..Long.MIN_VALUE).size()"), "2");
    assert_eq!(value("(Integer.MAX_VALUE..<Integer.MAX_VALUE).size()"), "0");
}

#[test]
fn membership() {
    assert_eq!(
        value("[2 in 1..3, 2L in 1..3, 2.5 in 1..3, 4 in 1..3]"),
        "[true, false, false, false]"
    );
    assert_eq!(
        value("[2.5 in (1.5..3), 2.0 in (1.5..3), 'b' in 'a'..'c']"),
        "[true, false, true]"
    );
}

#[test]
fn indexing_with_ranges() {
    assert_eq!(value("[10, 20, 30][2..0]"), "[30, 20, 10]");
    assert_eq!(
        value("[[10, 20, 30][0..<0], [10, 20, 30][-2..-1], [10, 20, 30][1..<3]]"),
        "[[], [20, 30], [20, 30]]"
    );
    let error = run("[10, 20, 30][0..5]").0.unwrap_err();
    assert_eq!(error, "IndexOutOfBoundsException: toIndex = 6");
    assert_eq!(value("['abcd'[3..1], 'abcd'[1..2]]"), "[dcb, bc]");
    assert_eq!(value("tuple(1, 2, 3)[0..1]"), "[1, 2]");
    assert_eq!(
        run("(1..3).add(4)").0.unwrap_err(),
        "UnsupportedOperationException: "
    );
}

/// A range with a null end fails with Groovy's message, as Nextflow 26.04.6 shows it for
/// `1..params.repetitions` without the param.
#[test]
fn null_ends() {
    assert_eq!(
        run("1..null").0.unwrap_err(),
        "IllegalArgumentException: Must specify a non-null value for the 'to' index in a Range"
    );
    assert_eq!(
        run("null..1").0.unwrap_err(),
        "IllegalArgumentException: Must specify a non-null value for the 'from' index in a Range"
    );
}
