//! Tests the global keywords: their values, rule name lists, handlers and storage.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use snakemake_parser::ast::{Directive, Statement, Value};
use snakemake_parser::keyword::GlobalKeyword;
use snakemake_parser::parse;
use snakemake_parser::ruff_python_ast::{Arguments, Stmt};
use snakemake_parser::ruff_text_size::Ranged;

fn statements(source: &str) -> Vec<Statement> {
    let snakefile = parse(source).unwrap_or_else(|error| panic!("{source}\n{error}"));
    snakefile.statements
}

fn globals(source: &str) -> Vec<Directive<GlobalKeyword>> {
    let statements = statements(source).into_iter();
    statements
        .map(|statement| match statement {
            Statement::Global(directive) => directive,
            other => panic!("expected a global keyword, found {other:?}"),
        })
        .collect()
}

fn arguments(directive: &Directive<GlobalKeyword>) -> &Arguments {
    match &directive.value {
        Value::Arguments(arguments) => arguments,
        other => panic!("expected arguments, found {other:?}"),
    }
}

fn names(directive: &Directive<GlobalKeyword>) -> Vec<&str> {
    match &directive.value {
        Value::Names(names) => names.iter().map(|name| name.text.as_str()).collect(),
        other => panic!("expected names, found {other:?}"),
    }
}

fn message(source: &str) -> String {
    match parse(source) {
        Ok(_) => panic!("{source}\nparsed without error"),
        Err(error) => error.message,
    }
}

#[test]
fn keywords_take_arguments() {
    let plain = GlobalKeyword::ALL.iter().filter(|keyword| {
        !matches!(
            keyword,
            GlobalKeyword::RuleOrder
                | GlobalKeyword::LocalRules
                | GlobalKeyword::OnStart
                | GlobalKeyword::OnSuccess
                | GlobalKeyword::OnError
                | GlobalKeyword::ResourceScopes
        )
    });
    for keyword in plain {
        let source = format!("{}: \"a\", b=\"c\"\n", keyword.text());
        let directives = globals(&source);
        assert_eq!(directives[0].keyword, *keyword);
        let arguments = arguments(&directives[0]);
        assert_eq!(
            (arguments.args.len(), arguments.keywords.len()),
            (1, 1),
            "{source}"
        );
    }
}

#[test]
fn values_span_blocks() {
    let source = "configfile: \"config/config.yaml\"\nwildcard_constraints:\n    sample=\"[^/]+\",\n    \
                  # a comment\n    unit=\"\\d+\"\ninclude: \"rules/a.smk\"  # comment\ninclude:\n    \"rules/b.smk\"\n";
    let directives = globals(source);
    let keywords: Vec<_> = directives
        .iter()
        .map(|directive| directive.keyword.text())
        .collect();
    assert_eq!(
        keywords,
        ["configfile", "wildcard_constraints", "include", "include"]
    );
    assert_eq!(arguments(&directives[1]).keywords.len(), 2);
    assert_eq!(&source[arguments(&directives[2]).range], "\"rules/a.smk\"");
    assert_eq!(
        &source[directives[3].range],
        "include:\n    \"rules/b.smk\""
    );
}

#[test]
fn rule_name_lists() {
    let source = "localrules: all, plot\nlocalrules:\n    a,  # first\n    # skipped\n    b,\n\
                  ruleorder: a > b > c\nruleorder:\n    x > y\n    > z\n";
    let directives = globals(source);
    assert_eq!(names(&directives[0]), ["all", "plot"]);
    assert_eq!(names(&directives[1]), ["a", "b"]);
    assert_eq!(names(&directives[2]), ["a", "b", "c"]);
    assert_eq!(names(&directives[3]), ["x", "y", "z"]);
    // parser.py turns names into strings, and Python joins adjacent strings.
    assert_eq!(names(&globals("localrules: a b, c\n")[0]), ["ab", "c"]);
    assert!(parse("localrules: , a\n").is_err());
    let order = message("ruleorder: a >> b\n");
    assert!(order.starts_with("Expected a descending order"), "{order}");
    let local = message("localrules: a; b\n");
    assert!(
        local.starts_with("Expected a comma separated list"),
        "{local}"
    );
}

#[test]
fn handlers_hold_python() {
    let source = "onstart:\n    print(\"start\")\nonsuccess: print(\"done\")\nonerror:\n    # notify\n    \
                  shell(\"mail -s failed {log}\")\n";
    let directives = globals(source);
    let keywords: Vec<_> = directives
        .iter()
        .map(|directive| directive.keyword)
        .collect();
    assert_eq!(
        keywords,
        [
            GlobalKeyword::OnStart,
            GlobalKeyword::OnSuccess,
            GlobalKeyword::OnError
        ]
    );
    for directive in &directives {
        let Value::Body(body) = &directive.value else {
            panic!("expected a body");
        };
        assert!(matches!(body[..], [Stmt::Expr(_)]));
    }
    let Value::Body(body) = &directives[2].value else {
        panic!("expected a body");
    };
    assert_eq!(&source[body[0].range()], "shell(\"mail -s failed {log}\")");
    assert!(
        parse("onstart:\nx = 1\n").is_err(),
        "a handler needs statements"
    );
}

#[test]
fn resource_scopes_checked() {
    let directives = globals("resource_scopes:\n    mem_mb=\"local\",\n    gpu='global'\n");
    assert_eq!(arguments(&directives[0]).keywords.len(), 2);
    let error = message("resource_scopes:\n    mem_mb=\"lokal\"\n");
    assert_eq!(
        error,
        "Invalid scope: mem_mb=\"lokal\". Scope must be set to either 'local' or 'global'"
    );
}

#[test]
fn storage_takes_keywords() {
    let source = "storage:\n    provider=\"s3\",\nstorage http_local:\n    provider=\"http\",\n    retries=3\n";
    let statements = statements(source);
    let storages: Vec<_> = statements
        .iter()
        .map(|statement| match statement {
            Statement::Storage(storage) => storage,
            other => panic!("expected storage, found {other:?}"),
        })
        .collect();
    assert_eq!(storages[0].tag, None);
    assert_eq!(
        storages[1].tag.as_ref().map(|tag| tag.text.as_str()),
        Some("http_local")
    );
    assert_eq!(storages[1].arguments.keywords.len(), 2);
    let error = message("storage:\n    \"s3\"\n");
    assert_eq!(error, "Positional argument cannot follow keyword argument");
}

#[test]
fn keywords_as_variables() {
    let error = message("def f():\n    report = 1\n");
    assert_eq!(error, "Colon expected after keyword report.");
    let error = message("x = dict(\n    container=1,\n)\n");
    assert_eq!(error, "Colon expected after keyword container.");
    let error = message("subworkflow other:\n    workdir: \"x\"\n");
    assert!(
        error.starts_with("Keyword subworkflow is deprecated."),
        "{error}"
    );
    assert!(
        parse("x = 1\ninput = 2\nconfig[\"a\"] = 3\n").is_ok(),
        "only line starts count"
    );
}
