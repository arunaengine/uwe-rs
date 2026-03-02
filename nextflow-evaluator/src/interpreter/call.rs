//! Calling functions and closures: argument binding, Groovy's list spreading and built-ins.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_parser::Span;

use super::statement::Completion;
use super::{Error, Frame, Interpreter, thrown, unsupported};
use crate::exception::Exception;
use crate::lowered::{Arguments, Code, Module, Place};
use crate::value::{Closure, List, ListKind, Value};

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
            values.push(self.expression(frame, argument)?);
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
        self.run(
            &closure.code,
            &closure.module,
            closure.captured.clone(),
            arguments,
        )
    }

    /// Calls a function of the script: the first one that takes the arguments as given, else
    /// the first one that takes the elements of a single list argument.
    pub(super) fn call_script_function(
        &mut self,
        module: &Rc<Module>,
        name: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        let candidates = module.function_names.get(name).cloned().unwrap_or_default();
        let functions: Vec<Rc<Code>> = candidates
            .iter()
            .map(|index| module.functions[*index].clone())
            .collect();
        if let Some(code) = functions.iter().find(|code| accepts(code, arguments.len())) {
            return self.run(code, module, Vec::new(), arguments);
        }
        if let Some(items) = spread(&arguments)
            && let Some(code) = functions.iter().find(|code| accepts(code, items.len()))
        {
            return self.run(code, module, Vec::new(), items);
        }
        Err(no_signature("Script", name, &arguments, span))
    }

    /// Runs code in a new frame, binding the arguments and evaluating missing defaults.
    fn run(
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
            frame.set(Place::Local(parameter.slot), value);
        }
        match self.block(&mut frame, &code.body)? {
            Completion::Normal(value) | Completion::Return(value) => Ok(value),
        }
    }

    /// Built-in functions of every script.
    pub(super) fn call_builtin(
        &mut self,
        name: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        match (name, arguments.as_slice()) {
            ("println", []) => self.host.print("\n"),
            ("println", [value]) => self.host.print(&format!("{value}\n")),
            ("print", [value]) => self.host.print(&value.to_string()),
            ("tuple", [Value::List(list)]) => {
                let items = list.borrow().items.clone();
                return Ok(tuple(items));
            }
            ("tuple", _) => return Ok(tuple(arguments)),
            ("error", []) => {
                return Err(thrown(Exception::new("AbortOperationException", ""), span));
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

fn tuple(items: Vec<Value>) -> Value {
    Value::List(Rc::new(RefCell::new(List {
        kind: ListKind::Tuple,
        items,
    })))
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

/// Groovy's argument adaptation: the arguments as given, else the elements of one list.
fn adapt(code: &Code, arguments: Vec<Value>) -> Option<Vec<Value>> {
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
fn no_signature(class: &str, method: &str, arguments: &[Value], span: Span) -> Error {
    let types: Vec<&str> = arguments.iter().map(Value::class_name).collect();
    let values: Vec<String> = arguments.iter().map(Value::to_string).collect();
    let message = format!(
        "No signature of method: {class}.{method}() is applicable for argument types: ({}) values: [{}]",
        types.join(", "),
        values.join(", ")
    );
    thrown(Exception::new("MissingMethodException", message), span)
}
