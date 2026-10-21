//! Methods and attributes of the builtin types: `str`, `bytes`, `list`, `tuple`, `dict`,
//! `set`, numbers and `range`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use indexmap::IndexSet;

use crate::builtins::{sort_values, update_dict};
use crate::error::Error;
use crate::hashing::register;
use crate::interpreter::Interpreter;
use crate::set_table::{Operand, derived, updated};
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
        Value::Iterator(iterator) => {
            let generator = (iterator.try_borrow()).map_or(true, |state| {
                matches!(&*state, crate::value::Iter::Generator(_))
            });
            match generator {
                true => &[&["__next__", "__iter__", "send", "throw", "close"]],
                false => &[&["__next__", "__iter__"]],
            }
        }
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
        Value::Iterator(_) => match name {
            "__next__" => interpreter
                .next_item(receiver)?
                .ok_or_else(Error::stop_iteration),
            "send" => {
                args.expect("send", 1, 1)?;
                let value = args.positional[0].clone();
                (interpreter.send_item(receiver, value)?).ok_or_else(Error::stop_iteration)
            }
            "throw" => {
                args.expect("throw", 1, 3)?;
                let error = thrown(interpreter, &args)?;
                (interpreter.throw_item(receiver, error)?).ok_or_else(Error::stop_iteration)
            }
            "close" => interpreter.close_item(receiver).map(|()| Value::None),
            _ => Ok(receiver.plain().clone()),
        },
        other => Err(Error::attribute_error(format!(
            "'{}' object has no attribute '{name}'",
            types::type_name(other)
        ))),
    }
}

/// The exception of `generator.throw(type[, value])` or `generator.throw(exception)`.
fn thrown(interpreter: &mut Interpreter, args: &Args) -> Result<Error, Error> {
    let first = &args.positional[0];
    let exception = match first.plain() {
        Value::Class(_) => {
            let value = args.positional.get(1).filter(|value| !value.is_none());
            let arguments = match value {
                Some(value) if crate::types::is_exception(value) => {
                    return Ok(Error::from_value(value.clone()));
                }
                Some(Value::Tuple(items)) => items.to_vec(),
                Some(value) => vec![value.clone()],
                None => Vec::new(),
            };
            interpreter.call(first, Args::new(arguments))?
        }
        _ => first.clone(),
    };
    if !crate::types::is_exception(&exception) {
        return Err(Error::type_error(
            "exceptions must be classes or instances deriving from BaseException, not ".to_owned()
                + &types::type_name(&exception),
        ));
    }
    Ok(Error::from_value(exception))
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

fn string_method(
    interpreter: &mut Interpreter,
    text: &Rc<str>,
    name: &str,
    mut args: Args,
) -> Result<Value, Error> {
    let text_ref: &str = text;
    Ok(match name {
        "lower" => Value::str(text_ref.to_lowercase()),
        "upper" => Value::str(text_ref.to_uppercase()),
        "casefold" => Value::str(text_ref.to_lowercase()),
        "swapcase" => Value::str(
            text_ref
                .chars()
                .map(|c| {
                    if c.is_uppercase() {
                        c.to_lowercase().collect::<String>()
                    } else {
                        c.to_uppercase().collect()
                    }
                })
                .collect::<String>(),
        ),
        "capitalize" => {
            let mut chars = text_ref.chars();
            Value::str(match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            })
        }
        "title" => {
            let mut result = String::new();
            let mut previous_letter = false;
            for c in text_ref.chars() {
                if c.is_alphabetic() {
                    if previous_letter {
                        result.extend(c.to_lowercase());
                    } else {
                        result.extend(c.to_uppercase());
                    }
                    previous_letter = true;
                } else {
                    result.push(c);
                    previous_letter = false;
                }
            }
            Value::str(result)
        }
        "strip" | "lstrip" | "rstrip" => {
            let chars = optional_text(&args, 0);
            Value::str(strip_set(
                text_ref,
                chars,
                name != "rstrip",
                name != "lstrip",
            ))
        }
        "split" | "rsplit" => {
            let separator = args.take(0, "sep").filter(|value| !value.is_none());
            let limit = args
                .take(1, "maxsplit")
                .and_then(|value| value.as_int())
                .filter(|limit| *limit >= 0)
                .map(|limit| limit as usize);
            let from_right = name == "rsplit";
            match separator {
                None => Value::list(split_whitespace(text_ref, limit, from_right)),
                Some(separator) => {
                    let separator = separator
                        .as_str()
                        .ok_or_else(|| Error::type_error("must be str or None"))?
                        .to_owned();
                    if separator.is_empty() {
                        return Err(Error::value_error("empty separator"));
                    }
                    let parts: Vec<Value> = match (limit, from_right) {
                        (None, _) => text_ref.split(separator.as_str()).map(Value::str).collect(),
                        (Some(limit), false) => text_ref
                            .splitn(limit + 1, separator.as_str())
                            .map(Value::str)
                            .collect(),
                        (Some(limit), true) => {
                            let mut parts: Vec<Value> = text_ref
                                .rsplitn(limit + 1, separator.as_str())
                                .map(Value::str)
                                .collect();
                            parts.reverse();
                            parts
                        }
                    };
                    Value::list(parts)
                }
            }
        }
        "splitlines" => {
            let keep = args
                .take(0, "keepends")
                .is_some_and(|value| matches!(value.plain(), Value::Bool(true)));
            let mut lines = Vec::new();
            let mut current = String::new();
            let mut chars = text_ref.chars().peekable();
            while let Some(c) = chars.next() {
                let is_break = matches!(
                    c,
                    '\n' | '\r'
                        | '\x0b'
                        | '\x0c'
                        | '\x1c'
                        | '\x1d'
                        | '\x1e'
                        | '\u{85}'
                        | '\u{2028}'
                        | '\u{2029}'
                );
                if is_break {
                    let mut ending = c.to_string();
                    if c == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                        ending.push('\n');
                    }
                    if keep {
                        current.push_str(&ending);
                    }
                    lines.push(Value::str(std::mem::take(&mut current)));
                } else {
                    current.push(c);
                }
            }
            if !current.is_empty() {
                lines.push(Value::str(current));
            }
            Value::list(lines)
        }
        "join" => {
            args.expect("join", 1, 1)?;
            let items = interpreter.iterate(&args.positional[0])?;
            let mut parts = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                match item.plain() {
                    Value::Str(part) => parts.push(part.to_string()),
                    Value::Native(object) if object.instance_of("str") => {
                        parts.push(interpreter.to_str(item)?)
                    }
                    other => {
                        return Err(Error::type_error(format!(
                            "sequence item {index}: expected str instance, {} found",
                            types::type_name(other)
                        )));
                    }
                }
            }
            Value::str(parts.join(text_ref))
        }
        "replace" => {
            args.expect("replace", 2, 3)?;
            let old = text_arg(&args, 0, "replace")?;
            let new = text_arg(&args, 1, "replace")?;
            match args
                .positional
                .get(2)
                .and_then(Value::as_int)
                .filter(|count| *count >= 0)
            {
                Some(count) => Value::str(text_ref.replacen(old.as_str(), &new, count as usize)),
                None => Value::str(text_ref.replace(old.as_str(), &new)),
            }
        }
        "startswith" | "endswith" => {
            args.expect(name, 1, 3)?;
            let chars = chars_of(text_ref);
            let (start, end) = char_range(&args, 1, chars.len());
            let part: String = if start <= end {
                chars[start..end].iter().collect()
            } else {
                String::new()
            };
            let candidates = match args.positional[0].plain() {
                Value::Tuple(items) => items.to_vec(),
                other => vec![other.clone()],
            };
            let mut found = false;
            for candidate in candidates {
                let candidate = candidate.as_str().ok_or_else(|| {
                    Error::type_error(format!(
                        "{name} first arg must be str or a tuple of str, not {}",
                        types::type_name(&candidate)
                    ))
                })?;
                if (name == "startswith" && part.starts_with(candidate))
                    || (name == "endswith" && part.ends_with(candidate))
                {
                    found = true;
                    break;
                }
            }
            Value::Bool(found)
        }
        "find" | "rfind" | "index" | "rindex" | "count" => {
            args.expect(name, 1, 3)?;
            let needle = chars_of(&text_arg(&args, 0, name)?);
            let chars = chars_of(text_ref);
            let (start, end) = char_range(&args, 1, chars.len());
            let window = if start <= end {
                &chars[start..end]
            } else {
                &chars[0..0]
            };
            if name == "count" {
                if needle.is_empty() {
                    return Ok(Value::Int(window.len() as i64 + 1));
                }
                let mut count = 0;
                let mut position = 0;
                while position + needle.len() <= window.len() {
                    if window[position..position + needle.len()] == needle[..] {
                        count += 1;
                        position += needle.len();
                    } else {
                        position += 1;
                    }
                }
                return Ok(Value::Int(count));
            }
            let found = find_in(window, &needle, name.starts_with('r')).map(|index| index + start);
            match (found, name) {
                (Some(index), _) => Value::Int(index as i64),
                (None, "find" | "rfind") => Value::Int(-1),
                (None, _) => return Err(Error::value_error("substring not found")),
            }
        }
        "partition" | "rpartition" => {
            let separator = text_arg(&args, 0, name)?;
            if separator.is_empty() {
                return Err(Error::value_error("empty separator"));
            }
            let found = if name == "partition" {
                text_ref.find(&separator)
            } else {
                text_ref.rfind(&separator)
            };
            let parts = match found {
                Some(index) => vec![
                    Value::str(&text_ref[..index]),
                    Value::str(separator.as_str()),
                    Value::str(&text_ref[index + separator.len()..]),
                ],
                None if name == "partition" => {
                    vec![Value::Str(text.clone()), Value::str(""), Value::str("")]
                }
                None => vec![Value::str(""), Value::str(""), Value::Str(text.clone())],
            };
            Value::tuple(parts)
        }
        "removeprefix" => {
            let prefix = text_arg(&args, 0, name)?;
            Value::str(text_ref.strip_prefix(prefix.as_str()).unwrap_or(text_ref))
        }
        "removesuffix" => {
            let suffix = text_arg(&args, 0, name)?;
            Value::str(text_ref.strip_suffix(suffix.as_str()).unwrap_or(text_ref))
        }
        "center" | "ljust" | "rjust" => {
            args.expect(name, 1, 2)?;
            let width = args.positional[0].as_int().unwrap_or(0).max(0) as usize;
            let fill = optional_text(&args, 1)
                .and_then(|fill| fill.chars().next())
                .unwrap_or(' ');
            let length = text_ref.chars().count();
            if length >= width {
                return Ok(Value::Str(text.clone()));
            }
            let missing = width - length;
            let pad = |count: usize| std::iter::repeat_n(fill, count).collect::<String>();
            Value::str(match name {
                "ljust" => format!("{text_ref}{}", pad(missing)),
                "rjust" => format!("{}{text_ref}", pad(missing)),
                _ => {
                    let left = missing / 2 + (missing & width & 1);
                    format!("{}{text_ref}{}", pad(left), pad(missing - left))
                }
            })
        }
        "zfill" => {
            let width = args
                .positional
                .first()
                .and_then(Value::as_int)
                .unwrap_or(0)
                .max(0) as usize;
            let length = text_ref.chars().count();
            if length >= width {
                return Ok(Value::Str(text.clone()));
            }
            let (sign, digits) = match text_ref.chars().next() {
                Some(c @ ('+' | '-')) => (c.to_string(), &text_ref[1..]),
                _ => (String::new(), text_ref),
            };
            Value::str(format!("{sign}{}{digits}", "0".repeat(width - length)))
        }
        "expandtabs" => {
            let size = args
                .positional
                .first()
                .and_then(Value::as_int)
                .unwrap_or(8)
                .max(0) as usize;
            let mut result = String::new();
            let mut column = 0;
            for c in text_ref.chars() {
                match c {
                    '\t' => {
                        let spaces = if size == 0 { 0 } else { size - column % size };
                        result.push_str(&" ".repeat(spaces));
                        column += spaces;
                    }
                    '\n' | '\r' => {
                        result.push(c);
                        column = 0;
                    }
                    other => {
                        result.push(other);
                        column += 1;
                    }
                }
            }
            Value::str(result)
        }
        "isdigit" | "isdecimal" | "isnumeric" => {
            Value::Bool(!text_ref.is_empty() && text_ref.chars().all(|c| c.is_numeric()))
        }
        "isalpha" => Value::Bool(!text_ref.is_empty() && text_ref.chars().all(char::is_alphabetic)),
        "isalnum" => {
            Value::Bool(!text_ref.is_empty() && text_ref.chars().all(char::is_alphanumeric))
        }
        "isspace" => Value::Bool(!text_ref.is_empty() && text_ref.chars().all(char::is_whitespace)),
        "isascii" => Value::Bool(text_ref.is_ascii()),
        "isprintable" => Value::Bool(text_ref.chars().all(|c| !c.is_control())),
        "islower" => Value::Bool(
            text_ref.chars().any(char::is_lowercase) && !text_ref.chars().any(char::is_uppercase),
        ),
        "isupper" => Value::Bool(
            text_ref.chars().any(char::is_uppercase) && !text_ref.chars().any(char::is_lowercase),
        ),
        "istitle" => {
            let title = string_method(interpreter, text, "title", Args::default())?;
            Value::Bool(
                title.as_str() == Some(text_ref) && text_ref.chars().any(char::is_alphabetic),
            )
        }
        "isidentifier" => {
            let mut chars = text_ref.chars();
            Value::Bool(
                chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
                    && chars.all(|c| c.is_alphanumeric() || c == '_'),
            )
        }
        "encode" => Value::Bytes(text_ref.as_bytes().into()),
        "format" => Value::str(crate::format::format_method(interpreter, text_ref, &args)?),
        "format_map" => {
            args.expect("format_map", 1, 1)?;
            let mapping = args.positional[0].clone();
            let mut resolve =
                |interpreter: &mut Interpreter, field: &str| -> Result<Value, Error> {
                    interpreter.get_item(&mapping, &Value::str(field))
                };
            let mut plain = |interpreter: &mut Interpreter, value: &Value, spec: &str| {
                crate::format::format_value(interpreter, value, spec)
            };
            Value::str(crate::format::vformat(
                interpreter,
                text_ref,
                &mut resolve,
                &mut plain,
            )?)
        }
        "translate" => {
            args.expect("translate", 1, 1)?;
            let table = args.positional[0].clone();
            let mut result = String::new();
            for c in text_ref.chars() {
                match interpreter.get_item(&table, &Value::Int(c as i64)) {
                    Ok(mapped) => match mapped.plain() {
                        Value::None => {}
                        Value::Str(replacement) => result.push_str(replacement),
                        Value::Int(code) => result.extend(char::from_u32(*code as u32)),
                        _ => result.push(c),
                    },
                    Err(error) if error.is("LookupError") => result.push(c),
                    Err(error) => return Err(error),
                }
            }
            Value::str(result)
        }
        "maketrans" => return maketrans(interpreter, args),
        "__len__" => Value::Int(text_ref.chars().count() as i64),
        "__contains__" => {
            Value::Bool(interpreter.contains(&Value::Str(text.clone()), &args.positional[0])?)
        }
        "__getitem__" => interpreter.get_item(&Value::Str(text.clone()), &args.positional[0])?,
        "__add__" => interpreter.binary(
            crate::interpreter::Operator::Add,
            &Value::Str(text.clone()),
            &args.positional[0],
        )?,
        "__mod__" => interpreter.binary(
            crate::interpreter::Operator::Mod,
            &Value::Str(text.clone()),
            &args.positional[0],
        )?,
        "__lt__" => interpreter.binary(
            crate::interpreter::Operator::Lt,
            &Value::Str(text.clone()),
            &args.positional[0],
        )?,
        "__eq__" => {
            Value::Bool(interpreter.equals(&Value::Str(text.clone()), &args.positional[0])?)
        }
        "__hash__" => {
            let key = Value::Str(text.clone());
            return interpreter.call(
                &interpreter.builtins.get("hash").unwrap_or(Value::None),
                Args::new(vec![key]),
            );
        }
        "__str__" => Value::Str(text.clone()),
        "__iter__" => interpreter.make_iterator(&Value::Str(text.clone()))?,
        _ => return Err(Error::unsupported(format!("str.{name}"))),
    })
}

/// `str.maketrans(x[, y[, z]])`.
pub fn maketrans(interpreter: &mut Interpreter, args: Args) -> Result<Value, Error> {
    let mut table = Dict::default();
    match args.positional.len() {
        1 => {
            for (key, value) in interpreter.mapping_items(&args.positional[0])? {
                let key = match key.plain() {
                    Value::Str(text) => Value::Int(text.chars().next().map_or(0, |c| c as i64)),
                    other => other.clone(),
                };
                table.entries.insert(Key::new(key)?, value);
            }
        }
        2 | 3 => {
            let from = text_arg(&args, 0, "maketrans")?;
            let to = text_arg(&args, 1, "maketrans")?;
            if from.chars().count() != to.chars().count() {
                return Err(Error::value_error(
                    "the first two maketrans arguments must have equal length",
                ));
            }
            for (a, b) in from.chars().zip(to.chars()) {
                table
                    .entries
                    .insert(Key(Value::Int(a as i64)), Value::str(b.to_string()));
            }
            if let Some(delete) = optional_text(&args, 2) {
                for c in delete.chars() {
                    table.entries.insert(Key(Value::Int(c as i64)), Value::None);
                }
            }
        }
        _ => return Err(Error::type_error("maketrans() takes 1 to 3 arguments")),
    }
    Ok(Value::dict(table))
}

fn sequence_method(
    interpreter: &mut Interpreter,
    items: &[Value],
    name: &str,
    args: Args,
    kind: &str,
) -> Result<Value, Error> {
    match name {
        "index" => {
            args.expect("index", 1, 3)?;
            let (start, end) = char_range(&args, 1, items.len());
            for (index, item) in items.iter().enumerate().take(end).skip(start) {
                if interpreter.equals(item, &args.positional[0])? {
                    return Ok(Value::Int(index as i64));
                }
            }
            Err(Error::value_error(format!(
                "{kind}.index(x): x not in {kind}"
            )))
        }
        "count" => {
            args.expect("count", 1, 1)?;
            let mut count = 0;
            for item in items {
                if interpreter.equals(item, &args.positional[0])? {
                    count += 1;
                }
            }
            Ok(Value::Int(count))
        }
        "__len__" => Ok(Value::Int(items.len() as i64)),
        "__contains__" => {
            let container = Value::tuple(items.to_vec());
            Ok(Value::Bool(
                interpreter.contains(&container, &args.positional[0])?,
            ))
        }
        "__getitem__" => interpreter.get_item(&Value::tuple(items.to_vec()), &args.positional[0]),
        "__iter__" => Ok(Value::iterator(items.to_vec())),
        _ => Err(Error::unsupported(format!("{kind}.{name}"))),
    }
}

fn list_method(
    interpreter: &mut Interpreter,
    list: &Value,
    name: &str,
    mut args: Args,
) -> Result<Value, Error> {
    let Value::List(items) = list else {
        unreachable!("a list");
    };
    match name {
        "append" => {
            args.expect("append", 1, 1)?;
            items.borrow_mut().push(args.positional.remove(0));
            Ok(Value::None)
        }
        "extend" => {
            args.expect("extend", 1, 1)?;
            let extra = interpreter.iterate(&args.positional[0])?;
            items.borrow_mut().extend(extra);
            Ok(Value::None)
        }
        "insert" => {
            args.expect("insert", 2, 2)?;
            let mut list = items.borrow_mut();
            let length = list.len() as i64;
            let index = args.positional[0].as_int().unwrap_or(0);
            let index = if index < 0 {
                (index + length).max(0)
            } else {
                index.min(length)
            };
            list.insert(index as usize, args.positional[1].clone());
            Ok(Value::None)
        }
        "remove" => {
            args.expect("remove", 1, 1)?;
            let snapshot = items.borrow().clone();
            for (index, item) in snapshot.iter().enumerate() {
                if interpreter.equals(item, &args.positional[0])? {
                    items.borrow_mut().remove(index);
                    return Ok(Value::None);
                }
            }
            Err(Error::value_error("list.remove(x): x not in list"))
        }
        "pop" => {
            let mut list = items.borrow_mut();
            if list.is_empty() {
                return Err(Error::index_error("pop from empty list"));
            }
            let length = list.len() as i64;
            let index = args
                .positional
                .first()
                .and_then(Value::as_int)
                .unwrap_or(-1);
            let index = if index < 0 { index + length } else { index };
            if index < 0 || index >= length {
                return Err(Error::index_error("pop index out of range"));
            }
            Ok(list.remove(index as usize))
        }
        "clear" => {
            items.borrow_mut().clear();
            Ok(Value::None)
        }
        "copy" => Ok(Value::list(items.borrow().clone())),
        "reverse" => {
            items.borrow_mut().reverse();
            Ok(Value::None)
        }
        "sort" => {
            let key = args.take_keyword("key").filter(|key| !key.is_none());
            let reverse = match args.take_keyword("reverse") {
                Some(value) => interpreter.truthy(&value)?,
                None => false,
            };
            let snapshot = items.borrow().clone();
            let sorted = sort_values(interpreter, snapshot, key.as_ref(), reverse)?;
            *items.borrow_mut() = sorted;
            Ok(Value::None)
        }
        "__iter__" => Ok(Value::iterator(items.borrow().clone())),
        "__add__" => {
            interpreter.binary(crate::interpreter::Operator::Add, list, &args.positional[0])
        }
        "__eq__" => Ok(Value::Bool(interpreter.equals(list, &args.positional[0])?)),
        _ => {
            let snapshot = items.borrow().clone();
            sequence_method(interpreter, &snapshot, name, args, "list")
        }
    }
}

fn dict_method(
    interpreter: &mut Interpreter,
    dict_value: &Value,
    name: &str,
    args: Args,
) -> Result<Value, Error> {
    let Value::Dict(dict) = dict_value else {
        unreachable!("a dict");
    };
    match name {
        // Views that read the dict as it is when they are used.
        "keys" | "values" | "items" => {
            let part = match name {
                "keys" => crate::views::Part::Keys,
                "values" => crate::views::Part::Values,
                _ => crate::views::Part::Items,
            };
            Ok(Value::native(crate::views::View {
                dict: dict.clone(),
                part,
            }))
        }
        "get" => {
            args.expect("get", 1, 2)?;
            let key = interpreter.dict_key(dict, &args.positional[0])?;
            let found = dict.borrow().entries.get(&key).cloned();
            Ok(found.unwrap_or_else(|| args.positional.get(1).cloned().unwrap_or(Value::None)))
        }
        "pop" => {
            args.expect("pop", 1, 2)?;
            let key = interpreter.dict_key(dict, &args.positional[0])?;
            let removed = dict.borrow_mut().entries.shift_remove(&key);
            match (removed, args.positional.get(1)) {
                (Some(value), _) => Ok(value),
                (None, Some(default)) => Ok(default.clone()),
                (None, None) => Err(Error::key_error(args.positional[0].clone())),
            }
        }
        "popitem" => {
            let popped = dict.borrow_mut().entries.pop();
            match popped {
                Some((key, value)) => Ok(Value::tuple(vec![key.0, value])),
                None => Err(Error::key_error(Value::str(
                    "popitem(): dictionary is empty",
                ))),
            }
        }
        "setdefault" => {
            args.expect("setdefault", 1, 2)?;
            let key = interpreter.dict_key(dict, &args.positional[0])?;
            let default = args.positional.get(1).cloned().unwrap_or(Value::None);
            let mut dict = dict.borrow_mut();
            Ok(dict.entries.entry(key).or_insert(default).clone())
        }
        "update" => {
            let is_counter = matches!(dict.borrow().kind, DictKind::Counter);
            if let Some(source) = args.positional.first() {
                if is_counter {
                    return counter_update(interpreter, dict_value, source, 1);
                }
                let mut target = std::mem::take(&mut *dict.borrow_mut());
                let result = update_dict(interpreter, &mut target, source);
                *dict.borrow_mut() = target;
                result?;
            }
            for (key, value) in args.keywords {
                dict.borrow_mut()
                    .entries
                    .insert(Key(Value::Str(key)), value);
            }
            Ok(Value::None)
        }
        "clear" => {
            dict.borrow_mut().entries.clear();
            Ok(Value::None)
        }
        "copy" => Ok(Value::dict(dict.borrow().clone())),
        "fromkeys" => {
            let class = types::builtin_class(types::TypeTag::Dict);
            let fromkeys = crate::builtins::class_attribute(interpreter, &class, "fromkeys")?
                .unwrap_or(Value::None);
            interpreter.call(&fromkeys, args)
        }
        "most_common" => {
            let entries: Vec<(Value, Value)> = dict
                .borrow()
                .entries
                .iter()
                .map(|(key, value)| (key.0.clone(), value.clone()))
                .collect();
            let pairs: Vec<Value> = entries
                .into_iter()
                .map(|(key, value)| Value::tuple(vec![key, value]))
                .collect();
            let count_key = Value::builtin("count", |interpreter, args| {
                let count = interpreter.get_item(&args.positional[0], &Value::Int(1))?;
                interpreter.unary("-", &count)
            });
            let mut sorted = sort_values(interpreter, pairs, Some(&count_key), false)?;
            if let Some(limit) = args.positional.first().and_then(Value::as_int) {
                sorted.truncate(limit.max(0) as usize);
            }
            Ok(Value::list(sorted))
        }
        "elements" => {
            let mut elements = Vec::new();
            for (key, count) in dict.borrow().entries.iter() {
                for _ in 0..count.as_int().unwrap_or(0).max(0) {
                    elements.push(key.0.clone());
                }
            }
            Ok(Value::iterator(elements))
        }
        "subtract" => {
            let source = args.positional.first().cloned().unwrap_or(Value::None);
            counter_update(interpreter, dict_value, &source, -1)
        }
        "total" => {
            let mut total = Value::Int(0);
            let values: Vec<Value> = dict.borrow().entries.values().cloned().collect();
            for value in values {
                total = interpreter.binary(crate::interpreter::Operator::Add, &total, &value)?;
            }
            Ok(total)
        }
        "__len__" => Ok(Value::Int(dict.borrow().entries.len() as i64)),
        "__contains__" => Ok(Value::Bool(
            interpreter.contains(dict_value, &args.positional[0])?,
        )),
        "__getitem__" => interpreter.get_item(dict_value, &args.positional[0]),
        "__setitem__" => {
            args.expect("__setitem__", 2, 2)?;
            interpreter.set_item(dict_value, &args.positional[0], args.positional[1].clone())?;
            Ok(Value::None)
        }
        "__iter__" => interpreter.make_iterator(dict_value),
        "__eq__" => Ok(Value::Bool(
            interpreter.equals(dict_value, &args.positional[0])?,
        )),
        _ => Err(Error::unsupported(format!("dict.{name}"))),
    }
}

fn counter_update(
    interpreter: &mut Interpreter,
    counter: &Value,
    source: &Value,
    sign: i64,
) -> Result<Value, Error> {
    let Value::Dict(dict) = counter else {
        return Ok(Value::None);
    };
    let pairs: Vec<(Value, Value)> = match source.plain() {
        Value::Dict(_) => interpreter.mapping_items(source)?,
        Value::None => Vec::new(),
        other => interpreter
            .iterate(other)?
            .into_iter()
            .map(|item| (item, Value::Int(1)))
            .collect(),
    };
    for (key, count) in pairs {
        let key = interpreter.key(&key)?;
        let current = dict
            .borrow()
            .entries
            .get(&key)
            .cloned()
            .unwrap_or(Value::Int(0));
        let change =
            interpreter.binary(crate::interpreter::Operator::Mul, &count, &Value::Int(sign))?;
        let updated = interpreter.binary(crate::interpreter::Operator::Add, &current, &change)?;
        dict.borrow_mut().entries.insert(key, updated);
    }
    Ok(Value::None)
}

fn set_members(interpreter: &mut Interpreter, value: &Value) -> Result<IndexSet<Key>, Error> {
    match value.plain() {
        Value::Set(set) => Ok(set.borrow().clone()),
        Value::FrozenSet(set) => Ok((**set).clone()),
        other => {
            let mut members = IndexSet::new();
            for item in interpreter.iterate(other)? {
                let key = interpreter.key(&item)?;
                interpreter.add_member(&mut members, key)?;
            }
            Ok(members)
        }
    }
}

fn set_method(
    interpreter: &mut Interpreter,
    set_value: &Value,
    name: &str,
    args: Args,
) -> Result<Value, Error> {
    if let Value::Set(set) = set_value
        && let Some(result) = changed_in_place(interpreter, set, name, &args)?
    {
        return Ok(result);
    }
    if name == "__len__" {
        return Ok(Value::Int(set_length(set_value)));
    }
    let members = set_members(interpreter, set_value)?;
    let frozen = matches!(set_value, Value::FrozenSet(_));
    let make = |members: IndexSet<Key>| {
        if frozen {
            Value::FrozenSet(Rc::new(members))
        } else {
            Value::set(members)
        }
    };
    let mutable = || match set_value {
        Value::Set(set) => Ok(set.clone()),
        _ => Err(Error::attribute_error(format!(
            "'frozenset' object has no attribute '{name}'"
        ))),
    };
    let takes_iterables = matches!(
        name,
        "update"
            | "intersection_update"
            | "difference_update"
            | "symmetric_difference_update"
            | "union"
            | "intersection"
            | "difference"
            | "symmetric_difference"
            | "issubset"
            | "issuperset"
            | "isdisjoint"
    );
    let others = if takes_iterables {
        args.positional
            .iter()
            .map(|other| set_members(interpreter, other))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    // The operands as CPython's set code walks them, which decides the order of the result.
    let operands = || -> Option<(Operand, Vec<Operand>)> {
        let this = Operand::of(set_value, &members)?;
        let others = (args.positional.iter().zip(&others))
            .map(|(value, members)| Operand::of(value, members))
            .collect::<Option<Vec<_>>>()?;
        Some((this, others))
    };
    match name {
        "update" | "intersection_update" | "difference_update" | "symmetric_difference_update" => {
            let set = mutable()?;
            let table = (operands()).and_then(|(this, others)| updated(&this, name, &others));
            let mut result = members;
            for other in others {
                let operation = name.trim_end_matches("_update").replace("update", "union");
                result = interpreter.combine(&operation, &result, &other)?;
            }
            *set.borrow_mut() = result;
            if let Some(table) = table {
                register(set_value, table);
            }
            Ok(Value::None)
        }
        "union" | "intersection" | "difference" | "symmetric_difference" => {
            let table = (operands()).and_then(|(this, others)| derived(name, &this, &others));
            let mut result = members;
            for other in others {
                result = interpreter.combine(name, &result, &other)?;
            }
            let value = make(result);
            if let Some(table) = table {
                register(&value, table);
            }
            Ok(value)
        }
        "issubset" => Ok(Value::Bool(
            others.first().is_some_and(|other| members.is_subset(other)),
        )),
        "issuperset" => Ok(Value::Bool(
            others
                .first()
                .is_some_and(|other| members.is_superset(other)),
        )),
        "isdisjoint" => Ok(Value::Bool(
            others
                .first()
                .is_some_and(|other| members.is_disjoint(other)),
        )),
        "copy" => {
            let table = (operands()).and_then(|(this, _)| derived("copy", &this, &[]));
            let value = make(members);
            if let Some(table) = table {
                register(&value, table);
            }
            Ok(value)
        }
        "__contains__" => Ok(Value::Bool(
            interpreter.contains(set_value, &args.positional[0])?,
        )),
        "__iter__" => Ok(Value::iterator(match set_value {
            Value::Set(set) => crate::hashing::set_order(set),
            Value::FrozenSet(set) => crate::hashing::frozen_order(set),
            _ => Vec::new(),
        })),
        "add" | "remove" | "discard" | "pop" | "clear" => mutable().map(|_| Value::None),
        _ => Err(Error::unsupported(format!("set.{name}"))),
    }
}

fn set_length(set: &Value) -> i64 {
    match set {
        Value::Set(set) => set.borrow().len() as i64,
        Value::FrozenSet(set) => set.len() as i64,
        _ => 0,
    }
}

/// The methods of a set that change it where it is, without a copy of its members, so that
/// a loop of them stays fast: `None` for the others.
fn changed_in_place(
    interpreter: &mut Interpreter,
    set: &Rc<std::cell::RefCell<IndexSet<Key>>>,
    name: &str,
    args: &Args,
) -> Result<Option<Value>, Error> {
    match name {
        "add" => {
            args.expect("add", 1, 1)?;
            let key = interpreter.member_key(set, &args.positional[0])?;
            if !set.borrow().contains(&key) {
                crate::hashing::change(set, Some(&key), true);
                set.borrow_mut().insert(key);
            }
        }
        "remove" | "discard" => {
            args.expect(name, 1, 1)?;
            let key = interpreter.member_key(set, &args.positional[0])?;
            if !set.borrow().contains(&key) {
                return match name {
                    "remove" => Err(Error::key_error(args.positional[0].clone())),
                    _ => Ok(Some(Value::None)),
                };
            }
            crate::hashing::change(set, Some(&key), false);
            // The table of the set, or else its insertion order, decides its order.
            set.borrow_mut().swap_remove(&key);
        }
        "pop" => {
            let Some(key) = crate::hashing::pop(set) else {
                return Err(Error::key_error(Value::str("pop from an empty set")));
            };
            set.borrow_mut().swap_remove(&key);
            return Ok(Some(key.0));
        }
        "clear" => {
            crate::hashing::change(set, None, false);
            set.borrow_mut().clear();
        }
        "__contains__" => {
            args.expect(name, 1, 1)?;
            let key = interpreter.member_key(set, &args.positional[0])?;
            return Ok(Some(Value::Bool(set.borrow().contains(&key))));
        }
        "update" | "difference_update" => {
            let mut others = Vec::new();
            for other in &args.positional {
                others.push((other.clone(), set_members(interpreter, other)?));
            }
            let operands = (others.iter())
                .map(|(value, members)| Operand::of(value, members))
                .collect::<Option<Vec<_>>>();
            crate::hashing::apply(set, name, operands.as_deref());
            for (_, other) in &others {
                for key in other {
                    // A member of another kind that is equal by `__eq__` counts as this one.
                    let found = interpreter.member_key(set, &key.0)?;
                    let mut members = set.borrow_mut();
                    match (name, members.contains(&found)) {
                        ("update", false) => members.insert(found),
                        ("update", true) => false,
                        _ => members.swap_remove(&found),
                    };
                }
            }
        }
        _ => return Ok(None),
    }
    Ok(Some(Value::None))
}

fn bytes_method(
    interpreter: &mut Interpreter,
    bytes: &Rc<[u8]>,
    name: &str,
    args: Args,
) -> Result<Value, Error> {
    let as_bytes = |value: &Value| -> Result<Vec<u8>, Error> {
        match value.plain() {
            Value::Bytes(bytes) => Ok(bytes.to_vec()),
            other => Err(Error::type_error(format!(
                "a bytes-like object is required, not '{}'",
                types::type_name(other)
            ))),
        }
    };
    Ok(match name {
        "decode" => Value::str(String::from_utf8_lossy(bytes).into_owned()),
        "hex" => Value::str(
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        ),
        "startswith" => Value::Bool(bytes.starts_with(&as_bytes(&args.positional[0])?)),
        "endswith" => Value::Bool(bytes.ends_with(&as_bytes(&args.positional[0])?)),
        "strip" | "rstrip" | "lstrip" | "split" | "lower" | "upper" | "replace" | "count"
        | "find" | "join" => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            let args = Args {
                positional: args
                    .positional
                    .iter()
                    .map(|value| match value.plain() {
                        Value::Bytes(bytes) => {
                            Value::str(String::from_utf8_lossy(bytes).into_owned())
                        }
                        other => other.clone(),
                    })
                    .collect(),
                keywords: args.keywords,
            };
            let result = string_method(interpreter, &Rc::from(text.as_str()), name, args)?;
            to_bytes(result)
        }
        _ => return Err(Error::unsupported(format!("bytes.{name}"))),
    })
}

fn to_bytes(value: Value) -> Value {
    match value {
        Value::Str(text) => Value::Bytes(text.as_bytes().into()),
        Value::List(items) => Value::list(items.borrow().iter().cloned().map(to_bytes).collect()),
        other => other,
    }
}

fn number_method(
    interpreter: &mut Interpreter,
    number: &Value,
    name: &str,
    args: Args,
) -> Result<Value, Error> {
    let _ = args;
    Ok(match (number, name) {
        (Value::Float(value), "is_integer") => {
            Value::Bool(value.fract() == 0.0 && value.is_finite())
        }
        (_, "is_integer") => Value::Bool(true),
        (Value::Float(value), "as_integer_ratio") => {
            let (mut numerator, mut denominator) = (*value, 1i64);
            while numerator.fract() != 0.0 && denominator < (1 << 52) {
                numerator *= 2.0;
                denominator *= 2;
            }
            Value::tuple(vec![
                crate::number::float_to_int(numerator)?,
                Value::Int(denominator),
            ])
        }
        (_, "conjugate") => number.clone(),
        (_, "bit_length") => {
            let big = crate::number::to_bigint(number).unwrap_or_default();
            Value::Int(big.bits() as i64)
        }
        (_, "__int__" | "__index__") => crate::builtins::to_int(interpreter, number)?,
        (_, "__float__") => Value::Float(crate::number::to_f64(number).unwrap_or(0.0)),
        _ => {
            return Err(Error::unsupported(format!(
                "{}.{name}",
                types::type_name(number)
            )));
        }
    })
}
