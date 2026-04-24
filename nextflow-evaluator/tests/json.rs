//! Groovy's JSON classes against Nextflow 26.04.6.
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

const VALUES: &str = "def m = [a: 1, b: 2.5, c: 'x\"y\\n/', d: null, e: [1, 'two', true], \
     f: [g: 1.0d, h: [:], i: []], h: 3L, l: 1.5f, u: 'ü\\t\\u0001', v: 1e20d, \
     w: 12345678901234567890, x: 1.0]\n";

#[test]
fn to_json_and_pretty_print() {
    assert_eq!(
        eval(&format!("{VALUES}groovy.json.JsonOutput.toJson(m)")).unwrap(),
        r#"{"a":1,"b":2.5,"c":"x\"y\n/","d":null,"e":[1,"two",true],"f":{"g":1.0,"h":{},"i":[]},"h":3,"l":1.5,"u":"\u00fc\t\u0001","v":1.0E20,"w":12345678901234567890,"x":1.0}"#
    );
    assert_eq!(
        eval("groovy.json.JsonOutput.prettyPrint('[1,{\"a\":[],\"b\":\"x\"}]')").unwrap(),
        "[\n    1,\n    {\n        \"a\": [\n            \n        ],\n        \"b\": \"x\"\n    }\n]"
    );
}

#[test]
fn generators_with_converters() {
    let body = "def gen = new groovy.json.JsonGenerator.Options().excludeNulls()\
                .addConverter(Path) { Path p -> p.toUriString() }\
                .addConverter(Duration) { Duration d -> d.toMillis() }\
                .addConverter(MemoryUnit) { MemoryUnit x -> x.toBytes() }.build()\n\
                gen.toJson([i: file('/tmp/x.txt'), j: 2.GB, k: 1.h, n: null])";
    assert_eq!(
        eval(body).unwrap(),
        r#"{"i":"/tmp/x.txt","j":2147483648,"k":3600000}"#
    );
}

#[test]
fn slurper() {
    let body = "def parsed = new groovy.json.JsonSlurper().parseText('{\"a\": [1, 2.5, \"x\", null, true], \
                \"b\": {\"c\": 1e3, \"d\": 12345678901234567890, \"e\": -4}}')\n\
                [parsed, parsed.a.collect { it?.getClass()?.simpleName }, \
                parsed.b.collect { k, v -> v.getClass().simpleName }]";
    assert_eq!(
        eval(body).unwrap(),
        "[[a:[1, 2.5, x, null, true], b:[c:1E+3, d:-6101065172474983726, e:-4]], \
         [Integer, BigDecimal, String, null, Boolean], [BigDecimal, Long, Integer]]"
    );
}

#[test]
fn builder() {
    let body = "def b = new groovy.json.JsonBuilder([a: 1, b: [2, 'x']])\n\
                [b.toString(), b.toPrettyString()]";
    assert_eq!(
        eval(body).unwrap(),
        "[{\"a\":1,\"b\":[2,\"x\"]}, {\n    \"a\": 1,\n    \"b\": [\n        2,\n        \"x\"\n    ]\n}]"
    );
}
