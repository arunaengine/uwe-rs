//! Checks map methods against Nextflow 26.04.6 (Groovy spec, sections 9.3 and 10.5).
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
fn replacing_values() {
    assert_eq!(
        value("def m = [a: '']\n[m.replace('a', 'P'), m.replace('b', 1), m]"),
        "[, null, [a:P]]"
    );
}

#[test]
fn maps_with_defaults() {
    assert_eq!(
        value(
            "def flags = [:].withDefault { [hasNo: false] }\n\
             flags['a'].hasNo = true\n\
             flags.b\n\
             def counts = [x: 1].withDefault { k -> k.size() }\n\
             [flags, counts.get('abc'), counts['zz'], counts.containsKey('q'), counts]"
        ),
        "[[a:[hasNo:true], b:[hasNo:false]], 3, 2, false, [x:1, abc:3, zz:2]]"
    );
}

#[test]
fn binds_map_closures() {
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
    assert_eq!(
        value("[a: 1, b: 2].collect { entry -> [entry['key'], entry['value']] }"),
        "[[a, 1], [b, 2]]"
    );
    assert_eq!(
        value("[a: 1, b: 2, c: 3].findResults { k, v -> v > 1 ? [k, v] : null }"),
        "[[b, 2], [c, 3]]"
    );
    // Legacy code copies a map through its class.
    assert_eq!(
        value(
            "def m = [a: 1, b: 2]\ndef c = m.getClass().newInstance(m)\nc.remove('a')\n\
             [m, c, c.getClass().name]"
        ),
        "[[a:1, b:2], [b:2], java.util.LinkedHashMap]"
    );
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

/// Groovy's `ConfigSlurper`, as nf-core/hicar reads `conf/modules.config` in `lib/`; Groovy 4
/// gives `[[:]/foo, --quiet, [:], [:]]`.
#[test]
fn config_slurper() {
    let text = "process {\n    publishDir = [ path: { \"${params.outdir}/x\" }, mode: params.publish_dir_mode ]\n    \
                time = { check_max(1.h, 'time') }\n    \
                withName: 'FOO' {\n        publishDir = [ path: { \"${params.outdir}/foo\" } ]\n        \
                ext.args = '--quiet'\n    }\n}\n";
    let body = format!(
        "def c = new ConfigSlurper().parse('''{text}''')\ndef prop = c.process['FOO']\n\
         [prop.publishDir.path(), prop.ext.args, c.process['BAR'], c.process.publishDir.mode.toString()]"
    );
    assert_eq!(value(&body), "[[:]/foo, --quiet, [:], [:]]");
}

/// Legacy syntax: the safe index `m?[key]`, as in sarek's `controlfreec` module; Nextflow
/// 26.04.6 gives `[yes, no, [b:1]]`.
#[test]
fn safe_index() {
    assert_eq!(
        value(
            "def m = [a: [b: 1]]\ndef n = null\n\
             [m?[\"a\"]?[\"b\"] ? 'yes' : 'no', n?[\"a\"]?[\"b\"] ? 'yes' : 'no', m?['a']]"
        ),
        "[yes, no, [b:1]]"
    );
}

/// Sorting, extremes, searches and conditional puts of maps, as Nextflow 26.04.6 prints them.
#[test]
fn searches_and_sorts() {
    let map = "def m = [b: 2, a: 1, c: 3]\n";
    let cases = [
        (
            "[m.sort { it.value }, m]",
            "[[a:1, b:2, c:3], [b:2, a:1, c:3]]",
        ),
        ("m.sort { x, y -> y.key <=> x.key }", "[c:3, b:2, a:1]"),
        ("[m.max { it.value }, m.min { it.value }]", "[c=3, a=1]"),
        ("m.max { x, y -> x.key <=> y.key }", "c=3"),
        ("m.findResult { k, v -> v > 1 ? k : null }", "b"),
        ("m.findResult('none') { k, v -> null }", "none"),
        ("m.takeWhile { k, v -> v > 1 }", "[b:2]"),
        ("m.dropWhile { k, v -> v > 1 }", "[a:1, c:3]"),
        ("m.takeWhile { e -> e.key != 'c' }", "[b:2, a:1]"),
    ];
    for (code, expected) in cases {
        assert_eq!(value(&format!("{map}{code}")), expected, "{code}");
    }
    assert_eq!(
        value(
            "def n = [a: 1]\n[n.computeIfAbsent('b') { k -> k + 'x' }, n.computeIfAbsent('a') \
             { k -> 9 }, n.putIfAbsent('a', 5), n.putIfAbsent('c', 5), n]"
        ),
        "[bx, 1, 1, null, [a:1, b:bx, c:5]]"
    );
}
