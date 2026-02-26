//! Tests statement parsing: declarations, assignments, control flow, labels and command calls.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::parser::Parser;
use crate::parser::expression::tests::render_statement;
use crate::token::{Dialect, TokenKind};

/// Parses a statement sequence and renders each statement; the whole input must be consumed.
fn parse(source: &str) -> Vec<String> {
    let mut parser = Parser::new(source, Dialect::Script);
    let statements = parser.statement_sequence(|_| false);
    assert!(parser.errors.is_empty(), "{source}: {:?}", parser.errors);
    let statements = statements.unwrap_or_else(|_| panic!("{source}: no statements"));
    assert_eq!(
        parser.peek(),
        TokenKind::EndOfFile,
        "{source}: input left over"
    );
    statements.iter().map(render_statement).collect()
}

fn one(source: &str) -> String {
    let mut statements = parse(source);
    assert_eq!(statements.len(), 1, "{source}: {statements:?}");
    statements.remove(0)
}

#[test]
fn variable_declarations() {
    assert_eq!(one("def x = 1"), "(def x 1)");
    assert_eq!(one("def x: Integer = 1"), "(def x: Integer 1)");
    assert_eq!(one("def x: Integer? = null"), "(def x: Integer? null)");
    assert_eq!(one("def (a, b) = [1, 2]"), "(def (a b) [1, 2])");
    assert_eq!(one("def String s = 'x'"), "(def String s 'x')");
    assert_eq!(one("List<String> names"), "(def List<String> names)");
    assert_eq!(one("int count =\n  0"), "(def int count 0)");
}

#[test]
fn assignment_forms() {
    assert_eq!(one("(a, b) = pair()"), "(set (a b) (pair))");
    assert_eq!(one("x += 1"), "(setAdd x 1)");
    assert_eq!(one("x\n  = 1"), "(setAssign x 1)");
    assert_eq!(one("meta.id = 'a'"), "(setAssign (. meta id) 'a')");
    assert_eq!(one("x ?= y"), "(setElvis x y)");
    assert_eq!(one("(a) + 1"), "(+ (paren a) 1)");
}

#[test]
fn control_flow() {
    assert_eq!(
        one("if (a) {\n  b\n} else if (c) d\nelse {\n  e\n}"),
        "(if a {b} {(if c {d} {e})})"
    );
    assert_eq!(
        one("try { a() }\ncatch (Exception e) { b }"),
        "(try 1 (catch Exception e))"
    );
    assert_eq!(one("try { a() } catch (e) { b }"), "(try 1 (catch  e))");
    assert_eq!(
        one("try { a() } catch (e: IOException | java.lang.Error) { b }"),
        "(try 1 (catch IOException|java.lang.Error e))"
    );
    assert_eq!(one("assert x : 'no x'"), "(assert x 'no x')");
    assert_eq!(parse("return\nreturn 1"), ["(return)", "(return 1)"]);
    assert_eq!(
        one("throw new Exception('x')"),
        "(throw (new Exception 'x'))"
    );
}

#[test]
fn closure_labels() {
    assert_eq!(
        one("ch.branch {\n  big: it > 10\n  small: true\n}"),
        "(cmd .branch ch {big: (> it 10); small: true})"
    );
}

#[test]
fn command_calls() {
    assert_eq!(
        one("tuple val(meta), path(\"*.bam\"), emit: bam"),
        "(cmd tuple (val meta) (path '*.bam') emit=bam)"
    );
    assert_eq!(
        one("publishDir \"${params.outdir}\",\n  mode: 'copy'"),
        "(cmd publishDir \"${(. params outdir)}\" mode='copy')"
    );
    assert_eq!(one("tag \"$meta.id\""), "(cmd tag \"${(. meta id)}\")");
    assert_eq!(one("log.info 'x'"), "(cmd .info log 'x')");
    assert_eq!(one("maxRetries -1"), "(- maxRetries 1)");
    assert_eq!(one("stdin"), "stdin");
    assert_eq!(
        one("path(\"x\") , emit: y"),
        "(cmd path (paren 'x') emit=y)"
    );
    assert_eq!(one("val(x), emit: v"), "(cmd val (paren x) emit=v)");
    assert_eq!(
        one("memory { 2.GB * task.attempt }"),
        "(cmd memory {(* (. 2 GB) (. task attempt))})"
    );
}

#[test]
fn keyword_arguments() {
    assert_eq!(one("foo in: 1"), "(cmd foo in=1)");
    assert_eq!(one("foo in, b"), "(cmd foo in b)");
    assert_eq!(one("foo in"), "(cmd foo in)");
    assert_eq!(one("foo.bar in > b"), "(cmd .bar foo (> in b))");
    assert_eq!(one("foo in b"), "(in foo b)");
    assert_eq!(
        one("foo as: 1, instanceof: 2"),
        "(cmd foo as=1 instanceof=2)"
    );
}

#[test]
fn stops_at_labels() {
    let mut parser = Parser::new("a\nb; c\ninput:\nd", Dialect::Script);
    let statements = parser
        .statement_sequence(|parser| parser.at_label(TokenKind::Input))
        .unwrap_or_else(|_| panic!("{:?}", parser.errors));
    assert_eq!(statements.len(), 3);
    assert!(parser.at_label(TokenKind::Input));
}

#[test]
fn rejects_missing_separator() {
    let mut parser = Parser::new("a b c", Dialect::Script);
    assert!(parser.statement_sequence(|_| false).is_err());
}

#[test]
fn rejects_reserved_names() {
    let cases = [
        ("def _ = 1", "`_` is not allowed as an identifier"),
        ("for (x in xs) { x }", "`for` loops are no longer supported"),
        ("while (x) { x }", "`while` loops are no longer supported"),
        (
            "def class = 1",
            "`class` is not allowed as an identifier because it is a Groovy keyword",
        ),
        ("xs.each { _ -> 1 }", "`_` is not allowed as an identifier"),
    ];
    for (source, message) in cases {
        let mut parser = Parser::new(source, Dialect::Script);
        let _ = parser.statement_sequence(|_| false);
        let found = parser
            .errors
            .iter()
            .any(|error| error.message.starts_with(message));
        assert!(found, "{source}: {:?}", parser.errors);
    }
    let mut parser = Parser::new("{ _ -> null }", Dialect::Config);
    assert!(parser.expression().is_ok() && parser.errors.is_empty());
}
