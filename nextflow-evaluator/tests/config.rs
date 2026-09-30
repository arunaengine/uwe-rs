//! Config values: the names they may use, and names resolved when a task runs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::lower::lower_config_value;
use nextflow_evaluator::lowered::{Code, Module};
use nextflow_evaluator::value::{MapKind, Value};
use nextflow_parser::ast::ConfigStatement;
use nextflow_parser::{LineIndex, parse_config};

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}

    fn environment(&self, name: &str) -> Option<String> {
        (name == "HOME").then(|| "/home/me".to_owned())
    }
}

/// The value of `x = <value>`, lowered as in the `process` scope or not.
fn lower(value: &str, process_scope: bool) -> Result<Rc<Code>, Vec<String>> {
    let source = format!("x = {value}");
    let parsed = parse_config(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let Some(ConfigStatement::Assign { value, .. }) = parsed.tree.statements.first() else {
        panic!("an assignment")
    };
    let lines = LineIndex::new(&source);
    lower_config_value(value, &source, process_scope)
        .map(Rc::new)
        .map_err(|errors| {
            errors
                .iter()
                .map(|error| lines.locate(error).to_string())
                .collect()
        })
}

fn evaluate(code: &Rc<Code>, interpreter: &mut Interpreter) -> String {
    let module = Rc::new(Module::default());
    match interpreter.evaluate(code, &module) {
        Ok(value) => value.to_string(),
        Err(error) => error.to_string(),
    }
}

#[test]
fn names_outside_closures() {
    assert!(lower("\"${params.outdir}/x\"", false).is_ok());
    assert!(lower("projectDir", false).is_ok());
    assert_eq!(
        lower("workflow", false).unwrap_err(),
        ["1:5: `workflow` is not defined"]
    );
    assert_eq!(
        lower("HOME", false).unwrap_err(),
        ["1:5: `HOME` is not defined (hint: use `env('...')` to access environment variable)"]
    );
    let code = lower("env('HOME')", false).unwrap();
    assert_eq!(
        evaluate(&code, &mut Interpreter::new(&mut Quiet)),
        "/home/me"
    );
}

#[test]
fn names_inside_closures() {
    assert!(lower("{ workflow.profile }", false).is_ok());
    assert_eq!(
        lower("{ meta.id }", false).unwrap_err(),
        ["1:7: `meta` is not defined"]
    );
    let code = lower("{ \"${meta}_${HOME}\" }", true).unwrap();
    let mut host = Quiet;
    let mut interpreter = Interpreter::new(&mut host);
    let Ok(Value::Closure(closure)) = interpreter.evaluate(&code, &Rc::new(Module::default()))
    else {
        panic!("a closure")
    };
    let error = interpreter.call_closure(&closure, Vec::new()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "MissingPropertyException: No such variable: meta"
    );
    interpreter.set_global("meta", Value::string("s1"));
    let value = interpreter.call_closure(&closure, Vec::new()).unwrap();
    assert_eq!(value.to_string(), "s1_/home/me");
}

#[test]
fn evaluates_lazy_maps() {
    let mut host = Quiet;
    let mut interpreter = Interpreter::new(&mut host);
    let module = Rc::new(Module::default());
    let args = interpreter
        .evaluate(&lower("{ \"--id ${meta}\" }", true).unwrap(), &module)
        .unwrap();
    let ext = Value::map([(Value::string("args"), args)]);
    let Value::Map(map) = &ext else {
        unreachable!()
    };
    map.borrow_mut().kind = MapKind::Lazy;
    interpreter.set_global("task", Value::map([(Value::string("ext"), ext)]));
    interpreter.set_global("meta", Value::string("s1"));
    let code = lower("{ task.ext.args + ' ' + task.ext.missing }", true).unwrap();
    let Ok(Value::Closure(closure)) = interpreter.evaluate(&code, &module) else {
        panic!("a closure")
    };
    let value = interpreter.call_closure(&closure, Vec::new()).unwrap();
    assert_eq!(value.to_string(), "--id s1 null");
}
