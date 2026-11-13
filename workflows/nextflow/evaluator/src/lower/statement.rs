//! Lowering of statements and blocks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::ast::{self, AssignOperator, ExpressionKind as Syntax, StatementKind as Kind};

use super::Lowering;
use crate::lowered::{Catch, Place, Statement, StatementKind, SwitchCase, Target, Update};
use crate::value::{Operator, Value};

impl Lowering<'_> {
    /// Lowers a block in its own scope.
    pub(super) fn block(&mut self, block: &ast::Block) -> Vec<Statement> {
        self.scopes.push_block();
        let statements = self.statements(&block.statements);
        self.scopes.pop_block();
        statements
    }

    pub(super) fn statements(&mut self, statements: &[ast::Statement]) -> Vec<Statement> {
        statements
            .iter()
            .filter_map(|statement| self.statement(statement))
            .collect()
    }

    pub(super) fn statement(&mut self, statement: &ast::Statement) -> Option<Statement> {
        let span = statement.span;
        // Nextflow's `set` sets the variable in the binding, also in a block of the script.
        if (self.in_workflow || self.in_initializer)
            && !self.scopes.in_closure()
            && let Some(set) = self.set_statement(statement)
        {
            return Some(set);
        }
        let kind = match &statement.kind {
            Kind::Expression(expression) => StatementKind::Expression(self.expression(expression)),
            Kind::Declaration {
                names,
                legacy_type,
                initializer,
                destructure,
                ..
            } => {
                let value = initializer.as_ref().map(|value| self.expression(value));
                let slots = names
                    .iter()
                    .map(|typed| {
                        self.declare_typed(
                            &typed.name,
                            typed.annotation.as_ref().or(legacy_type.as_ref()),
                        )
                    })
                    .collect();
                StatementKind::Declare {
                    slots,
                    classes: names
                        .iter()
                        .map(|typed| self.scopes.class(&typed.name.text))
                        .collect(),
                    value,
                    destructure: *destructure,
                }
            }
            Kind::Assign {
                target,
                operator,
                value,
            } => {
                let value = self.expression(value);
                // Legacy syntax: a script of the legacy parser may set a name of its binding
                // like `projectDir`, which keeps the value it had, as it is the same here.
                if let Syntax::Variable(name) = &target.kind
                    && super::names::SCRIPT_CONSTANTS.contains(&name.as_str())
                    && *operator == AssignOperator::Assign
                    && !self.in_process
                    && !self.in_function
                    && !self.scopes.in_closure()
                    && self.scopes.resolve(name).is_none()
                {
                    self.legacy("Built-in constant or namespace cannot be re-assigned", span);
                    return Some(Statement {
                        kind: StatementKind::Expression(value),
                        span,
                    });
                }
                let target = self.target(target)?;
                match update(*operator) {
                    None => StatementKind::Assign { target, value },
                    Some(update) => StatementKind::Update {
                        target,
                        update,
                        value,
                    },
                }
            }
            Kind::MultipleAssign { names, value } => {
                let value = self.expression(value);
                let places = names
                    .iter()
                    .map(|name| self.assigned_variable(&name.text, name.span))
                    .collect::<Option<Vec<Place>>>()?;
                StatementKind::Destructure {
                    places,
                    classes: names
                        .iter()
                        .map(|name| self.scopes.class(&name.text))
                        .collect(),
                    value,
                }
            }
            Kind::If {
                condition,
                then,
                otherwise,
            } => StatementKind::If {
                condition: self.expression(condition),
                then: self.block(then),
                otherwise: otherwise
                    .as_ref()
                    .map(|block| self.block(block))
                    .unwrap_or_default(),
            },
            Kind::Try { body, catches } => StatementKind::Try {
                body: self.block(body),
                catches: catches.iter().map(|catch| self.catch(catch)).collect(),
            },
            Kind::Return(value) => {
                StatementKind::Return(value.as_ref().map(|value| self.expression(value)))
            }
            Kind::Throw(value) => StatementKind::Throw(self.expression(value)),
            Kind::Assert { condition, message } => StatementKind::Assert {
                text: condition.span.text(self.source).into(),
                condition: self.expression(condition),
                message: message.as_ref().map(|message| self.expression(message)),
            },
            // Labels only matter in `branch` and `multiMap` criteria, which come later.
            Kind::Labeled { statement, .. } => return self.statement(statement),
            Kind::ForIn {
                variable,
                annotation,
                items,
                body,
            } => {
                let items = self.expression(items);
                self.scopes.push_block();
                let slot = self.declare_typed(variable, annotation.as_ref());
                let class = self.scopes.class(&variable.text);
                let body = self.block(body);
                self.scopes.pop_block();
                StatementKind::ForIn {
                    slot,
                    class,
                    items,
                    body,
                }
            }
            // The variables of the first part belong to the loop.
            Kind::For {
                init,
                condition,
                update,
                body,
            } => {
                self.scopes.push_block();
                let init = init.as_ref().and_then(|init| self.statement(init));
                let condition = condition.as_ref().map(|value| self.expression(value));
                let update = self.statements(update);
                let body = self.block(body);
                self.scopes.pop_block();
                let kind = StatementKind::Loop {
                    condition,
                    update,
                    body,
                };
                let looped = Statement { kind, span };
                return Some(Statement {
                    kind: StatementKind::If {
                        condition: crate::lowered::Expression {
                            kind: crate::lowered::ExpressionKind::Literal(Value::Boolean(true)),
                            span,
                        },
                        then: init.into_iter().chain([looped]).collect(),
                        otherwise: Vec::new(),
                    },
                    span,
                });
            }
            Kind::While { condition, body } => StatementKind::Loop {
                condition: Some(self.expression(condition)),
                update: Vec::new(),
                body: self.block(body),
            },
            Kind::Switch { subject, cases } => StatementKind::Switch {
                subject: self.expression(subject),
                cases: cases
                    .iter()
                    .map(|case| {
                        let values = case
                            .values
                            .iter()
                            .map(|value| self.expression(value))
                            .collect();
                        self.scopes.push_block();
                        let body = self.statements(&case.body);
                        self.scopes.pop_block();
                        SwitchCase {
                            values,
                            default: case.default,
                            body,
                        }
                    })
                    .collect(),
            },
            // A process inside a block belongs to a DSL1 script, which fails to load earlier.
            Kind::Process(process) => {
                self.error("Unexpected input: 'process'", process.span);
                return None;
            }
            // Legacy syntax: an include inside an `if` of the script, or inside a workflow as
            // Nextflow 21.04 takes it; elsewhere the legacy parser does not take it either.
            Kind::Include(include) => match self.conditional_includes.get(&include.span).cloned() {
                Some(indexes) => {
                    let params = include.params.as_ref().map(|params| {
                        Box::new(Statement {
                            kind: StatementKind::IncludeParams {
                                indexes: indexes.clone(),
                                add: params.add,
                                arguments: self.arguments(&params.arguments),
                            },
                            span: params.span,
                        })
                    });
                    StatementKind::Include { indexes, params }
                }
                None => {
                    self.error("Unexpected input: 'include'", span);
                    return None;
                }
            },
            Kind::Break => StatementKind::Break,
            Kind::Continue => StatementKind::Continue,
            Kind::Empty => return None,
        };
        Some(Statement { kind, span })
    }

    fn catch(&mut self, catch: &ast::Catch) -> Catch {
        self.scopes.push_block();
        let slot = self.declare(&catch.name);
        let classes = catch
            .types
            .iter()
            .filter_map(|name| name.parts.last())
            .map(|name| name.text.as_str().into())
            .collect();
        let body = self.block(&catch.body);
        self.scopes.pop_block();
        Catch {
            slot,
            classes,
            body,
        }
    }

    /// The target of an assignment: a variable, a property or an index.
    pub(super) fn target(&mut self, target: &ast::Expression) -> Option<Target> {
        match &target.kind {
            Syntax::Variable(name)
                if self.scopes.resolve(name).is_none()
                    && let Some(crate::lowered::ExpressionKind::Binding(binding)) = self
                        .lookup
                        .and_then(|lookup| lookup(name, self.scopes.in_closure())) =>
            {
                Some(Target::Binding(binding))
            }
            // A field of the class being lowered, which a method sets by its name alone.
            Syntax::Variable(name)
                if self.scopes.resolve(name).is_none()
                    && let Some(crate::lowered::ExpressionKind::Property {
                        target: object,
                        name,
                        ..
                    }) = self.class_member(name, target.span) =>
            {
                Some(Target::Property {
                    target: *object,
                    name,
                })
            }
            Syntax::Variable(name) => self
                .assigned_variable(name, target.span)
                .map(|place| self.variable_target(name, place)),
            Syntax::Property {
                target: object,
                name,
                ..
            } => Some(Target::Property {
                target: self.expression(object),
                name: name.text.as_str().into(),
            }),
            Syntax::Index {
                target: object,
                index,
            } if index.len() == 1 => Some(Target::Index {
                target: self.expression(object),
                index: self.expression(&index[0]),
            }),
            _ => {
                self.error("Invalid assignment target", target.span);
                None
            }
        }
    }

    pub(super) fn variable_target(&self, name: &str, place: Place) -> Target {
        match self.scopes.class(name) {
            Some(class) => Target::TypedVariable { place, class },
            None => Target::Variable(place),
        }
    }

    /// A variable on the left of `=`, which must be declared in functions and closures.
    pub(super) fn assigned_variable(
        &mut self,
        name: &str,
        span: nextflow_parser::Span,
    ) -> Option<Place> {
        if let Some(place) = self.scopes.resolve(name) {
            return Some(place);
        }
        if super::names::SCRIPT_CONSTANTS.contains(&name) || (self.in_process && name == "task") {
            self.error("Built-in constant or namespace cannot be re-assigned", span);
            return None;
        }
        // In a workflow, an operator name resolves, so Nextflow accepts the assignment even in a
        // closure; like Groovy's binding, the name then holds the value.
        if self.in_workflow && super::names::OPERATORS.contains(&name) {
            return Some(Place::Local(self.scopes.declare_in_frame(name)));
        }
        // Legacy syntax: Groovy keeps the variable in the binding of the script; here it
        // belongs to the function or closure.
        if !self.implicit_declarations {
            self.legacy(format!("`{name}` was assigned but not declared"), span);
        } else if self.scopes.in_closure() {
            self.legacy("Variables in a closure should be declared with `def`", span);
        }
        Some(Place::Local(self.scopes.declare_in_frame(name)))
    }
}

pub(super) fn update(operator: AssignOperator) -> Option<Update> {
    use AssignOperator as A;
    let operator = match operator {
        A::Assign => return None,
        A::Elvis => return Some(Update::Elvis),
        A::Add => Operator::Add,
        A::Subtract => Operator::Subtract,
        A::Multiply => Operator::Multiply,
        A::Divide => Operator::Divide,
        A::Modulo => Operator::Modulo,
        A::Power => Operator::Power,
        A::BitAnd => Operator::BitAnd,
        A::BitOr => Operator::BitOr,
        A::Xor => Operator::Xor,
        A::LeftShift => Operator::LeftShift,
        A::RightShift => Operator::RightShift,
        A::UnsignedShift => Operator::UnsignedShift,
    };
    Some(Update::Operator(operator))
}
