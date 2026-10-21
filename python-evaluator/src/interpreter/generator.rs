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
    /// The `__exit__` methods of a `with` statement whose body yields, innermost last.
    Exit(Vec<Value>),
    /// The `except` clauses of a `try` statement whose body runs; its `else` runs after.
    Handlers(Rc<ast::StmtTry>),
    /// The `finally` clause of a `try` statement, which runs however the statement ends.
    Finally(Rc<ast::StmtTry>),
    /// An `except` clause that runs, with its name and the exception it handles.
    Handling(Option<String>, Error),
    /// An exception that goes on once a `finally` clause ran.
    Raise(Error),
}

pub struct Generator {
    frame: Frame,
    tasks: Vec<Task>,
    /// Whether the body ran up to a `yield`, which `send` needs before a value.
    started: bool,
    /// The value that `send` gives the `yield` where the body waits, or that the generator
    /// that a `yield from` delegated to returned.
    sent: Option<Value>,
    /// What the body returned, which its `StopIteration` holds.
    pub(crate) returned: Option<Value>,
}

impl Generator {
    pub(super) fn new(frame: Frame, body: &[Stmt]) -> Self {
        Self {
            frame,
            tasks: vec![Task::Suite(Rc::new(body.to_vec()), 0)],
            started: false,
            sent: None,
            returned: None,
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
            started: false,
            sent: None,
            returned: None,
        }
    }

    fn suite(&mut self, body: &[Stmt]) {
        self.tasks.push(Task::Suite(Rc::new(body.to_vec()), 0));
    }

    /// Leaves the innermost loop, or the body for a `return` (`exit`), so that the `with`
    /// and `finally` clauses that the jump leaves still run.
    fn leave(&mut self, interpreter: &mut Interpreter, keep: bool, exit: bool) {
        let mut cleanup = Vec::new();
        while let Some(task) = self.tasks.pop() {
            match task {
                Task::For(..) | Task::While(..) if !exit => {
                    if keep {
                        self.tasks.push(task);
                    }
                    break;
                }
                Task::Exit(_) | Task::Finally(_) => cleanup.push(task),
                Task::Handling(name, _) => self.release(interpreter, name),
                _ => {}
            }
        }
        self.tasks.extend(cleanup.into_iter().rev());
    }

    /// Ends an `except` clause: its exception is no longer handled and its name is unbound.
    fn release(&self, interpreter: &mut Interpreter, name: Option<String>) {
        interpreter.handling.pop();
        if let Some(name) = name {
            let _ = interpreter.unbind(&self.frame, &name);
        }
    }

    /// The exceptions of the `except` clauses that the body is in, outermost first.
    fn handled(&self) -> Vec<Error> {
        (self.tasks.iter())
            .filter_map(|task| match task {
                Task::Handling(_, error) => Some(error.clone()),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn next(&mut self, interpreter: &mut Interpreter) -> Result<Option<Value>, Error> {
        self.run(interpreter, Generator::resume)
    }

    /// `generator.send(value)`: the `yield` where the body waits gives `value`.
    pub(crate) fn send(
        &mut self,
        interpreter: &mut Interpreter,
        value: Value,
    ) -> Result<Option<Value>, Error> {
        if !self.started && !value.is_none() && !self.tasks.is_empty() {
            return Err(Error::type_error(
                "can't send non-None value to a just-started generator",
            ));
        }
        // A bare `yield` statement drops the value.
        if matches!(self.tasks.last(), Some(Task::Assign(_) | Task::Delegate(_))) {
            self.sent = Some(value);
        }
        self.next(interpreter)
    }

    /// `generator.throw(exception)`: the `yield` where the body waits raises it.
    pub(crate) fn throw(
        &mut self,
        interpreter: &mut Interpreter,
        error: Error,
    ) -> Result<Option<Value>, Error> {
        if self.tasks.is_empty() {
            return Err(error);
        }
        self.run(interpreter, |generator, interpreter| {
            generator.raise_inside(interpreter, error)
        })
    }

    /// `generator.close()`: `GeneratorExit` at the `yield` where the body waits, which the
    /// body must not answer with another value.
    pub(crate) fn close(&mut self, interpreter: &mut Interpreter) -> Result<(), Error> {
        if !self.started || self.tasks.is_empty() {
            self.tasks.clear();
            return Ok(());
        }
        let class = crate::types::exception_class("GeneratorExit").expect("GeneratorExit exists");
        let exit = Error::from_value(crate::error::exception(&class, Vec::new()));
        match self.throw(interpreter, exit) {
            Ok(Some(_)) => {
                self.tasks.clear();
                Err(Error::runtime_error("generator ignored GeneratorExit"))
            }
            Ok(None) => Ok(()),
            Err(error) if error.is("GeneratorExit") || error.is("StopIteration") => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Raises an error at the `yield` where the body waits, or in the generator that it
    /// delegates to, and runs on from the clause that takes it.
    fn raise_inside(
        &mut self,
        interpreter: &mut Interpreter,
        mut error: Error,
    ) -> Result<Option<Value>, Error> {
        match self.tasks.last() {
            Some(Task::Assign(_)) => {
                self.tasks.pop();
            }
            Some(Task::Delegate(iterator)) => {
                let iterator = iterator.clone();
                match interpreter.throw_item(&iterator, error) {
                    Ok(Some(value)) => return Ok(Some(value)),
                    Ok(None) => {
                        self.tasks.pop();
                        return self.resume(interpreter);
                    }
                    Err(next) => {
                        self.tasks.pop();
                        error = next;
                    }
                }
            }
            _ => {}
        }
        loop {
            match self.unwind(interpreter, error) {
                Ok(()) => break,
                Err(next) if !self.tasks.is_empty() => error = next,
                Err(next) => return Err(next),
            }
        }
        self.resume(interpreter)
    }

    /// Runs the body with `step`, with the exceptions of its clauses handled meanwhile.
    fn run(
        &mut self,
        interpreter: &mut Interpreter,
        step: impl FnOnce(&mut Self, &mut Interpreter) -> Result<Option<Value>, Error>,
    ) -> Result<Option<Value>, Error> {
        interpreter.enter_call()?;
        self.started = true;
        // The exceptions of the clauses that the body is in are handled while it runs.
        interpreter.handling.extend(self.handled());
        let result = step(self, interpreter);
        self.sent = None;
        let kept = interpreter.handling.len() - self.handled().len();
        interpreter.handling.truncate(kept);
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

    /// Runs the body up to its next value; an error leaves the statements it is in up to an
    /// `except` clause that takes it, a `finally` clause or a `with` that suppresses it.
    fn resume(&mut self, interpreter: &mut Interpreter) -> Result<Option<Value>, Error> {
        loop {
            let mut error = match self.advance(interpreter) {
                Err(error) => error,
                done => return done,
            };
            loop {
                match self.unwind(interpreter, error) {
                    Ok(()) => break,
                    Err(next) if !self.tasks.is_empty() => error = next,
                    Err(next) => return Err(next),
                }
            }
        }
    }

    /// Leaves tasks for an error until one takes it; an error of a clause goes on instead.
    fn unwind(&mut self, interpreter: &mut Interpreter, error: Error) -> Result<(), Error> {
        while let Some(task) = self.tasks.pop() {
            let exception = match &error.kind {
                crate::error::ErrorKind::Exception(exception) => Some(exception.clone()),
                _ => None,
            };
            match (task, exception) {
                (Task::Handling(name, _), _) => self.release(interpreter, name),
                (Task::Exit(exits), Some(exception)) => {
                    let class = Value::Class(crate::types::class_of(&exception));
                    let mut suppressed = false;
                    for exit in exits.iter().rev() {
                        let args = vec![class.clone(), exception.clone(), Value::None];
                        let answer = interpreter.call(exit, crate::value::Args::new(args))?;
                        suppressed = suppressed || interpreter.truthy(&answer)?;
                    }
                    if suppressed {
                        return Ok(());
                    }
                }
                (Task::Handlers(attempt), Some(exception)) => {
                    for ast::ExceptHandler::ExceptHandler(handler) in &attempt.handlers {
                        if let Some(kind) = &handler.type_ {
                            let kind = interpreter.eval(&self.frame, kind)?;
                            if !interpreter.exception_matches(&exception, &kind)? {
                                continue;
                            }
                        }
                        let name = handler.name.as_ref().map(|name| name.id.to_string());
                        if let Some(name) = &name {
                            interpreter.bind(&self.frame, name, exception.clone());
                        }
                        interpreter.handling.push(error.clone());
                        self.tasks.push(Task::Handling(name, error));
                        self.suite(&handler.body);
                        return Ok(());
                    }
                }
                (Task::Finally(attempt), _) => {
                    self.tasks.push(Task::Raise(error));
                    self.suite(&attempt.finalbody);
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(error)
    }

    fn advance(&mut self, interpreter: &mut Interpreter) -> Result<Option<Value>, Error> {
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
                    let item = match self.sent.take() {
                        Some(value) if !value.is_none() => {
                            interpreter.send_item(&iterator, value)?
                        }
                        _ => interpreter.next_item(&iterator)?,
                    };
                    if let Some(item) = item {
                        self.tasks.push(Task::Delegate(iterator));
                        return Ok(Some(item));
                    }
                    // `x = yield from g` takes what `g` returned.
                    if matches!(self.tasks.last(), Some(Task::Assign(_))) {
                        self.sent = Some(interpreter.returned_value(&iterator));
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
                    let sent = self.sent.take().unwrap_or(Value::None);
                    for target in targets {
                        interpreter.assign(&self.frame, &target, sent.clone())?;
                    }
                }
                Task::Exit(exits) => {
                    for exit in exits.iter().rev() {
                        let args = vec![Value::None, Value::None, Value::None];
                        interpreter.call(exit, crate::value::Args::new(args))?;
                    }
                }
                Task::Handlers(attempt) => self.suite(&attempt.orelse),
                Task::Finally(attempt) => self.suite(&attempt.finalbody),
                Task::Handling(name, _) => self.release(interpreter, name),
                Task::Raise(error) => return Err(error),
            }
        }
        Ok(None)
    }

    fn statement(
        &mut self,
        interpreter: &mut Interpreter,
        statement: &Stmt,
    ) -> Result<Option<Value>, Error> {
        let statement_slice = std::slice::from_ref(statement);
        // A `yield` inside an expression first moves to a statement of its own.
        if super::hoist::needed(statement) {
            let statements = super::hoist::statement(statement)?;
            self.suite(&statements);
            return Ok(None);
        }
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
            Stmt::Assign(assign) if matches!(&*assign.value, Expr::YieldFrom(_)) => {
                let Expr::YieldFrom(expression) = &*assign.value else {
                    unreachable!()
                };
                let value = interpreter.eval(&self.frame, &expression.value)?;
                let iterator = interpreter.make_iterator(&value)?;
                self.tasks.push(Task::Assign(assign.targets.clone()));
                self.tasks.push(Task::Delegate(iterator));
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
            Stmt::With(with) if !with.is_async && contains_yield(&with.body) => {
                let mut exits = Vec::new();
                for item in &with.items {
                    let manager = interpreter.eval(&self.frame, &item.context_expr)?;
                    let enter = interpreter.find_attribute(&manager, "__enter__")?;
                    let exit = interpreter.find_attribute(&manager, "__exit__")?;
                    let (Some(enter), Some(exit)) = (enter, exit) else {
                        return Err(Error::type_error(format!(
                            "'{}' object does not support the context manager protocol",
                            crate::types::type_name(&manager)
                        )));
                    };
                    let entered = interpreter.call(&enter, crate::value::Args::default())?;
                    if let Some(target) = &item.optional_vars {
                        interpreter.assign(&self.frame, target, entered)?;
                    }
                    exits.push(exit);
                }
                self.tasks.push(Task::Exit(exits));
                self.suite(&with.body);
            }
            Stmt::Try(attempt) if !attempt.is_star && contains_yield(statement_slice) => {
                let attempt = Rc::new(attempt.clone());
                if !attempt.finalbody.is_empty() {
                    self.tasks.push(Task::Finally(attempt.clone()));
                }
                self.tasks.push(Task::Handlers(attempt.clone()));
                self.suite(&attempt.body);
            }
            Stmt::Match(choice) if (choice.cases.iter()).any(|case| contains_yield(&case.body)) => {
                let subject = interpreter.eval(&self.frame, &choice.subject)?;
                for case in &choice.cases {
                    if !interpreter.pattern_matches(&self.frame, &case.pattern, &subject)? {
                        continue;
                    }
                    if let Some(guard) = &case.guard {
                        let test = interpreter.eval(&self.frame, guard)?;
                        if !interpreter.truthy(&test)? {
                            continue;
                        }
                    }
                    self.suite(&case.body);
                    break;
                }
            }
            _ => self.ordinary(interpreter, statement)?,
        }
        Ok(None)
    }

    fn ordinary(&mut self, interpreter: &mut Interpreter, statement: &Stmt) -> Result<(), Error> {
        if contains_yield(std::slice::from_ref(statement)) {
            return Err(Error::unsupported("generator suspension in this statement"));
        }
        match interpreter.exec(&self.frame, statement)? {
            Flow::Next => {}
            Flow::Return(value) => {
                self.returned = Some(value);
                self.leave(interpreter, false, true)
            }
            Flow::Break => self.leave(interpreter, false, false),
            Flow::Continue => self.leave(interpreter, true, false),
        }
        Ok(())
    }
}
