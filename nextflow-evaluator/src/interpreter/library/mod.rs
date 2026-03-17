//! The standard library: methods of values by their Java class. It grows with what the
//! nf-core corpus uses; a method it lacks is an error that says so.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod format;
mod iterable;
mod json;
mod list;
mod map;
mod number;
mod path;
mod regex;
mod statics;
mod string;
mod yaml;

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
                None => match self.file_method(path, name, arguments, span)? {
                    Some(value) => Some(value),
                    // Groovy iterates a path: a staged input file as itself, as Nextflow's
                    // task paths do, and another path as its name parts.
                    None => {
                        let items = path_items(path);
                        self.iterable_method(object, &items, name, arguments, span)?
                    }
                },
            },
            Value::Pattern(pattern) => regex::pattern_method(pattern, name, arguments),
            Value::Matcher(matcher) => regex::matcher_method(matcher, name, arguments),
            Value::MemoryUnit(_) | Value::Duration(_) => {
                number::unit_method(object, name, arguments)
            }
            Value::Object(object) => self.object_method(object, name, arguments, span)?,
            Value::Class(class) => match (name, arguments) {
                // An array class like `[Ljava.lang.String;` is `String[]`.
                ("getSimpleName", []) => Some(Value::string(
                    match class.strip_prefix("[L").and_then(|c| c.strip_suffix(';')) {
                        Some(element) => format!("{}[]", crate::value::simple_class_name(element)),
                        None => crate::value::simple_class_name(class).to_owned(),
                    },
                )),
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
        // The method of an operator, like `multiply` for `*`, applies the operator.
        if let [other] = arguments {
            use crate::value::Operator::*;
            let operator = [Add, Subtract, Multiply, Divide, Modulo, Power]
                .into_iter()
                .find(|operator| operator.method() == name);
            if let Some(operator) = operator
                && !matches!(object, Value::List(_) | Value::Set(_) | Value::Map(_))
                && let Ok(value) = object.apply(operator, other)
            {
                return Ok(Some(value));
            }
        }
        // Groovy iterates any other object as one element.
        if let ("collect", [Value::Closure(f)]) = (name, arguments) {
            let result = self.call_closure_value(f, vec![object.clone()], span)?;
            return Ok(Some(Value::list(vec![result])));
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

/// The elements Groovy iterates for a path: a staged input file is one element, itself, as
/// Nextflow's task paths are; another path gives its name parts.
pub(in crate::interpreter) fn path_items(
    path: &std::rc::Rc<crate::value::PathValue>,
) -> Vec<Value> {
    if path.staged.is_some() {
        return vec![Value::Path(path.clone())];
    }
    path.path
        .split('/')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut name = crate::value::PathValue::parse(part, "/");
            name.path = part.to_owned();
            Value::Path(std::rc::Rc::new(name))
        })
        .collect()
}

/// A new `java.net.URI` of the text.
pub(in crate::interpreter) fn uri(text: &str) -> Value {
    Value::Object(std::rc::Rc::new(std::cell::RefCell::new(
        crate::value::Object {
            class: "java.net.URI",
            state: crate::value::State::Uri(text.into()),
        },
    )))
}

/// A part of a URI, as `java.net.URI` gives it: `scheme`, `authority`, `host`, `port`, `path`,
/// `query` or `fragment`; `None` when the URI has no such part.
pub(in crate::interpreter) fn uri_part(text: &str, part: &str) -> Option<String> {
    let (scheme, rest) = match text.split_once(':') {
        Some((scheme, rest))
            if scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)) =>
        {
            (Some(scheme), rest)
        }
        _ => (None, text),
    };
    let (rest, fragment) = match rest.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (rest, None),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((rest, query)) => (rest, Some(query)),
        None => (rest, None),
    };
    let (authority, path) = match rest.strip_prefix("//") {
        Some(rest) => match rest.find('/') {
            Some(slash) => (Some(&rest[..slash]), &rest[slash..]),
            None => (Some(rest), ""),
        },
        None => (None, rest),
    };
    let host_port = authority.map(|authority| authority.rsplit('@').next().unwrap_or(authority));
    let value = match part {
        "scheme" => scheme,
        "authority" => authority.filter(|a| !a.is_empty()),
        "host" => host_port
            .map(|h| h.split(':').next().unwrap_or(h))
            .filter(|h| !h.is_empty()),
        "port" => host_port.and_then(|h| h.split_once(':').map(|(_, port)| port)),
        "path" => Some(path),
        "query" => query,
        "fragment" => fragment,
        _ => None,
    };
    value.map(str::to_owned)
}

/// A new input stream over bytes.
pub(in crate::interpreter) fn stream(bytes: Vec<u8>) -> Value {
    Value::Object(std::rc::Rc::new(std::cell::RefCell::new(
        crate::value::Object {
            class: "java.io.InputStream",
            state: crate::value::State::Stream(bytes.into(), 0),
        },
    )))
}
