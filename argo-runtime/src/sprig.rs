//! Evaluates the Sprig functions available in Argo expressions.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use crate::expr::{ExprError, equal};
use crate::value::Value;

type Result<T> = std::result::Result<T, ExprError>;

/// Calls the Sprig function `name`.
pub fn call(name: &str, arguments: Vec<Value>) -> Result<Value> {
    let arity = |count: usize| -> Result<()> {
        if arguments.len() < count {
            Err(ExprError::new(format!(
                "sprig.{name} needs {count} arguments, got {}",
                arguments.len()
            )))
        } else {
            Ok(())
        }
    };
    let text = |index: usize| -> Result<String> {
        match arguments.get(index) {
            Some(Value::String(text)) => Ok(text.clone()),
            Some(other) => Ok(other.to_go_string()),
            None => Err(ExprError::new(format!(
                "sprig.{name}: missing argument {}",
                index + 1
            ))),
        }
    };
    let first = arguments.first().cloned().unwrap_or(Value::Nil);
    Ok(match name {
        // Paths
        "base" | "osBase" => Value::String(path_base(&text(0)?)),
        "dir" | "osDir" => Value::String(path_dir(&text(0)?)),
        "clean" | "osClean" => Value::String(path_clean(&text(0)?)),
        "ext" | "osExt" => Value::String(path_ext(&text(0)?)),
        "isAbs" | "osIsAbs" => Value::Bool(text(0)?.starts_with('/')),
        // Strings
        "trim" => Value::String(text(0)?.trim().to_owned()),
        "trimAll" => {
            let cutset = text(0)?;
            Value::String(text(1)?.trim_matches(|c| cutset.contains(c)).to_owned())
        }
        "trimSuffix" => {
            let (suffix, subject) = (text(0)?, text(1)?);
            Value::String(subject.strip_suffix(&suffix).unwrap_or(&subject).to_owned())
        }
        "trimPrefix" => {
            let (prefix, subject) = (text(0)?, text(1)?);
            Value::String(subject.strip_prefix(&prefix).unwrap_or(&subject).to_owned())
        }
        "upper" => Value::String(text(0)?.to_uppercase()),
        "lower" => Value::String(text(0)?.to_lowercase()),
        "title" => Value::String(title(&text(0)?)),
        "repeat" => {
            arity(2)?;
            let count = usize::try_from(to_int64(&arguments[0])).unwrap_or(0);
            let subject = text(1)?;
            let length = subject
                .len()
                .checked_mul(count)
                .ok_or_else(|| ExprError::new("sprig.repeat result length overflows"))?;
            let mut repeated = String::new();
            repeated
                .try_reserve_exact(length)
                .map_err(|_| ExprError::new("sprig.repeat result cannot be allocated"))?;
            if !subject.is_empty() {
                for _ in 0..count {
                    repeated.push_str(&subject);
                }
            }
            Value::String(repeated)
        }
        "substr" => {
            arity(3)?;
            let characters: Vec<char> = text(2)?.chars().collect();
            let length = characters.len() as i64;
            let start = to_int64(&arguments[0]);
            let end = to_int64(&arguments[1]);
            let start = if start < 0 { 0 } else { start.min(length) };
            let end = if end < 0 || end > length { length } else { end };
            Value::String(if start > end {
                String::new()
            } else {
                characters[start as usize..end as usize].iter().collect()
            })
        }
        "trunc" => {
            arity(2)?;
            let characters: Vec<char> = text(1)?.chars().collect();
            let count = to_int64(&arguments[0]);
            let length = characters.len() as i64;
            Value::String(if count >= 0 {
                characters[..count.min(length) as usize].iter().collect()
            } else {
                characters[(length + count).max(0) as usize..]
                    .iter()
                    .collect()
            })
        }
        "nospace" => Value::String(text(0)?.chars().filter(|c| !c.is_whitespace()).collect()),
        "contains" => Value::Bool(text(1)?.contains(&text(0)?)),
        "hasPrefix" => Value::Bool(text(1)?.starts_with(&text(0)?)),
        "hasSuffix" => Value::Bool(text(1)?.ends_with(&text(0)?)),
        "replace" => Value::String(text(2)?.replace(&text(0)?, &text(1)?)),
        "quote" | "squote" => {
            let quote = if name == "quote" { '"' } else { '\'' };
            Value::String(
                arguments
                    .iter()
                    .filter(|value| **value != Value::Nil)
                    .map(|value| format!("{quote}{}{quote}", value.to_go_string()))
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
        "cat" => Value::String(
            arguments
                .iter()
                .filter(|value| **value != Value::Nil)
                .map(Value::to_go_string)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        "indent" | "nindent" => {
            arity(2)?;
            let padding = " ".repeat(usize::try_from(to_int64(&arguments[0])).unwrap_or(0));
            let indented = format!(
                "{padding}{}",
                text(1)?.replace('\n', &format!("\n{padding}"))
            );
            Value::String(if name == "nindent" {
                format!("\n{indented}")
            } else {
                indented
            })
        }
        "split" | "splitn" => {
            let (separator, subject, limit) = if name == "split" {
                (text(0)?, text(1)?, None)
            } else {
                arity(3)?;
                (text(0)?, text(2)?, Some(to_int64(&arguments[1])))
            };
            let parts: Vec<String> = match limit {
                Some(limit) if limit > 0 => subject
                    .splitn(limit as usize, separator.as_str())
                    .map(str::to_owned)
                    .collect(),
                _ => subject
                    .split(separator.as_str())
                    .map(str::to_owned)
                    .collect(),
            };
            Value::Map(
                parts
                    .into_iter()
                    .enumerate()
                    .map(|(index, part)| (format!("_{index}"), Value::String(part)))
                    .collect(),
            )
        }
        "splitList" => Value::Array(
            text(1)?
                .split(text(0)?.as_str())
                .map(|part| Value::String(part.to_owned()))
                .collect(),
        ),
        "join" => {
            arity(2)?;
            Value::String(
                strings(&arguments[1])
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(&text(0)?),
            )
        }
        "sortAlpha" => {
            let mut items = strings(&first);
            items.sort();
            Value::Array(items.into_iter().map(Value::String).collect())
        }
        "toString" => Value::String(first.to_go_string()),
        "toStrings" => Value::Array(strings(&first).into_iter().map(Value::String).collect()),
        "snakecase" | "kebabcase" | "camelcase" => Value::String(case(name, &text(0)?)),
        // Numbers
        "int" | "int64" | "atoi" => Value::Int(to_int64(&first)),
        "float64" => Value::Float(to_float64(&first)),
        "add" => Value::Int(arguments.iter().map(to_int64).fold(0, i64::wrapping_add)),
        "add1" => Value::Int(to_int64(&first).wrapping_add(1)),
        "mul" => Value::Int(arguments.iter().map(to_int64).fold(1, i64::wrapping_mul)),
        "sub" | "div" | "mod" => {
            arity(2)?;
            let (a, b) = (to_int64(&arguments[0]), to_int64(&arguments[1]));
            Value::Int(match name {
                "sub" => a.wrapping_sub(b),
                _ if b == 0 => return Err(ExprError::new("integer divide by zero")),
                "div" => a.wrapping_div(b),
                _ => a.wrapping_rem(b),
            })
        }
        "max" | "biggest" => Value::Int(arguments.iter().map(to_int64).max().unwrap_or(0)),
        "min" => Value::Int(arguments.iter().map(to_int64).min().unwrap_or(0)),
        "floor" => Value::Float(to_float64(&first).floor()),
        "ceil" => Value::Float(to_float64(&first).ceil()),
        "round" => {
            let places = arguments.get(1).map_or(0, to_int64) as i32;
            let factor = 10f64.powi(places);
            Value::Float((to_float64(&first) * factor).round() / factor)
        }
        "until" => Value::Array((0..to_int64(&first).max(0)).map(Value::Int).collect()),
        "untilStep" => {
            arity(3)?;
            let (start, stop, step) = (
                to_int64(&arguments[0]),
                to_int64(&arguments[1]),
                to_int64(&arguments[2]),
            );
            let mut items = Vec::new();
            let mut current = start;
            while step != 0 && ((step > 0 && current < stop) || (step < 0 && current > stop)) {
                items.push(Value::Int(current));
                current += step;
            }
            Value::Array(items)
        }
        // Defaults and logic
        "default" => {
            let given = arguments.get(1).cloned().unwrap_or(Value::Nil);
            if empty(&given) { first } else { given }
        }
        "empty" => Value::Bool(empty(&first)),
        "coalesce" => arguments
            .iter()
            .find(|value| !empty(value))
            .cloned()
            .unwrap_or(Value::Nil),
        "all" => Value::Bool(arguments.iter().all(|value| !empty(value))),
        "any" => Value::Bool(arguments.iter().any(|value| !empty(value))),
        "ternary" => {
            arity(3)?;
            if arguments[2] == Value::Bool(true) {
                arguments[0].clone()
            } else {
                arguments[1].clone()
            }
        }
        "deepEqual" => {
            arity(2)?;
            Value::Bool(equal(&arguments[0], &arguments[1]))
        }
        "kindOf" | "typeOf" => Value::String(
            match first {
                Value::Nil => "invalid",
                Value::Bool(_) => "bool",
                Value::Int(_) => "int",
                Value::Float(_) => "float64",
                Value::String(_) => "string",
                Value::Array(_) => "slice",
                Value::Map(_) => "map",
            }
            .to_owned(),
        ),
        // Lists
        "list" => Value::Array(arguments),
        "first" => list(&first)?.first().cloned().unwrap_or(Value::Nil),
        "last" => list(&first)?.last().cloned().unwrap_or(Value::Nil),
        "rest" => Value::Array(list(&first)?.iter().skip(1).cloned().collect()),
        "initial" => {
            let items = list(&first)?;
            Value::Array(items[..items.len().saturating_sub(1)].to_vec())
        }
        "append" | "push" => {
            arity(2)?;
            let mut items = list(&first)?.to_vec();
            items.push(arguments[1].clone());
            Value::Array(items)
        }
        "prepend" => {
            arity(2)?;
            let mut items = vec![arguments[1].clone()];
            items.extend(list(&first)?.iter().cloned());
            Value::Array(items)
        }
        "concat" => {
            let mut items = Vec::new();
            for argument in &arguments {
                items.extend(list(argument)?.iter().cloned());
            }
            Value::Array(items)
        }
        "reverse" => Value::Array(list(&first)?.iter().rev().cloned().collect()),
        "uniq" => {
            let mut items: Vec<Value> = Vec::new();
            for item in list(&first)? {
                if !items.iter().any(|seen| equal(seen, item)) {
                    items.push(item.clone());
                }
            }
            Value::Array(items)
        }
        "compact" => Value::Array(
            list(&first)?
                .iter()
                .filter(|item| !empty(item))
                .cloned()
                .collect(),
        ),
        "without" => Value::Array(
            list(&first)?
                .iter()
                .filter(|item| !arguments[1..].iter().any(|other| equal(item, other)))
                .cloned()
                .collect(),
        ),
        "has" => {
            arity(2)?;
            Value::Bool(list(&arguments[1])?.iter().any(|item| equal(item, &first)))
        }
        "slice" => {
            let items = list(&first)?;
            let start = arguments.get(1).map_or(0, to_int64).max(0) as usize;
            let end = arguments.get(2).map_or(items.len() as i64, to_int64).max(0) as usize;
            Value::Array(
                items
                    .get(start.min(items.len())..end.min(items.len()).max(start.min(items.len())))
                    .unwrap_or_default()
                    .to_vec(),
            )
        }
        // Dictionaries
        "dict" => {
            let mut map = BTreeMap::new();
            for pair in arguments.chunks(2) {
                map.insert(
                    pair[0].to_go_string(),
                    pair.get(1).cloned().unwrap_or(Value::String(String::new())),
                );
            }
            Value::Map(map)
        }
        "get" => match &first {
            Value::Map(map) => map
                .get(&text(1)?)
                .cloned()
                .unwrap_or(Value::String(String::new())),
            _ => Value::String(String::new()),
        },
        "hasKey" => match &first {
            Value::Map(map) => Value::Bool(map.contains_key(&text(1)?)),
            _ => Value::Bool(false),
        },
        "keys" => {
            let mut keys = Vec::new();
            for argument in &arguments {
                if let Value::Map(map) = argument {
                    keys.extend(map.keys().cloned().map(Value::String));
                }
            }
            Value::Array(keys)
        }
        "values" => match &first {
            Value::Map(map) => Value::Array(map.values().cloned().collect()),
            _ => Value::Array(Vec::new()),
        },
        "pluck" => {
            let key = text(0)?;
            Value::Array(
                arguments[1..]
                    .iter()
                    .filter_map(|argument| match argument {
                        Value::Map(map) => map.get(&key).cloned(),
                        _ => None,
                    })
                    .collect(),
            )
        }
        "pick" | "omit" => {
            let Value::Map(map) = &first else {
                return Ok(Value::Map(BTreeMap::new()));
            };
            let names: Vec<String> = arguments[1..].iter().map(Value::to_go_string).collect();
            Value::Map(
                map.iter()
                    .filter(|(key, _)| names.contains(key) == (name == "pick"))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
        }
        "merge" => {
            let mut merged = BTreeMap::new();
            for argument in &arguments {
                if let Value::Map(map) = argument {
                    for (key, value) in map {
                        merged.entry(key.clone()).or_insert_with(|| value.clone());
                    }
                }
            }
            Value::Map(merged)
        }
        // Encodings
        "b64enc" => Value::String(base64_encode(text(0)?.as_bytes())),
        "b64dec" => Value::String(match base64_decode(&text(0)?) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => error,
        }),
        "toJson" | "toRawJson" | "mustToJson" | "mustToRawJson" => Value::String(first.to_json()),
        "toPrettyJson" | "mustToPrettyJson" => Value::String(first.to_json_indented()),
        "fromJson" => Value::parse_json(&text(0)?).unwrap_or(Value::Nil),
        "mustFromJson" => Value::parse_json(&text(0)?).map_err(ExprError::new)?,
        // Regular expressions
        "regexMatch" | "mustRegexMatch" => Value::Bool(regex(&text(0)?)?.is_match(&text(1)?)),
        "regexFind" | "mustRegexFind" => Value::String(
            regex(&text(0)?)?
                .find(&text(1)?)
                .map(|found| found.as_str().to_owned())
                .unwrap_or_default(),
        ),
        "regexFindAll" | "mustRegexFindAll" => {
            let limit = arguments.get(2).map_or(-1, to_int64);
            let subject = text(1)?;
            let pattern = regex(&text(0)?)?;
            let found = pattern
                .find_iter(&subject)
                .map(|found| Value::String(found.as_str().to_owned()));
            Value::Array(if limit < 0 {
                found.collect()
            } else {
                found.take(limit as usize).collect()
            })
        }
        // Go and the regex crate both write group references as `$1` or `${name}`.
        "regexReplaceAll" | "mustRegexReplaceAll" => Value::String(
            regex(&text(0)?)?
                .replace_all(&text(1)?, text(2)?.as_str())
                .into_owned(),
        ),
        "regexReplaceAllLiteral" | "mustRegexReplaceAllLiteral" => Value::String(
            regex(&text(0)?)?
                .replace_all(&text(1)?, regex::NoExpand(&text(2)?))
                .into_owned(),
        ),
        "regexSplit" | "mustRegexSplit" => {
            let limit = arguments.get(2).map_or(-1, to_int64);
            let subject = text(1)?;
            let pattern = regex(&text(0)?)?;
            Value::Array(if limit < 0 {
                pattern
                    .split(&subject)
                    .map(|part| Value::String(part.to_owned()))
                    .collect()
            } else {
                pattern
                    .splitn(&subject, limit as usize)
                    .map(|part| Value::String(part.to_owned()))
                    .collect()
            })
        }
        "regexQuoteMeta" => Value::String(regex::escape(&text(0)?)),
        // Dates, from the time of the run
        "now" => Value::String(crate::time::go_time_string(jiff::Timestamp::now())),
        "date" | "dateInZone" => {
            arity(2)?;
            let time = match &arguments[1] {
                Value::Int(seconds) => jiff::Timestamp::from_second(*seconds)
                    .unwrap_or_else(|_| jiff::Timestamp::now()),
                Value::Float(seconds) => jiff::Timestamp::from_second(*seconds as i64)
                    .unwrap_or_else(|_| jiff::Timestamp::now()),
                _ => jiff::Timestamp::now(),
            };
            Value::String(crate::time::go_format(&text(0)?, time))
        }
        "unixEpoch" => Value::String(jiff::Timestamp::now().as_second().to_string()),
        _ => {
            return Err(ExprError::new(format!("sprig.{name} is not supported")));
        }
    })
}

fn regex(pattern: &str) -> Result<regex::Regex> {
    regex::Regex::new(pattern).map_err(|error| ExprError::new(error.to_string()))
}

fn list(value: &Value) -> Result<&[Value]> {
    match value {
        Value::Array(items) => Ok(items),
        Value::Nil => Ok(&[]),
        other => Err(ExprError::new(format!(
            "expected a list, got {}",
            other.type_name()
        ))),
    }
}

/// Sprig's `strslice`: the texts of a list, or a single text.
fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter(|item| **item != Value::Nil)
            .map(Value::to_go_string)
            .collect(),
        Value::Nil => Vec::new(),
        other => vec![other.to_go_string()],
    }
}

/// Sprig's `empty`: nil, zero, false, and empty texts and collections.
pub fn empty(value: &Value) -> bool {
    match value {
        Value::Nil => true,
        Value::Bool(flag) => !flag,
        Value::Int(number) => *number == 0,
        Value::Float(number) => *number == 0.0,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Map(map) => map.is_empty(),
    }
}

/// `cast.ToInt64`: numbers truncate, texts parse with Go's base prefixes, and anything else
/// is 0.
pub fn to_int64(value: &Value) -> i64 {
    match value {
        Value::Int(number) => *number,
        Value::Float(number) => *number as i64,
        Value::Bool(flag) => i64::from(*flag),
        Value::String(text) => parse_go_int(text).unwrap_or(0),
        _ => 0,
    }
}

fn to_float64(value: &Value) -> f64 {
    match value {
        Value::Int(number) => *number as f64,
        Value::Float(number) => *number,
        Value::Bool(flag) => f64::from(u8::from(*flag)),
        Value::String(text) => text.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `strconv.ParseInt(text, 0, 64)` after cast's trimming of a zero fraction like `3.0`.
fn parse_go_int(text: &str) -> Option<i64> {
    let text = match text.split_once('.') {
        Some((whole, fraction)) if fraction.chars().all(|c| c == '0') => whole,
        _ => text,
    };
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let digits: String = digits.chars().filter(|c| *c != '_').collect();
    let (radix, rest) = match digits.get(..2) {
        Some("0x" | "0X") => (16, &digits[2..]),
        Some("0o" | "0O") => (8, &digits[2..]),
        Some("0b" | "0B") => (2, &digits[2..]),
        _ if digits.len() > 1 && digits.starts_with('0') => (8, &digits[1..]),
        _ => (10, digits.as_str()),
    };
    let magnitude = i64::from_str_radix(rest, radix).ok()?;
    Some(if negative { -magnitude } else { magnitude })
}

/// Go's `path.Base`.
pub fn path_base(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_owned()
}

/// Go's `path.Dir`.
pub fn path_dir(path: &str) -> String {
    let directory = match path.rfind('/') {
        Some(index) => &path[..=index],
        None => "",
    };
    path_clean(directory)
}

/// Go's `path.Ext`.
pub fn path_ext(path: &str) -> String {
    for (index, character) in path.char_indices().rev() {
        match character {
            '/' => break,
            '.' => return path[index..].to_owned(),
            _ => {}
        }
    }
    String::new()
}

/// Go's `path.Clean`: the shortest equivalent path, by lexical processing.
pub fn path_clean(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let rooted = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !rooted {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

fn title(text: &str) -> String {
    let mut out = String::new();
    let mut start = true;
    for character in text.chars() {
        if start && character.is_alphabetic() {
            out.extend(character.to_uppercase());
        } else {
            out.push(character);
        }
        start = !character.is_alphanumeric();
    }
    out
}

fn case(name: &str, text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for character in text.chars() {
        if !character.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if character.is_uppercase() && previous_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = character.is_lowercase() || character.is_ascii_digit();
        current.push(character);
    }
    if !current.is_empty() {
        words.push(current);
    }
    match name {
        "camelcase" => words
            .iter()
            .map(|word| title(&word.to_lowercase()))
            .collect(),
        "kebabcase" => words
            .iter()
            .map(|word| word.to_lowercase())
            .collect::<Vec<_>>()
            .join("-"),
        _ => words
            .iter()
            .map(|word| word.to_lowercase())
            .collect::<Vec<_>>()
            .join("_"),
    }
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, as Go's `base64.StdEncoding`.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(BASE64[((value >> (18 - 6 * index)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decodes standard base64, or explains why the text is not base64.
pub fn base64_decode(text: &str) -> std::result::Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for (index, character) in text.bytes().enumerate() {
        if character == b'=' {
            break;
        }
        let Some(value) = BASE64.iter().position(|c| *c == character) else {
            return Err(format!("illegal base64 data at input byte {index}"));
        };
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "sprig_tests.rs"]
mod tests;
