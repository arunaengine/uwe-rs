//! Tests `module` blocks and the forms of `use rule`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use snakemake_parser::ast::{Module, Name, Statement, UseRule, Value};
use snakemake_parser::keyword::ModuleKeyword;
use snakemake_parser::parse;
use snakemake_parser::ruff_python_ast::Expr;

fn statements(source: &str) -> Vec<Statement> {
    let snakefile = parse(source).unwrap_or_else(|error| panic!("{source}\n{error}"));
    snakefile.statements
}

fn module(source: &str) -> Module {
    match statements(source).remove(0) {
        Statement::Module(module) => module,
        other => panic!("expected a module, found {other:?}"),
    }
}

fn uses(source: &str) -> Vec<UseRule> {
    let statements = statements(source).into_iter();
    statements
        .filter_map(|statement| match statement {
            Statement::UseRule(rule) => Some(rule),
            _ => None,
        })
        .collect()
}

fn texts(names: &[Name]) -> Vec<&str> {
    names.iter().map(|name| name.text.as_str()).collect()
}

fn text(name: &Option<Name>) -> Option<&str> {
    name.as_ref().map(|name| name.text.as_str())
}

fn message(source: &str) -> String {
    match parse(source) {
        Ok(_) => panic!("{source}\nparsed without error"),
        Err(error) => error.message,
    }
}

#[test]
fn module_settings() {
    let source = "module dna_seq:\n    \"\"\"A module.\"\"\"\n    snakefile:\n        github(\"org/repo\", \
                  path=\"workflow/Snakefile\", tag=\"v1\")\n    config: config[\"dna\"]\n    prefix: \"dna\"\n    \
                  replace_prefix: {\"results/\": \"out/\"}\n    skip_validation: True\n    pathvars: raw=\"data\"\n";
    let module = module(source);
    assert_eq!(text(&module.name), Some("dna_seq"));
    let keywords: Vec<_> = module
        .settings
        .iter()
        .map(|setting| setting.keyword)
        .collect();
    let expected = [
        ModuleKeyword::Snakefile,
        ModuleKeyword::Config,
        ModuleKeyword::Prefix,
        ModuleKeyword::ReplacePrefix,
        ModuleKeyword::SkipValidation,
        ModuleKeyword::PathVariables,
    ];
    assert_eq!(keywords, expected);
    assert!(
        matches!(&module.settings[0].value, Value::Expression(value) if matches!(**value, Expr::Call(_)))
    );
    assert!(
        matches!(&module.settings[5].value, Value::Arguments(arguments) if arguments.keywords.len() == 1)
    );
}

#[test]
fn module_names() {
    let module = module("module:\n    name: \"other\"\n    meta_wrapper: \"v1/meta/bio/x\"\n");
    assert_eq!(module.name, None);
    assert_eq!(module.settings[0].keyword, ModuleKeyword::Name);
    let missing = message("module:\n    snakefile: \"a\"\n");
    assert!(missing.starts_with("Missing module name."), "{missing}");
    let twice = message("module m:\n    name: \"n\"\n    snakefile: \"a\"\n");
    assert!(twice.starts_with("Ambiguous module name."), "{twice}");
    let source = message("module m:\n    config: config\n");
    assert_eq!(
        source,
        "A module needs either a path to a Snakefile or a meta wrapper URL."
    );
    let unknown = message("module m:\n    snakefile: \"a\"\n    input: \"x\"\n");
    assert_eq!(unknown, "Unexpected keyword input in module definition");
    let positional = message("module m:\n    snakefile: \"a\", \"b\"\n");
    assert_eq!(
        positional,
        "Positional argument cannot follow keyword argument"
    );
    // parser.py accepts this, but `workflow.module()` then fails on the unknown keyword.
    let keyword = message("module m:\n    snakefile: \"a\", tag=\"b\"\n");
    assert_eq!(keyword, "Expected a single value");
}

#[test]
fn use_rule_forms() {
    let source = "use rule * from dna_seq as dna_*\nuse rule a, b from other\n\
                  use rule * from other exclude c, d as other_*\nuse rule map from other as map2 with:\n    \
                  threads: 8\nuse rule local as copy with:\n    \"\"\"A copy.\"\"\"\n    input: \"y\"\n    \
                  # comment\n    output: \"z\"\nuse rule plain as renamed\n";
    let uses = uses(source);
    assert_eq!(texts(&uses[0].rules), ["*"]);
    assert_eq!(text(&uses[0].module), Some("dna_seq"));
    assert_eq!(text(&uses[0].alias), Some("dna_*"));
    assert_eq!(uses[0].with, None);
    assert_eq!(texts(&uses[1].rules), ["a", "b"]);
    assert_eq!(texts(&uses[2].exclude), ["c", "d"]);
    assert_eq!(text(&uses[2].alias), Some("other_*"));
    let with = uses[3].with.as_ref().expect("a with block");
    assert_eq!(with[0].keyword.text(), "threads");
    assert_eq!(uses[4].module, None);
    assert_eq!(uses[4].docstrings.len(), 1);
    let with = uses[4].with.as_ref().expect("a with block");
    let keywords: Vec<_> = with
        .iter()
        .map(|directive| directive.keyword.text())
        .collect();
    assert_eq!(keywords, ["input", "output"]);
    assert_eq!(text(&uses[5].alias), Some("renamed"));
    assert_eq!(uses[5].with, None);
}

#[test]
fn use_rule_errors() {
    let star = message("use rule * from m as m_* with:\n    input: \"x\"\n");
    assert!(
        star.starts_with("Keyword 'with' in 'use rule' statement is not allowed"),
        "{star}"
    );
    let run = message("use rule a as b with:\n    shell: \"x\"\n");
    assert_eq!(run, "Unexpected keyword shell in rule definition");
    assert_eq!(
        message("use a\n"),
        "Expecting keyword 'rule' after keyword 'use'"
    );
    assert!(
        parse("use rule a\n").is_err(),
        "a rule needs `from` or `as`"
    );
    assert!(
        parse("use rule a from\n").is_err(),
        "`from` needs a module name"
    );
    // parser.py drops the line break after an exclude list, so the next line continues it.
    assert!(parse("use rule * from m exclude a\nrule b:\n    input: \"x\"\n").is_err());
    assert!(parse("use rule * from m exclude a\n\nrule b:\n    input: \"x\"\n").is_ok());
}
