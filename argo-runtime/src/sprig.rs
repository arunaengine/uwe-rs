//! The Sprig functions (github.com/Masterminds/sprig v3.3.0) that Argo offers in expressions as
//! `sprig.<name>(...)`. Argo leaves out `env` and `expandenv`. Conversions follow Sprig's use
//! of `cast`: a text that is not a number converts to 0 rather than failing.
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
            Value::String(text(1)?.repeat(count))
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
