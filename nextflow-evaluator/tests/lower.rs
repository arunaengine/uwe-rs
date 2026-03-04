//! Checks that lowering resolves names and reports the load-time errors of `nextflow lint`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::lowered::{ExpressionKind, Module, Place, StatementKind};
use nextflow_parser::{LineIndex, parse_script};

fn lower(source: &str) -> Result<Module, Vec<String>> {
    let parsed = parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let lines = LineIndex::new(source);
    lower_module(&parsed.tree, source).map_err(|errors| {
        errors
            .iter()
            .map(|error| lines.locate(error).to_string())
            .collect()
    })
}

fn errors(source: &str) -> Vec<String> {
    lower(source).expect_err("lowering fails")
}

#[test]
fn undefined_names() {
    assert_eq!(errors("def f() { x }"), ["1:11: `x` is not defined"]);
    assert_eq!(errors("def f() { g(1) }"), ["1:11: `g` is not defined"]);
    assert_eq!(errors("def f(c) { c() }"), ["1:12: `c` is not defined"]);
    assert!(lower("def f() { g() }\ndef g() { 1 }").is_ok());
    assert!(lower("def f() { println(params.x + workflow.runName) }").is_ok());
    assert!(lower("def f() { Math.max(1, 2) + Integer.MAX_VALUE }").is_ok());
}

#[test]
fn declarations_and_assignments() {
    let source = "def f(a) {\n    def a = 1\n}";
    assert_eq!(errors(source), ["2:9: `a` is already declared"]);
    let source = "def f() {\n    def x = 1\n    def c = { def x = 2 }\n}";
    assert_eq!(errors(source), ["3:19: `x` is already declared"]);
    assert_eq!(
        errors("def f() { y = 1 }"),
        ["1:11: `y` was assigned but not declared"]
    );
    let source = "def f() { workflow = 1 }";
    assert_eq!(
        errors(source),
        ["1:11: Built-in constant or namespace cannot be re-assigned"]
    );
    // Sibling blocks may declare the same name.
    assert!(lower("def f(c) { if (c) { def x = 1 } else { def x = 2 } }").is_ok());
}

#[test]
fn closures_capture_enclosing_variables() {
    let source = "def f() {\n    def a = 1\n    def b = 2\n    return { x -> { b + a + x } }\n}";
    let module = lower(source).expect("lowers");
    let function = &module.functions[0];
    let StatementKind::Return(Some(outer)) = &function.body[2].kind else {
        panic!("return statement")
    };
    let ExpressionKind::Closure(outer) = &outer.kind else {
        panic!("closure")
    };
    assert_eq!(outer.captures, [Place::Local(1), Place::Local(0)]);
    let StatementKind::Expression(inner) = &outer.body[0].kind else {
        panic!("statement")
    };
    let ExpressionKind::Closure(inner) = &inner.kind else {
        panic!("closure")
    };
    assert_eq!(
        inner.captures,
        [Place::Captured(0), Place::Captured(1), Place::Local(0)]
    );
    assert!(inner.implicit_parameter);
}

#[test]
fn nested_gstrings_stay_gstrings() {
    let module = lower("def f(v) { \"a${ \"b${v}\" }\" }").expect("lowers");
    let StatementKind::Expression(outer) = &module.functions[0].body[0].kind else {
        panic!("expression statement")
    };
    let ExpressionKind::Interpolate {
        parts,
        keep_gstring,
        ..
    } = &outer.kind
    else {
        panic!("GString")
    };
    assert!(!keep_gstring);
    let nextflow_evaluator::lowered::Part::Value(inner) = &parts[1] else {
        panic!("value")
    };
    assert!(matches!(
        inner.kind,
        ExpressionKind::Interpolate {
            keep_gstring: true,
            ..
        }
    ));
}

#[test]
fn unsupported_items_fail_loudly() {
    assert_eq!(
        errors("workflow {\n    main:\n    println(1)\n}"),
        ["1:1: This script item is not supported yet"]
    );
}
