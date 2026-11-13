//! `java.lang.StringBuilder` and `StringBuffer`: text that their methods change in place.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use nextflow_parser::Span;

use super::super::{Error, thrown};
use crate::exception::Exception;
use crate::value::{Object, State, Value};

/// A new builder of the class, `StringBuilder` or `StringBuffer`, with this text.
pub(in crate::interpreter) fn builder(class: &str, text: String) -> Value {
    let class = match class {
        "StringBuffer" => "java.lang.StringBuffer",
        _ => "java.lang.StringBuilder",
    };
    Value::Object(Rc::new(RefCell::new(Object {
        class,
        state: State::Builder(text),
    })))
}

/// A method of a builder; positions count characters.
pub(super) fn builder_method(
    object: &Rc<RefCell<Object>>,
    name: &str,
    arguments: &[Value],
    span: Span,
) -> Result<Option<Value>, Error> {
    let this = Value::Object(object.clone());
    let mut object = object.borrow_mut();
    let State::Builder(text) = &mut object.state else {
        return Ok(None);
    };
    let mut characters: Vec<char> = text.chars().collect();
    let length = characters.len();
    let position = |value: &Value, limit: usize| match value {
        Value::Number(number) => usize::try_from(number.to_i64())
            .ok()
            .filter(|p| *p <= limit),
        _ => None,
    };
    let outside = |value: &Value| {
        let message = format!("index {value},length {length}");
        Err(thrown(
            Exception::new("StringIndexOutOfBoundsException", message),
            span,
        ))
    };
    let result = match (name, arguments) {
        ("append" | "leftShift", [value]) => {
            text.push_str(&value.to_string());
            return Ok(Some(this));
        }
        ("insert", [at, value]) => {
            let Some(at) = position(at, length) else {
                return outside(at);
            };
            characters.splice(at..at, value.to_string().chars());
            this
        }
        ("reverse", []) => {
            characters.reverse();
            this
        }
        ("setLength", [size]) => {
            let Some(size) = position(size, usize::MAX) else {
                return outside(size);
            };
            characters.resize(size, '\0');
            Value::Null
        }
        ("deleteCharAt", [at]) => {
            let Some(at) = position(at, length).filter(|at| *at < length) else {
                return outside(at);
            };
            characters.remove(at);
            this
        }
        ("delete", [from, to]) => {
            let to = position(to, usize::MAX).map(|to| to.min(length));
            let (Some(from), Some(to)) = (position(from, length), to) else {
                return outside(from);
            };
            characters.drain(from..to.max(from));
            this
        }
        ("replace", [from, to, value]) => {
            let to = position(to, usize::MAX).map(|to| to.min(length));
            let (Some(from), Some(to)) = (position(from, length), to) else {
                return outside(from);
            };
            characters.splice(from..to.max(from), value.to_string().chars());
            this
        }
        ("setCharAt", [at, Value::Character(unit)]) => {
            let Some(at) = position(at, length).filter(|at| *at < length) else {
                return outside(at);
            };
            characters[at] = char::from_u32(u32::from(*unit)).unwrap_or('\u{fffd}');
            Value::Null
        }
        ("toString", []) => return Ok(Some(Value::string(text.as_str()))),
        ("length" | "size", []) => return Ok(Some(Value::integer(length as i32))),
        ("isEmpty", []) => return Ok(Some(Value::Boolean(length == 0))),
        ("plus", [value]) => return Ok(Some(Value::string(format!("{text}{value}")))),
        _ => return Ok(None),
    };
    *text = characters.into_iter().collect();
    Ok(Some(result))
}
