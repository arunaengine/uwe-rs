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
