//! Execution of statements.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;
use ruff_python_ast::{self as ast, ExceptHandler, Expr, Pattern, Stmt};
use ruff_text_size::Ranged;

use super::{Frame, FrameKind, Interpreter, Operator};
use crate::error::{Error, ErrorKind};
use crate::types;
use crate::value::{Args, Code, Declared, Function, Name, Scope, Value};

/// How a statement ended.
pub enum Flow {
    Next,
    Return(Value),
    Break,
    Continue,
}

impl Interpreter {
    pub(crate) fn exec_suite(&mut self, frame: &Frame, suite: &[Stmt]) -> Result<Flow, Error> {
        for statement in suite {
            match self.exec(frame, statement)? {
                Flow::Next => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Next)
    }

    /// Executes one statement; errors get the statement's line if they have none yet.
    pub fn exec(&mut self, frame: &Frame, statement: &Stmt) -> Result<Flow, Error> {
        self.count_step()?;
        self.exec_inner(frame, statement)
            .map_err(|error| error.with_location(frame.source.location(statement.start())))
    }

    fn exec_inner(&mut self, frame: &Frame, statement: &Stmt) -> Result<Flow, Error> {
        match statement {
            Stmt::Expr(expression) => {
                if let Expr::EllipsisLiteral(ellipsis) = &*expression.value
                    && let Some(hook) = self.hook.clone()
                    && hook.run(self, frame, ellipsis.start())?
                {
                    return Ok(Flow::Next);
                }
                self.eval(frame, &expression.value)?;
            }
            Stmt::Assign(assign) => {
                let value = self.eval(frame, &assign.value)?;
                for target in &assign.targets {
                    self.assign(frame, target, value.clone())?;
                }
            }
            Stmt::AnnAssign(assign) => {
                if let Some(value) = &assign.value {
                    let value = self.eval(frame, value)?;
                    self.assign(frame, &assign.target, value)?;
                } else if frame.kind == FrameKind::Class
                    && let Expr::Name(name) = &*assign.target
                {
                    // Annotations of a class body, for dataclasses.
                    record_annotation(frame, name.id.as_str(), None);
                }
                if frame.kind == FrameKind::Class
                    && let Expr::Name(name) = &*assign.target
                    && assign.value.is_some()
                {
                    record_annotation(frame, name.id.as_str(), Some(()));
                }
            }
            Stmt::AugAssign(assign) => self.augmented_assign(frame, assign)?,
            Stmt::If(branch) => {
                let test = self.eval(frame, &branch.test)?;
                if self.truthy(&test)? {
                    return self.exec_suite(frame, &branch.body);
                }
                for clause in &branch.elif_else_clauses {
                    let taken = match &clause.test {
                        Some(test) => {
                            let test = self.eval(frame, test)?;
                            self.truthy(&test)?
                        }
                        None => true,
                    };
                    if taken {
                        return self.exec_suite(frame, &clause.body);
                    }
                }
            }
            Stmt::For(each) => return self.exec_for(frame, each),
            Stmt::While(each) => loop {
                let test = self.eval(frame, &each.test)?;
                if !self.truthy(&test)? {
                    return self.exec_suite(frame, &each.orelse);
                }
                match self.exec_suite(frame, &each.body)? {
                    Flow::Break => break,
                    Flow::Return(value) => return Ok(Flow::Return(value)),
                    Flow::Next | Flow::Continue => {}
                }
            },
            Stmt::FunctionDef(def) => self.define_function(frame, def)?,
            Stmt::ClassDef(class) => self.define_class(frame, class)?,
            Stmt::Return(statement) => {
                let value = match &statement.value {
                    Some(value) => self.eval(frame, value)?,
                    None => Value::None,
                };
                return Ok(Flow::Return(value));
            }
            Stmt::Pass(_) => {}
            Stmt::Break(_) => return Ok(Flow::Break),
            Stmt::Continue(_) => return Ok(Flow::Continue),
            Stmt::Global(_) | Stmt::Nonlocal(_) => {}
            Stmt::Delete(delete) => {
                for target in &delete.targets {
                    self.delete(frame, target)?;
                }
            }
            Stmt::Raise(raise) => return Err(self.raise(frame, raise)?),
            Stmt::Assert(assert) => {
                let test = self.eval(frame, &assert.test)?;
                if !self.truthy(&test)? {
                    let message = match &assert.msg {
                        Some(message) => {
                            let message = self.eval(frame, message)?;
                            self.to_str(&message)?
                        }
                        None => String::new(),
                    };
                    return Err(if message.is_empty() {
                        let class = types::exception_class("AssertionError").expect("exists");
                        Error::from_value(crate::error::exception(&class, Vec::new()))
                    } else {
                        Error::new("AssertionError", message)
                    });
                }
            }
            Stmt::Try(attempt) => return self.exec_try(frame, attempt),
            Stmt::With(with) => return self.exec_with(frame, with, 0),
            Stmt::Import(import) => self.exec_import(frame, import)?,
            Stmt::ImportFrom(import) => self.exec_import_from(frame, import)?,
            Stmt::Match(choice) => return self.exec_match(frame, choice),
            Stmt::TypeAlias(_) => return Err(Error::unsupported("type aliases")),
            Stmt::IpyEscapeCommand(_) => return Err(Error::unsupported("IPython commands")),
        }
        Ok(Flow::Next)
    }

    fn exec_for(&mut self, frame: &Frame, each: &ast::StmtFor) -> Result<Flow, Error> {
        let iterable = self.eval(frame, &each.iter)?;
        // Ranges and iterators are walked lazily; everything else is read at once.
        let mut lazy = match iterable.plain() {
            Value::Range(_) | Value::Iterator(_) => Some(self.make_iterator(&iterable)?),
            _ => None,
        };
        let mut items = match &lazy {
            Some(_) => Vec::new().into_iter(),
            None => self.iterate(&iterable)?.into_iter(),
        };
        loop {
            let item = match &mut lazy {
                Some(Value::Iterator(iterator)) => iterator.borrow_mut().next_value(),
                _ => items.next(),
            };
            let Some(item) = item else {
                return self.exec_suite(frame, &each.orelse);
            };
            self.count_step()?;
            self.assign(frame, &each.target, item)?;
            match self.exec_suite(frame, &each.body)? {
                Flow::Break => break,
                Flow::Return(value) => return Ok(Flow::Return(value)),
                Flow::Next | Flow::Continue => {}
            }
        }
        if lazy.is_some() {
            lazy = None;
        }
        let _ = lazy;
        Ok(Flow::Next)
    }

    fn augmented_assign(
        &mut self,
        frame: &Frame,
        assign: &ast::StmtAugAssign,
    ) -> Result<(), Error> {
        let operator = Operator::from_ast(assign.op);
        match &*assign.target {
            Expr::Name(name) => {
                let current = self.lookup(frame, name.id.as_str())?;
                let value = self.eval(frame, &assign.value)?;
                let result = self.in_place(operator, &current, &value)?;
                self.bind(frame, name.id.as_str(), result);
            }
            Expr::Attribute(attribute) => {
                let object = self.eval(frame, &attribute.value)?;
                let current = self.get_attribute(&object, attribute.attr.as_str())?;
                let value = self.eval(frame, &assign.value)?;
                let result = self.in_place(operator, &current, &value)?;
                self.set_attribute(&object, attribute.attr.as_str(), result)?;
            }
            Expr::Subscript(subscript) => {
                let object = self.eval(frame, &subscript.value)?;
                let key = self.eval(frame, &subscript.slice)?;
                let current = self.get_item(&object, &key)?;
                let value = self.eval(frame, &assign.value)?;
                let result = self.in_place(operator, &current, &value)?;
                self.set_item(&object, &key, result)?;
            }
            _ => {
                return Err(Error::new(
                    "SyntaxError",
                    "illegal expression for augmented assignment",
                ));
            }
        }
        Ok(())
    }

    /// `a op= b`: lists, sets and dicts change in place, other values get a new value.
    fn in_place(
        &mut self,
        operator: Operator,
        current: &Value,
        value: &Value,
    ) -> Result<Value, Error> {
        match (operator, current.plain()) {
            (Operator::Add, Value::List(items)) => {
                let extra = self.iterate(value)?;
                items.borrow_mut().extend(extra);
                Ok(current.plain().clone())
            }
            (Operator::Mul, Value::List(items)) if value.as_int().is_some() => {
                let count = value.as_int().unwrap_or(0).max(0) as usize;
                let original = items.borrow().clone();
                let mut repeated = Vec::new();
                for _ in 0..count {
                    repeated.extend(original.iter().cloned());
                }
                *items.borrow_mut() = repeated;
                Ok(current.plain().clone())
            }
            (Operator::BitOr, Value::Dict(dict)) => {
                for (key, item) in self.mapping_items(value)? {
                    dict.borrow_mut().entries.insert(self.key(&key)?, item);
                }
                Ok(current.plain().clone())
            }
            (
                Operator::BitOr | Operator::BitAnd | Operator::Sub | Operator::BitXor,
                Value::Set(set),
            ) => {
                let result = self.binary(operator, current, value)?;
                if let Value::Set(result) = result.plain() {
                    *set.borrow_mut() = result.borrow().clone();
                }
                Ok(current.plain().clone())
            }
            (_, Value::Instance(_)) => {
                let name = match operator {
                    Operator::Add => "__iadd__",
                    Operator::Sub => "__isub__",
                    Operator::Mul => "__imul__",
                    Operator::BitOr => "__ior__",
                    _ => "",
                };
                if let Some(method) = self.lookup_special(current, name) {
                    return self.call(&method, Args::new(vec![value.clone()]));
                }
                self.binary(operator, current, value)
            }
            _ => self.binary(operator, current, value),
        }
    }

    fn delete(&mut self, frame: &Frame, target: &Expr) -> Result<(), Error> {
        match target {
            Expr::Name(name) => self.unbind(frame, name.id.as_str()),
            Expr::Subscript(subscript) => {
                let object = self.eval(frame, &subscript.value)?;
                let key = self.eval(frame, &subscript.slice)?;
                self.delete_item(&object, &key)
            }
            Expr::Attribute(attribute) => {
                let object = self.eval(frame, &attribute.value)?;
                match object.plain() {
                    Value::Instance(instance) => {
                        instance
                            .dict
                            .borrow_mut()
                            .shift_remove(attribute.attr.as_str());
                        Ok(())
                    }
                    Value::Module(module) => {
                        module
                            .globals
                            .vars
                            .borrow_mut()
                            .shift_remove(attribute.attr.as_str());
                        Ok(())
                    }
                    _ => Err(Error::unsupported("deleting attributes of this object")),
                }
            }
            Expr::Tuple(tuple) => {
                for element in &tuple.elts {
                    self.delete(frame, element)?;
                }
                Ok(())
            }
            Expr::List(list) => {
                for element in &list.elts {
                    self.delete(frame, element)?;
                }
                Ok(())
            }
            _ => Err(Error::new("SyntaxError", "cannot delete expression")),
        }
    }

    /// The error that a `raise` statement raises.
    fn raise(&mut self, frame: &Frame, raise: &ast::StmtRaise) -> Result<Error, Error> {
        let Some(exception) = &raise.exc else {
            return match self.handling.last() {
                Some(error) => Ok(error.clone()),
                None => Err(Error::runtime_error("No active exception to reraise")),
            };
        };
        let value = self.eval(frame, exception)?;
        self.exception_error(value)
    }

    /// An error for a raised value: an exception object, or a class to instantiate.
    pub fn exception_error(&mut self, value: Value) -> Result<Error, Error> {
        let value = match value.plain() {
            Value::Class(class) if class.builtin == Some(types::TypeTag::BaseException) => {
                self.call(value.plain(), Args::default())?
            }
            other => other.clone(),
        };
        if !types::is_exception(&value) {
            return Err(Error::type_error(
                "exceptions must derive from BaseException",
            ));
        }
        Ok(Error::from_value(value))
    }

    fn exec_try(&mut self, frame: &Frame, attempt: &ast::StmtTry) -> Result<Flow, Error> {
        let result = match self.exec_suite(frame, &attempt.body) {
            Ok(Flow::Next) => self.exec_suite(frame, &attempt.orelse),
            Ok(other) => Ok(other),
            Err(error) => self.handle(frame, attempt, error),
        };
        if attempt.finalbody.is_empty() {
            return result;
        }
        match self.exec_suite(frame, &attempt.finalbody)? {
            Flow::Next => result,
            other => Ok(other),
        }
    }

    fn handle(
        &mut self,
        frame: &Frame,
        attempt: &ast::StmtTry,
        error: Error,
    ) -> Result<Flow, Error> {
        let ErrorKind::Exception(exception) = &error.kind else {
            return Err(error);
        };
        let exception = exception.clone();
        for ExceptHandler::ExceptHandler(handler) in &attempt.handlers {
            let matches = match &handler.type_ {
                None => true,
                Some(kind) => {
                    let kind = self.eval(frame, kind)?;
                    self.exception_matches(&exception, &kind)?
                }
            };
            if !matches {
                continue;
            }
            if let Some(name) = &handler.name {
                self.bind(frame, name.id.as_str(), exception.clone());
            }
            let result =
                self.with_handling(error.clone(), |this| this.exec_suite(frame, &handler.body));
            if let Some(name) = &handler.name {
                let _ = self.unbind(frame, name.id.as_str());
            }
            return result;
        }
        Err(error)
    }

    /// Whether an exception object matches a class or tuple of classes of an `except` clause.
    fn exception_matches(&mut self, exception: &Value, kind: &Value) -> Result<bool, Error> {
        match kind.plain() {
            Value::Class(class) => Ok(types::is_instance(exception, class)),
            Value::Tuple(kinds) => {
                for kind in kinds.iter() {
                    if self.exception_matches(exception, kind)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            _ => Err(Error::type_error(
                "catching classes that do not inherit from BaseException is not allowed",
            )),
        }
    }
}
