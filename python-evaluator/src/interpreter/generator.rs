//! Suspended execution of generator bodies.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use ruff_python_ast::{self as ast, Expr, Stmt};
use ruff_text_size::Ranged;

use super::{
    Frame, Interpreter,
    statement::{Flow, contains_yield},
};
use crate::error::Error;
use crate::value::Value;

enum Task {
    Suite(Rc<Vec<Stmt>>, usize),
    For(Rc<ast::StmtFor>, Value),
    While(Rc<ast::StmtWhile>),
    Delegate(Value),
    Assign(Vec<Expr>),
    Comprehension(Rc<ast::ExprGenerator>, usize, Value),
}

pub struct Generator {
    frame: Frame,
    tasks: Vec<Task>,
}

impl Generator {
    pub(super) fn new(frame: Frame, body: &[Stmt]) -> Self {
        Self {
            frame,
            tasks: vec![Task::Suite(Rc::new(body.to_vec()), 0)],
        }
    }

    pub(super) fn expression(
        frame: Frame,
        expression: &ast::ExprGenerator,
        iterator: Value,
    ) -> Self {
        Self {
            frame,
            tasks: vec![Task::Comprehension(
                Rc::new(expression.clone()),
                0,
                iterator,
            )],
        }
    }

    fn suite(&mut self, body: &[Stmt]) {
        self.tasks.push(Task::Suite(Rc::new(body.to_vec()), 0));
    }

    fn loop_flow(&mut self, keep: bool) {
        while let Some(task) = self.tasks.pop() {
            if matches!(task, Task::For(..) | Task::While(..)) {
                if keep {
                    self.tasks.push(task);
                }
                break;
            }
        }
    }

    pub(crate) fn next(&mut self, interpreter: &mut Interpreter) -> Result<Option<Value>, Error> {
        interpreter.enter_call()?;
        let result = self.resume(interpreter);
        interpreter.leave_call();
        if !matches!(result, Ok(Some(_))) {
            self.tasks.clear();
        }
        result.map_err(|error| {
            if error.is("StopIteration") {
                Error::runtime_error("generator raised StopIteration")
            } else {
                error
            }
        })
    }

    fn resume(&mut self, interpreter: &mut Interpreter) -> Result<Option<Value>, Error> {
        while let Some(task) = self.tasks.pop() {
            match task {
                Task::Suite(body, index) => {
                    let Some(statement) = body.get(index) else {
                        continue;
                    };
                    self.tasks.push(Task::Suite(body.clone(), index + 1));
                    interpreter.count_step()?;
                    let result = self.statement(interpreter, statement);
                    if let Some(value) = result.map_err(|error| {
                        error.with_location(self.frame.source.location(statement.start()))
                    })? {
                        return Ok(Some(value));
                    }
                }
                Task::For(each, iterator) => match interpreter.next_item(&iterator)? {
                    Some(item) => {
                        interpreter.count_step()?;
                        interpreter.assign(&self.frame, &each.target, item)?;
                        self.tasks.push(Task::For(each.clone(), iterator));
                        self.suite(&each.body);
                    }
                    None => self.suite(&each.orelse),
                },
                Task::While(each) => {
                    interpreter.count_step()?;
                    let test = interpreter.eval(&self.frame, &each.test)?;
                    if interpreter.truthy(&test)? {
                        self.tasks.push(Task::While(each.clone()));
                        self.suite(&each.body);
                    } else {
                        self.suite(&each.orelse);
                    }
                }
                Task::Delegate(iterator) => {
                    if let Some(item) = interpreter.next_item(&iterator)? {
                        self.tasks.push(Task::Delegate(iterator));
                        return Ok(Some(item));
                    }
                }
                Task::Comprehension(expression, level, iterator) => {
                    let clause = &expression.generators[level];
                    let Some(item) = interpreter.next_item(&iterator)? else {
                        continue;
                    };
                    interpreter.count_step()?;
                    interpreter.assign(&self.frame, &clause.target, item)?;
                    self.tasks
                        .push(Task::Comprehension(expression.clone(), level, iterator));
                    let mut accepted = true;
                    for condition in &clause.ifs {
                        let value = interpreter.eval(&self.frame, condition)?;
                        if !interpreter.truthy(&value)? {
                            accepted = false;
                            break;
                        }
                    }
                    if !accepted {
                        continue;
                    }
                    match expression.generators.get(level + 1) {
                        Some(inner) => {
                            let value = interpreter.eval(&self.frame, &inner.iter)?;
                            let iterator = interpreter.make_iterator(&value)?;
                            self.tasks.push(Task::Comprehension(
                                expression.clone(),
                                level + 1,
                                iterator,
                            ));
                        }
                        None => return interpreter.eval(&self.frame, &expression.elt).map(Some),
                    }
                }
                Task::Assign(targets) => {
                    for target in targets {
                        interpreter.assign(&self.frame, &target, Value::None)?;
                    }
                }
            }
        }
        Ok(None)
    }

    fn statement(
        &mut self,
        interpreter: &mut Interpreter,
        statement: &Stmt,
    ) -> Result<Option<Value>, Error> {
        match statement {
            Stmt::Expr(expression) => match &*expression.value {
                Expr::Yield(expression) => {
                    return Ok(Some(match &expression.value {
                        Some(value) => interpreter.eval(&self.frame, value)?,
                        None => Value::None,
                    }));
                }
                Expr::YieldFrom(expression) => {
                    let value = interpreter.eval(&self.frame, &expression.value)?;
                    let iterator = interpreter.make_iterator(&value)?;
                    self.tasks.push(Task::Delegate(iterator));
                }
                _ => self.ordinary(interpreter, statement)?,
            },
            Stmt::Assign(assign) if matches!(&*assign.value, Expr::Yield(_)) => {
                let Expr::Yield(expression) = &*assign.value else {
                    unreachable!()
                };
                let value = match &expression.value {
                    Some(value) => interpreter.eval(&self.frame, value)?,
                    None => Value::None,
                };
                self.tasks.push(Task::Assign(assign.targets.clone()));
                return Ok(Some(value));
            }
            Stmt::If(branch) => {
                let test = interpreter.eval(&self.frame, &branch.test)?;
                if interpreter.truthy(&test)? {
                    self.suite(&branch.body);
                } else {
                    for clause in &branch.elif_else_clauses {
                        let taken = match &clause.test {
                            Some(test) => {
                                let value = interpreter.eval(&self.frame, test)?;
                                interpreter.truthy(&value)?
                            }
                            None => true,
                        };
                        if taken {
                            self.suite(&clause.body);
                            break;
                        }
                    }
                }
            }
            Stmt::For(each) => {
                if each.is_async {
                    return Err(Error::unsupported("asynchronous generators"));
                }
                let value = interpreter.eval(&self.frame, &each.iter)?;
                let iterator = interpreter.make_iterator(&value)?;
                self.tasks.push(Task::For(Rc::new(each.clone()), iterator));
            }
            Stmt::While(each) => self.tasks.push(Task::While(Rc::new(each.clone()))),
            _ => self.ordinary(interpreter, statement)?,
        }
        Ok(None)
    }

    fn ordinary(&mut self, interpreter: &mut Interpreter, statement: &Stmt) -> Result<(), Error> {
        if contains_yield(std::slice::from_ref(statement)) {
            return Err(Error::unsupported(
                "generator suspension in nested expressions, try, with or match",
            ));
        }
        match interpreter.exec(&self.frame, statement)? {
            Flow::Next => {}
            Flow::Return(_) => self.tasks.clear(),
            Flow::Break => self.loop_flow(false),
            Flow::Continue => self.loop_flow(true),
        }
        Ok(())
    }
}
