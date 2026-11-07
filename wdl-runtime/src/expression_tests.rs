//! Checks expressions, placeholders and standard library calls on fixed values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use wdl_ast::v1::Expr;

use super::{Environment, Error, Stream, evaluate};
use crate::value::Value;

/// Names and files of a test: files are texts by path; written files get numbered names.
#[derive(Default)]
struct Fixed {
    names: BTreeMap<String, Value>,
    files: BTreeMap<String, String>,
}

impl Environment for Fixed {
    fn lookup(&self, name: &str) -> Option<Value> {
        self.names.get(name).cloned()
    }

    fn read(&mut self, file: &str) -> Result<String, Error> {
        (self.files.get(file).cloned()).ok_or_else(|| Error::failed(format!("no {file}")))
    }

    fn stream(&mut self, _stream: Stream) -> Result<Value, Error> {
        Ok(Value::File("stdout".into()))
    }

    fn write(&mut self, content: String) -> Result<Value, Error> {
        let path = format!("written-{}", self.files.len());
        self.files.insert(path.clone(), content);
        Ok(Value::File(path))
    }
}

/// The expression of the declaration `x` in a workflow.
fn parse(expression: &str) -> Expr {
    let source = format!("version 1.2\nworkflow w {{\n  String x = {expression}\n}}\n");
    let (document, diagnostics) = wdl_ast::Document::parse(&source, None);
    assert!(diagnostics.is_empty(), "{expression}: {diagnostics:?}");
    let wdl_ast::Ast::V1(ast) = document.ast() else {
        panic!("a WDL 1 document");
    };
    let workflow = ast.workflows().next().expect("a workflow");
    let statement = workflow.statements().next().expect("a statement");
    statement.unwrap_declaration().expr()
}

fn value(expression: &str, env: &mut Fixed) -> Result<Value, Error> {
    evaluate(&parse(expression), env)
}

fn text(expression: &str, env: &mut Fixed) -> String {
    match value(expression, env) {
        Ok(Value::String(text)) => text,
        other => panic!("{expression} gave {other:?}"),
    }
}

#[test]
fn evaluates_operators() {
    let mut env = Fixed::default();
    env.names.insert("n".into(), Value::Int(3));
    assert_eq!(value("n * 2 + 1", &mut env), Ok(Value::Int(7)));
    assert_eq!(value("7 / 2", &mut env), Ok(Value::Int(3)));
    assert_eq!(
        value("7 % 2 == 1 && !false", &mut env),
        Ok(Value::Boolean(true))
    );
    assert_eq!(value("1 + 0.5", &mut env), Ok(Value::Float(1.5)));
    assert_eq!(
        value("if n > 2 then \"big\" else \"small\"", &mut env),
        Ok(Value::String("big".into()))
    );
    assert_eq!(value("[1, 2, 3][n - 1]", &mut env), Ok(Value::Int(3)));
    assert_eq!(value("{\"a\": 1}[\"a\"]", &mut env), Ok(Value::Int(1)));
    assert_eq!(
        value("(1, \"b\").right", &mut env),
        Ok(Value::String("b".into()))
    );
    assert!(matches!(value("[1][4]", &mut env), Err(Error::Failed(_))));
    assert!(matches!(value("1 / 0", &mut env), Err(Error::Failed(_))));
}

#[test]
fn fills_placeholders() {
    let mut env = Fixed::default();
    env.names.insert(
        "names".into(),
        Value::Array(vec![Value::String("a".into()), Value::String("b".into())]),
    );
    env.names.insert("flag".into(), Value::Boolean(false));
    env.names.insert("missing".into(), Value::None);
    env.names.insert("ratio".into(), Value::Float(0.5));
    assert_eq!(text("\"~{sep=',' names}\"", &mut env), "a,b");
    assert_eq!(text("\"~{true='-y' false='-n' flag}\"", &mut env), "-n");
    assert_eq!(text("\"[~{default='none' missing}]\"", &mut env), "[none]");
    assert_eq!(text("\"[~{missing}]\"", &mut env), "[]");
    assert_eq!(text("\"r=~{ratio}\"", &mut env), "r=0.500000");
    assert_eq!(text("\"a\" + 1", &mut env), "a1");
}

#[test]
fn reads_files() {
    let mut env = Fixed::default();
    env.files.insert("lines.txt".into(), "x\ny\n".into());
    env.files.insert("count.txt".into(), " 42\n".into());
    assert_eq!(text("basename(\"/a/b/c.txt\", \".txt\")", &mut env), "c");
    assert_eq!(text("sub(\"a-b-c\", \"-\", \"_\")", &mut env), "a_b_c");
    assert_eq!(
        text("sub(\"ab\", \"(a)(b)\", \"\\\\2\\\\1$\")", &mut env),
        "ba$"
    );
    assert_eq!(
        value("read_int(\"count.txt\")", &mut env),
        Ok(Value::Int(42))
    );
    let lines = value("read_lines(\"lines.txt\")", &mut env);
    assert_eq!(
        lines,
        Ok(Value::Array(vec![
            Value::String("x".into()),
            Value::String("y".into())
        ]))
    );
    assert!(matches!(
        value("read_objects(\"x\")", &mut env),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn lists_referenced_names() {
    let names = super::names(&parse("a + b.out[c] + length(d)"));
    let expected: Vec<&str> = vec!["a", "b", "c", "d"];
    assert_eq!(
        names.iter().map(String::as_str).collect::<Vec<_>>(),
        expected
    );
}
