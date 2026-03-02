//! The interpreter: runs the lowered tree with Groovy's semantics.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod access;
mod call;
mod expression;
mod statement;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use nextflow_parser::Span;

use crate::exception::Exception;
use crate::lowered::{Module, Place};
use crate::value::{Closure, Value};

/// What the interpreter needs from its surroundings.
pub trait Host {
    /// Writes text to standard output, as `print` and `println` do.
    fn print(&mut self, text: &str);
}

/// Why running code stopped before its end.
#[derive(Clone, Debug)]
pub enum Error {
    /// A Java exception or error. Scripts can catch exceptions with `try` and `catch`.
    Thrown {
        exception: Rc<Exception>,
        span: Span,
    },
    /// A feature that Nextflow supports but iunctus does not yet. Scripts cannot catch it.
    Unsupported { message: String, span: Span },
}

impl Error {
    pub fn span(&self) -> Span {
        match self {
            Error::Thrown { span, .. } | Error::Unsupported { span, .. } => *span,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Thrown { exception, .. } => write!(f, "{exception}"),
            Error::Unsupported { message, .. } => write!(f, "Not supported yet: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// Runs functions and closures of lowered scripts.
pub struct Interpreter<'h> {
    host: &'h mut dyn Host,
    globals: HashMap<Rc<str>, Value>,
}

impl<'h> Interpreter<'h> {
    pub fn new(host: &'h mut dyn Host) -> Self {
        Self {
            host,
            globals: HashMap::new(),
        }
    }

    /// Sets a built-in constant, like `params` or `workflow`.
    pub fn set_global(&mut self, name: &str, value: Value) {
        self.globals.insert(name.into(), value);
    }

    /// Calls a function of a script by name, as Groovy selects it by its arguments.
    pub fn call_function(
        &mut self,
        module: &Rc<Module>,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        self.call_script_function(module, name, arguments, Span::default())
    }

    /// Calls a closure with Groovy's argument adaptation.
    pub fn call_closure(
        &mut self,
        closure: &Rc<Closure>,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        self.call_closure_value(closure, arguments, Span::default())
    }
}

/// The variables of one running function or closure.
struct Frame {
    module: Rc<Module>,
    slots: Vec<Rc<RefCell<Value>>>,
    captured: Vec<Rc<RefCell<Value>>>,
}

impl Frame {
    fn cell(&self, place: Place) -> &Rc<RefCell<Value>> {
        match place {
            Place::Local(slot) => &self.slots[slot],
            Place::Captured(index) => &self.captured[index],
        }
    }

    fn get(&self, place: Place) -> Value {
        self.cell(place).borrow().clone()
    }

    fn set(&self, place: Place, value: Value) {
        *self.cell(place).borrow_mut() = value;
    }
}

/// Attaches a position to a Java exception thrown by value code.
fn thrown(exception: Exception, span: Span) -> Error {
    Error::Thrown {
        exception: Rc::new(exception),
        span,
    }
}

fn unsupported(message: impl Into<String>, span: Span) -> Error {
    Error::Unsupported {
        message: message.into(),
        span,
    }
}

/// Converts a value operation's result, adding the position of the operation.
trait AtSpan<T> {
    fn at(self, span: Span) -> Result<T, Error>;
}

impl<T> AtSpan<T> for Result<T, Exception> {
    fn at(self, span: Span) -> Result<T, Error> {
        self.map_err(|exception| thrown(exception, span))
    }
}
