//! Properties, indexes, method calls, casts, `instanceof` and `new`, for the classes whose
//! rules are part of the core. The standard library adds the other methods.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_parser::Span;

use super::{AtSpan, Error, Interpreter, thrown, unsupported};
use crate::exception::{self, Exception};
use crate::number::Number;
use crate::value::{Key, List, ListKind, MapKind, Value};

impl Interpreter<'_> {
    /// `object.name`. A map reads the key, so `m.class` is `m.get('class')`.
    pub(super) fn property(
        &mut self,
        object: &Value,
        name: &str,
        span: Span,
    ) -> Result<Value, Error> {
        Ok(match object {
            Value::Null => {
                let message = format!("Cannot get property '{name}' on null object");
                return Err(thrown(
                    Exception::new("NullPointerException", message),
                    span,
                ));
            }
            Value::Map(map) => {
                let key = Key(Value::string(name));
                let value = map.borrow().entries.get(&key).cloned();
                match value {
                    Some(Value::Closure(closure)) if map.borrow().kind == MapKind::Lazy => {
                        self.call_closure_value(&closure, Vec::new(), span)?
                    }
                    value => value.unwrap_or(Value::Null),
                }
            }
            Value::ChannelOut(out) => {
                let found = out
                    .names
                    .iter()
                    .position(|emit| emit.as_deref() == Some(name));
                match found {
                    Some(index) => out.values[index].clone(),
                    None => {
                        let message = format!("Unknown output `{name}`");
                        return Err(thrown(
                            Exception::new("MissingPropertyException", message),
                            span,
                        ));
                    }
                }
            }
            Value::Definition(definition) => self.definition_property(definition, name, span)?,
            Value::Exception(exception) if name == "message" => match exception.message.as_str() {
                "" => Value::Null,
                message => Value::string(message),
            },
            Value::List(_) | Value::Set(_) => match name {
                "size" | "first" | "last" => self.call_method(object, name, Vec::new(), span)?,
                "empty" => self.call_method(object, "isEmpty", Vec::new(), span)?,
                // Other names are read from each element that is not null.
                _ => {
                    let mut values = Vec::new();
                    for item in super::library::elements(object) {
                        if !matches!(item, Value::Null) {
                            values.push(self.property(&item, name, span)?);
                        }
                    }
                    Value::list(values)
                }
            },
            _ => return self.getter(object, name, span),
        })
    }

    /// A property of an object that is not a map: its getter `getName()` or `isName()`. A number
    /// without such a getter reads a unit, like `2.GB`.
    fn getter(&mut self, object: &Value, name: &str, span: Span) -> Result<Value, Error> {
        let mut capitalized = name.chars();
        let first = capitalized
            .next()
            .map(|c| c.to_uppercase().collect::<String>());
        let capitalized = format!("{}{}", first.unwrap_or_default(), capitalized.as_str());
        for prefix in ["get", "is"] {
            let getter = format!("{prefix}{capitalized}");
            if let Some(value) = self.library_method(object, &getter, &[], span)? {
                return Ok(value);
            }
        }
        if let Value::Number(number) = object {
            return super::library::unit_property(number, name, span);
        }
        let class = object.class_name();
        Err(unsupported(
            format!("the property `{name}` of {class}"),
            span,
        ))
    }

    /// `object.name = value`. Records reject changes.
    pub(super) fn set_property(
        &mut self,
        object: &Value,
        name: &str,
        value: Value,
        span: Span,
    ) -> Result<(), Error> {
        match object {
            Value::Null => {
                let message = format!("Cannot set property '{name}' on null object");
                Err(thrown(
                    Exception::new("NullPointerException", message),
                    span,
                ))
            }
            Value::Map(map) if map.borrow().kind == MapKind::Record => Err(rejected(span)),
            Value::Map(map) => {
                map.borrow_mut()
                    .entries
                    .insert(Key(Value::string(name)), value);
                Ok(())
            }
            _ => {
                let class = object.class_name();
                Err(unsupported(
                    format!("setting the property `{name}` of {class}"),
                    span,
                ))
            }
        }
    }

    /// `object[index]`, with Groovy's rules: past the end of a list is `null`, a negative index
    /// counts from the end, and a string gives a one-character string.
    pub(super) fn index(
        &mut self,
        object: &Value,
        index: &[Value],
        span: Span,
    ) -> Result<Value, Error> {
        let [index] = index else {
            return Err(unsupported("indexing with several indexes", span));
        };
        Ok(match (object, index) {
            (Value::Null, _) => {
                let message = "Cannot invoke method getAt() on null object";
                return Err(thrown(
                    Exception::new("NullPointerException", message),
                    span,
                ));
            }
            (Value::Map(map), key) => map
                .borrow()
                .entries
                .get(&Key(key.clone()))
                .cloned()
                .unwrap_or(Value::Null),
            // All matches from the start; a pattern with groups gives a list per match.
            (Value::ChannelOut(out), Value::Number(number)) if integral(number) => {
                match position(number.to_i64(), out.values.len()) {
                    Some(position) if position < out.values.len() => out.values[position].clone(),
                    _ => {
                        let message = format!(
                            "Index {number} out of bounds for length {}",
                            out.values.len()
                        );
                        return Err(thrown(
                            Exception::new("IndexOutOfBoundsException", message),
                            span,
                        ));
                    }
                }
            }
            (Value::Matcher(matcher), Value::Number(number)) if integral(number) => {
                let count = matcher.matches.len();
                let groups = match position(number.to_i64(), count).filter(|p| *p < count) {
                    Some(position) => &matcher.matches[position],
                    None => {
                        let message = format!(
                            "index is out of range {}..{} (index = {number})",
                            -(count as i64),
                            count as i64 - 1
                        );
                        return Err(thrown(
                            Exception::new("IndexOutOfBoundsException", message),
                            span,
                        ));
                    }
                };
                let text =
                    |group: &Option<String>| group.as_deref().map_or(Value::Null, Value::string);
                if matcher.pattern.group_count() == 0 {
                    text(&groups[0])
                } else {
                    Value::list(groups.iter().map(text).collect())
                }
            }
            (Value::List(list), Value::List(indexes)) if indexes.borrow().range.is_some() => {
                let list = list.borrow();
                let (from, to, reverse) = borders(&indexes.borrow(), list.items.len(), span)?;
                let mut items = list.items[from..to].to_vec();
                if reverse {
                    items.reverse();
                }
                Value::list(items)
            }
            (Value::String(text) | Value::GString(text), Value::List(indexes))
                if indexes.borrow().range.is_some() =>
            {
                let units: Vec<u16> = text.encode_utf16().collect();
                let (from, to, reverse) = borders(&indexes.borrow(), units.len(), span)?;
                let part = String::from_utf16_lossy(&units[from..to]);
                Value::string(if reverse {
                    part.chars().rev().collect()
                } else {
                    part
                })
            }
            (Value::List(list), Value::Number(number)) if integral(number) => {
                let list = list.borrow();
                let length = list.items.len();
                match position(number.to_i64(), length) {
                    Some(position) if position < length => list.items[position].clone(),
                    Some(_) if list.kind != ListKind::Array => Value::Null,
                    _ => {
                        return Err(out_of_bounds(
                            "ArrayIndexOutOfBoundsException",
                            number,
                            length,
                            span,
                        ));
                    }
                }
            }
            (Value::String(text) | Value::GString(text), Value::Number(number))
                if integral(number) =>
            {
                let units: Vec<u16> = text.encode_utf16().collect();
                match position(number.to_i64(), units.len()) {
                    Some(position) if position < units.len() => {
                        Value::string(String::from_utf16_lossy(&units[position..=position]))
                    }
                    Some(_) => {
                        return Err(out_of_bounds(
                            "StringIndexOutOfBoundsException",
                            number,
                            units.len(),
                            span,
                        ));
                    }
                    None => {
                        return Err(out_of_bounds(
                            "ArrayIndexOutOfBoundsException",
                            number,
                            units.len(),
                            span,
                        ));
                    }
                }
            }
            _ => {
                let class = object.class_name();
                let index = index.class_name();
                return Err(unsupported(format!("indexing {class} with {index}"), span));
            }
        })
    }

    /// `object[index] = value`. A list grows with `null`s; tuples and records reject changes.
    pub(super) fn set_index(
        &mut self,
        object: &Value,
        index: &Value,
        value: Value,
        span: Span,
    ) -> Result<(), Error> {
        match (object, index) {
            (Value::Null, _) => {
                let message = "Cannot invoke method putAt() on null object";
                Err(thrown(
                    Exception::new("NullPointerException", message),
                    span,
                ))
            }
            (Value::Map(map), key) => {
                if map.borrow().kind == MapKind::Record {
                    return Err(rejected(span));
                }
                map.borrow_mut().entries.insert(Key(key.clone()), value);
                Ok(())
            }
            (Value::List(list), Value::Number(number)) if integral(number) => {
                let mut list = list.borrow_mut();
                if matches!(list.kind, ListKind::Tuple | ListKind::Range) {
                    return Err(rejected(span));
                }
                let length = list.items.len();
                let Some(position) = position(number.to_i64(), length) else {
                    return Err(out_of_bounds(
                        "ArrayIndexOutOfBoundsException",
                        number,
                        length,
                        span,
                    ));
                };
                if position >= length {
                    if list.kind == ListKind::Array {
                        return Err(out_of_bounds(
                            "ArrayIndexOutOfBoundsException",
                            number,
                            length,
                            span,
                        ));
                    }
                    list.items.resize(position + 1, Value::Null);
                }
                list.items[position] = value;
                Ok(())
            }
            _ => {
                let class = object.class_name();
                Err(unsupported(format!("setting an index of {class}"), span))
            }
        }
    }

    /// `object.name(arguments)`. Only `call` on closures and the core of exceptions so far.
    pub(super) fn call_method(
        &mut self,
        object: &Value,
        name: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        match (object, name) {
            (Value::Null, _) => {
                let message = format!("Cannot invoke method {name}() on null object");
                Err(thrown(
                    Exception::new("NullPointerException", message),
                    span,
                ))
            }
            (Value::Closure(closure), "call") => self.call_closure_value(closure, arguments, span),
            (Value::Namespace(namespace), _) => {
                let namespace = namespace.clone();
                self.namespace_call(&namespace, name, arguments, span)
            }
            (Value::Channel(_) | Value::ChannelOut(_), _) => {
                self.operator(object, name, arguments, span)
            }
            (Value::Exception(_), "getMessage") if arguments.is_empty() => {
                self.property(object, "message", span)
            }
            _ => {
                if let Some(value) = self.library_method(object, name, &arguments, span)? {
                    return Ok(value);
                }
                let class = object.class_name();
                Err(unsupported(format!("the method `{name}` of {class}"), span))
            }
        }
    }

    /// `value as Class` for the conversions of the core; `null` stays `null`.
    pub(super) fn cast(&mut self, value: Value, class: &str, span: Span) -> Result<Value, Error> {
        if matches!(value, Value::Null) || instance_of(&value, class) && class != "String" {
            return Ok(value);
        }
        if let Some(converted) = super::library::convert(&value, class) {
            return converted.at(span);
        }
        Ok(match (class, &value) {
            ("String", _) => Value::string(value.to_groovy_string()),
            ("Boolean", _) => Value::Boolean(value.truth()),
            ("Integer", Value::Number(number)) => Value::integer(number.to_i32()),
            ("Long", Value::Number(number)) => Value::Number(Number::Long(number.to_i64())),
            ("List", Value::Set(_) | Value::List(_)) => {
                Value::list(super::library::elements(&value))
            }
            ("List", Value::String(text) | Value::GString(text)) => {
                Value::list(text.chars().map(|c| Value::string(c.to_string())).collect())
            }
            ("Set", Value::List(_)) => Value::set(super::library::elements(&value)),
            ("Integer" | "Long", Value::String(text) | Value::GString(text)) => {
                let trimmed = text.trim();
                let parsed: Option<i64> = trimmed.parse().ok();
                match (class, parsed) {
                    ("Integer", Some(number)) if i32::try_from(number).is_ok() => {
                        Value::integer(number as i32)
                    }
                    ("Long", Some(number)) => Value::Number(Number::Long(number)),
                    _ => return Err(thrown(Exception::number_format(trimmed), span)),
                }
            }
            _ => {
                let from = value.class_name();
                return Err(unsupported(format!("casting {from} to {class}"), span));
            }
        })
    }

    /// `new Class(arguments)`, so far for exceptions only.
    pub(super) fn new_object(
        &mut self,
        class: &str,
        arguments: Vec<Value>,
        span: Span,
    ) -> Result<Value, Error> {
        match (exception::class_name(class), arguments.as_slice()) {
            (Some(class), []) => Ok(Value::Exception(Rc::new(Exception::new(class, "")))),
            (Some(class), [message]) => {
                let exception = Exception::new(class, message.to_string());
                Ok(Value::Exception(Rc::new(exception)))
            }
            _ => Err(unsupported(format!("`new {class}(...)`"), span)),
        }
    }
}

/// `value instanceof Class`, by the simple class name and its supertypes.
pub(super) fn instance_of(value: &Value, class: &str) -> bool {
    let own = value.class_name();
    let supertypes: &[&str] = match value {
        Value::Null => return false,
        Value::Boolean(_) => &["Comparable"],
        Value::Number(_) => &["Number", "Comparable"],
        Value::String(_) => &["CharSequence", "Comparable"],
        Value::GString(_) => &["GString", "CharSequence", "Comparable"],
        Value::List(list) if list.borrow().kind == ListKind::Array => &[],
        Value::List(list) if list.borrow().kind == ListKind::Range => {
            &["Range", "List", "Collection", "Iterable"]
        }
        Value::List(list) if list.borrow().kind == ListKind::Tuple => {
            &["ArrayList", "List", "Collection", "Iterable"]
        }
        Value::List(_) => &["List", "Collection", "Iterable"],
        Value::Set(_) => &["HashSet", "Set", "Collection", "Iterable"],
        Value::Map(map) if map.borrow().kind == MapKind::Record => &["LinkedHashMap", "Map"],
        Value::Map(_) => &["Map"],
        Value::MemoryUnit(_) | Value::Duration(_) => &["Comparable"],
        Value::Exception(exception) => return class == "Object" || exception.is_instance(class),
        Value::Path(_) => &["Path", "Comparable"],
        Value::Channel(_) | Value::ChannelOut(_) => &["Channel"],
        Value::Definition(_) | Value::Namespace(_) | Value::Operator(_) => &[],
        Value::MapEntry(_) | Value::Closure(_) | Value::Pattern(_) | Value::Matcher(_) => &[],
    };
    class == "Object" || class == own || supertypes.contains(&class)
}

/// The part of a list that an integer range selects, as Groovy's `IntRange.subListBorders`:
/// negative ends count from the end, and a descending range gives the elements reversed.
fn borders(indexes: &List, size: usize, span: Span) -> Result<(usize, usize, bool), Error> {
    let bounds = indexes
        .range
        .as_ref()
        .and_then(|range| match (&range.from, &range.to) {
            (Value::Number(Number::Integer(from)), Value::Number(Number::Integer(to))) => {
                Some((i64::from(*from), i64::from(*to), range.exclusive))
            }
            _ => None,
        });
    let Some((from, to, exclusive)) = bounds else {
        return Err(unsupported(
            "indexing with a range that is not of integers",
            span,
        ));
    };
    let size = size as i64;
    let from = if from < 0 { from + size } else { from };
    let to = if to < 0 { to + size } else { to };
    let (start, end, reverse) = if from > to {
        (if exclusive { to + 1 } else { to }, from + 1, true)
    } else {
        (from, if exclusive { to } else { to + 1 }, false)
    };
    // Java's `List.subList` checks, with its messages.
    let failure = if start < 0 {
        Some(("IndexOutOfBoundsException", format!("fromIndex = {start}")))
    } else if end > size {
        Some(("IndexOutOfBoundsException", format!("toIndex = {end}")))
    } else if start > end {
        Some((
            "IllegalArgumentException",
            format!("fromIndex({start}) > toIndex({end})"),
        ))
    } else {
        None
    };
    if let Some((class, message)) = failure {
        return Err(thrown(Exception::new(class, message), span));
    }
    Ok((start as usize, end as usize, reverse))
}

/// Whether a number is an index: `Integer`, `Long` or `BigInteger`.
fn integral(number: &Number) -> bool {
    matches!(
        number,
        Number::Integer(_) | Number::Long(_) | Number::BigInteger(_)
    )
}

/// A list position, counting a negative index from the end; `None` before the start.
fn position(index: i64, length: usize) -> Option<usize> {
    let position = if index < 0 {
        length as i64 + index
    } else {
        index
    };
    usize::try_from(position).ok()
}

fn out_of_bounds(class: &'static str, index: &Number, length: usize, span: Span) -> Error {
    let message = format!("Index {index} out of bounds for length {length}");
    thrown(Exception::new(class, message), span)
}

/// The `UnsupportedOperationException` of a tuple or record that rejects a change.
fn rejected(span: Span) -> Error {
    thrown(Exception::new("UnsupportedOperationException", ""), span)
}
