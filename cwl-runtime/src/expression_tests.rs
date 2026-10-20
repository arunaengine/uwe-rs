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
fn evaluates_parameter_references() {
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
    let error = eval(&plain, "$(inputs.nothing)").expect_err("no input");
    assert!(error.contains("there is no input `nothing`"), "{error}");
    let error = eval(&plain, "$(inputs.record.c)").expect_err("no key");
    assert!(error.contains("`inputs.record` has no `c`"), "{error}");
}

#[test]
fn interpolates_sorted_json() {
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
fn uses_enabled_javascript() {
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
fn preserves_dirent_whitespace() {
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
fn rejects_code_without() {
    let plain = Evaluator::default();
    for text in [
        "OUT=${HOME}/out",
        "$(runtime.cores * 2)",
        "$(1)",
        "$(inputs.nothing)",
    ] {
        let error = eval(&plain, text).expect_err(text);
        assert!(error.contains("InlineJavascriptRequirement"), "{error}");
    }
    assert_eq!(eval(&plain, "\\${HOME}"), Ok(Value::string("${HOME}")));
    let javascript = Evaluator::with_javascript(Vec::new());
    assert!(eval(&javascript, "${HOME}").is_err());
}
