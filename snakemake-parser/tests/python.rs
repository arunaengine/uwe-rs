//! Tests the Python code around Snakemake statements and the positions of errors.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use snakemake_parser::Error;
use snakemake_parser::ast::{Snakefile, Statement};
use snakemake_parser::parse;
use snakemake_parser::ruff_python_ast::{Expr, Stmt};
use snakemake_parser::ruff_text_size::Ranged;

fn accept(source: &str) -> Snakefile {
    parse(source).unwrap_or_else(|error| panic!("{source}\n{error}"))
}

fn reject(source: &str) -> Error {
    match parse(source) {
        Ok(_) => panic!("{source}\nparsed without error"),
        Err(error) => error,
    }
}

/// The statement that a `...` placeholder stands for.
fn placeholder<'a>(snakefile: &'a Snakefile, statement: &Stmt) -> Option<&'a Statement> {
    match statement {
        Stmt::Expr(expression) if matches!(*expression.value, Expr::EllipsisLiteral(_)) => {
            snakefile.statement_at(statement.range().start())
        }
        _ => None,
    }
}

#[test]
fn statements_become_placeholders() {
    let source = "import os\n\nSAMPLES = [\"a\", \"b\"]\n\nconfigfile: \"config.yaml\"\n\nrule all:\n    \
                  input: expand(\"{s}.txt\", s=SAMPLES)\n\ndef helper(wildcards):\n    return 1\n";
    let snakefile = accept(source);
    assert_eq!(snakefile.python.len(), 5);
    let texts: Vec<_> = snakefile
        .python
        .iter()
        .map(|statement| &source[statement.range()])
        .collect();
    assert_eq!(texts[0], "import os");
    assert_eq!(texts[1], "SAMPLES = [\"a\", \"b\"]");
    assert!(texts[4].starts_with("def helper(wildcards):"));
    let configfile = placeholder(&snakefile, &snakefile.python[2]).expect("a placeholder");
    assert!(matches!(configfile, Statement::Global(_)));
    let rule = placeholder(&snakefile, &snakefile.python[3]).expect("a placeholder");
    assert!(matches!(rule, Statement::Rule(_)));
    assert_eq!(
        &source[rule.range()],
        "rule all:\n    input: expand(\"{s}.txt\", s=SAMPLES)"
    );
    assert_eq!(placeholder(&snakefile, &snakefile.python[0]), None);
}

#[test]
fn nested_placeholders() {
    let source = "if config[\"x\"]:\n    include: \"a.smk\"\nelse:\n    x = 1\n    rule b:\n        \
                  input: \"b\"\n    y = 2\n";
    let snakefile = accept(source);
    let [Stmt::If(branch)] = &snakefile.python[..] else {
        panic!("expected one if statement");
    };
    assert!(matches!(
        placeholder(&snakefile, &branch.body[0]),
        Some(Statement::Global(_))
    ));
    let orelse = &branch.elif_else_clauses[0].body;
    assert_eq!(orelse.len(), 3);
    assert!(matches!(
        placeholder(&snakefile, &orelse[1]),
        Some(Statement::Rule(_))
    ));
    assert_eq!(&source[orelse[2].range()], "y = 2");
}

#[test]
fn line_start_keywords() {
    // Python reads `include: "a"` after `if x:` as an annotated name, as parser.py does.
    let snakefile = accept("if x: include: \"a\"\ny = rule\n");
    assert!(snakefile.statements.is_empty());
    assert!(matches!(&snakefile.python[0], Stmt::If(_)));
    let value = accept("x = [\ninclude: \"a\"\n]\n");
    assert_eq!(
        value.statements.len(),
        1,
        "keywords count at line starts inside brackets too"
    );
}

#[test]
fn errors_carry_positions() {
    let python = reject("x = 1\ny = (\n");
    assert_eq!((python.line, python.column), (3, 1));
    let value = reject("rule a:\n    input:\n        \"a\",\n        b=,\n");
    assert_eq!((value.line, value.column), (4, 11));
    let keyword = reject("rule a:\n    input: \"x\"\n    outputs: \"y\"\n");
    assert_eq!((keyword.line, keyword.column), (3, 5));
    assert_eq!(
        keyword.to_string(),
        "3:5: Unexpected keyword outputs in rule definition"
    );
    let string = reject("rule a:\n    shell: \"unterminated\n");
    assert_eq!((string.line, string.column), (2, 12));
    let tabs = reject("if x:\n        a = 1\n\tb = 2\n");
    assert_eq!(tabs.line, 3);
    // Columns count characters, not bytes.
    let unicode = reject("x = \"ä\"; rule = 1 +\n");
    assert_eq!((unicode.line, unicode.column), (1, 20));
}

#[test]
fn rejects_conditional_assignment() {
    // Snakemake's test_issue1256, whose SyntaxError is at line 9, as Python 3 puts it.
    let source = "rule all:\n    input:\n        'done.txt',\n\nrule trim:\n    output:\n        \
                  done.txt,\n    run:\n        if 1 = 2:\n            print('foo')\n";
    let error = reject(source);
    assert_eq!(error.line, 9);
    let expected = "cannot assign to literal here. Maybe you meant '==' instead of '='?";
    assert_eq!(error.message, expected);
    let named = reject("while x = 1:\n    pass\n");
    let expected = "invalid syntax. Maybe you meant '==' or ':=' instead of '='?";
    assert_eq!(named.message, expected);
    let attribute = reject("if a.b = 1:\n    pass\n");
    assert!(
        attribute
            .message
            .starts_with("cannot assign to attribute here")
    );
}

#[test]
fn keyword_errors_first() {
    // parser.py compiles the Python only after its state machine has read the whole file.
    let error = reject("rule a:\n    input: f(1,,)\n    oops: 1\n");
    assert_eq!(error.message, "Unexpected keyword oops in rule definition");
    assert_eq!(error.line, 3);
}

#[test]
fn control_flow_checked() {
    let error = reject("for x in y:\n    pass\nelse:\n    continue\n");
    assert_eq!(error.message, "'continue' not properly in loop");
    let error = reject("x = 1\nreturn x\n");
    assert_eq!(error.message, "'return' outside function");
    let error = reject("for x in y:\n    def f():\n        break\n");
    assert_eq!(error.message, "'break' outside loop");
    let valid = "for x in y:\n    if x:\n        continue\n    try:\n        break\n    finally:\n        \
                 pass\nrule a:\n    run:\n        return 1\n";
    let snakefile = accept(valid);
    assert!(matches!(snakefile.python[0], Stmt::For(_)));
}
