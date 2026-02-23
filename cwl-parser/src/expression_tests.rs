//! Tests how expression strings split into text, parameter references and JavaScript.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::expression::{Expression, ExpressionKind, Part};

fn parts(text: &str) -> Vec<Part> {
    Expression::parse(text, false)
        .unwrap_or_else(|error| panic!("{text}: {error}"))
        .parts
}

fn kind(text: &str) -> ExpressionKind {
    Expression::parse(text, false)
        .unwrap_or_else(|error| panic!("{text}: {error}"))
        .kind()
}

fn text(value: &str) -> Part {
    Part::Text(value.to_owned())
}

#[test]
fn classifies_expressions() {
    assert_eq!(kind("plain text"), ExpressionKind::Literal);
    assert_eq!(kind(""), ExpressionKind::Literal);
    assert_eq!(kind("$(inputs.reads.path)"), ExpressionKind::Reference);
    assert_eq!(
        kind("$(inputs.reads.nameroot + '.bam')"),
        ExpressionKind::Javascript
    );
    assert_eq!(kind("${ return 1; }\n"), ExpressionKind::Javascript);
    assert_eq!(kind("out_$(inputs.x).txt"), ExpressionKind::Interpolation);
}

#[test]
fn recognizes_parameter_references() {
    for reference in [
        "inputs.x",
        "self[0].basename",
        "inputs['my file'].path",
        "inputs[\"x\"]",
        "runtime.outdir",
        "inputs.x.length",
    ] {
        let text = format!("$({reference})");
        assert_eq!(
            parts(&text),
            [Part::Reference(reference.to_owned())],
            "{text}"
        );
    }
    for code in [
        "inputs.x + 1",
        "inputs.x[i]",
        "inputs['']",
        " inputs.x",
        "inputs..x",
    ] {
        let text = format!("$({code})");
        assert_eq!(parts(&text), [Part::Javascript(code.to_owned())], "{text}");
    }
}

#[test]
fn splits_interpolations() {
    assert_eq!(
        parts("a $(inputs.x) b ${return 'c';}"),
        [
            text("a "),
            Part::Reference("inputs.x".into()),
            text(" b "),
            Part::Function("return 'c';".into()),
        ]
    );
    assert_eq!(
        parts("$(inputs.x.split(')')[0])"),
        [Part::Javascript("inputs.x.split(')')[0]".into())]
    );
    assert_eq!(
        parts("${ var a = {b: \"}\"}; return a; }"),
        [Part::Function(" var a = {b: \"}\"}; return a; ".into())]
    );
}

#[test]
fn applies_escapes() {
    assert_eq!(
        parts("cost \\$(1) $(inputs.x)"),
        [text("cost $(1) "), Part::Reference("inputs.x".into())]
    );
    assert_eq!(parts("a\\\\b $(inputs.x)")[0], text("a\\b "));
    assert_eq!(parts("a\\nb $(inputs.x)")[0], text("a\\nb "));
    let old = Expression::parse("a\\nb $(inputs.x)", true).expect("valid");
    assert_eq!(old.parts[0], text("anb "));
}

#[test]
fn rejects_unfinished_blocks() {
    let error = Expression::parse("echo $(inputs.x", false).expect_err("unfinished");
    assert_eq!(error, "unfinished expression at `$(inputs.x`");
    assert_eq!(parts("costs 5$"), [text("costs 5$")]);
}

#[test]
fn keeps_surrounding_space() {
    let expression = Expression::parse("  $(inputs.x)\n", false).expect("valid");
    assert_eq!(expression.text, "  $(inputs.x)\n");
    assert_eq!(expression.kind(), ExpressionKind::Reference);
    assert!(!expression.uses_javascript());
    assert_eq!(parts(" padded "), [text(" padded ")]);
}
