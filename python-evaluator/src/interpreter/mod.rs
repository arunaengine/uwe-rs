//! The interpreter: frames and scopes, and the object protocol that expressions and library
//! functions share (attributes, calls, operators, iteration and text of values).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

pub(crate) mod call;
mod expression;
pub mod generator;
mod import;
mod operators;
mod statement;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ruff_python_ast::Suite;
use ruff_text_size::TextSize;

use crate::error::{Error, Location};
use crate::host::Host;
use crate::types;
use crate::value::{Class, Declared, Name, Scope, Value};

pub use operators::Operator;

/// A file of Python code, for the line numbers of errors.
pub struct Source {
    pub path: Rc<str>,
    pub text: Rc<str>,
    line_starts: Vec<u32>,
}

impl Source {
    pub fn new(path: &str, text: &str) -> Rc<Source> {
        let mut line_starts = vec![0];
        for (index, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(index as u32 + 1);
            }
        }
        Rc::new(Source {
            path: path.into(),
            text: text.into(),
            line_starts,
        })
    }

    /// The 1-based line of an offset.
    pub fn line(&self, offset: TextSize) -> usize {
        let offset = offset.to_u32();
        match self.line_starts.binary_search(&offset) {
            Ok(index) => index + 1,
            Err(index) => index,
        }
    }

    pub fn location(&self, offset: TextSize) -> Location {
        Location {
            file: self.path.clone(),
            line: self.line(offset),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Module,
    Function,
    Class,
    Comprehension,
}

/// The state of one running body: its variables and where globals come from.
pub struct Frame {
    pub locals: Rc<Scope>,
    pub globals: Rc<Scope>,
    pub source: Rc<Source>,
    pub kind: FrameKind,
    pub declared: Rc<Declared>,
    /// What a generator body yielded so far.
    pub yields: Option<RefCell<Vec<Value>>>,
    /// The class a method was defined in, and its first argument, for `super()`.
    pub method: Option<(Rc<Class>, Value)>,
}

impl Frame {
    /// The frame of a module's top level, whose locals are its globals.
    pub fn module(globals: Rc<Scope>, source: Rc<Source>) -> Frame {
        Frame {
            locals: globals.clone(),
            globals,
            source,
            kind: FrameKind::Module,
            declared: Rc::new(Declared::default()),
            yields: None,
            method: None,
        }
    }

    /// The scope that functions defined here close over.
    pub fn closure(&self) -> Option<Rc<Scope>> {
        match self.kind {
            FrameKind::Module => None,
            FrameKind::Class => self.locals.parent.clone(),
            FrameKind::Function | FrameKind::Comprehension => Some(self.locals.clone()),
        }
    }
}

/// Runs statements that the host marks in the syntax tree, like Snakemake's rules, which the
/// parser leaves as `...` placeholders in the Python code.
pub trait StatementHook {
    /// Runs the host statement at `offset` of `source`; `false` when there is none.
    fn run(
        &self,
        interpreter: &mut Interpreter,
        frame: &Frame,
        offset: TextSize,
    ) -> Result<bool, Error>;
}

/// Gives attributes to tagged values beyond those of the value they wrap, like the methods
/// of the file objects of a workflow language, which are texts with more methods.
pub trait AttributeHook {
    /// The attribute `name` of a tagged value, or `None` when the host has none.
    fn attribute(
        &self,
        interpreter: &mut Interpreter,
        value: &Value,
        name: &str,
    ) -> Result<Option<Value>, Error>;
}

/// A module the host provides, made when first imported.
pub type ModuleFactory = Rc<dyn Fn(&mut Interpreter) -> Result<Value, Error>>;

pub struct Interpreter {
    pub host: Rc<dyn Host>,
    pub builtins: Rc<Scope>,
    /// Imported modules by full name, like `sys.modules`.
    pub modules: HashMap<String, Value>,
    /// Modules the host provides, by full name.
    pub host_modules: HashMap<String, ModuleFactory>,
    /// Folders searched for `.py` modules: the list that `sys.path` shows.
    pub sys_path: Value,
    /// Parsed modules, kept while functions defined in them may run.
    pub(crate) retained: Vec<Rc<Suite>>,
    pub hook: Option<Rc<dyn StatementHook>>,
    pub attribute_hook: Option<Rc<dyn AttributeHook>>,
    /// The local names of the code that calls a native object, for natives that read them,
    /// like Snakemake's `shell()`, which formats its command with them.
    pub caller_locals: Option<Rc<Scope>>,
    /// Statements executed in the current evaluation, and the most allowed, so that a loop
    /// that never ends stops. Each evaluation the host starts gets the whole budget.
    pub steps: u64,
    pub step_limit: u64,
    evaluating: bool,
    depth: usize,
    pub depth_limit: usize,
    /// Where the stack was when the interpreter was made, and how many bytes below it calls
    /// may use before they fail with `RecursionError` instead of overflowing the stack.
    stack_start: usize,
    pub stack_limit: usize,
    /// Exceptions being handled, innermost last, for a bare `raise`.
    handling: Vec<Error>,
    representing: Vec<usize>,
    /// The `sys.argv` and similar values that code may ask for.
    pub argv: Vec<String>,
    /// The Python process of the packages without a stand-in (decision 19).
    pub packages: Rc<crate::packages::Packages>,
}

impl Interpreter {
    pub fn new(host: Rc<dyn Host>) -> Interpreter {
        let builtins = Scope::new(None);
        let mut interpreter = Interpreter {
            host,
            builtins: builtins.clone(),
            modules: HashMap::new(),
            host_modules: HashMap::new(),
            sys_path: Value::list(Vec::new()),
            retained: Vec::new(),
            hook: None,
            attribute_hook: None,
            caller_locals: None,
            steps: 0,
            step_limit: 500_000_000,
            evaluating: false,
            depth: 0,
            depth_limit: 1000,
            stack_start: stack_address(),
            stack_limit: 1 << 20,
            handling: Vec::new(),
            representing: Vec::new(),
            argv: vec!["snakemake".into()],
            packages: Rc::default(),
        };
        crate::builtins::install(&mut interpreter);
        interpreter
    }

    /// A new module scope with `__name__` set.
    pub fn new_globals(&self, name: &str, file: Option<&str>) -> Rc<Scope> {
        let globals = Scope::new(None);
        globals.set("__name__", Value::str(name));
        globals.set("__builtins__", Value::None);
        globals.set("__doc__", Value::None);
        if let Some(file) = file {
            globals.set("__file__", Value::str(file));
        }
        globals
    }

    /// Parses Python code.
    pub fn parse(path: &str, text: &str) -> Result<Suite, Error> {
        match ruff_python_parser::parse_module(text) {
            Ok(parsed) => Ok(parsed.into_syntax().body),
            Err(error) => {
                let source = Source::new(path, text);
                Err(Error::new("SyntaxError", error.error.to_string())
                    .with_location(source.location(error.location.start())))
            }
        }
    }

    /// Runs a module's statements with the given globals.
    pub fn run(&mut self, suite: &[ruff_python_ast::Stmt], frame: &Frame) -> Result<(), Error> {
        match self.evaluation(|this| this.exec_suite(frame, suite))? {
            statement::Flow::Next => Ok(()),
            _ => Err(Error::new("SyntaxError", "'return' outside function")),
        }
    }

    /// Runs work that the host starts with a fresh step budget; work it starts in turn,
    /// like calls from a module body, shares that budget.
    pub(crate) fn evaluation<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        if self.evaluating {
            return body(self);
        }
        self.evaluating = true;
        self.steps = 0;
        let result = body(self);
        self.evaluating = false;
        result
    }

    /// Runs Python code given as text in new globals, and returns the globals.
    pub fn run_text(&mut self, path: &str, text: &str) -> Result<Rc<Scope>, Error> {
        let suite = Interpreter::parse(path, text)?;
        let globals = self.new_globals("__main__", Some(path));
        let frame = Frame::module(globals.clone(), Source::new(path, text));
        self.run(&suite, &frame)?;
        Ok(globals)
    }

    /// Runs statements given as text in the given globals, like `exec(text, globals)`.
    pub fn exec_text(&mut self, path: &str, text: &str, globals: &Rc<Scope>) -> Result<(), Error> {
        let suite = Interpreter::parse(path, text)?;
        let frame = Frame::module(globals.clone(), Source::new(path, text));
        self.run(&suite, &frame)
    }

    /// Evaluates one expression given as text, with the given globals.
    pub fn eval_text(&mut self, text: &str, globals: &Rc<Scope>) -> Result<Value, Error> {
        let parsed = ruff_python_parser::parse_expression(text)
            .map_err(|error| Error::new("SyntaxError", error.error.to_string()))?;
        let frame = Frame::module(globals.clone(), Source::new("<string>", text));
        self.evaluation(|this| this.eval(&frame, parsed.expr()))
    }

    fn count_step(&mut self) -> Result<(), Error> {
        self.steps += 1;
        if self.steps > self.step_limit {
            return Err(Error::unsupported(format!(
                "code that runs more than {} steps",
                self.step_limit
            )));
        }
        Ok(())
    }

    /// A variable's value as the frame sees it.
    pub fn lookup(&self, frame: &Frame, name: &str) -> Result<Value, Error> {
        if !frame.declared.global.iter().any(|global| &**global == name) {
            let mut scope = Some(&frame.locals);
            while let Some(current) = scope {
                if let Some(value) = current.get(name) {
                    return Ok(value);
                }
                scope = current.parent.as_ref();
            }
        }
        if let Some(value) = frame.globals.get(name) {
            return Ok(value);
        }
        if let Some(value) = self.builtins.get(name) {
            return Ok(value);
        }
        Err(Error::name_error(name))
    }

    /// Binds a variable in the frame, honoring `global` and `nonlocal`.
    pub fn bind(&self, frame: &Frame, name: &str, value: Value) {
        if frame.declared.global.iter().any(|global| &**global == name) {
            frame.globals.set(name, value);
            return;
        }
        if frame
            .declared
            .nonlocal
            .iter()
            .any(|nonlocal| &**nonlocal == name)
        {
            let mut scope = frame.locals.parent.as_ref();
            while let Some(current) = scope {
                if current.vars.borrow().contains_key(name) {
                    current.set(name, value);
                    return;
                }
                scope = current.parent.as_ref();
            }
        }
        frame.locals.set(name, value);
    }

    fn unbind(&self, frame: &Frame, name: &str) -> Result<(), Error> {
        let scope = if frame.declared.global.iter().any(|global| &**global == name) {
            &frame.globals
        } else {
            &frame.locals
        };
        if scope.vars.borrow_mut().shift_remove(name).is_none() {
            return Err(Error::name_error(name));
        }
        Ok(())
    }

    fn enter_call(&mut self) -> Result<(), Error> {
        self.depth += 1;
        let used = self.stack_start.saturating_sub(stack_address());
        if self.depth > self.depth_limit || used > self.stack_limit {
            self.depth -= 1;
            return Err(Error::new(
                "RecursionError",
                "maximum recursion depth exceeded",
            ));
        }
        Ok(())
    }

    fn leave_call(&mut self) {
        self.depth -= 1;
    }

    /// `str(value)`.
    pub fn to_str(&mut self, value: &Value) -> Result<String, Error> {
        match value.plain() {
            Value::Str(text) => Ok(text.to_string()),
            Value::Instance(_) => {
                if let Some(method) = self.lookup_special(value, "__str__") {
                    let text = self.call(&method, crate::value::Args::default())?;
                    return Ok(text
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| crate::format::plain_str(&text)));
                }
                if types::is_exception(value) {
                    return Ok(crate::error::exception_text(value));
                }
                self.repr(value)
            }
            Value::Native(object) => match object.to_str(self)? {
                Some(text) => Ok(text),
                None => object.clone().repr(self),
            },
            other => self.repr(other),
        }
    }

    /// `repr(value)`.
    pub fn repr(&mut self, value: &Value) -> Result<String, Error> {
        let recursive = match value.plain() {
            Value::List(_) => Some("[...]"),
            Value::Dict(_) => Some("{...}"),
            Value::Tuple(_) => Some("(...)"),
            _ => None,
        };
        let identity = value.address();
        if let Some(placeholder) = recursive
            && self.representing.contains(&identity)
        {
            return Ok(placeholder.to_owned());
        }
        self.enter_call()?;
        if recursive.is_some() {
            self.representing.push(identity);
        }
        let result = self.repr_inner(value);
        if recursive.is_some() {
            self.representing.pop();
        }
        self.leave_call();
        result
    }

    fn repr_inner(&mut self, value: &Value) -> Result<String, Error> {
        let value = value.plain();
        match value {
            Value::List(items) => {
                let items = items.borrow().clone();
                let parts = items
                    .iter()
                    .map(|item| self.repr(item))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("[{}]", parts.join(", ")))
            }
            Value::Tuple(items) => {
                let parts = items
                    .iter()
                    .map(|item| self.repr(item))
                    .collect::<Result<Vec<_>, _>>()?;
                if parts.len() == 1 {
                    Ok(format!("({},)", parts[0]))
                } else {
                    Ok(format!("({})", parts.join(", ")))
                }
            }
            Value::Dict(dict) => {
                let entries: Vec<(Value, Value)> = dict
                    .borrow()
                    .entries
                    .iter()
                    .map(|(key, value)| (key.0.clone(), value.clone()))
                    .collect();
                let mut parts = Vec::new();
                for (key, value) in entries {
                    parts.push(format!("{}: {}", self.repr(&key)?, self.repr(&value)?));
                }
                let body = format!("{{{}}}", parts.join(", "));
                Ok(match &dict.borrow().kind {
                    crate::value::DictKind::Plain => body,
                    crate::value::DictKind::OrderedDict => {
                        let items: Vec<String> = parts
                            .iter()
                            .map(|part| {
                                let (key, value) = part.split_once(": ").unwrap_or((part, ""));
                                format!("({key}, {value})")
                            })
                            .collect();
                        format!("OrderedDict([{}])", items.join(", "))
                    }
                    crate::value::DictKind::Default(factory) => {
                        format!(
                            "defaultdict({}, {body})",
                            crate::format::plain_repr(factory)
                        )
                    }
                    crate::value::DictKind::Counter => format!("Counter({body})"),
                })
            }
            Value::Instance(_) => {
                if let Some(method) = self.lookup_special(value, "__repr__") {
                    let text = self.call(&method, crate::value::Args::default())?;
                    return Ok(text.as_str().map(str::to_owned).unwrap_or_default());
                }
                if let Value::Instance(instance) = value
                    && instance.class.builtin != Some(types::TypeTag::BaseException)
                {
                    let module = &instance.class.module;
                    return Ok(format!(
                        "<{module}.{} object at {:#x}>",
                        instance.class.name,
                        value.address()
                    ));
                }
                Ok(crate::format::plain_repr(value))
            }
            Value::Native(object) => object.clone().repr(self),
            other => Ok(crate::format::plain_repr(other)),
        }
    }

    /// A special method of an object's class, bound to the object, if the class defines it.
    pub fn lookup_special(&mut self, value: &Value, name: &str) -> Option<Value> {
        match value.plain() {
            Value::Instance(instance) => {
                let function = instance.class.lookup(name)?;
                Some(bind_method(value.plain(), function))
            }
            _ => None,
        }
    }

    /// The text for an error location, if the frame knows one.
    pub fn error_at(&self, error: Error, frame: &Frame, offset: TextSize) -> Error {
        error.with_location(frame.source.location(offset))
    }

    /// Records an exception as being handled while `body` runs, for a bare `raise`.
    fn with_handling<T>(
        &mut self,
        error: Error,
        body: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.handling.push(error);
        let result = body(self);
        self.handling.pop();
        result
    }

    pub fn current_exception(&self) -> Option<&Error> {
        self.handling.last()
    }
}

/// The address of a local variable: how deep the stack is now.
#[inline(never)]
fn stack_address() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

/// Binds a function found on a class to an object, as attribute lookup does.
pub fn bind_method(receiver: &Value, function: Value) -> Value {
    match &function {
        Value::Function(_) | Value::Builtin(_) => Value::Method(Rc::new(crate::value::Method {
            receiver: receiver.clone(),
            function: crate::value::MethodFunction::Value(function),
        })),
        Value::Native(object) if object.type_name() == "staticmethod" => object
            .as_any()
            .downcast_ref::<crate::builtins::Wrapper>()
            .map_or(function.clone(), |wrapper| wrapper.0.clone()),
        Value::Native(object) if object.type_name() == "classmethod" => {
            let inner = object
                .as_any()
                .downcast_ref::<crate::builtins::Wrapper>()
                .map(|wrapper| wrapper.0.clone())
                .unwrap_or(Value::None);
            let class = Value::Class(types::class_of(receiver));
            let class = if let Value::Class(_) = receiver {
                receiver.clone()
            } else {
                class
            };
            Value::Method(Rc::new(crate::value::Method {
                receiver: class,
                function: crate::value::MethodFunction::Value(inner),
            }))
        }
        _ => function,
    }
}

/// The attribute name of a special method, for error messages.
pub fn name_of(value: &Value) -> Name {
    match value.plain() {
        Value::Function(function) => function.name.clone(),
        Value::Builtin(builtin) => builtin.name.clone(),
        Value::Class(class) => class.name.clone(),
        other => types::type_name(other),
    }
}
