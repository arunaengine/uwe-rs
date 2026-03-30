//! Tests of expressions: parameter references, interpolation, escapes and JavaScript.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

fn inputs() -> Value {
    Value::parse(
        "{file: {class: File, basename: a.txt, path: /in/a.txt, size: 3}, \
          list: [1, 2, 3], record: {b: 2, a: [x]}, 'odd key': yes, missing: null}",
    )
    .expect("YAML")
}

fn eval(evaluator: &Evaluator, text: &str) -> Result<Value, String> {
    let expression = Expression::parse(text, false).expect("an expression");
    let inputs = inputs();
    let runtime = Value::parse("{outdir: /out, cores: 2}").expect("YAML");
    let this = Value::string("me");
    evaluator.evaluate(
        &expression,
        Variables {
            inputs: &inputs,
            this: &this,
            runtime: &runtime,
        },
    )
}

#[test]
fn evaluates_parameter_references_without_javascript() {
    let plain = Evaluator::default();
    assert_eq!(eval(&plain, "$(inputs.file.size)"), Ok(Value::Integer(3)));
    assert_eq!(eval(&plain, "$(inputs.list.length)"), Ok(Value::Integer(3)));
    assert_eq!(
        eval(&plain, "$(inputs['odd key'])"),
        Ok(Value::string("yes"))
    );
    assert_eq!(eval(&plain, "$(inputs.list[1])"), Ok(Value::Integer(2)));
    assert_eq!(eval(&plain, "$(self)"), Ok(Value::string("me")));
    assert_eq!(eval(&plain, "$(null)"), Ok(Value::Null));
    assert!(eval(&plain, "$(inputs.missing.basename)").is_err());
    assert!(eval(&plain, "$(inputs.nothing)").is_err());
    assert_eq!(
        eval(&plain, "$(inputs.list.length + 1)"),
        Ok(Value::Integer(4))
    );
}

#[test]
fn interpolates_values_as_sorted_json() {
    let plain = Evaluator::default();
    assert_eq!(
        eval(&plain, "$(runtime.outdir)/$(inputs.file.basename)"),
        Ok(Value::string("/out/a.txt"))
    );
    assert_eq!(
        eval(&plain, "r=$(inputs.record) n=$(inputs.missing)"),
        Ok(Value::string("r={\"a\": [\"x\"], \"b\": 2} n=null"))
    );
    assert_eq!(
        eval(&plain, "  $(inputs.list)\n"),
        Ok(Value::parse("[1, 2, 3]").expect("a list"))
    );
    assert_eq!(
        eval(&plain, "\\$(inputs.list)"),
        Ok(Value::string("$(inputs.list)"))
    );
    assert_eq!(
        eval(&plain, "plain text "),
        Ok(Value::string("plain text "))
    );
}

#[test]
fn falls_back_to_javascript_when_it_is_enabled() {
    let javascript = Evaluator::with_javascript(vec!["function f(x) { return x + 1; }".into()]);
    assert_eq!(
        eval(&javascript, "$(f(inputs.list.length))"),
        Ok(Value::Integer(4))
    );
    assert_eq!(eval(&javascript, "$(inputs.nothing)"), Ok(Value::Null));
    assert_eq!(
        eval(
            &javascript,
            "${ var n = 0; for (var i in inputs.list) { n += inputs.list[i]; } return n; }"
        ),
        Ok(Value::Integer(6))
    );
    assert_eq!(
        eval(&javascript, "-t $(runtime.cores * 2)"),
        Ok(Value::string("-t 4"))
    );
    let error = eval(&Evaluator::default(), "$(f(1))").expect_err("no expression library");
    assert!(error.contains("InlineJavascriptRequirement"), "{error}");
    assert!(eval(&Evaluator::default(), "$(inputs.nothing)").is_err());
}

#[test]
fn keeps_white_space_around_dirent_entries() {
    let plain = Evaluator::default();
    let expression = Expression::parse("$(inputs.list)\n", false).expect("an expression");
    let inputs = inputs();
    let variables = Variables {
        inputs: &inputs,
        this: &Value::Null,
        runtime: &Value::Null,
    };
    assert_eq!(
        plain.evaluate_untrimmed(&expression, variables),
        Ok(Value::string("[1, 2, 3]\n"))
    );
    assert_eq!(
        plain.evaluate(&expression, variables),
        Ok(Value::parse("[1, 2, 3]").expect("a list"))
    );
}

#[test]
fn keeps_shell_variables_without_the_javascript_requirement() {
    let plain = Evaluator::default();
    assert_eq!(
        eval(&plain, "OUT=${HOME}/out; echo ${@:2}"),
        Ok(Value::string("OUT=${HOME}/out; echo ${@:2}"))
    );
    assert_eq!(eval(&plain, "$(runtime.cores * 2)"), Ok(Value::Integer(4)));
    let javascript = Evaluator::with_javascript(Vec::new());
    assert!(eval(&javascript, "${HOME}").is_err());
}
