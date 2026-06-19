//! Lowering of workflows: takes, the `main:` body and emits.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::ast::{
    self, Argument, ExpressionKind as Syntax, NamedOutput, StatementKind as Kind, WorkflowTake,
};

use super::Lowering;
use crate::lowered::{Expression, ExpressionKind, Statement, StatementKind, Target, Workflow};

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
                        // Legacy syntax: `name = value` also sets the variable, which later
                        // emits may read, as in `sample_size = meta.count()`.
                        Some(value) => {
                            let span = value.span;
                            let value = self.expression(value);
                            self.implicit_declarations = true;
                            let place = self.assigned_variable(&name.text, name.span);
                            self.implicit_declarations = false;
                            match place {
                                Some(place) => Expression {
                                    kind: ExpressionKind::Assign {
                                        target: Box::new(Target::Variable(place)),
                                        update: None,
                                        value: Box::new(value),
                                    },
                                    span,
                                },
                                None => value,
                            }
                        }
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
