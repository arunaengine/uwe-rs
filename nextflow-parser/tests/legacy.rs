//! Script syntax of the legacy parser that strict syntax rejects: the tree keeps it with its
//! legacy meaning, and `Parsed::legacy` lists the errors of strict syntax (decision 14).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::ast::{
    ExpressionKind, GStringPart, Item, NamedOutput, OutputTarget, StatementKind,
};
use nextflow_parser::parse_script;

/// The items of a script without errors, and the messages of its legacy syntax.
fn parse(source: &str) -> (Vec<Item>, Vec<String>) {
    let parsed = parse_script(source);
    assert!(
        parsed.errors.is_empty(),
        "unexpected errors: {:?}",
        parsed.errors
    );
    let legacy = parsed
        .legacy
        .into_iter()
        .map(|error| error.message)
        .collect();
    (parsed.tree.items, legacy)
}

/// The expression of a script that is one expression statement.
fn expression(source: &str) -> (ExpressionKind, Vec<String>) {
    let (items, legacy) = parse(source);
    let [Item::Statement(statement)] = items.as_slice() else {
        panic!("one statement: {items:?}");
    };
    let StatementKind::Expression(expression) = &statement.kind else {
        panic!("an expression: {statement:?}");
    };
    (expression.kind.clone(), legacy)
}

#[test]
fn line_break_after_a_dot() {
    let (kind, legacy) = expression("ch.join(other).\n    map { it }\n");
    assert!(matches!(kind, ExpressionKind::MethodCall { name, .. } if name.text == "map"));
    assert_eq!(legacy, ["Expected a property name"]);
}

#[test]
fn java_lambda() {
    let (kind, legacy) = expression("ch.map ( gene -> gene.trim() )\n");
    let ExpressionKind::MethodCall { arguments, .. } = kind else {
        panic!("a call");
    };
    let ExpressionKind::Closure(closure) = &arguments[0].value.kind else {
        panic!("a closure: {arguments:?}");
    };
    let parameters = closure.parameters.as_ref().expect("parameters");
    assert_eq!(parameters[0].name.text, "gene");
    assert_eq!(legacy, ["Unexpected input: '->'"]);
}

#[test]
fn assignment_in_a_conditional() {
    let source = "meta.single ? reads1 = reads : reads.each { reads1 << it }\n";
    let (kind, legacy) = expression(source);
    let ExpressionKind::Ternary { then, .. } = kind else {
        panic!("a conditional");
    };
    assert!(matches!(then.kind, ExpressionKind::Assign { .. }));
    assert_eq!(legacy, ["Expected ':' of the conditional expression"]);
}

#[test]
fn slashy_string_with_interpolation() {
    let (kind, legacy) = expression("logo ? /--cl-config 'logo: \"${logo}\"' \\/x/ : ''\n");
    let ExpressionKind::Ternary { then, .. } = kind else {
        panic!("a conditional");
    };
    let ExpressionKind::GString(parts) = &then.kind else {
        panic!("a GString: {then:?}");
    };
    assert!(matches!(&parts[0], GStringPart::Text(text) if text == "--cl-config 'logo: \""));
    assert!(matches!(&parts[2], GStringPart::Text(text) if text == "\"' /x"));
    assert_eq!(
        legacy,
        ["Slashy strings with interpolations are not supported"]
    );
    // A slashy string without interpolations is strict syntax.
    let (_, legacy) = parse("x = /a.b/\n");
    assert!(legacy.is_empty());
}

#[test]
fn empty_take_section() {
    let source = "workflow INIT {\n    take:\n\n    main:\n    x = 1\n\n    emit:\n    x\n}\n";
    let (items, legacy) = parse(source);
    assert!(matches!(&items[0], Item::Workflow(workflow) if workflow.takes.is_empty()));
    assert_eq!(
        legacy,
        ["Invalid workflow definition -- check for missing or out-of-order section labels"]
    );
}

#[test]
fn preview_output_syntax() {
    let source = "workflow {\n    main:\n    ch = channel.of(1)\n    publish:\n    ch >> 'numbers'\n}\n\
                  output {\n    'numbers' {\n        path 'n'\n    }\n}\n";
    let (items, legacy) = parse(source);
    let Item::Workflow(workflow) = &items[0] else {
        panic!("a workflow");
    };
    let [NamedOutput::Named { target, value }] = workflow.publishers.as_slice() else {
        panic!("one publisher: {:?}", workflow.publishers);
    };
    assert_eq!(target.name.text, "numbers");
    assert!(
        matches!(&value.as_ref().expect("a value").kind, ExpressionKind::Variable(name) if name == "ch")
    );
    let Item::Output { targets, .. } = &items[1] else {
        panic!("an output block");
    };
    assert!(matches!(&targets[0], OutputTarget::Target { name, .. } if name.text == "numbers"));
    assert_eq!(
        legacy,
        [
            "Invalid workflow publish statement -- must be an assignment",
            "Invalid output declaration"
        ]
    );
}

#[test]
fn several_variables_in_one_declaration() {
    let (items, legacy) = parse("def a = [], b\nprintln(a)\n");
    let Item::Statement(statement) = &items[0] else {
        panic!("a statement");
    };
    let StatementKind::Declaration {
        names,
        initializer,
        destructure,
        ..
    } = &statement.kind
    else {
        panic!("a declaration: {statement:?}");
    };
    assert_eq!(names.len(), 2);
    assert!(*destructure);
    let Some(ExpressionKind::List(values)) = initializer.as_ref().map(|value| &value.kind) else {
        panic!("a list of values");
    };
    assert_eq!(values.len(), 2);
    assert_eq!(legacy, ["Expected a new line or ';' after the statement"]);
}

/// Legacy syntax: a `when:` section of several lines, whose last line is the condition.
#[test]
fn when_sections_of_several_lines() {
    let source = "process P {\n    input:\n    val x\n    when:\n    x != null\n    x > 1\n    script:\n    \"echo\"\n}\n";
    let parsed = nextflow_parser::parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    assert_eq!(parsed.legacy.len(), 1, "{:?}", parsed.legacy);
    let nextflow_parser::ast::Item::Process(process) = &parsed.tree.items[0] else {
        panic!("a process");
    };
    let condition = process.when.as_ref().expect("a condition");
    let (start, end) = (condition.span.start as usize, condition.span.end as usize);
    assert_eq!(&source[start..end], "x > 1");
    assert!(process.body.is_some());
}

/// An include inside an `if`, which Nextflow's strict parser rejects with this message.
#[test]
fn include_inside_an_if() {
    let (items, legacy) = parse("if (x) {\n    include { A as C } from './a'\n}\n");
    let [Item::Statement(statement)] = items.as_slice() else {
        panic!("one statement: {items:?}");
    };
    let StatementKind::If { then, .. } = &statement.kind else {
        panic!("an if: {statement:?}");
    };
    assert!(
        matches!(&then.statements[0].kind, StatementKind::Include(include) if include.source.text == "./a")
    );
    assert_eq!(legacy, ["Unexpected input: 'include'"]);
}

/// The inputs and outputs of DSL1 processes are Groovy command chains: `file 'x' into a, b`
/// calls `into(a, b)` on the result of `file('x')`.
#[test]
fn command_chains_of_dsl1_processes() {
    let (kind, legacy) = expression("file 'x.txt' optional true into ch_a, ch_b\n");
    let ExpressionKind::MethodCall {
        target: Some(target),
        name,
        arguments,
        ..
    } = kind
    else {
        panic!("a call");
    };
    assert_eq!(name.text, "into");
    assert_eq!(arguments.len(), 2);
    assert!(
        matches!(&target.kind, ExpressionKind::MethodCall { name, .. } if name.text == "optional")
    );
    assert_eq!(
        legacy,
        ["Unexpected input: 'optional'", "Unexpected input: 'into'"]
    );
    let (kind, _) = expression("set val(a), file(b) from ch.collect()\n");
    assert!(matches!(kind, ExpressionKind::MethodCall { name, .. } if name.text == "from"));
}

/// The legacy parser takes the sections of a process in any order, like `when:` before
/// `input:` in nf-core/mnaseseq, and empty sections, like the `input:` of nf-core/exoseq.
#[test]
fn process_sections_in_any_order() {
    let (items, legacy) = parse(
        "process P {\n    when:\n    true\n    input:\n    val x\n    script:\n    'true'\n}\n",
    );
    let [Item::Process(process)] = items.as_slice() else {
        panic!("a process: {items:?}");
    };
    assert!(process.when.is_some());
    assert_eq!(process.inputs.len(), 1);
    assert_eq!(
        legacy,
        ["Invalid process definition -- check for missing or out-of-order section labels"]
    );
    let (_, legacy) =
        parse("process P {\n    input:\n\n    output:\n    stdout\n    script:\n    'true'\n}\n");
    assert_eq!(
        legacy,
        ["Invalid process definition -- check for missing or out-of-order section labels"]
    );
}

/// A command as the last argument of a command, like `exit 1, log.info "x"` of
/// nf-core/denovohybrid, which Groovy reads as `exit(1, log.info("x"))`.
#[test]
fn commands_as_arguments() {
    let (kind, legacy) = expression("exit 1, log.info \"x\"\n");
    let ExpressionKind::MethodCall { arguments, .. } = kind else {
        panic!("a call");
    };
    assert!(
        matches!(&arguments[1].value.kind, ExpressionKind::MethodCall { name, .. } if name.text == "info")
    );
    assert_eq!(legacy, ["Unexpected input: '\"x\"'"]);
}

/// `Object[]` names the class of arrays, like in `[Collection, Object[]]` of nf-core/quantms.
#[test]
fn array_classes() {
    let (kind, legacy) = expression("[Collection, Object[]]\n");
    let ExpressionKind::List(items) = kind else {
        panic!("a list");
    };
    assert!(matches!(&items[1].kind, ExpressionKind::Variable(name) if name == "Object[]"));
    assert_eq!(legacy, ["Unexpected input: '['"]);
}

/// `addParams(...)` and `params(...)` after an include give the included script params, as
/// nf-core/scflow has.
#[test]
fn include_params() {
    let parsed = nextflow_parser::parse_script(
        "include { A } from './a' addParams( options: [x: 1] )\ninclude { B } from './b' params(y: 2)\n",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let params: Vec<(bool, usize)> = parsed
        .tree
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Include(include) => include.params.as_ref(),
            _ => None,
        })
        .map(|params| (params.add, params.arguments.len()))
        .collect();
    assert_eq!(params, [(true, 1), (false, 1)]);
    let legacy: Vec<String> = parsed
        .legacy
        .into_iter()
        .map(|error| error.message)
        .collect();
    assert_eq!(
        legacy,
        [
            "Unexpected input: 'addParams'",
            "Unexpected input: 'params'"
        ]
    );
}

/// The publish statements of Nextflow 24.10, like `ch >> 'name'`, in a named workflow, as
/// nf-core/references has; Nextflow 24.10.1, the release it names, runs it.
#[test]
fn preview_publish_sections_of_named_workflows() {
    let parsed = nextflow_parser::parse_script(
        "workflow W {\n    main:\n    ch = channel.of(1)\n    publish:\n    ch >> 'reference'\n}\n",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let legacy: Vec<String> = parsed
        .legacy
        .into_iter()
        .map(|error| error.message)
        .collect();
    assert!(
        legacy.contains(&"Named workflow cannot have a publish section".to_owned()),
        "{legacy:?}"
    );
}

/// Dollar slashy strings, `$/.../$`, as sanger-tol/treeval writes scripts; Nextflow 26.04.6
/// prints `a x `b` $ c /` for `$/a $f `b` $$ c $//$`.
#[test]
fn dollar_slashy_strings() {
    let (kind, legacy) = expression("$/a $f `b` $$ c $//$\n");
    let ExpressionKind::GString(parts) = kind else {
        panic!("a GString: {kind:?}");
    };
    let texts: Vec<String> = parts
        .iter()
        .filter_map(|part| match part {
            nextflow_parser::ast::GStringPart::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["a ", " `b` $ c /"]);
    assert_eq!(legacy, ["Unexpected character: '$'"]);
}

/// Statements in an interpolation, like `${ if (...) { ... } else { ... } }` of Pipeface, run
/// as a closure; Nextflow 26.04.6 prints `v many end` for this string.
#[test]
fn statements_in_interpolations() {
    let (kind, legacy) =
        expression("\"\"\"v ${\n  if (x.size() > 1) { 'many' } else { 'one' }\n} end\"\"\"\n");
    let ExpressionKind::GString(parts) = kind else {
        panic!("a GString: {kind:?}");
    };
    assert!(matches!(
        &parts[1],
        nextflow_parser::ast::GStringPart::Value(value)
            if matches!(value.kind, ExpressionKind::Call { .. })
    ));
    assert_eq!(legacy, ["Unexpected input: '{'"]);
}

/// `assert condition, message`, as the Polygenic Score Catalog Calculator writes it; Nextflow
/// 26.04.6 runs it like `assert condition : message`.
#[test]
fn asserts_with_a_comma() {
    let parsed = nextflow_parser::parse_script("assert 1 == 1, 'bad'\n");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let Item::Statement(statement) = &parsed.tree.items[0] else {
        panic!("a statement");
    };
    assert!(matches!(
        &statement.kind,
        nextflow_parser::ast::StatementKind::Assert { message: Some(_), .. }
    ));
}
