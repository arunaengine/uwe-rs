//! Tests how rules and checkpoints parse: names, directives, values, bodies and layout.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use snakemake_parser::ast::{Directive, Rule, Value};
use snakemake_parser::keyword::RuleKeyword;
use snakemake_parser::parse;
use snakemake_parser::ruff_python_ast::{Arguments, Expr, Stmt, Suite};
use snakemake_parser::ruff_text_size::Ranged;

fn rules(source: &str) -> Vec<Rule> {
    let snakefile = parse(source).unwrap_or_else(|error| panic!("{source}\n{error}"));
    snakefile.rules().cloned().collect()
}

fn one(source: &str) -> Rule {
    let mut rules = rules(source);
    assert_eq!(rules.len(), 1, "{source}");
    rules.remove(0)
}

fn keywords(rule: &Rule) -> Vec<&str> {
    let directives = rule.directives.iter();
    directives
        .map(|directive| directive.keyword.text())
        .collect()
}

fn directive(rule: &Rule, keyword: RuleKeyword) -> &Directive<RuleKeyword> {
    let mut directives = rule.directives.iter();
    directives
        .find(|directive| directive.keyword == keyword)
        .unwrap_or_else(|| panic!("no {} directive", keyword.text()))
}

fn arguments(rule: &Rule, keyword: RuleKeyword) -> &Arguments {
    match &directive(rule, keyword).value {
        Value::Arguments(arguments) => arguments,
        other => panic!("expected arguments, found {other:?}"),
    }
}

fn body(rule: &Rule) -> &Suite {
    match &directive(rule, RuleKeyword::Run).value {
        Value::Body(body) => body,
        other => panic!("expected a body, found {other:?}"),
    }
}

/// The source text of each positional argument and each keyword argument name.
fn texts<'a>(source: &'a str, arguments: &Arguments) -> Vec<&'a str> {
    let positional = arguments.args.iter().map(|arg| &source[arg.range()]);
    let names = arguments.keywords.iter().map(|keyword| match &keyword.arg {
        Some(name) => &source[name.range()],
        None => &source[keyword.range()],
    });
    positional.chain(names).collect()
}

fn string(expression: &Expr) -> String {
    match expression {
        Expr::StringLiteral(literal) => literal.value.to_str().to_string(),
        other => panic!("expected a string, found {other:?}"),
    }
}

fn message(source: &str) -> String {
    match parse(source) {
        Ok(_) => panic!("{source}\nparsed without error"),
        Err(error) => error.message,
    }
}

#[test]
fn directives_keep_order() {
    let source = "rule align:\n    input: \"a.fq\", index=\"ref.fa\"\n    output: \"a.bam\"\n    \
                  params: extra=\"-x\"\n    threads: 4\n    resources: mem_mb=100\n    \
                  log: \"a.log\"\n    shell: \"bwa {input} > {output}\"\n";
    let rule = one(source);
    assert!(!rule.checkpoint);
    assert_eq!(
        rule.name.as_ref().map(|name| name.text.as_str()),
        Some("align")
    );
    let expected = [
        "input",
        "output",
        "params",
        "threads",
        "resources",
        "log",
        "shell",
    ];
    assert_eq!(keywords(&rule), expected);
    let input = arguments(&rule, RuleKeyword::Input);
    assert_eq!(texts(source, input), ["\"a.fq\"", "index"]);
    assert_eq!(&source[input.range], "\"a.fq\", index=\"ref.fa\"");
    let directive = directive(&rule, RuleKeyword::Output);
    assert_eq!(&source[directive.keyword_range], "output");
    assert_eq!(&source[directive.range], "output: \"a.bam\"");
    assert_eq!(&source[rule.range], source.trim_end());
}

#[test]
fn checkpoints_and_anonymous() {
    let source = "checkpoint split:\n    output: directory(\"parts\")\nrule:\n    name: \"merge\"\n    input: \"x\"\n";
    let rules = rules(source);
    assert!(rules[0].checkpoint);
    assert_eq!(rules[1].name, None);
    assert_eq!(keywords(&rules[1]), ["name", "input"]);
    let name = arguments(&rules[1], RuleKeyword::Name);
    assert_eq!(string(&name.args[0]), "merge");
}

#[test]
fn every_directive_parses() {
    let mut source = String::from("rule every:\n");
    for keyword in RuleKeyword::ALL
        .iter()
        .filter(|keyword| !keyword.is_execution())
    {
        source.push_str(&format!("    {}: \"x\"\n", keyword.text()));
    }
    source.push_str("    shell: \"x\"\n");
    let rule = one(&source);
    assert_eq!(rule.directives.len(), RuleKeyword::ALL.len() - 6);
    for keyword in ["script", "notebook", "wrapper", "template_engine", "cwl"] {
        let rule = one(&format!("rule a:\n    {keyword}: \"x\"\n"));
        assert_eq!(keywords(&rule), [keyword]);
    }
}

#[test]
fn nested_indentation() {
    let source = "if config.get(\"a\"):\n    rule a:\n        input:\n            \"x\",\n            \
                  \"y\",\n        output: \"z\"\nelse:\n    for i in range(2):\n        rule:\n            \
                  name: f\"b{i}\"\n            shell: \"echo\"\n\ndef f():\n    if True:\n        \
                  rule c:\n            input: \"c\"\n    return 1\n";
    let rules = rules(source);
    let names: Vec<_> = rules
        .iter()
        .map(|rule| rule.name.as_ref().map(|name| name.text.as_str()))
        .collect();
    assert_eq!(names, [Some("a"), None, Some("c")]);
    assert_eq!(keywords(&rules[0]), ["input", "output"]);
    assert_eq!(
        texts(source, arguments(&rules[0], RuleKeyword::Input)),
        ["\"x\"", "\"y\""]
    );
    assert_eq!(keywords(&rules[1]), ["name", "shell"]);
    assert_eq!(keywords(&rules[2]), ["input"]);
}

#[test]
fn continuation_needs_indentation() {
    let rule = one("rule a:\n    params: a=1,\n        b=2\n    shell: \"x\"\n");
    assert_eq!(arguments(&rule, RuleKeyword::Parameters).keywords.len(), 2);
    let source = "rule a:\n    input: expand(\"{x}\",\n        x=[1, 2])\n";
    assert_eq!(message(source), "Unexpected keyword x in rule definition");
}

#[test]
fn run_block_statements() {
    let source = "rule a:\n    input: \"x\"\n    run:\n        total = 0\n        for line in open(input[0]):\n            \
                  if not line:\n                continue\n            total += 1\n        shell(\"echo {total}\")\n        \
                  return\nrule b:\n    run: print(1)\n";
    let rules = rules(source);
    let first = body(&rules[0]);
    assert_eq!(first.len(), 4);
    assert!(matches!(first[1], Stmt::For(_)));
    assert_eq!(&source[first[0].range()], "total = 0");
    assert_eq!(&source[first[3].range()], "return");
    assert_eq!(&source[body(&rules[1])[0].range()], "print(1)");
    assert_eq!(
        message("rule a:\n    run:\n        break\n"),
        "'break' outside loop"
    );
    let source = "rule a:\n    run:\nrule b:\n    input: \"x\"\n";
    assert!(parse(source).is_err(), "a run block needs statements");
}

#[test]
fn multiline_strings() {
    let source = "rule a:\n    input: \"x\"\n    shell:\n        \"\"\"\nset -e\n  echo {input}\n\"\"\"\n    \
                  \nrule b:\n    shell: \"echo \"\n        \"more\"\n";
    let rules = rules(source);
    let shell = arguments(&rules[0], RuleKeyword::Shell);
    assert_eq!(string(&shell.args[0]), "\nset -e\n  echo {input}\n");
    assert_eq!(keywords(&rules[1]), ["shell"]);
    let shell = arguments(&rules[1], RuleKeyword::Shell);
    assert_eq!(string(&shell.args[0]), "echo more");
}

#[test]
fn comments_after_values() {
    let source = "rule a:  # the first rule\n    input: \"x\"  # the input\n\n    # a note\n# column zero\n    \
                  output:\n        \"y\",  # the output\n        # more\n    shell: \"cat {input}\"  # run\n";
    let rule = one(source);
    assert_eq!(keywords(&rule), ["input", "output", "shell"]);
    assert_eq!(&source[arguments(&rule, RuleKeyword::Input).range], "\"x\"");
    assert_eq!(
        &source[arguments(&rule, RuleKeyword::Output).range],
        "\"y\","
    );
    assert_eq!(
        &source[arguments(&rule, RuleKeyword::Shell).range],
        "\"cat {input}\""
    );
}

#[test]
fn docstrings_in_rules() {
    let rule = one("rule a:\n    \"\"\"Aligns reads.\"\"\"\n    input: \"x\"\n");
    assert_eq!(rule.docstrings.len(), 1);
    assert_eq!(string(&rule.docstrings[0]), "Aligns reads.");
    assert_eq!(keywords(&rule), ["input"]);
    let error = message("rule a:\n    f\"doc\"\n    input: \"x\"\n");
    assert_eq!(
        error,
        "Expecting rule keyword, comment or docstrings inside a rule definition."
    );
}

#[test]
fn fstrings_in_values() {
    let source =
        "rule a:\n    input: f\"{config['dir']}/{{sample}}.txt\"\n    shell: f\"echo {x!r:>10}\"";
    let rule = one(source);
    let input = arguments(&rule, RuleKeyword::Input);
    assert!(matches!(input.args[0], Expr::FString(_)));
    assert_eq!(&source[input.range], "f\"{config['dir']}/{{sample}}.txt\"");
    assert!(matches!(
        arguments(&rule, RuleKeyword::Shell).args[0],
        Expr::FString(_)
    ));
    let nested = one("rule a:\n    input: f\"{f'{x}'}\" \"plain\"\n");
    assert!(matches!(
        arguments(&nested, RuleKeyword::Input).args[0],
        Expr::FString(_)
    ));
}

#[test]
fn single_line_rules() {
    assert_eq!(keywords(&one("rule a: input: \"x\"\n")), ["input"]);
    let error = message("rule a: input: \"x\"\nrule b: input: \"y\"\n");
    assert_eq!(error, "Unexpected keyword rule in rule definition");
}

#[test]
fn keyword_rule_names() {
    let rules = rules(
        "rule import:\n    input: \"x\"\nrule match:\n    input: \"y\"\nrule a b:\n    input: \"z\"\n",
    );
    let names: Vec<_> = rules
        .iter()
        .filter_map(|rule| rule.name.as_ref())
        .map(|name| name.text.as_str())
        .collect();
    assert_eq!(names, ["import", "match", "b"]);
}

#[test]
fn endings_and_tabs() {
    let rule = one("rule a:\r\n    input: \"x\"\r\n    shell: \"echo\"\r\n");
    assert_eq!(keywords(&rule), ["input", "shell"]);
    let rule = one("rule a:\n\tinput: \"x\"\n\tshell: \"echo\"\n");
    assert_eq!(keywords(&rule), ["input", "shell"]);
    assert!(
        parse("rule a:\n    input: \"x\"\n\toutput: \"y\"\n").is_err(),
        "mixed tabs and spaces"
    );
}

#[test]
fn execution_comes_last() {
    let multiple = message("rule a:\n    shell: \"a\"\n    run: pass\n");
    assert_eq!(multiple, "Multiple execution keywords in rule a.");
    let after = message("rule a:\n    shell: \"a\"\n    output: \"y\"\n");
    assert_eq!(
        after,
        "No rule keywords allowed after the execution keyword in rule a."
    );
    let unknown = message("rule a:\n    inputs: \"x\"\n");
    assert_eq!(unknown, "Unexpected keyword inputs in rule definition");
    let version = message("rule a:\n    version: \"1\"\n");
    assert!(
        version.starts_with("Keyword version is deprecated."),
        "{version}"
    );
    let name = message("rule a-b:\n    input: \"x\"\n");
    assert!(
        name.starts_with("Expected name or colon after rule"),
        "{name}"
    );
}

#[test]
fn command_arguments() {
    let rule = one("rule a:\n    shell: \"a\", extra=1\n");
    assert_eq!(arguments(&rule, RuleKeyword::Shell).keywords.len(), 1);
    let error = message("rule a:\n    script: \"a.py\", extra=1\n");
    assert_eq!(error, "Positional argument cannot follow keyword argument");
    assert!(
        parse("rule a:\n    shell: \"a\",\n").is_err(),
        "the generated call rejects a trailing comma"
    );
    let empty = message("rule a:\n    shell:\nrule b:\n    input: \"x\"\n");
    assert_eq!(
        empty,
        "Command must be given as string after the shell keyword."
    );
    let rule = one("rule a:\n    input:\n    output: \"y\"\n");
    assert!(arguments(&rule, RuleKeyword::Input).args.is_empty());
}
