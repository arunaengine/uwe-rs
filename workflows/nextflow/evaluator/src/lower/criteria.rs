//! Lowers labelled branch and multiMap criteria into returned labels or value maps.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::Span;
use nextflow_parser::ast::{
    Block, Closure, Expression, ExpressionKind, Literal, MapEntry, MapKey, Name, Statement,
    StatementKind, TypedName,
};

/// A `branch` closure as a closure that returns `[label, value]` for the first label whose
/// condition holds, and `null` when none does. The result has the labels in order.
pub(super) fn branch(closure: &Closure) -> Result<(Vec<String>, Closure), (String, Span)> {
    if closure.parameters.as_ref().is_some_and(Vec::is_empty) {
        let message = "Branch criteria should declare at least one parameter or use the implicit `it` parameter";
        return Err((message.into(), closure.body.span));
    }
    let mut prelude = Vec::new();
    let mut branches: Vec<(String, Expression, Vec<Statement>)> = Vec::new();
    for statement in &closure.body.statements {
        match &statement.kind {
            StatementKind::Labeled { label, statement } => {
                if branches.iter().any(|(name, ..)| *name == label.text) {
                    return Err((
                        format!("Branch label already declared: {}", label.text),
                        label.span,
                    ));
                }
                let StatementKind::Expression(condition) = &statement.kind else {
                    return Err((
                        format!("Unexpected statement in label {}", label.text),
                        statement.span,
                    ));
                };
                branches.push((label.text.clone(), condition.clone(), Vec::new()));
            }
            _ => match branches.last_mut() {
                Some((_, _, code)) => code.push(statement.clone()),
                None => prelude.push(statement.clone()),
            },
        }
    }
    if branches.is_empty() {
        let message = "Branch criteria should declare at least one branch";
        return Err((message.into(), closure.body.span));
    }
    let labels: Vec<String> = branches.iter().map(|(label, ..)| label.clone()).collect();
    let mut chain: Option<Statement> = None;
    for (label, condition, code) in branches.into_iter().rev() {
        let span = condition.span;
        let then = branch_block(closure, &label, code, span)?;
        chain = Some(statement(
            StatementKind::If {
                condition,
                then,
                otherwise: chain.map(|next| Block {
                    span: next.span,
                    statements: vec![next],
                }),
            },
            span,
        ));
    }
    prelude.extend(chain);
    Ok((labels, with_body(closure, prelude)))
}

/// The code of one branch: it returns `[label, value]`. The value is the closure's parameter
/// when the branch has no code, else what its `return` statements or its last expression give.
fn branch_block(
    closure: &Closure,
    label: &str,
    mut code: Vec<Statement>,
    span: Span,
) -> Result<Block, (String, Span)> {
    let choice = |value: Expression| {
        let span = value.span;
        expression(ExpressionKind::List(vec![text(label, span), value]), span)
    };
    if code.is_empty() {
        let parameters: Vec<Expression> = match &closure.parameters {
            None => vec![variable("it", span)],
            Some(parameters) => parameters
                .iter()
                .map(|p| variable(&p.name.text, span))
                .collect(),
        };
        let value = match <[Expression; 1]>::try_from(parameters) {
            Ok([single]) => single,
            Err(parameters) => expression(ExpressionKind::List(parameters), span),
        };
        code.push(statement(StatementKind::Return(Some(choice(value))), span));
    } else if returns(&code) {
        code = code.into_iter().map(|s| with_returns(s, &choice)).collect();
    } else {
        let last = code.pop().expect("code");
        let StatementKind::Expression(value) = last.kind else {
            return Err(("Unexpected statement in branch condition".into(), last.span));
        };
        code.push(statement(
            StatementKind::Return(Some(choice(value))),
            last.span,
        ));
    }
    Ok(Block {
        statements: code,
        span,
    })
}

/// A `multiMap` closure as a closure that returns a map of each label's value. A labelled
/// block's value is its last expression; `a: b: value` gives both labels the same value.
pub(super) fn multi_map(closure: &Closure) -> Result<(Vec<String>, Closure), (String, Span)> {
    let mut body = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut outputs = Vec::new();
    let statements = &closure.body.statements;
    for (index, statement) in statements.iter().enumerate() {
        let (block_labels, inner) = labels_of(statement);
        let mut inner = inner.clone();
        if !block_labels.is_empty() {
            current = block_labels;
            for label in &current {
                if labels.contains(label) {
                    return Err((
                        format!("multiMap label already declared: {label}"),
                        statement.span,
                    ));
                }
                labels.push(label.clone());
            }
        }
        let next_starts_block = statements
            .get(index + 1)
            .is_none_or(|next| !labels_of(next).0.is_empty());
        if current.is_empty() || !next_starts_block {
            body.push(inner);
            continue;
        }
        // The last statement of a labelled block gives the value of its labels.
        let StatementKind::Expression(value) =
            std::mem::replace(&mut inner.kind, StatementKind::Empty)
        else {
            return Err((
                "multiMap block must end with an expression statement".into(),
                statement.span,
            ));
        };
        let first = format!("$out_{}", current[0]);
        body.push(declaration(&first, value, statement.span));
        for label in &current[1..] {
            body.push(declaration(
                &format!("$out_{label}"),
                variable(&first, statement.span),
                statement.span,
            ));
        }
        outputs.extend(current.iter().cloned());
    }
    if labels.is_empty() {
        let message = "multiMap criteria should declare at least two outputs";
        return Err((message.into(), closure.body.span));
    }
    let span = closure.body.span;
    let entries = outputs
        .iter()
        .map(|label| MapEntry {
            key: MapKey::Expression(text(label, span)),
            value: variable(&format!("$out_{label}"), span),
        })
        .collect();
    body.push(statement(
        StatementKind::Expression(expression(ExpressionKind::Map(entries), span)),
        span,
    ));
    Ok((labels, with_body(closure, body)))
}

/// The labels of a statement, outermost first, and the statement inside them.
fn labels_of(statement: &Statement) -> (Vec<String>, &Statement) {
    match &statement.kind {
        StatementKind::Labeled { label, statement } => {
            let (mut labels, inner) = labels_of(statement);
            labels.insert(0, label.text.clone());
            (labels, inner)
        }
        _ => (Vec::new(), statement),
    }
}

/// Whether code has a `return` statement, outside closures.
fn returns(code: &[Statement]) -> bool {
    code.iter().any(|statement| match &statement.kind {
        StatementKind::Return(_) => true,
        StatementKind::If {
            then, otherwise, ..
        } => {
            returns(&then.statements) || otherwise.as_ref().is_some_and(|b| returns(&b.statements))
        }
        StatementKind::Try { body, catches } => {
            returns(&body.statements) || catches.iter().any(|c| returns(&c.body.statements))
        }
        StatementKind::Labeled { statement, .. } => returns(std::slice::from_ref(statement)),
        _ => false,
    })
}

/// A statement whose `return value` statements return `choice(value)` instead.
fn with_returns(mut statement: Statement, choice: &dyn Fn(Expression) -> Expression) -> Statement {
    let block = |block: Block| Block {
        statements: block
            .statements
            .into_iter()
            .map(|s| with_returns(s, choice))
            .collect(),
        span: block.span,
    };
    statement.kind = match statement.kind {
        StatementKind::Return(value) => {
            let value = value.unwrap_or_else(|| {
                expression(ExpressionKind::Literal(Literal::Null), statement.span)
            });
            StatementKind::Return(Some(choice(value)))
        }
        StatementKind::If {
            condition,
            then,
            otherwise,
        } => StatementKind::If {
            condition,
            then: block(then),
            otherwise: otherwise.map(block),
        },
        other => other,
    };
    statement
}

fn with_body(closure: &Closure, statements: Vec<Statement>) -> Closure {
    Closure {
        parameters: closure.parameters.clone(),
        body: Block {
            statements,
            span: closure.body.span,
        },
    }
}

fn declaration(name: &str, value: Expression, span: Span) -> Statement {
    statement(
        StatementKind::Declaration {
            names: vec![TypedName {
                name: Name {
                    text: name.into(),
                    span,
                },
                annotation: None,
            }],
            legacy_type: None,
            initializer: Some(value),
            destructure: false,
        },
        span,
    )
}

fn statement(kind: StatementKind, span: Span) -> Statement {
    Statement { kind, span }
}

fn expression(kind: ExpressionKind, span: Span) -> Expression {
    Expression { kind, span }
}

fn variable(name: &str, span: Span) -> Expression {
    expression(ExpressionKind::Variable(name.into()), span)
}

fn text(value: &str, span: Span) -> Expression {
    expression(ExpressionKind::Literal(Literal::String(value.into())), span)
}
