//! Calling functions and closures: argument binding, Groovy's list spreading and built-ins.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_parser::Span;

use super::{AtSpan, Error, Frame, Interpreter, thrown, unsupported};
use crate::exception::Exception;
use crate::lowered::{Arguments, Code, Module, Place};
use crate::value::{Closure, List, ListKind, PathValue, Value};

/// The deepest nesting of function and closure calls. Nextflow 26.04.6 overflows its stack
/// after several hundred calls, so scripts that run there stay well below this.
const DEPTH_LIMIT: usize = 4000;

/// When less stack than this is left at a call, the call runs on a new stack segment of
/// `STACK_SEGMENT` bytes; debug builds need far more stack per call than release builds.
const STACK_RESERVE: usize = 1 << 20;
const STACK_SEGMENT: usize = 32 << 20;

impl Interpreter<'_> {
    /// Evaluates call arguments. Named arguments become one map in first position.
    pub(super) fn arguments(
        &mut self,
        frame: &mut Frame,
        arguments: &Arguments,
    ) -> Result<Vec<Value>, Error> {
        let mut values = Vec::with_capacity(arguments.positional.len() + 1);
        if !arguments.named.is_empty() {
            let mut entries = Vec::with_capacity(arguments.named.len());
            for (name, value) in &arguments.named {
                entries.push((Value::String(name.clone()), self.expression(frame, value)?));
            }
            values.push(Value::map(entries));
        }
        for argument in &arguments.positional {
            match &argument.kind {
                // Legacy syntax: `*items` passes the elements of the list.
                crate::lowered::ExpressionKind::Spread(items) => {
                    match self.expression(frame, items)? {
                        Value::List(list) => values.extend(list.borrow().items.iter().cloned()),
                        Value::Set(set) => {
                            values.extend(set.borrow().items.iter().map(|key| key.0.clone()))
                        }
                        Value::Null => values.push(Value::Null),
                        other => values.push(other),
                    }
                }
                _ => values.push(self.expression(frame, argument)?),
            }
        }
        Ok(values)
    }

    pub(super) fn call_closure_value(
        &mut self,
        closure: &Rc<Closure>,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        let Some(arguments) = adapt(&closure.code, arguments.clone()) else {
            return Err(no_signature(
                "groovy.lang.Closure",
                "doCall",
                &arguments,
                span,
            ));
        };
        if argument_rank(&closure.code, &arguments).is_none() {
            return Err(no_signature(
                "groovy.lang.Closure",
                "doCall",
                &arguments,
                span,
            ));
        }
        self.run(
            &closure.code,
            &closure.module,
            closure.captured.clone(),
            arguments,
        )
    }

    /// Calls a function of the script: the first one that takes the arguments as given, else
    /// the first one that takes the elements of a single list argument. A reflective call
    /// wraps what the function throws, as Nextflow's calls by reflection do.
    pub(super) fn call_script_function(
        &mut self,
        module: &Rc<Module>,
        (name, reflective): (&str, bool),
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        let candidates = module.function_names.get(name).cloned().unwrap_or_default();
        let functions: Vec<Rc<Code>> = candidates
            .iter()
            .map(|index| module.functions[*index].clone())
            .collect();
        let (code, arguments) = match choose_function(&functions, &arguments, name, span)? {
            Some(code) => (code, arguments),
            None => match spread(&arguments) {
                Some(items) => match choose_function(&functions, &items, name, span)? {
                    Some(code) => (code, items),
                    None => return Err(no_signature("Script", name, &arguments, span)),
                },
                None => return Err(no_signature("Script", name, &arguments, span)),
            },
        };
        let code = code.clone();
        let result = self.run(&code, module, Vec::new(), arguments);
        match reflective {
            true => result.map_err(invoked),
            false => result,
        }
    }

    /// Runs code in a new frame, on a stack that grows as needed, up to a call depth where
    /// Nextflow would have run out of stack long before.
    pub(super) fn run(
        &mut self,
        code: &Rc<Code>,
        module: &Rc<Module>,
        captured: Vec<Rc<RefCell<Value>>>,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        if self.depth >= DEPTH_LIMIT {
            // Like Groovy's, it is an `Error`, which `catch (Exception e)` does not catch.
            let message = format!("more than {DEPTH_LIMIT} nested calls of functions or closures");
            let error = Exception::new("StackOverflowError", message);
            return Err(thrown(error, code.span));
        }
        self.depth += 1;
        // Operators are typed by the script where the code is written.
        let outer = std::mem::replace(&mut self.types, module.types);
        let result = stacker::maybe_grow(STACK_RESERVE, STACK_SEGMENT, || {
            self.run_frame(code, module, captured, arguments)
        });
        self.types = outer;
        self.depth -= 1;
        result
    }

    /// Binds the arguments in a new frame, evaluating missing defaults, and runs the body.
    fn run_frame(
        &mut self,
        code: &Rc<Code>,
        module: &Rc<Module>,
        captured: Vec<Rc<RefCell<Value>>>,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        let mut frame = Frame {
            module: module.clone(),
            slots: (0..code.slot_count)
                .map(|_| Rc::new(RefCell::new(Value::Null)))
                .collect(),
            captured,
            unset: RefCell::default(),
        };
        let mut arguments = arguments.into_iter();
        if code.implicit_parameter {
            frame.set(Place::Local(0), arguments.next().unwrap_or(Value::Null));
        }
        for parameter in &code.parameters {
            let value = match (arguments.next(), &parameter.default) {
                (Some(value), _) => value,
                (None, Some(default)) => self.expression(&mut frame, default)?,
                (None, None) => Value::Null,
            };
            let value = self.convert(value, parameter.class.as_deref(), code.span)?;
            frame.set(Place::Local(parameter.slot), value);
        }
        let value = self.block(&mut frame, &code.body)?.value();
        self.convert(value, code.returns.as_deref(), code.span)
    }

    /// Built-in functions of every script.
    pub(super) fn call_builtin(
        &mut self,
        name: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        match (name, arguments.as_slice()) {
            // `record(a: 1, b: 2)`: a `RecordMap` of the named arguments, in their order.
            ("record", [] | [Value::Map(_)]) => {
                let record = match arguments.first() {
                    Some(fields) => super::library::copy_map(fields),
                    None => Value::map([]),
                };
                if let Value::Map(map) = &record {
                    map.borrow_mut().kind = crate::value::MapKind::Record;
                }
                return Ok(record);
            }
            ("println", []) => self.host.print("\n"),
            ("println", [value]) => self.host.print(&format!("{value}\n")),
            ("print", [value]) => self.host.print(&value.to_string()),
            // Groovy's `evaluate(file)`: runs the statements of a Groovy script, like the
            // `Map.metaClass` additions of lib/MapExtended.groovy.
            ("evaluate", [file @ (Value::Path(_) | Value::String(_) | Value::GString(_))]) => {
                let path = match file {
                    Value::Path(path) => (**path).clone(),
                    other => PathValue::parse(&other.to_string(), &self.host.launch_dir()),
                };
                let bytes = self.host.read(&path).map_err(|error| error.or_at(span))?;
                let source = String::from_utf8_lossy(&bytes).into_owned();
                let parsed = nextflow_parser::parse_script(&source);
                let lowered = match parsed.errors.first() {
                    None => crate::lower::lower_module(&parsed.tree, &source).map_err(|errors| {
                        errors
                            .first()
                            .map(|e| e.message.clone())
                            .unwrap_or_default()
                    }),
                    Some(error) => Err(error.message.clone()),
                };
                let module = match lowered {
                    Ok(module) => Rc::new(module),
                    Err(message) => {
                        let message = format!("{}: {message}", path.text());
                        return Err(thrown(
                            Exception::new("MultipleCompilationErrorsException", message),
                            span,
                        ));
                    }
                };
                *module.source.borrow_mut() = Some(source.into());
                *module.path.borrow_mut() = Some(path.text().into());
                self.run_entry(&module)?;
            }
            // Groovy's `sprintf` and `printf`: Java's `String.format`.
            ("sprintf" | "printf", [pattern, rest @ ..]) => {
                let rest = match rest {
                    [Value::List(list)] if list.borrow().kind == ListKind::Array => {
                        list.borrow().items.clone()
                    }
                    rest => rest.to_vec(),
                };
                let text = super::library::format_text(&pattern.to_string(), &rest).at(span)?;
                if name == "sprintf" {
                    return Ok(Value::string(text));
                }
                self.host.print(&text);
            }
            ("tuple", [Value::List(list)]) => {
                let items = list.borrow().items.clone();
                return Ok(tuple(items));
            }
            ("tuple", _) => return Ok(tuple(arguments)),
            ("file" | "files", [.., pattern]) => {
                let options = match arguments.as_slice() {
                    [options @ Value::Map(_), _] => Some(options.clone()),
                    _ => None,
                };
                return self.file(pattern, options.as_ref(), name == "files", span);
            }
            // The criteria of `branch` and `multiMap`, with the labels that lowering adds.
            ("branchCriteria" | "multiMapCriteria", [labels, criteria]) => {
                return Ok(Value::list(vec![labels.clone(), criteria.clone()]));
            }
            ("sleep", [Value::Number(millis)]) => {
                let millis = u64::try_from(millis.to_i64()).unwrap_or(0);
                std::thread::sleep(std::time::Duration::from_millis(millis));
            }
            ("groupKey", [key, Value::Number(size)]) => {
                let key = key.group_target().unwrap_or_else(|| key.clone());
                return Ok(Value::Object(Rc::new(std::cell::RefCell::new(
                    crate::value::Object {
                        class: "nextflow.extension.GroupKey",
                        state: crate::value::State::GroupKey(key, size.to_i32()),
                    },
                ))));
            }
            ("env", [name]) => {
                let value = self.host.environment(&name.to_string());
                return Ok(value.map_or(Value::Null, Value::string));
            }
            ("error", []) => {
                return Err(thrown(Exception::new("AbortOperationException", ""), span));
            }
            // Legacy `exit 1, "message"` ends the run with the message, like `error`.
            ("exit", [Value::Number(code), rest @ ..]) if code.to_i64() != 0 && rest.len() < 2 => {
                let message = rest.first().map_or_else(
                    || format!("the run exits with code {code}"),
                    Value::to_string,
                );
                let exception = Exception::new("AbortOperationException", message);
                return Err(thrown(exception, span));
            }
            ("error", [message]) => {
                let exception = Exception::new("AbortOperationException", message.to_string());
                return Err(thrown(exception, span));
            }
            _ => return Err(unsupported(format!("the function `{name}`"), span)),
        }
        Ok(Value::Null)
    }
}

impl Interpreter<'_> {
    /// `file(pattern)` for a single path. Relative paths start at the launch directory.
    fn file(
        &mut self,
        pattern: &Value,
        options: Option<&Value>,
        list: bool,
        span: Span,
    ) -> Result<Value, Error> {
        let text = match pattern {
            Value::Null => None,
            Value::String(text) | Value::GString(text) => Some(text.to_string()),
            Value::Path(path) => Some(path.uri()),
            other => {
                let message = format!(
                    "Invalid file path type - offending value: {other} [{}]",
                    other.java_class_name()
                );
                return Err(thrown(Exception::illegal_argument(message), span));
            }
        };
        let text = match text {
            Some(text) if !text.is_empty() => text,
            empty => {
                let what = if empty.is_some() { "empty" } else { "null" };
                let message = format!("Argument of `file()` function cannot be {what}");
                return Err(thrown(Exception::illegal_argument(message), span));
            }
        };
        let option = |name: &str| match options {
            Some(Value::Map(map)) => map
                .borrow()
                .entries
                .get(&crate::value::Key(Value::string(name)))
                .cloned(),
            _ => None,
        };
        let check = option("checkIfExists").is_some_and(|value| value.truth());
        if super::glob::is_glob(&text) && option("glob").is_none_or(|value| value.truth()) {
            let options = super::glob::GlobOptions {
                hidden: option("hidden").map(|value| value.truth()),
                ..Default::default()
            };
            let paths =
                super::glob::glob(&mut *self.host, &text, &options).map_err(|e| e.or_at(span))?;
            if paths.is_empty() && check {
                return Err(super::glob::no_match(&text).or_at(span));
            }
            let paths = paths
                .into_iter()
                .map(|path| Value::Path(Rc::new(path)))
                .collect();
            let list = crate::value::List::new(crate::value::ListKind::LinkedList, paths);
            return Ok(Value::List(Rc::new(std::cell::RefCell::new(list))));
        }
        let launch = self.host.launch_dir();
        let path = PathValue::parse(&text, &launch);
        if check
            && self
                .host
                .metadata(&path)
                .map_err(|e| e.or_at(span))?
                .is_none()
        {
            let message = format!("No such file or directory: {}", path.uri());
            return Err(thrown(Exception::new("NoSuchFileException", message), span));
        }
        let path = Value::Path(Rc::new(path));
        Ok(if list { Value::list(vec![path]) } else { path })
    }
}

/// What a function throws as the cause of an `InvocationTargetException`, at the same place.
pub(super) fn invoked(error: Error) -> Error {
    match error {
        Error::Thrown { exception, span } => {
            let mut wrapper = Exception::new("InvocationTargetException", "");
            wrapper.cause = Some(Box::new((*exception).clone()));
            Error::Thrown {
                exception: Rc::new(wrapper),
                span,
            }
        }
        other => other,
    }
}

fn tuple(items: Vec<Value>) -> Value {
    Value::List(Rc::new(RefCell::new(List::new(ListKind::Tuple, items))))
}

/// Whether the code takes this many arguments. Parameters with defaults are optional.
fn accepts(code: &Code, count: usize) -> bool {
    if code.implicit_parameter {
        return count <= 1;
    }
    let required = code
        .parameters
        .iter()
        .filter(|p| p.default.is_none())
        .count();
    (required..=code.parameters.len()).contains(&count)
}

fn argument_rank(code: &Code, arguments: &[Value]) -> Option<usize> {
    code.parameters
        .iter()
        .zip(arguments)
        .map(|(parameter, value)| match parameter.class.as_deref() {
            Some(class) => super::classes::type_rank(class, value),
            None => Some(128),
        })
        .sum()
}

fn choose_function<'a>(
    functions: &'a [Rc<Code>],
    arguments: &[Value],
    name: &str,
    span: Span,
) -> Result<Option<&'a Rc<Code>>, Error> {
    let mut best = None;
    let mut ambiguous = false;
    for code in functions {
        if !accepts(code, arguments.len()) {
            continue;
        }
        let Some(rank) = argument_rank(code, arguments) else {
            continue;
        };
        match best {
            Some((_, previous)) if rank > previous => {}
            Some((_, previous)) if rank == previous => ambiguous = true,
            _ => {
                best = Some((code, rank));
                ambiguous = false;
            }
        }
    }
    if ambiguous {
        return Err(ambiguous_overload(name, span));
    }
    Ok(best.map(|(code, _)| code))
}

pub(super) fn ambiguous_overload(name: &str, span: Span) -> Error {
    thrown(
        Exception::new(
            "GroovyRuntimeException",
            format!("Ambiguous method overloading for method {name}"),
        ),
        span,
    )
}

/// Groovy's argument adaptation: the arguments as given, else the elements of one list. A last
/// parameter of an array type takes the remaining arguments as a list.
fn adapt(code: &Code, mut arguments: Vec<Value>) -> Option<Vec<Value>> {
    if code.parameters.last().is_some_and(|last| last.varargs)
        && arguments.len() + 1 >= code.parameters.len()
        && !code.implicit_parameter
    {
        let rest = arguments.split_off(code.parameters.len() - 1);
        arguments.push(Value::list(rest));
        return Some(arguments);
    }
    if accepts(code, arguments.len()) {
        return Some(arguments);
    }
    spread(&arguments).filter(|items| accepts(code, items.len()))
}

/// The elements of a single list argument; arrays and sets are not spread.
fn spread(arguments: &[Value]) -> Option<Vec<Value>> {
    match arguments {
        [Value::List(list)] if list.borrow().kind != ListKind::Array => {
            Some(list.borrow().items.clone())
        }
        _ => None,
    }
}

/// Groovy's `MissingMethodException` for a call whose arguments fit no method.
pub(super) fn no_signature(class: &str, method: &str, arguments: &[Value], span: Span) -> Error {
    let types: Vec<&str> = arguments.iter().map(Value::class_name).collect();
    let values: Vec<String> = arguments.iter().map(Value::to_string).collect();
    let message = format!(
        "No signature of method: {class}.{method}() is applicable for argument types: ({}) values: [{}]",
        types.join(", "),
        values.join(", ")
    );
    thrown(Exception::new("MissingMethodException", message), span)
}
