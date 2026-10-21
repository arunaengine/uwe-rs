//! Errors of evaluation: Python exceptions, which code can catch, and unsupported features
//! and conditions that stop the run, which it cannot.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::types;
use crate::value::{Instance, Value};

/// Where an error happened: a file and a 1-based line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub file: Rc<str>,
    pub line: usize,
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.file, self.line)
    }
}

#[derive(Clone)]
pub enum ErrorKind {
    /// A raised Python exception object.
    Exception(Value),
    /// A feature of Python or of a library that iunctus does not support; `except` does
    /// not catch it.
    Unsupported(String),
    /// A condition that stops the run, like a Python package that needs a container when no
    /// container runtime is available (decision 19); `except` does not catch it.
    Stop(String),
}

/// An error with the innermost place of the Python code that raised it.
#[derive(Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub location: Option<Location>,
}

impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(location) = &self.location {
            write!(f, "{location}: ")?;
        }
        match &self.kind {
            ErrorKind::Unsupported(message) => write!(f, "Not supported yet: {message}"),
            ErrorKind::Stop(message) => write!(f, "{message}"),
            ErrorKind::Exception(_) => {
                let message = self.message();
                if message.is_empty() {
                    write!(f, "{}", self.class_name())
                } else {
                    write!(f, "{}: {}", self.class_name(), message)
                }
            }
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// A builtin exception with one text argument, like `ValueError("bad value")`.
    pub fn new(class: &str, message: impl Into<String>) -> Error {
        let class = types::exception_class(class)
            .unwrap_or_else(|| types::exception_class("Exception").expect("Exception exists"));
        Error::from_value(exception(&class, vec![Value::str(message.into())]))
    }

    pub fn from_value(value: Value) -> Error {
        Error {
            kind: ErrorKind::Exception(value),
            location: None,
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Error {
        Error {
            kind: ErrorKind::Unsupported(message.into()),
            location: None,
        }
    }

    /// An error that stops the run and that `except` does not catch.
    pub fn stop(message: impl Into<String>) -> Error {
        Error {
            kind: ErrorKind::Stop(message.into()),
            location: None,
        }
    }

    pub fn type_error(message: impl Into<String>) -> Error {
        Error::new("TypeError", message)
    }

    pub fn value_error(message: impl Into<String>) -> Error {
        Error::new("ValueError", message)
    }

    pub fn index_error(message: impl Into<String>) -> Error {
        Error::new("IndexError", message)
    }

    pub fn attribute_error(message: impl Into<String>) -> Error {
        Error::new("AttributeError", message)
    }

    pub fn name_error(name: &str) -> Error {
        Error::new("NameError", format!("name '{name}' is not defined"))
    }

    pub fn runtime_error(message: impl Into<String>) -> Error {
        Error::new("RuntimeError", message)
    }

    pub fn zero_division(message: impl Into<String>) -> Error {
        Error::new("ZeroDivisionError", message)
    }

    pub fn stop_iteration() -> Error {
        let class = types::exception_class("StopIteration").expect("StopIteration exists");
        Error::from_value(exception(&class, Vec::new()))
    }

    /// `KeyError(key)`, whose message is the key's repr.
    pub fn key_error(key: Value) -> Error {
        let class = types::exception_class("KeyError").expect("KeyError exists");
        Error::from_value(exception(&class, vec![key]))
    }

    /// An `OSError` subclass with errno and file name, as Python gives for a failed call.
    pub fn os_error(error: &std::io::Error, path: &str) -> Error {
        let (class, errno, text) = match error.kind() {
            std::io::ErrorKind::NotFound => ("FileNotFoundError", 2, "No such file or directory"),
            std::io::ErrorKind::PermissionDenied => ("PermissionError", 13, "Permission denied"),
            std::io::ErrorKind::AlreadyExists => ("FileExistsError", 17, "File exists"),
            std::io::ErrorKind::IsADirectory => ("IsADirectoryError", 21, "Is a directory"),
            std::io::ErrorKind::NotADirectory => ("NotADirectoryError", 20, "Not a directory"),
            _ => ("OSError", 5, "Input/output error"),
        };
        let class = types::exception_class(class).expect("OS errors exist");
        let value = exception(
            &class,
            vec![Value::Int(errno), Value::str(text), Value::str(path)],
        );
        if let Value::Instance(instance) = &value {
            let mut dict = instance.dict.borrow_mut();
            dict.insert("errno".into(), Value::Int(errno));
            dict.insert("strerror".into(), Value::str(text));
            dict.insert("filename".into(), Value::str(path));
        }
        Error::from_value(value)
    }

    pub fn file_not_found(path: &str) -> Error {
        Error::os_error(&std::io::Error::from(std::io::ErrorKind::NotFound), path)
    }

    pub fn with_location(mut self, location: Location) -> Error {
        if self.location.is_none() {
            self.location = Some(location);
        }
        self
    }

    pub fn is_unsupported(&self) -> bool {
        matches!(self.kind, ErrorKind::Unsupported(_))
    }

    /// The exception object, if the error is an exception.
    pub fn exception(&self) -> Option<&Value> {
        match &self.kind {
            ErrorKind::Exception(value) => Some(value),
            ErrorKind::Unsupported(_) | ErrorKind::Stop(_) => None,
        }
    }

    /// Whether the error is an exception of the named class or a subclass of it.
    pub fn is(&self, class: &str) -> bool {
        match &self.kind {
            ErrorKind::Exception(value) => types::class_of(value).derives_from(class),
            ErrorKind::Unsupported(_) | ErrorKind::Stop(_) => false,
        }
    }

    pub fn class_name(&self) -> String {
        match &self.kind {
            ErrorKind::Exception(value) => types::type_name(value).to_string(),
            ErrorKind::Unsupported(_) => "Unsupported".into(),
            ErrorKind::Stop(_) => "Stop".into(),
        }
    }

    /// `str()` of the exception, without calling Python code.
    pub fn message(&self) -> String {
        match &self.kind {
            ErrorKind::Unsupported(message) | ErrorKind::Stop(message) => message.clone(),
            ErrorKind::Exception(value) => exception_text(value),
        }
    }
}

/// A new exception object of a class with the given arguments.
pub fn exception(class: &Rc<crate::value::Class>, args: Vec<Value>) -> Value {
    let mut dict = IndexMap::new();
    // `StopIteration.value`, what a generator returned.
    if class.derives_from("StopIteration") {
        dict.insert("value".into(), args.first().cloned().unwrap_or(Value::None));
    }
    dict.insert("args".into(), Value::tuple(args));
    Value::Instance(Rc::new(Instance {
        class: class.clone(),
        dict: RefCell::new(dict),
        hashed: Default::default(),
    }))
}

/// The arguments of an exception object.
pub fn exception_args(value: &Value) -> Vec<Value> {
    if let Value::Instance(instance) = value.plain()
        && let Some(Value::Tuple(args)) = instance.dict.borrow().get("args")
    {
        return args.to_vec();
    }
    Vec::new()
}

/// `str(exception)` for exceptions whose class does not define `__str__`.
pub fn exception_text(value: &Value) -> String {
    let args = exception_args(value);
    let class = types::class_of(value);
    let key_error = class.derives_from("KeyError");
    if class.derives_from("OSError")
        && let [Value::Int(errno), Value::Str(text), path] = args.as_slice()
    {
        return format!(
            "[Errno {errno}] {text}: {}",
            crate::format::plain_repr(path)
        );
    }
    match args.as_slice() {
        [] => String::new(),
        [single] if key_error => crate::format::plain_repr(single),
        [single] => crate::format::plain_str(single),
        _ => crate::format::plain_repr(&Value::tuple(args)),
    }
}
