//! Detects unsupported DSL1 scripts whose processes use from and into channels.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::ast::{
    self, ExpressionKind as Syntax, Item, Literal, NamedOutput, ProcessInput, Statement,
    StatementKind as Kind,
};

/// Whether a script is DSL1: `nextflow.enable.dsl = 1`, or a script without workflows with a
/// process that reads `from` a channel or writes `into` one.
pub(super) fn is_dsl1(script: &ast::Script) -> bool {
    let flag = script.items.iter().any(|item| {
        matches!(item, Item::FeatureFlag { name, value, .. }
            if name.iter().map(|part| part.text.as_str()).eq(["enable", "dsl"])
                && matches!(&value.kind, Syntax::Literal(Literal::Integer(text)) if text == "1"))
    });
    let workflows = script
        .items
        .iter()
        .any(|item| matches!(item, Item::Workflow(_)));
    let chained = script.items.iter().any(|item| match item {
        Item::Process(process) => is_dsl1_process(process),
        // A process inside a block of the script is DSL1 too.
        Item::Statement(statement) => has_process(statement),
        _ => false,
    });
    flag || (!workflows && chained)
}

/// Whether a process reads `from` a channel or writes `into` one.
fn is_dsl1_process(process: &ast::Process) -> bool {
    let inputs = process.inputs.iter().filter_map(|input| match input {
        ProcessInput::Statement(statement) => Some(statement),
        _ => None,
    });
    let outputs = process.outputs.iter().filter_map(|output| match output {
        NamedOutput::Statement(statement) => Some(statement),
        NamedOutput::Named { .. } => None,
    });
    inputs.chain(outputs).any(|statement| {
        matches!(&statement.kind, Kind::Expression(ast::Expression {
            kind: Syntax::MethodCall { target: Some(_), name, parentheses: false, .. },
            ..
        }) if matches!(name.text.as_str(), "from" | "into"))
    })
}

/// Whether a statement holds a process, like one inside an `if`.
fn has_process(statement: &Statement) -> bool {
    match &statement.kind {
        Kind::Process(_) => true,
        Kind::If {
            then, otherwise, ..
        } => (then.statements.iter())
            .chain(otherwise.iter().flat_map(|block| &block.statements))
            .any(has_process),
        _ => false,
    }
}
