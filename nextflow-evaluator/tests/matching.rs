//! Checks `=~`, `==~`, patterns and matchers against Nextflow 26.04.6 (Groovy spec, section 8.6).
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
fn matchers() {
    assert_eq!(
        value("('sample_R1_001.fastq.gz' =~ /(.+)_R(\\d)_/)[0]"),
        "[sample_R1_, sample, 1]"
    );
    assert_eq!(value("('a1b2' =~ /\\d/)[1]"), "2");
    assert_eq!(value("('a1b2' =~ /\\d/)[-1]"), "2");
    assert_eq!(
        value("def m = 'a1b2' =~ /\\d/\n[m.count, m.size(), m.matches()]"),
        "[2, 2, false]"
    );
    assert_eq!(value("if( null =~ /nu/ ) { 'yes' } else { 'no' }"), "yes");
    assert_eq!(value("if( 'abc' =~ /x/ ) { 'yes' } else { 'no' }"), "no");
    assert_eq!(value("('a' == 'a' =~ /true/) as Boolean"), "true");
    let error = eval("('a1b2' =~ /\\d/)[5]").unwrap_err();
    assert_eq!(
        error,
        "IndexOutOfBoundsException: index is out of range -2..1 (index = 5)"
    );
}

#[test]
fn matchers_iterate_their_matches() {
    assert_eq!(
        value(
            "[('-o out.fa --x' =~ /-o(?:utput)?\\s(.*)\\s?/).findAll(), \
             (null =~ /-o\\s(.*)/).findAll(), ('a1b22' =~ /\\d+/).findAll(), \
             ('a1b22' =~ /(\\d)(x)?/).collect { m -> m }, \
             ('a1b22' =~ /\\d+/).findAll { m -> m.size() > 1 }]"
        ),
        "[[[-o out.fa --x, out.fa --x]], [], [1, 22], [[1, 1, null], [2, 2, null], [2, 2, null]], [22]]"
    );
}

#[test]
fn full_matches_and_patterns() {
    assert_eq!(
        value("['ABC' ==~ /(?i)abc/, 'abc' ==~ /b/, null ==~ /x/]"),
        "[true, false, false]"
    );
    assert_eq!(value("\"${~/a.c/}\""), "a.c");
    assert_eq!(
        value("['abc' in ~/a.c/, 'abcd' in ~/a.c/]"),
        "[true, false]"
    );
    assert_eq!(value("'file.fastq.gz' - ~/\\.gz$/"), "file.fastq");
    assert_eq!(
        value("'sample_R1.fq'.replaceAll(~/_R(\\d)/, '-$1')"),
        "sample-1.fq"
    );
    assert_eq!(value("(~/b/).matcher('abcb').count"), "2");
}
