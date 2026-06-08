//! Lowering of workflows: takes, the `main:` body and emits.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::ast::{
    self, Argument, ExpressionKind as Syntax, NamedOutput, StatementKind as Kind, WorkflowTake,
};

use super::Lowering;
use crate::lowered::{Expression, Statement, StatementKind, Target, Workflow};

impl Lowering<'_> {
    /// Lowers a workflow as Nextflow's `visitWorkflow` checks it: the takes are declared, the
    /// body shares the workflow's scope, so the emits can read what it declares.
    pub(super) fn workflow(&mut self, workflow: &ast::Workflow) -> Workflow {
        self.scopes.push_frame(false);
        self.in_workflow = true;
        let mut takes = Vec::new();
        for take in &workflow.takes {
            match take {
                WorkflowTake::Named(typed) => {
                    let slot = self.declare_as(&typed.name, true);
                    takes.push((typed.name.text.as_str().into(), slot));
                }
                WorkflowTake::Statement(statement) => {
                    self.error("Invalid workflow take", statement.span);
                }
            }
        }
        self.implicit_declarations = true;
        let main = self.statements(&workflow.main.statements);
        self.implicit_declarations = false;
        let mut emits = Vec::new();
        let mut bare_emits = Vec::new();
        for emit in &workflow.emits {
            bare_emits.push(matches!(emit, NamedOutput::Named { value: None, .. }));
            match emit {
                NamedOutput::Named { target, value } => {
                    let name = &target.name;
                    let value = match value {
                        Some(value) => self.expression(value),
                        None => Expression {
                            kind: self.variable(&name.text, name.span),
                            span: name.span,
                        },
                    };
                    emits.push((name.text.as_str().into(), value));
                }
                // Nextflow names an emit that is only an expression `$out`.
                NamedOutput::Statement(ast::Statement {
                    kind: Kind::Expression(value),
                    ..
                }) => emits.push(("$out".into(), self.expression(value))),
                NamedOutput::Statement(statement) => {
                    self.error("Invalid workflow emit", statement.span);
                }
            }
        }
        let mut publishers = Vec::new();
        for publisher in &workflow.publishers {
            match publisher {
                NamedOutput::Named {
                    target,
                    value: Some(value),
                } => publishers.push((target.name.text.as_str().into(), self.expression(value))),
                NamedOutput::Named { target, .. } => {
                    self.error("Invalid workflow publish statement", target.name.span);
                }
                NamedOutput::Statement(statement) => {
                    self.error("Invalid workflow publish statement", statement.span);
                }
            }
        }
        for block in [&workflow.on_complete, &workflow.on_error]
            .into_iter()
            .flatten()
        {
            self.error(
                "Workflow `onComplete:` and `onError:` sections are not supported yet",
                block.span,
            );
        }
        self.in_workflow = false;
        let layout = self.scopes.pop_frame();
        Workflow {
            name: workflow.name.as_ref().map(|name| name.text.as_str().into()),
            slot_count: layout.slot_count,
            takes,
            main,
            emits,
            bare_emits,
            publishers,
            span: workflow.span,
        }
    }

    /// `ch.set { x }` or `ch.tap { x }` in a workflow body, which assigns the channel to `x` and
    /// declares it, as Nextflow's `checkSetAssignment` does; `None` for other statements.
    pub(super) fn set_statement(&mut self, statement: &ast::Statement) -> Option<Statement> {
        if let Some((target, names)) = into_parts(statement) {
            return self.lower_into(target, &names, statement.span);
        }
        let (target, variable, span) = set_parts(statement)?;
        let value = self.expression(target);
        let place = self.assigned_variable(variable, span)?;
        Some(Statement {
            kind: StatementKind::Assign {
                target: Target::Variable(place),
                value,
            },
            span: statement.span,
        })
    }
}

impl Lowering<'_> {
    /// Legacy syntax: `ch.into { a; b }` of DSL1, which gives the channel to each name. The
    /// first name takes the channel, and the others the first.
    fn lower_into(
        &mut self,
        target: &ast::Expression,
        names: &[(&str, nextflow_parser::Span)],
        span: nextflow_parser::Span,
    ) -> Option<Statement> {
        let mut value = self.expression(target);
        let mut assignments = Vec::new();
        for (name, name_span) in names {
            let place = self.assigned_variable(name, *name_span)?;
            assignments.push(Statement {
                kind: StatementKind::Assign {
                    target: Target::Variable(place),
                    value,
                },
                span,
            });
            value = Expression {
                kind: crate::lowered::ExpressionKind::Variable(place),
                span: *name_span,
            };
        }
        // A block of statements, as an `if` that always runs.
        Some(Statement {
            kind: StatementKind::If {
                condition: Expression {
                    kind: crate::lowered::ExpressionKind::Literal(crate::value::Value::Boolean(
                        true,
                    )),
                    span,
                },
                then: assignments,
                otherwise: Vec::new(),
            },
            span,
        })
    }
}

/// Legacy syntax: the parts of `source.into { a; b }` of DSL1: the source, and the names with
/// their positions.
pub(super) fn into_parts(
    statement: &ast::Statement,
) -> Option<(&ast::Expression, Vec<(&str, nextflow_parser::Span)>)> {
    let Kind::Expression(expression) = &statement.kind else {
        return None;
    };
    let Syntax::MethodCall {
        target: Some(target),
        name,
        arguments,
        ..
    } = &expression.kind
    else {
        return None;
    };
    let [argument] = arguments.as_slice() else {
        return None;
    };
    let Syntax::Closure(closure) = &argument.value.kind else {
        return None;
    };
    if name.text != "into" {
        return None;
    }
    let names = closure
        .body
        .statements
        .iter()
        .map(|statement| match &statement.kind {
            Kind::Expression(ast::Expression {
                kind: Syntax::Variable(name),
                span,
            }) => Some((name.as_str(), *span)),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some((target, names))
}

/// The parts of the statement `source.set { x }` or `source.tap { x }`: the source, and the
/// name `x` with its position.
pub(super) fn set_parts(
    statement: &ast::Statement,
) -> Option<(&ast::Expression, &str, nextflow_parser::Span)> {
    let Kind::Expression(expression) = &statement.kind else {
        return None;
    };
    let Syntax::MethodCall {
        target: Some(target),
        name,
        arguments,
        ..
    } = &expression.kind
    else {
        return None;
    };
    if !matches!(name.text.as_str(), "set" | "tap") {
        return None;
    }
    let (variable, span) = set_target(arguments)?;
    Some((target, variable, span))
}

/// The variable of `set { x }`: a closure whose body is that one name.
pub(super) fn set_target(arguments: &[Argument]) -> Option<(&str, nextflow_parser::Span)> {
    let [argument] = arguments else { return None };
    let Syntax::Closure(closure) = &argument.value.kind else {
        return None;
    };
    let [statement] = closure.body.statements.as_slice() else {
        return None;
    };
    match &statement.kind {
        Kind::Expression(ast::Expression {
            kind: Syntax::Variable(name),
            span,
        }) => Some((name, *span)),
        _ => None,
    }
}
