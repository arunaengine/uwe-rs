//! Methods and attributes of the builtin types: `str`, `bytes`, `list`, `tuple`, `dict`,
//! `set`, numbers and `range`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use indexmap::IndexSet;

use crate::builtins::{sort_values, update_dict};
use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::types;
use crate::value::{Args, Dict, DictKind, Key, Method, MethodFunction, Value};

const STR_METHODS: &[&str] = &[
    "capitalize",
    "casefold",
    "center",
    "count",
    "encode",
    "endswith",
    "expandtabs",
    "find",
    "format",
    "format_map",
    "index",
    "isalnum",
    "isalpha",
    "isascii",
    "isdecimal",
    "isdigit",
    "isidentifier",
    "islower",
    "isnumeric",
    "isprintable",
    "isspace",
    "istitle",
    "isupper",
    "join",
    "ljust",
    "lower",
    "lstrip",
    "partition",
    "removeprefix",
    "removesuffix",
    "replace",
    "rfind",
    "rindex",
    "rjust",
    "rpartition",
    "rsplit",
    "rstrip",
    "split",
    "splitlines",
    "startswith",
    "strip",
    "swapcase",
    "title",
    "translate",
    "upper",
    "zfill",
    "__len__",
    "__contains__",
    "__getitem__",
    "__add__",
    "__mod__",
    "__eq__",
    "__hash__",
    "__str__",
    "__lt__",
    "__iter__",
    "maketrans",
];

const LIST_METHODS: &[&str] = &[
    "append",
    "extend",
    "insert",
    "remove",
    "pop",
    "clear",
    "index",
    "count",
    "sort",
    "reverse",
    "copy",
    "__len__",
    "__contains__",
    "__getitem__",
    "__iter__",
    "__add__",
    "__eq__",
];

const TUPLE_METHODS: &[&str] = &[
    "index",
    "count",
    "__len__",
    "__contains__",
    "__getitem__",
    "__iter__",
];

const DICT_METHODS: &[&str] = &[
    "keys",
    "values",
    "items",
    "get",
    "pop",
    "popitem",
    "setdefault",
    "update",
    "clear",
    "copy",
    "fromkeys",
    "__len__",
    "__contains__",
    "__getitem__",
    "__setitem__",
    "__iter__",
    "__eq__",
];

const COUNTER_METHODS: &[&str] = &["most_common", "elements", "subtract", "total"];

const SET_METHODS: &[&str] = &[
    "add",
    "remove",
    "discard",
    "pop",
    "clear",
    "update",
    "union",
    "intersection",
    "difference",
    "symmetric_difference",
    "issubset",
    "issuperset",
    "isdisjoint",
    "copy",
    "intersection_update",
    "difference_update",
    "symmetric_difference_update",
    "__len__",
    "__contains__",
    "__iter__",
];

const BYTES_METHODS: &[&str] = &[
    "decode",
    "startswith",
    "endswith",
    "strip",
    "rstrip",
    "lstrip",
    "split",
    "hex",
    "join",
    "count",
    "find",
    "replace",
    "lower",
    "upper",
];

const INT_METHODS: &[&str] = &[
    "bit_length",
    "conjugate",
    "is_integer",
    "__index__",
    "__int__",
    "__float__",
];

const FLOAT_METHODS: &[&str] = &[
    "is_integer",
    "conjugate",
    "as_integer_ratio",
    "__int__",
    "__float__",
];

/// Whether iunctus implements a method of a builtin value.
pub fn has_method(value: &Value, name: &str) -> bool {
    let table: &[&[&str]] = match value.plain() {
        Value::Str(_) => &[STR_METHODS],
        Value::List(_) => &[LIST_METHODS],
        Value::Tuple(_) => &[TUPLE_METHODS],
        Value::Dict(dict) => match dict.borrow().kind {
            DictKind::Counter => &[DICT_METHODS, COUNTER_METHODS],
            _ => &[DICT_METHODS],
        },
        Value::Set(_) => &[SET_METHODS],
        Value::FrozenSet(_) => &[SET_METHODS],
        Value::Bytes(_) => &[BYTES_METHODS],
        Value::Int(_) | Value::BigInt(_) | Value::Bool(_) => &[INT_METHODS],
        Value::Float(_) => &[FLOAT_METHODS],
        Value::Range(_) => &[&["index", "count"]],
        Value::Iterator(_) => &[&["__next__", "__iter__"]],
        _ => &[],
    };
    table.iter().any(|names| names.contains(&name))
}

/// Whether Python has an attribute that iunctus does not implement, so that using it is
/// unsupported rather than an `AttributeError`.
pub fn python_has(value: &Value, name: &str) -> bool {
    const MORE: &[&str] = &[
        "__doc__",
        "__dict__",
        "__format__",
        "__sizeof__",
        "__reduce__",
    ];
    match value.plain() {
        Value::Str(_) | Value::List(_) | Value::Dict(_) | Value::Set(_) | Value::Tuple(_) => {
            name.starts_with("__") || MORE.contains(&name)
        }
        _ => false,
    }
}

/// An attribute of a builtin value: a bound method or a data attribute.
pub fn attribute(
    _interpreter: &mut Interpreter,
    value: &Value,
    name: &str,
) -> Result<Option<Value>, Error> {
    let data = match (value.plain(), name) {
        (Value::Range(range), "start") => Some(Value::Int(range.start)),
        (Value::Range(range), "stop") => Some(Value::Int(range.stop)),
        (Value::Range(range), "step") => Some(Value::Int(range.step)),
        (Value::Slice(parts), "start") => Some(parts[0].clone()),
        (Value::Slice(parts), "stop") => Some(parts[1].clone()),
        (Value::Slice(parts), "step") => Some(parts[2].clone()),
        (Value::Int(_) | Value::BigInt(_) | Value::Bool(_), "real" | "numerator") => {
            Some(value.plain().clone())
        }
        (Value::Int(_) | Value::BigInt(_) | Value::Bool(_), "imag") => Some(Value::Int(0)),
        (Value::Int(_) | Value::BigInt(_) | Value::Bool(_), "denominator") => Some(Value::Int(1)),
        (Value::Float(_), "real") => Some(value.plain().clone()),
        (Value::Float(_), "imag") => Some(Value::Float(0.0)),
        (Value::Dict(dict), "default_factory") => match &dict.borrow().kind {
            DictKind::Default(factory) => Some(factory.clone()),
            _ => None,
        },
        _ => None,
    };
    if data.is_some() {
        return Ok(data);
    }
    if has_method(value, name) {
        return Ok(Some(Value::Method(Rc::new(Method {
            receiver: value.plain().clone(),
            function: MethodFunction::Builtin(name.into()),
        }))));
    }
    Ok(None)
}

/// Calls a method of a builtin value by name.
pub fn call(
    interpreter: &mut Interpreter,
    receiver: &Value,
    name: &str,
    args: Args,
) -> Result<Value, Error> {
    match receiver.plain() {
        Value::Str(text) => {
            let text = text.clone();
            string_method(interpreter, &text, name, args)
        }
        Value::List(_) => list_method(interpreter, receiver.plain(), name, args),
        Value::Tuple(items) => {
            let items = items.to_vec();
            sequence_method(interpreter, &items, name, args, "tuple")
        }
        Value::Dict(_) => dict_method(interpreter, receiver.plain(), name, args),
        Value::Set(_) | Value::FrozenSet(_) => {
            set_method(interpreter, receiver.plain(), name, args)
        }
        Value::Bytes(bytes) => {
            let bytes = bytes.clone();
            bytes_method(interpreter, &bytes, name, args)
        }
        Value::Int(_) | Value::BigInt(_) | Value::Bool(_) | Value::Float(_) => {
            number_method(interpreter, receiver.plain(), name, args)
        }
        Value::Range(range) => {
            let items: Vec<Value> = (0..range.len())
                .map(|index| Value::Int(range.get(index)))
                .collect();
            sequence_method(interpreter, &items, name, args, "range")
        }
        Value::Iterator(iterator) => match name {
            "__next__" => iterator
                .borrow_mut()
                .next_value()
                .ok_or_else(Error::stop_iteration),
            _ => Ok(receiver.plain().clone()),
        },
        other => Err(Error::attribute_error(format!(
            "'{}' object has no attribute '{name}'",
            types::type_name(other)
        ))),
    }
}

fn text_arg(args: &Args, index: usize, method: &str) -> Result<String, Error> {
    match args.positional.get(index).map(Value::plain) {
        Some(Value::Str(text)) => Ok(text.to_string()),
        Some(other) => Err(Error::type_error(format!(
            "{method}() argument must be str, not {}",
            types::type_name(other)
        ))),
        None => Err(Error::type_error(format!(
            "{method}() missing required argument"
        ))),
    }
}

fn optional_text(args: &Args, index: usize) -> Option<String> {
    match args.positional.get(index).map(Value::plain) {
        Some(Value::Str(text)) => Some(text.to_string()),
        _ => None,
    }
}

/// The char range `[start, end)` of optional start and end arguments.
fn char_range(args: &Args, first: usize, length: usize) -> (usize, usize) {
    let bound = |value: Option<&Value>, default: usize| -> usize {
        match value.map(Value::plain) {
            Some(Value::Int(index)) => {
                let index = if *index < 0 {
                    *index + length as i64
                } else {
                    *index
                };
                index.clamp(0, length as i64) as usize
            }
            _ => default,
        }
    };
    (
        bound(args.positional.get(first), 0),
        bound(args.positional.get(first + 1), length),
    )
}

fn chars_of(text: &str) -> Vec<char> {
    text.chars().collect()
}

fn find_in(haystack: &[char], needle: &[char], from_right: bool) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    let positions = 0..=haystack.len() - needle.len();
    let matches = |start: &usize| haystack[*start..*start + needle.len()] == *needle;
    if from_right {
        positions.rev().find(matches)
    } else {
        positions.clone().find(matches)
    }
}

fn split_whitespace(text: &str, limit: Option<usize>, from_right: bool) -> Vec<Value> {
    let limit = limit.unwrap_or(usize::MAX);
    if !from_right {
        let mut parts = Vec::new();
        let mut rest = text.trim_start();
        while !rest.is_empty() {
            if parts.len() == limit {
                parts.push(Value::str(rest));
                return parts;
            }
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            parts.push(Value::str(&rest[..end]));
            rest = rest[end..].trim_start();
        }
        parts
    } else {
        let mut parts = Vec::new();
        let mut rest = text.trim_end();
        while !rest.is_empty() {
            if parts.len() == limit {
                parts.push(Value::str(rest));
                break;
            }
            let start = rest.rfind(char::is_whitespace).map_or(0, |index| {
                index + rest[index..].chars().next().map_or(1, char::len_utf8)
            });
            parts.push(Value::str(&rest[start..]));
            rest = rest[..start].trim_end();
        }
        parts.reverse();
        parts
    }
}

fn strip_set(text: &str, chars: Option<String>, left: bool, right: bool) -> String {
    let matches = |c: char| match &chars {
        Some(set) => set.contains(c),
        None => c.is_whitespace(),
    };
    let mut result = text;
    if left {
        result = result.trim_start_matches(matches);
    }
    if right {
        result = result.trim_end_matches(matches);
    }
    result.to_owned()
}
