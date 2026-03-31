//! Running statements. As in Groovy, a block's value is the value of its last statement.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_parser::Span;

use super::{AtSpan, Error, Frame, Interpreter, thrown};
use crate::exception::Exception;
use crate::lowered::{Place, Statement, StatementKind, Target, Update};
use crate::value::Value;

/// How a statement or block ended.
pub(super) enum Completion {
    /// It ran to its end, with the value of its last statement.
    Normal(Value),
    /// A `return` ended the function or closure.
    Return(Value),
    /// A `break` of legacy code ended the loop.
    Break,
    /// A `continue` of legacy code ended the round of the loop.
    Continue,
}

impl Completion {
    /// The value of a function or closure that ended so.
    pub(super) fn value(self) -> Value {
        match self {
            Completion::Normal(value) | Completion::Return(value) => value,
            Completion::Break | Completion::Continue => Value::Null,
        }
    }
}

impl Interpreter<'_> {
    pub(super) fn block(
        &mut self,
        frame: &mut Frame,
        statements: &[Statement],
    ) -> Result<Completion, Error> {
        let mut last = Value::Null;
        for statement in statements {
            let completion = match self.statement(frame, statement) {
                Ok(completion) => completion,
                Err(error) => {
                    // The innermost statement knows the script of the error.
                    if self.error_module.is_none() {
                        self.error_module = Some(frame.module.clone());
                    }
                    return Err(error.or_at(statement.span));
                }
            };
            match completion {
                Completion::Normal(value) => last = value,
                done => return Ok(done),
            }
        }
        Ok(Completion::Normal(last))
    }

    fn statement(&mut self, frame: &mut Frame, statement: &Statement) -> Result<Completion, Error> {
        let span = statement.span;
        let value = match &statement.kind {
            StatementKind::Expression(expression) => self.expression(frame, expression)?,
            StatementKind::Declare {
                slots,
                value,
                destructure,
            } => {
                let value = match value {
                    Some(value) => self.expression(frame, value)?,
                    None => Value::Null,
                };
                if *destructure {
                    for (index, slot) in slots.iter().enumerate() {
                        let item = self.element(&value, index, span)?;
                        frame.set(Place::Local(*slot), item);
                    }
                } else if let Some(slot) = slots.first() {
                    frame.set(Place::Local(*slot), value.clone());
                }
                value
            }
            StatementKind::Assign { target, value } => {
                let value = self.expression(frame, value)?;
                let target = self.target(frame, target)?;
                self.store(frame, &target, value.clone(), span)?;
                value
            }
            StatementKind::Update {
                target,
                update,
                value,
            } => {
                let target = self.target(frame, target)?;
                let current = self.load(frame, &target, span)?;
                let value = match update {
                    Update::Elvis if current.truth() => current,
                    Update::Elvis => self.expression(frame, value)?,
                    Update::Operator(operator) => {
                        let right = self.expression(frame, value)?;
                        current.apply(*operator, &right).at(span)?
                    }
                };
                self.store(frame, &target, value.clone(), span)?;
                value
            }
            StatementKind::Destructure { places, value } => {
                let value = self.expression(frame, value)?;
                for (index, place) in places.iter().enumerate() {
                    let item = self.element(&value, index, span)?;
                    frame.set(*place, item);
                }
                value
            }
            StatementKind::If {
                condition,
                then,
                otherwise,
            } => {
                let branch = if self.expression(frame, condition)?.truth() {
                    then
                } else {
                    otherwise
                };
                return self.block(frame, branch);
            }
            StatementKind::Try { body, catches } => {
                let error = match self.block(frame, body) {
                    Err(Error::Thrown { exception, span }) => Error::Thrown { exception, span },
                    other => return other,
                };
                let Error::Thrown { exception, .. } = &error else {
                    unreachable!("only thrown errors are caught")
                };
                let caught = catches.iter().find(|catch| {
                    if catch.classes.is_empty() {
                        exception.is_instance("Exception")
                    } else {
                        catch
                            .classes
                            .iter()
                            .any(|class| exception.is_instance(class))
                    }
                });
                let Some(catch) = caught else {
                    return Err(error);
                };
                self.error_module = None;
                let place = Place::Local(catch.slot);
                frame.set(place, Value::Exception(exception.clone()));
                return self.block(frame, &catch.body);
            }
            StatementKind::Return(value) => {
                let value = match value {
                    Some(value) => self.expression(frame, value)?,
                    None => Value::Null,
                };
                return Ok(Completion::Return(value));
            }
            StatementKind::Throw(value) => {
                return Err(match self.expression(frame, value)? {
                    Value::Exception(exception) => Error::Thrown { exception, span },
                    other => thrown(
                        Exception::new(
                            "ClassCastException",
                            format!(
                                "Cannot cast object '{other}' with class '{}' to class \
                                 'java.lang.Throwable'",
                                other.java_class_name()
                            ),
                        ),
                        span,
                    ),
                });
            }
            StatementKind::ForIn { slot, items, body } => {
                let items = self.expression(frame, items)?;
                for item in self.loop_items(&items, span)? {
                    frame.set(Place::Local(*slot), item);
                    match self.block(frame, body)? {
                        Completion::Break => break,
                        done @ Completion::Return(_) => return Ok(done),
                        Completion::Normal(_) | Completion::Continue => {}
                    }
                }
                Value::Null
            }
            StatementKind::Loop {
                condition,
                update,
                body,
            } => {
                loop {
                    if let Some(condition) = condition
                        && !self.expression(frame, condition)?.truth()
                    {
                        break;
                    }
                    match self.block(frame, body)? {
                        Completion::Break => break,
                        done @ Completion::Return(_) => return Ok(done),
                        Completion::Normal(_) | Completion::Continue => {}
                    }
                    self.block(frame, update)?;
                }
                Value::Null
            }
            StatementKind::Break => return Ok(Completion::Break),
            StatementKind::Continue => return Ok(Completion::Continue),
            StatementKind::Assert {
                condition,
                message,
                text,
            } => {
                if !self.expression(frame, condition)?.truth() {
                    let message = match message {
                        Some(message) => {
                            let message = self.expression(frame, message)?;
                            format!("{message}. Expression: {text}")
                        }
                        // Groovy's power assert also prints the values of the parts.
                        None => format!("assert {text}"),
                    };
                    return Err(thrown(Exception::assertion(message), span));
                }
                Value::Null
            }
        };
        Ok(Completion::Normal(value))
    }

    /// The elements that `for (x in items)` visits, as Groovy's `iterator()` gives them: a
    /// map gives its entries, a string its characters, `null` nothing.
    fn loop_items(&mut self, items: &Value, span: Span) -> Result<Vec<Value>, Error> {
        Ok(match items {
            Value::Null => Vec::new(),
            Value::List(list) => list.borrow().items.clone(),
            Value::Set(set) => set.borrow().items.iter().map(|key| key.0.clone()).collect(),
            Value::Map(map) => map
                .borrow()
                .entries
                .iter()
                .map(|(key, value)| Value::MapEntry(Rc::new((key.0.clone(), value.clone()))))
                .collect(),
            Value::String(text) | Value::GString(text) => {
                text.chars().map(|c| Value::string(c.to_string())).collect()
            }
            other => {
                let message = format!("iterating over {} with `for`", other.class_name());
                return Err(super::unsupported(message, span));
            }
        })
    }

    /// An assignment used as a value in legacy code, like `a = b = c`: the value stored.
    pub(super) fn assign_value(
        &mut self,
        frame: &mut Frame,
        target: &Target,
        update: Option<Update>,
        value: &crate::lowered::Expression,
        span: Span,
    ) -> Result<Value, Error> {
        let target = self.target(frame, target)?;
        let result = match update {
            None => self.expression(frame, value)?,
            Some(update) => {
                let current = self.load(frame, &target, span)?;
                match update {
                    Update::Elvis if current.truth() => current,
                    Update::Elvis => self.expression(frame, value)?,
                    Update::Operator(operator) => {
                        let right = self.expression(frame, value)?;
                        current.apply(operator, &right).at(span)?
                    }
                }
            }
        };
        self.store(frame, &target, result.clone(), span)?;
        Ok(result)
    }

    /// `x++` or `x--` of legacy code: adds or subtracts one, and gives the value before.
    pub(super) fn step(
        &mut self,
        frame: &mut Frame,
        target: &Target,
        increment: bool,
        span: Span,
    ) -> Result<Value, Error> {
        let target = self.target(frame, target)?;
        let current = self.load(frame, &target, span)?;
        let operator = if increment {
            crate::value::Operator::Add
        } else {
            crate::value::Operator::Subtract
        };
        let next = current.apply(operator, &Value::integer(1)).at(span)?;
        self.store(frame, &target, next, span)?;
        Ok(current)
    }

    /// Evaluates the parts of an assignment target that must only run once.
    fn target(&mut self, frame: &mut Frame, target: &Target) -> Result<Resolved, Error> {
        Ok(match target {
            Target::Variable(place) => Resolved::Variable(*place),
            Target::Property { target, name } => {
                Resolved::Property(self.expression(frame, target)?, name.clone())
            }
            Target::Index { target, index } => {
                let object = self.expression(frame, target)?;
                Resolved::Index(object, self.expression(frame, index)?)
            }
        })
    }

    fn load(&mut self, frame: &Frame, target: &Resolved, span: Span) -> Result<Value, Error> {
        match target {
            Resolved::Variable(place) => Ok(frame.get(*place)),
            Resolved::Property(object, name) => self.property(object, name, span),
            Resolved::Index(object, index) => self.index(object, std::slice::from_ref(index), span),
        }
    }

    fn store(
        &mut self,
        frame: &Frame,
        target: &Resolved,
        value: Value,
        span: Span,
    ) -> Result<(), Error> {
        match target {
            Resolved::Variable(place) => {
                frame.set(*place, value);
                Ok(())
            }
            Resolved::Property(object, name) => self.set_property(object, name, value, span),
            Resolved::Index(object, index) => self.set_index(object, index, value, span),
        }
    }

    /// The element of a multiple assignment: `value[index]` with Groovy's rules.
    fn element(&mut self, value: &Value, index: usize, span: Span) -> Result<Value, Error> {
        let index = Value::integer(index as i32);
        self.index(value, std::slice::from_ref(&index), span)
    }
}

/// An assignment target whose object and index are evaluated.
enum Resolved {
    Variable(Place),
    Property(Value, Rc<str>),
    Index(Value, Value),
}
