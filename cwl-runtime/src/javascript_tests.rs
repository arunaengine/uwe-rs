//! Tests of the JavaScript sandbox: globals, the expression library, strict mode and results.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

fn run(library: &[&str], body: &str) -> Result<Value, String> {
    let inputs = Value::parse("{n: 3, reads: [{basename: a.fastq.gz}], s: 'a,b'}").expect("YAML");
    let library: Vec<String> = library.iter().map(|part| (*part).to_owned()).collect();
    evaluate(
        &library,
        &[
            ("inputs", &inputs),
            ("self", &Value::Null),
            ("runtime", &Value::Null),
        ],
        body,
    )
}

#[test]
fn evaluates_expressions_on_the_inputs() {
    let value = run(&[], "return inputs.s.split(',').concat([inputs.n * 2]);").expect("runs");
    assert_eq!(value.to_json(), "[\"a\",\"b\",6]");
    let value = run(
        &[],
        "return inputs.reads.map(f => f.basename.replace(/\\.gz$/, ''));",
    )
    .expect("runs");
    assert_eq!(value.to_json(), "[\"a.fastq\"]");
    assert_eq!(
        run(&[], "return 'abc'.substr(1, 1);").expect("runs"),
        Value::string("b")
    );
}

#[test]
fn defines_the_expression_library_first() {
    let library = ["function twice(x) { return 2 * x; }", "var base = 10;"];
    let value = run(&library, "return twice(inputs.n) + base;").expect("runs");
    assert_eq!(value, Value::Integer(16));
}

#[test]
fn turns_undefined_into_null_and_keeps_big_numbers_as_floats() {
    assert_eq!(run(&[], "return undefined;").expect("runs"), Value::Null);
    assert_eq!(
        run(&[], "return inputs.missing;").expect("runs"),
        Value::Null
    );
    assert_eq!(run(&[], "return 1e21;").expect("runs"), Value::Float(1e21));
    assert_eq!(
        run(&[], "return 0.5 + 1;").expect("runs"),
        Value::Float(1.5)
    );
}

#[test]
fn runs_in_sloppy_mode_and_reports_errors() {
    assert_eq!(
        run(&[], "undeclared = 1; return undeclared;").expect("runs"),
        Value::Integer(1)
    );
    let error = run(&[], "return null.x;").expect_err("a TypeError");
    assert!(error.starts_with("TypeError: "), "{error}");
    assert!(
        !error.contains(" at "),
        "no place in the generated code: {error}"
    );
    let error = run(&[], "return HOME;").expect_err("a ReferenceError");
    assert_eq!(error, "ReferenceError: HOME is not defined");
}

#[test]
fn drops_console_output() {
    let value = run(&[], "console.log('debug'); return inputs.n;").expect("runs");
    assert_eq!(value, Value::Integer(3));
}

#[test]
fn total_vm_budget_stops_many_individually_short_loops() {
    let mut context = Context::default();
    context.runtime_limits_mut().set_loop_iteration_limit(100);
    let error = bounded_eval(
        "function part() { for (var inner = 0; inner < 90; inner++) {} } \
         for (var outer = 0; outer < 90; outer++) part();",
        &mut context,
        20,
        Instant::now() + Duration::from_secs(60),
    )
    .expect_err("the whole evaluation exceeds its budget");
    assert!(error.contains("total VM work budget"), "{error}");
}

#[test]
fn vm_budget_stops_infinite_work_and_later_evaluations_still_succeed() {
    let mut context = Context::default();
    let error = bounded_eval(
        "while (true) {}",
        &mut context,
        4,
        Instant::now() + Duration::from_secs(60),
    )
    .expect_err("infinite work ends");
    assert!(error.contains("total VM work budget"), "{error}");
    drop(context);
    assert_eq!(
        run(&[], "return inputs.n + 1;").expect("fresh evaluation"),
        Value::Integer(4)
    );
}

#[test]
fn vm_slices_preserve_bounded_ordinary_loop_results() {
    let mut context = Context::default();
    let result = bounded_eval(
        "var total = 0; for (var i = 0; i < 1000; i++) total += i; total",
        &mut context,
        10_000,
        Instant::now() + Duration::from_secs(60),
    )
    .expect("bounded loop");
    assert_eq!(result.as_number(), Some(499_500.0));
}

#[test]
fn cooperative_deadline_stops_execution_between_vm_slices() {
    let mut context = Context::default();
    let error = bounded_eval(
        "while (true) {}",
        &mut context,
        u64::MAX,
        Instant::now() + Duration::from_millis(1),
    )
    .expect_err("deadline ends execution");
    assert!(error.contains("time deadline"), "{error}");
}

#[test]
fn expired_deadlines_do_not_start_the_vm() {
    let mut context = Context::default();
    let error = bounded_eval(
        "throw new Error('VM started')",
        &mut context,
        100,
        Instant::now() - Duration::from_secs(1),
    )
    .expect_err("expired deadline");
    assert!(error.contains("time deadline"), "{error}");
    assert!(!error.contains("VM started"), "{error}");
}
