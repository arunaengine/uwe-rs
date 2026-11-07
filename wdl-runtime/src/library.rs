//! The WDL standard library: the functions that expressions call.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use indexmap::IndexMap;
use wdl_analysis::types::Type;

use crate::expression::{Environment, Error, Stream, equal};
use crate::value::Value;

/// The value of a call of a standard library function.
pub fn call(name: &str, arguments: Vec<Value>, env: &mut dyn Environment) -> Result<Value, Error> {
    let fail = |message: String| Err(Error::Failed(format!("{name}(): {message}")));
    match (name, arguments.as_slice()) {
        ("floor", [value]) => Ok(Value::Int(float(value)?.floor() as i64)),
        ("ceil", [value]) => Ok(Value::Int(float(value)?.ceil() as i64)),
        ("round", [value]) => Ok(Value::Int(float(value)?.round() as i64)),
        ("min" | "max", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int(if name == "min" {
            *a.min(b)
        } else {
            *a.max(b)
        })),
        ("min" | "max", [a, b]) => {
            let (a, b) = (float(a)?, float(b)?);
            Ok(Value::Float(if name == "min" {
                a.min(b)
            } else {
                a.max(b)
            }))
        }
        ("sub", [input, pattern, replace]) => {
            let pattern = regex(&text(pattern)?)?;
            let replace = text(replace)?.replace('$', "$$");
            let replace = regex(r"\\(\d)")?.replace_all(&replace, "$${$1}");
            Ok(Value::String(
                pattern
                    .replace_all(&text(input)?, replace.as_ref())
                    .into_owned(),
            ))
        }
        ("find", [input, pattern]) => Ok(match regex(&text(pattern)?)?.find(&text(input)?) {
            Some(found) => Value::String(found.as_str().to_owned()),
            None => Value::None,
        }),
        ("matches", [input, pattern]) => Ok(Value::Boolean(
            regex(&text(pattern)?)?.is_match(&text(input)?),
        )),
        ("basename", [path, rest @ ..]) if rest.len() <= 1 => {
            let path = text(path)?;
            let mut name = path
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default();
            if let [suffix] = rest {
                let suffix = text(suffix)?;
                name = name.strip_suffix(suffix.as_str()).unwrap_or(name);
            }
            Ok(Value::String(name.to_owned()))
        }
        ("join_paths", [first, rest @ ..]) if rest.len() <= 1 => {
            let mut parts = match rest {
                [] => strings(first)?,
                [Value::Array(_)] => [vec![text(first)?], strings(&rest[0])?].concat(),
                _ => vec![text(first)?, text(&rest[0])?],
            };
            if parts.iter().skip(1).any(|part| part.starts_with('/')) {
                return fail("only the first part may be an absolute path".into());
            }
            let first = parts.remove(0);
            let joined = (parts.iter()).fold(first, |path, part| {
                format!("{}/{part}", path.trim_end_matches('/'))
            });
            Ok(Value::File(joined))
        }
        ("sep", [separator, items]) => Ok(Value::String(strings(items)?.join(&text(separator)?))),
        ("prefix" | "suffix", [affix, items]) => {
            let affix = text(affix)?;
            let joined = strings(items)?.into_iter().map(|item| match name {
                "prefix" => format!("{affix}{item}"),
                _ => format!("{item}{affix}"),
            });
            Ok(Value::Array(joined.map(Value::String).collect()))
        }
        ("quote" | "squote", [items]) => {
            let mark = if name == "quote" { '"' } else { '\'' };
            let quoted = strings(items)?
                .into_iter()
                .map(|item| format!("{mark}{item}{mark}"));
            Ok(Value::Array(quoted.map(Value::String).collect()))
        }
        ("stdout", []) => env.stream(Stream::Stdout),
        ("stderr", []) => env.stream(Stream::Stderr),
        ("glob", [pattern]) => {
            let found = env.glob(&text(pattern)?)?;
            Ok(Value::Array(found.into_iter().map(Value::File).collect()))
        }
        ("size", [value, rest @ ..]) if rest.len() <= 1 => {
            let unit = match rest {
                [unit] => unit_bytes(&text(unit)?).ok_or_else(|| Error::failed("unknown unit"))?,
                _ => 1.0,
            };
            let mut paths = Vec::new();
            value
                .clone()
                .visit_paths(&mut |path, _| paths.push(path.clone()));
            let mut total = 0;
            for path in paths {
                total += env.size(&path)?;
            }
            Ok(Value::Float(total as f64 / unit))
        }
        ("read_string", [file]) => {
            let content = env.read(&text(file)?)?;
            Ok(Value::String(
                content.trim_end_matches(['\n', '\r']).to_owned(),
            ))
        }
        ("read_int" | "read_float" | "read_boolean", [file]) => {
            let content = env.read(&text(file)?)?;
            let content = content.trim();
            let value = match name {
                "read_int" => content.parse().ok().map(Value::Int),
                "read_float" => content.parse().ok().map(Value::Float),
                _ => match content.to_ascii_lowercase().as_str() {
                    "true" => Some(Value::Boolean(true)),
                    "false" => Some(Value::Boolean(false)),
                    _ => None,
                },
            };
            value.map_or_else(|| fail(format!("cannot read `{content}`")), Ok)
        }
        ("read_lines", [file]) => {
            let content = env.read(&text(file)?)?;
            Ok(Value::Array(lines(&content).map(Value::String).collect()))
        }
        ("read_tsv", [file]) => {
            let content = env.read(&text(file)?)?;
            let rows = lines(&content).map(|line| {
                Value::Array(
                    line.split('\t')
                        .map(|cell| Value::String(cell.into()))
                        .collect(),
                )
            });
            Ok(Value::Array(rows.collect()))
        }
        ("read_map", [file]) => {
            let content = env.read(&text(file)?)?;
            let mut entries: Vec<(Value, Value)> = Vec::new();
            for line in lines(&content) {
                let Some((key, value)) = line.split_once('\t') else {
                    return fail(format!("line `{line}` does not have two columns"));
                };
                let key = Value::String(key.to_owned());
                if entries.iter().any(|(known, _)| equal(known, &key)) {
                    return fail(format!("the key {} repeats", key.to_json()));
                }
                entries.push((key, Value::String(value.to_owned())));
            }
            Ok(Value::Map(entries))
        }
        ("read_json", [file]) => {
            let content = env.read(&text(file)?)?;
            let json: serde_json::Value =
                serde_json::from_str(&content).map_err(|error| Error::failed(error.to_string()))?;
            Value::from_json(&json, &Type::Union, std::path::Path::new("")).map_err(Error::Failed)
        }
        _ if KNOWN.contains(&name) => fail(format!("wrong arguments: {}", kinds(&arguments))),
        _ => Err(Error::Unsupported(format!(
            "the function `{name}` is not supported"
        ))),
    }
}

/// Functions of the library that the match above implements.
const KNOWN: &[&str] = &[
    "floor",
    "ceil",
    "round",
    "min",
    "max",
    "sub",
    "find",
    "matches",
    "basename",
    "join_paths",
    "sep",
    "prefix",
    "suffix",
    "quote",
    "squote",
    "stdout",
    "stderr",
    "glob",
    "size",
    "read_string",
    "read_int",
    "read_float",
    "read_boolean",
    "read_lines",
    "read_tsv",
    "read_map",
    "read_json",
];

fn kinds(arguments: &[Value]) -> String {
    let kinds: Vec<&str> = arguments.iter().map(Value::kind).collect();
    kinds.join(", ")
}

fn float(value: &Value) -> Result<f64, Error> {
    match value {
        Value::Int(number) => Ok(*number as f64),
        Value::Float(number) => Ok(*number),
        value => Err(Error::failed(format!("a {} is not a number", value.kind()))),
    }
}

fn text(value: &Value) -> Result<String, Error> {
    (value.text()).ok_or_else(|| Error::failed(format!("a {} is not a string", value.kind())))
}

fn array(value: &Value) -> Result<&[Value], Error> {
    match value {
        Value::Array(items) => Ok(items),
        value => Err(Error::failed(format!("a {} is not an array", value.kind()))),
    }
}

/// The texts of an array of primitive values.
fn strings(value: &Value) -> Result<Vec<String>, Error> {
    array(value)?.iter().map(text).collect()
}

fn regex(pattern: &str) -> Result<regex::Regex, Error> {
    regex::Regex::new(pattern).map_err(|error| Error::failed(error.to_string()))
}

/// The lines of a text; a last newline ends the last line.
fn lines(content: &str) -> impl Iterator<Item = String> + '_ {
    content.lines().map(str::to_owned)
}

/// The bytes of a unit of `size()`, like `GiB`.
fn unit_bytes(unit: &str) -> Option<f64> {
    let (base, power): (f64, i32) = match unit.trim_end_matches('B') {
        "" => return Some(1.0),
        "K" | "k" => (1000.0, 1),
        "M" => (1000.0, 2),
        "G" => (1000.0, 3),
        "T" => (1000.0, 4),
        "Ki" => (1024.0, 1),
        "Mi" => (1024.0, 2),
        "Gi" => (1024.0, 3),
        "Ti" => (1024.0, 4),
        _ => return None,
    };
    Some(base.powi(power))
}

/// The bytes of a memory amount of a task, like `"2 GiB"` or `2000000000`.
pub fn memory_bytes(value: &Value) -> Option<u64> {
    match value {
        Value::Int(bytes) => u64::try_from(*bytes).ok(),
        Value::String(text) => {
            let text = text.trim();
            let split = text
                .find(|c: char| c.is_ascii_alphabetic())
                .unwrap_or(text.len());
            let (number, unit) = text.split_at(split);
            let number: f64 = number.trim().parse().ok()?;
            Some((number * unit_bytes(unit.trim())?) as u64)
        }
        _ => None,
    }
}

/// An object of named values, as the outputs of a call.
pub fn object(fields: impl IntoIterator<Item = (String, Value)>) -> Value {
    Value::Object(fields.into_iter().collect::<IndexMap<_, _>>())
}
