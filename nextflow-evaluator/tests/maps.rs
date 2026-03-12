//! Checks map methods against Nextflow 26.04.6 (Groovy spec, sections 9.3 and 10.5).
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
fn closures_get_keys_and_values_or_entries() {
    assert_eq!(value("[a: 1, b: 2].findAll { k, v -> v > 1 }"), "[b:2]");
    assert_eq!(value("[a: 1].find { it.value == 1 }"), "a=1");
    assert_eq!(value("[a: 1].find { it.value == 1 }.key"), "a");
    assert_eq!(value("[a: 1].collect { e -> e.key }"), "[a]");
    assert_eq!(
        value("[a: 1, b: 2].collectEntries { k, v -> [v, k] }"),
        "[1:a, 2:b]"
    );
    assert_eq!(
        value("[a: 1, b: 2].groupBy { k, v -> v % 2 }"),
        "[1:[a:1], 0:[b:2]]"
    );
    assert_eq!(value("[a: 1, b: 2].inject(0) { s, k, v -> s + v }"), "3");
    assert_eq!(value("[a: 1, b: 2].count { k, v -> v > 1 }"), "1");
    assert_eq!(
        value("[[a: 1, b: 2].any { k, v -> v > 1 }, [a: 1, b: 2].every { k, v -> v > 1 }]"),
        "[true, false]"
    );
    assert_eq!(
        value("def out = []\n[a: 1, b: 2].each { k, v -> out << k + v }\nout"),
        "[a1, b2]"
    );
}

#[test]
fn reading_and_changing() {
    assert_eq!(value("def m = [a: 1]\nm.get('b', 5)\nm"), "[a:1, b:5]");
    assert_eq!(
        value("[[a: 1].getOrDefault('x', 0), [a: 1].containsKey('a'), [a: 2].containsValue(2L)]"),
        "[0, true, false]"
    );
    assert_eq!(
        value("[[a: 1].keySet(), [a: 1].values(), [a: 1].entrySet()]"),
        "[[a], [1], [a=1]]"
    );
    assert_eq!(value("[b: 1, a: 2].sort()"), "[a:2, b:1]");
    assert_eq!(value("[a: 1, b: 2].subMap(['b', 'x'])"), "[b:2]");
    assert_eq!(
        value("def m = [a: 1]\nm.put('b', 2)\nm.remove('a')\nm"),
        "[b:2]"
    );
    assert_eq!(
        value("def m = [a: 1]\ndef c = m.clone()\nc.b = 2\n[m, c]"),
        "[[a:1], [a:1, b:2]]"
    );
}

#[test]
fn to_map_copies() {
    assert_eq!(
        value("def m = [a: 1]\ndef c = m.toMap()\nc.b = 2\n[m, c]"),
        "[[a:1], [a:1, b:2]]"
    );
}
