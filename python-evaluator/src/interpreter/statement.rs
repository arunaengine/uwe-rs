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
            Value::Native(object)
                if let Some(class) = object
                    .as_any()
                    .downcast_ref::<crate::packages::PackageObject>() =>
            {
                Ok(class.catches(exception))
            }
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

    fn exec_with(
        &mut self,
        frame: &Frame,
        with: &ast::StmtWith,
        index: usize,
    ) -> Result<Flow, Error> {
        let Some(item) = with.items.get(index) else {
            return self.exec_suite(frame, &with.body);
        };
        let manager = self.eval(frame, &item.context_expr)?;
        let enter = self.find_attribute(&manager, "__enter__")?.ok_or_else(|| {
            Error::type_error(format!(
                "'{}' object does not support the context manager protocol",
                types::type_name(&manager)
            ))
        })?;
        let exit = self.find_attribute(&manager, "__exit__")?;
        let entered = self.call(&enter, Args::default())?;
        if let Some(target) = &item.optional_vars {
            self.assign(frame, target, entered)?;
        }
        let result = self.exec_with(frame, with, index + 1);
        let Some(exit) = exit else {
            return result;
        };
        match result {
            Ok(flow) => {
                self.call(
                    &exit,
                    Args::new(vec![Value::None, Value::None, Value::None]),
                )?;
                Ok(flow)
            }
            Err(error) => {
                if let ErrorKind::Exception(exception) = &error.kind {
                    let class = Value::Class(types::class_of(exception));
                    let suppress = self.call(
                        &exit,
                        Args::new(vec![class, exception.clone(), Value::None]),
                    )?;
                    if self.truthy(&suppress)? {
                        return Ok(Flow::Next);
                    }
                }
                Err(error)
            }
        }
    }

    fn define_function(&mut self, frame: &Frame, def: &ast::StmtFunctionDef) -> Result<(), Error> {
        if def.is_async {
            return Err(Error::unsupported("async functions"));
        }
        let decorators = def
            .decorator_list
            .iter()
            .map(|decorator| self.eval(frame, &decorator.expression))
            .collect::<Result<Vec<_>, _>>()?;
        let defaults = self.eval_defaults(frame, Some(&def.parameters))?;
        self.eval_annotations(frame, def)?;
        let name = def.name.id.as_str();
        let qualname: Name = match frame.kind {
            FrameKind::Class => match frame.locals.get("__qualname__") {
                Some(Value::Str(class)) => format!("{class}.{name}").into(),
                _ => name.into(),
            },
            _ => name.into(),
        };
        let mut declared = Declared::default();
        declarations(&def.body, &mut declared);
        let function = Value::Function(Rc::new(Function {
            name: name.into(),
            qualname,
            code: Code::Def(Rc::new(def.clone())),
            defaults,
            globals: frame.globals.clone(),
            closure: frame.closure(),
            source: frame.source.clone(),
            generator: contains_yield(&def.body),
            declared: Rc::new(declared),
            attributes: RefCell::new(IndexMap::new()),
        }));
        let mut value = function;
        for decorator in decorators.into_iter().rev() {
            value = self.call(&decorator, Args::new(vec![value]))?;
        }
        self.bind(frame, name, value);
        Ok(())
    }

    /// Evaluates the annotations of a function as it is defined, as Python before 3.14 does,
    /// unless the module imports `annotations` from `__future__`.
    fn eval_annotations(&mut self, frame: &Frame, def: &ast::StmtFunctionDef) -> Result<(), Error> {
        let lazy = frame.globals.get("annotations").is_some_and(|value| {
            value
                .as_native::<crate::modules::misc::Feature>()
                .is_some_and(|feature| feature.0 == "annotations")
        });
        if lazy {
            return Ok(());
        }
        let parameters = &def.parameters;
        let annotations = parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
            .map(|parameter| &parameter.parameter)
            .chain(parameters.vararg.as_deref())
            .chain(parameters.kwarg.as_deref())
            .filter_map(|parameter| parameter.annotation.as_deref())
            .chain(def.returns.as_deref());
        for annotation in annotations {
            self.eval(frame, annotation)?;
        }
        Ok(())
    }

    fn define_class(&mut self, frame: &Frame, class: &ast::StmtClassDef) -> Result<(), Error> {
        let decorators = class
            .decorator_list
            .iter()
            .map(|decorator| self.eval(frame, &decorator.expression))
            .collect::<Result<Vec<_>, _>>()?;
        let mut bases = Vec::new();
        if let Some(arguments) = &class.arguments {
            let args = self.eval_arguments(frame, arguments)?;
            for base in args.positional {
                match base.plain() {
                    Value::Class(base) => {
                        let allowed = base.builtin.is_none()
                            || matches!(
                                base.builtin,
                                Some(types::TypeTag::Object | types::TypeTag::BaseException)
                            )
                            || base.module.as_ref() != "builtins";
                        if !allowed {
                            return Err(Error::unsupported(format!(
                                "classes that derive from {}",
                                base.name
                            )));
                        }
                        bases.push(base.clone());
                    }
                    // Protocols and generic classes of `typing` are plain classes when the
                    // code runs.
                    Value::Native(object)
                        if object
                            .as_any()
                            .downcast_ref::<crate::modules::misc::TypingName>()
                            .is_some_and(|name| matches!(name.0, "Protocol" | "Generic")) => {}
                    Value::Native(object) => {
                        let base = match object
                            .as_any()
                            .downcast_ref::<crate::packages::PackageObject>()
                        {
                            Some(class) => format!(
                                "{}, a class of a Python package, which runs in the container of decision 19",
                                class.class_name().unwrap_or(object.type_name())
                            ),
                            None => object.type_name().to_owned(),
                        };
                        return Err(Error::unsupported(format!(
                            "classes that derive from {base}"
                        )));
                    }
                    other => {
                        return Err(Error::type_error(format!(
                            "bases must be types, not {}",
                            types::type_name(other)
                        )));
                    }
                }
            }
            if let Some((name, _)) = args.keywords.first() {
                if &**name != "metaclass" {
                    return Err(Error::unsupported(format!("class keyword {name}")));
                }
                return Err(Error::unsupported("metaclasses"));
            }
        }
        if bases.is_empty() {
            bases.push(types::builtin_class(types::TypeTag::Object));
        }
        let name = class.name.id.as_str();
        let scope = Scope::new(frame.closure());
        scope.set("__qualname__", Value::str(name));
        scope.set(
            "__module__",
            frame
                .globals
                .get("__name__")
                .unwrap_or(Value::str("__main__")),
        );
        let body_frame = Frame {
            locals: scope.clone(),
            globals: frame.globals.clone(),
            source: frame.source.clone(),
            kind: FrameKind::Class,
            declared: Rc::new(Declared::default()),
            yields: None,
            method: None,
        };
        self.exec_suite(&body_frame, &class.body)?;
        let module = frame
            .globals
            .get("__name__")
            .and_then(|name| name.as_str().map(str::to_owned))
            .unwrap_or_else(|| "__main__".into());
        let new = types::new_class(name, &module, bases, None);
        {
            let mut dict = new.dict.borrow_mut();
            for (key, value) in scope.vars.borrow().iter() {
                dict.insert(key.clone(), value.clone());
            }
        }
        crate::modules::enums::make_members(&new)?;
        let mut value = Value::Class(new);
        for decorator in decorators.into_iter().rev() {
            value = self.call(&decorator, Args::new(vec![value]))?;
        }
        self.bind(frame, name, value);
        Ok(())
    }

    fn exec_match(&mut self, frame: &Frame, choice: &ast::StmtMatch) -> Result<Flow, Error> {
        let subject = self.eval(frame, &choice.subject)?;
        for case in &choice.cases {
            if !self.pattern_matches(frame, &case.pattern, &subject)? {
                continue;
            }
            if let Some(guard) = &case.guard {
                let test = self.eval(frame, guard)?;
                if !self.truthy(&test)? {
                    continue;
                }
            }
            return self.exec_suite(frame, &case.body);
        }
        Ok(Flow::Next)
    }

    fn pattern_matches(
        &mut self,
        frame: &Frame,
        pattern: &Pattern,
        subject: &Value,
    ) -> Result<bool, Error> {
        match pattern {
            Pattern::MatchValue(value) => {
                let value = self.eval(frame, &value.value)?;
                self.equals(subject, &value)
            }
            Pattern::MatchSingleton(singleton) => Ok(match singleton.value {
                ast::Singleton::None => subject.is_none(),
                ast::Singleton::True => matches!(subject.plain(), Value::Bool(true)),
                ast::Singleton::False => matches!(subject.plain(), Value::Bool(false)),
            }),
            Pattern::MatchAs(capture) => {
                if let Some(inner) = &capture.pattern
                    && !self.pattern_matches(frame, inner, subject)?
                {
                    return Ok(false);
                }
                if let Some(name) = &capture.name {
                    self.bind(frame, name.id.as_str(), subject.clone());
                }
                Ok(true)
            }
            Pattern::MatchOr(alternatives) => {
                for alternative in &alternatives.patterns {
                    if self.pattern_matches(frame, alternative, subject)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Pattern::MatchSequence(sequence) => {
                if !matches!(subject.plain(), Value::List(_) | Value::Tuple(_)) {
                    return Ok(false);
                }
                let items = self.iterate(subject)?;
                let star = sequence
                    .patterns
                    .iter()
                    .position(|pattern| matches!(pattern, Pattern::MatchStar(_)));
                match star {
                    None => {
                        if items.len() != sequence.patterns.len() {
                            return Ok(false);
                        }
                        for (pattern, item) in sequence.patterns.iter().zip(&items) {
                            if !self.pattern_matches(frame, pattern, item)? {
                                return Ok(false);
                            }
                        }
                        Ok(true)
                    }
                    Some(position) => {
                        let after = sequence.patterns.len() - position - 1;
                        if items.len() < position + after {
                            return Ok(false);
                        }
                        let rest_end = items.len() - after;
                        for (pattern, item) in
                            sequence.patterns[..position].iter().zip(&items[..position])
                        {
                            if !self.pattern_matches(frame, pattern, item)? {
                                return Ok(false);
                            }
                        }
                        if let Pattern::MatchStar(star) = &sequence.patterns[position]
                            && let Some(name) = &star.name
                        {
                            self.bind(
                                frame,
                                name.id.as_str(),
                                Value::list(items[position..rest_end].to_vec()),
                            );
                        }
                        for (pattern, item) in sequence.patterns[position + 1..]
                            .iter()
                            .zip(&items[rest_end..])
                        {
                            if !self.pattern_matches(frame, pattern, item)? {
                                return Ok(false);
                            }
                        }
                        Ok(true)
                    }
                }
            }
            Pattern::MatchMapping(mapping) => {
                let Value::Dict(dict) = subject.plain() else {
                    return Ok(false);
                };
                for (key, pattern) in mapping.keys.iter().zip(&mapping.patterns) {
                    let key = self.eval(frame, key)?;
                    let found = dict.borrow().entries.get(&self.key(&key)?).cloned();
                    match found {
                        Some(value) => {
                            if !self.pattern_matches(frame, pattern, &value)? {
                                return Ok(false);
                            }
                        }
                        None => return Ok(false),
                    }
                }
                Ok(true)
            }
            Pattern::MatchClass(class) => {
                let kind = self.eval(frame, &class.cls)?;
                let Value::Class(kind) = kind.plain() else {
                    return Err(Error::type_error("called match pattern must be a class"));
                };
                if !types::is_instance(subject, kind) {
                    return Ok(false);
                }
                if !class.arguments.patterns.is_empty() || !class.arguments.keywords.is_empty() {
                    return Err(Error::unsupported("class patterns with arguments"));
                }
                Ok(true)
            }
            Pattern::MatchStar(_) => Ok(true),
        }
    }
}

/// Keeps the names that a class body annotates, in order, for dataclasses.
fn record_annotation(frame: &Frame, name: &str, _with_default: Option<()>) {
    let annotations = frame.locals.get("__annotations__").unwrap_or_else(|| {
        let dict = Value::dict(Default::default());
        frame.locals.set("__annotations__", dict.clone());
        dict
    });
    if let Value::Dict(dict) = annotations {
        dict.borrow_mut().insert(name, Value::None);
    }
}

/// Collects `global` and `nonlocal` names of a function body, without nested functions.
fn declarations(suite: &[Stmt], declared: &mut Declared) {
    for statement in suite {
        match statement {
            Stmt::Global(global) => declared
                .global
                .extend(global.names.iter().map(|name| Name::from(name.id.as_str()))),
            Stmt::Nonlocal(nonlocal) => declared.nonlocal.extend(
                nonlocal
                    .names
                    .iter()
                    .map(|name| Name::from(name.id.as_str())),
            ),
            Stmt::If(branch) => {
                declarations(&branch.body, declared);
                for clause in &branch.elif_else_clauses {
                    declarations(&clause.body, declared);
                }
            }
            Stmt::For(each) => {
                declarations(&each.body, declared);
                declarations(&each.orelse, declared);
            }
            Stmt::While(each) => {
                declarations(&each.body, declared);
                declarations(&each.orelse, declared);
            }
            Stmt::With(with) => declarations(&with.body, declared),
            Stmt::Try(attempt) => {
                declarations(&attempt.body, declared);
                for ExceptHandler::ExceptHandler(handler) in &attempt.handlers {
                    declarations(&handler.body, declared);
                }
                declarations(&attempt.orelse, declared);
                declarations(&attempt.finalbody, declared);
            }
            _ => {}
        }
    }
}

/// Whether a function body yields, without looking into nested functions and classes.
fn contains_yield(suite: &[Stmt]) -> bool {
    use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
    struct Finder(bool);
    impl<'a> Visitor<'a> for Finder {
        fn visit_stmt(&mut self, statement: &'a Stmt) {
            match statement {
                Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
                other => walk_stmt(self, other),
            }
        }
        fn visit_expr(&mut self, expression: &'a Expr) {
            match expression {
                Expr::Yield(_) | Expr::YieldFrom(_) => self.0 = true,
                Expr::Lambda(_) => {}
                other => walk_expr(self, other),
            }
        }
    }
    let mut finder = Finder(false);
    for statement in suite {
        finder.visit_stmt(statement);
    }
    finder.0
}
