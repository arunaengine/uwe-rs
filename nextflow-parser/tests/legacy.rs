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
