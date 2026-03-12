//! The standard library: methods of values by their Java class. It grows with what the
//! nf-core corpus uses; a method it lacks is an error that says so.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod iterable;
mod json;
mod list;
mod map;
mod number;
mod path;
mod regex;
mod statics;
mod string;

pub(super) use iterable::elements;
pub use json::parse as parse_json;
pub(super) use number::{convert, unit_property};

use nextflow_parser::Span;

use super::{AtSpan, Error, Interpreter};
use crate::value::Value;

impl Interpreter<'_> {
    /// A library method of the receiver's class, or `None` when the class has no method of
    /// that name for these arguments.
    pub(super) fn library_method(
        &mut self,
        object: &Value,
        name: &str,
        arguments: &[Value],
        span: Span,
    ) -> Result<Option<Value>, Error> {
        let found = match object {
            Value::String(text) | Value::GString(text) => {
                self.string_method(text, name, arguments, span)?
            }
            Value::List(list) => match self.list_method(list, name, arguments, span)? {
                Some(value) => Some(value),
                None => {
                    let items = list.borrow().items.clone();
                    self.iterable_method(object, &items, name, arguments, span)?
                }
            },
            Value::Set(set) => match self.set_method(set, name, arguments, span)? {
                Some(value) => Some(value),
                None => {
                    let items: Vec<Value> = set
                        .borrow()
                        .items
                        .iter()
                        .map(|item| item.0.clone())
                        .collect();
                    self.iterable_method(object, &items, name, arguments, span)?
                }
            },
            Value::Map(map) => self.map_method(map, name, arguments, span)?,
            Value::MapEntry(pair) => map::entry_method(pair, name, arguments),
            Value::Number(number) => self.number_method(number, name, arguments, span)?,
            Value::Path(path) => match path::path_method(path, name, arguments) {
                Some(value) => Some(value),
                None => self.file_method(path, name, arguments, span)?,
            },
            Value::Pattern(pattern) => regex::pattern_method(pattern, name, arguments),
            Value::Matcher(matcher) => regex::matcher_method(matcher, name, arguments),
            Value::MemoryUnit(_) | Value::Duration(_) => {
                number::unit_method(object, name, arguments)
            }
            Value::Object(object) => self.object_method(object, name, arguments, span)?,
            Value::Class(class) => match (name, arguments) {
                ("getSimpleName", []) => {
                    Some(Value::string(crate::value::simple_class_name(class)))
                }
                ("getName", []) => Some(Value::String(class.clone())),
                _ => None,
            },
            Value::Boolean(value) => match (name, arguments) {
                ("toBoolean" | "booleanValue", []) => Some(Value::Boolean(*value)),
                _ => None,
            },
            Value::Date(millis) => match (name, arguments) {
                ("format", [pattern]) => {
                    let text = crate::date::format(*millis, &pattern.to_string()).at(span)?;
                    Some(Value::string(text))
                }
                ("getTime", []) => Some(Value::Number(crate::number::Number::Long(*millis))),
                _ => None,
            },
            _ => None,
        };
        if found.is_some() {
            return Ok(found);
        }
        Ok(object_method(object, name, arguments))
    }
}

/// Methods every object has: Groovy's `toString`, `equals` and `hashCode`.
fn object_method(object: &Value, name: &str, arguments: &[Value]) -> Option<Value> {
    Some(match (name, arguments) {
        ("toString", []) => Value::string(object.to_groovy_string()),
        ("equals", [other]) => Value::Boolean(match object {
            Value::List(_) | Value::Map(_) | Value::Set(_) => object.groovy_equals(other),
            _ => object.java_equals(other),
        }),
        ("hashCode", []) => Value::integer(object.java_hash()),
        ("getClass", []) => Value::Class(object.java_class_name().into()),
        ("inspect", []) => Value::string(inspect(object)),
        _ => return None,
    })
}

/// Groovy's `inspect()`: text in quotes with escapes, and lists and maps of inspected values.
fn inspect(value: &Value) -> String {
    match value {
        Value::String(text) | Value::GString(text) => {
            let escaped = text
                .replace('\\', "\\\\")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t")
                .replace('\u{c}', "\\f")
                .replace('\'', "\\'");
            format!("'{escaped}'")
        }
        Value::List(list) => {
            let items: Vec<String> = list.borrow().items.iter().map(inspect).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Map(map) => {
            let map = map.borrow();
            if map.entries.is_empty() {
                return "[:]".into();
            }
            let entries: Vec<String> = map
                .entries
                .iter()
                .map(|(key, value)| format!("{}:{}", inspect(&key.0), inspect(value)))
                .collect();
            format!("[{}]", entries.join(", "))
        }
        other => other.to_groovy_string(),
    }
}

/// A new map with the entries of a map, as `toMap()` gives it.
pub(in crate::interpreter) fn copy_map(map: &Value) -> Value {
    match map {
        Value::Map(map) => Value::Map(std::rc::Rc::new(std::cell::RefCell::new(
            crate::value::Map {
                kind: crate::value::MapKind::LinkedHashMap,
                entries: map.borrow().entries.clone(),
            },
        ))),
        other => other.clone(),
    }
}
