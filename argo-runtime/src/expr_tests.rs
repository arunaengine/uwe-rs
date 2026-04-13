//! Tests for the expr-lang evaluator, with expressions from the Argo corpus.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use super::{evaluate, evaluate_bool, identifiers};
use crate::tags::Variables;
use crate::value::Value;

fn environment(pairs: &[(&str, &str)]) -> Variables {
    Variables::new(
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
    )
}

fn run(source: &str, variables: &Variables) -> String {
    evaluate(source, variables.environment())
        .unwrap_or_else(|error| panic!("`{source}` fails: {error}"))
        .to_go_string()
}

#[test]
fn arithmetic_follows_expr() {
    let empty = BTreeMap::new();
    let value = |source: &str| evaluate(source, &empty).expect("evaluates");
    assert_eq!(value("1 + 2 * 3"), Value::Int(7));
    assert_eq!(value("7 / 2"), Value::Float(3.5));
    assert_eq!(value("7 % 3"), Value::Int(1));
    assert_eq!(value("2 ** 3"), Value::Float(8.0));
    assert_eq!(value("-3 + 1"), Value::Int(-2));
    assert_eq!(
        value("1..3"),
        Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
    assert_eq!(value("'a' + \"b\""), Value::string("ab"));
    assert_eq!(value("1 == 1.0"), Value::Bool(true));
    assert_eq!(value("true ? 'x' : 'y'"), Value::string("x"));
    assert_eq!(value("nil ?? 'd'"), Value::string("d"));
    assert_eq!(
        value("'abc' contains 'b' and 'abc' startsWith 'a'"),
        Value::Bool(true)
    );
    assert_eq!(value("2 not in [1, 3]"), Value::Bool(true));
    assert_eq!(value("'x-1' matches '^x-[0-9]$'"), Value::Bool(true));
    assert_eq!(value("let x = 2; x * x"), Value::Int(4));
    assert!(evaluate("1 && true", &empty).is_err());
}

#[test]
fn reads_argo_variables() {
    let variables = environment(&[
        ("inputs.parameters.input-bam", "/data/run/sample.bam"),
        ("inputs.parameters.thread-count", "8"),
        ("workflow.parameters.numbers", "64"),
        (
            "inputs.parameters.run-data",
            r#"{"samples": [{"sampleId": "S1"}], "dataType": "fastq"}"#,
        ),
    ]);
    assert_eq!(
        run("sprig.osBase(inputs.parameters['input-bam'])", &variables),
        "sample.bam"
    );
    assert_eq!(
        run("sprig.osDir(inputs.parameters['input-bam'])", &variables),
        "/data/run"
    );
    assert_eq!(
        run(
            "sprig.int(inputs.parameters['thread-count']) - 2",
            &variables
        ),
        "6"
    );
    assert_eq!(
        run("asInt(workflow.parameters.numbers)/16", &variables),
        "4"
    );
    assert_eq!(
        run(
            "jsonpath(inputs.parameters['run-data'], '$.samples[0].sampleId')",
            &variables
        ),
        "S1"
    );
    assert_eq!(
        run(
            "sprig.trimSuffix(sprig.ext(inputs.parameters['input-bam']), sprig.base(inputs.parameters['input-bam']))",
            &variables
        ),
        "sample"
    );
    assert_eq!(
        run("toJson(filter([1, 3], {# > 1}))", &environment(&[])),
        "[3]"
    );
    assert_eq!(
        run("map([{'n': 'a'}, {'n': 'b'}], {.n})", &environment(&[])),
        "[a b]"
    );
    assert!(
        evaluate_bool(
            "jsonpath(inputs.parameters['run-data'], '$.dataType') == 'fastq'",
            variables.environment()
        )
        .expect("a boolean")
    );
}

#[test]
fn unknown_names_fail_and_missing_keys_are_nil() {
    let variables = environment(&[("inputs.parameters.a", "1")]);
    assert!(evaluate("tasks.x.outputs.result", variables.environment()).is_err());
    assert_eq!(
        evaluate("inputs.parameters.b", variables.environment()),
        Ok(Value::Nil)
    );
}

#[test]
fn lists_identifiers() {
    assert_eq!(identifiers("sprig.int(retries) + 1"), ["sprig", "retries"]);
}
