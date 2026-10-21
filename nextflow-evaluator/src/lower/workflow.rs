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
        let mut main = self.statements(&workflow.main.statements);
        self.implicit_declarations = false;
        // The `onComplete:` section runs as the handler that `workflow.onComplete { ... }` at
        // the end of the body adds. Nextflow 26.04.6 fails to call the `onError:` section, so it
        // is checked but never runs.
        if let Some(block) = &workflow.on_complete {
            main.extend(self.statement(&handler_statement("onComplete", block)));
        }
        if let Some(block) = &workflow.on_error {
            drop(self.statement(&handler_statement("onError", block)));
        }
        let mut emits = Vec::new();
        let mut bare_emits = Vec::new();
        for emit in &workflow.emits {
            bare_emits.push(matches!(emit, NamedOutput::Named { value: None, .. }));
            match emit {
                NamedOutput::Named { target, value } => {
                    let name = &target.name;
                    let value = match value {
                        // Legacy emits assign names for later emits while retaining an existing
                        // body variable's value for closures that read it later.
                        Some(value) => {
                            let span = value.span;
                            let value = self.expression(value);
                            let place = match self.scopes.resolve(&name.text) {
                                Some(_) => None,
                                None => {
                                    self.implicit_declarations = true;
                                    let place = self.assigned_variable(&name.text, name.span);
                                    self.implicit_declarations = false;
                                    place
                                }
                            };
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
        // Typed workflows refuse `set` and `tap` when they run, as Nextflow does.
        if self.types {
            let name = match &statement.kind {
                Kind::Expression(ast::Expression {
                    kind: Syntax::MethodCall { name, .. },
                    ..
                }) => name.text.as_str().into(),
                _ => "set".into(),
            };
            let call = ExpressionKind::CallMethod {
                target: Box::new(value),
                name,
                access: ast::Access::Normal,
                arguments: Default::default(),
            };
            let expression = Expression {
                kind: call,
                span: statement.span,
            };
            return Some(Statement {
                kind: StatementKind::Expression(expression),
                span: statement.span,
            });
        }
        Some(Statement {
            kind: StatementKind::Assign {
                target: self.variable_target(variable, place),
                value,
            },
            span: statement.span,
        })
    }
}

/// The statement `workflow.<handler> { <block> }`.
fn handler_statement(handler: &str, block: &ast::Block) -> ast::Statement {
    let span = block.span;
    let expression = |kind| ast::Expression { kind, span };
    let closure = ast::Closure {
        parameters: None,
        body: block.clone(),
    };
    let call = Syntax::MethodCall {
        target: Some(Box::new(expression(Syntax::Variable("workflow".into())))),
        name: ast::Name {
            text: handler.into(),
            span,
        },
        access: ast::Access::Normal,
        arguments: vec![Argument {
            name: None,
            value: expression(Syntax::Closure(Box::new(closure))),
        }],
        parentheses: true,
    };
    ast::Statement {
        kind: Kind::Expression(expression(call)),
        span,
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
