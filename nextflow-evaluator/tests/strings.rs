//! Checks String methods against Nextflow 26.04.6 (Groovy spec, sections 8.6, 8.9 and 10.1).
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

/// The value of an expression, as its Groovy text, or the error.
fn eval(expression: &str) -> Result<String, String> {
    let source = format!("def f() {{ {expression} }}");
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

fn value(expression: &str) -> String {
    eval(expression).expect("evaluates")
}

#[test]
fn splitting_and_tokens() {
    assert_eq!(value("'a,b,,c'.tokenize(',')"), "[a, b, c]");
    assert_eq!(value("'  a  b '.tokenize()"), "[a, b]");
    assert_eq!(value("'x_y_z'.split('_')"), "[x, y, z]");
    assert_eq!(value("'a.b.c'.split('.')"), "[]");
    assert_eq!(value("''.split(',')"), "[]");
    assert_eq!(value("'a,b,,'.split(',', -1)"), "[a, b, , ]");
    assert_eq!(value("' a  b\\tc '.split()"), "[a, b, c]");
    assert_eq!(
        value("' a '.split().getClass().getSimpleName()"),
        "String[]"
    );
    assert_eq!(value("'a\\nb\\r\\nc\\n'.readLines()"), "[a, b, c]");
    assert_eq!(value("'abc'.toList()"), "[a, b, c]");
}

#[test]
fn searching_and_slicing() {
    assert_eq!(value("'file.fastq.gz'.endsWith('.gz')"), "true");
    assert_eq!(value("'file.fastq.gz'.startsWith('file')"), "true");
    assert_eq!(value("'abc'.contains('bc')"), "true");
    assert_eq!(
        value("['abcabc'.indexOf('c'), 'abcabc'.lastIndexOf('c'), 'abcabc'.indexOf('c', 3)]"),
        "[2, 5, 5]"
    );
    assert_eq!(value("'abc'.indexOf('x')"), "-1");
    assert_eq!(
        value("['abc'.substring(1), 'abc'.substring(1, 2)]"),
        "[bc, b]"
    );
    let error = eval("'abc'.substring(5)").unwrap_err();
    assert_eq!(
        error,
        "StringIndexOutOfBoundsException: begin 5, end 3, length 3"
    );
    assert_eq!(
        value("['abc'.size(), 'abc'.length(), ''.empty, 'a'.empty]"),
        "[3, 3, true, false]"
    );
    assert_eq!(value("'aaa'.count('aa')"), "2");
    assert_eq!(value("'sample_R1_001.fastq.gz'.find(/_R\\d_/)"), "_R1_");
}

#[test]
fn replacing() {
    assert_eq!(
        value("'sample_R1.fq'.replaceAll(/_R(\\d)/, '-$1')"),
        "sample-1.fq"
    );
    assert_eq!(value("'a.b.c'.replace('.', '_')"), "a_b_c");
    assert_eq!(value("'aXbX'.replaceFirst('X', '-')"), "a-bX");
    assert_eq!(
        value("'a1b2'.replaceAll(/\\d/) { d -> \"<${d}>\" }"),
        "a<1>b<2>"
    );
    assert_eq!(
        value("'k=v'.replaceAll(/(\\w)=(\\w)/) { all, k, v -> v + k }"),
        "vk"
    );
    assert_eq!(value("'ab'.matches('a.')"), "true");
    assert_eq!(value("'ab'.matches('a')"), "false");
}

#[test]
fn conversions() {
    assert_eq!(value("' 42 '.toInteger()"), "42");
    assert_eq!(
        eval("'4.0'.toInteger()").unwrap_err(),
        "NumberFormatException: For input string: \"4.0\""
    );
    assert_eq!(
        value("['1.5'.toDouble(), '1e3'.toBigDecimal(), '2'.toLong()]"),
        "[1.5, 1E+3, 2]"
    );
    assert_eq!(
        value("['yes'.toBoolean(), ' Y '.toBoolean(), '1'.toBoolean()]"),
        "[false, true, true]"
    );
    let checks =
        "['NaN'.isDouble(), '1e3'.isDouble(), ' 42'.isInteger(), '99999999999'.isInteger()]";
    assert_eq!(value(checks), "[true, true, true, false]");
}

#[test]
fn case_whitespace_and_order() {
    assert_eq!(value("'  a b \\t'.trim()"), "a b");
    assert_eq!(
        value("'MiXeD'.toLowerCase() + 'MiXeD'.toUpperCase()"),
        "mixedMIXED"
    );
    assert_eq!(value("'hello world'.capitalize()"), "Hello world");
    assert_eq!(value("'abc'.reverse()"), "cba");
    assert_eq!(value("'    a\\n  '.stripIndent() == '  a\\n'"), "true");
    assert_eq!(value("'  a  \\n  b'.stripIndent() == 'a\\nb'"), "true");
    assert_eq!(
        value("['a'.compareTo('c'), 'ab'.compareTo('a')]"),
        "[-2, 1]"
    );
    assert_eq!(
        value("['a'.equals('a'), 'a'.equals(\"${'a'}\"), 'a'.hashCode()]"),
        "[true, true, 97]"
    );
}

#[test]
fn inspect_quotes_and_escapes() {
    assert_eq!(
        value("[[a: \"x\\ty\", 1: [2, null, true]].inspect(), \"it's\".inspect()]"),
        "[['a':'x\\ty', 1:[2, null, true]], 'it\\'s']"
    );
}

#[test]
fn simple_template_engine() {
    assert_eq!(
        value(
            "new groovy.text.SimpleTemplateEngine().createTemplate('Hi ${name}, $workflow.manifest.name v${x + 1} \\\\$HOME end.')\
             .make([name: 'you', workflow: [manifest: [name: 'demo']], x: 1]).toString()"
        ),
        "Hi you, demo v2 $HOME end."
    );
}
