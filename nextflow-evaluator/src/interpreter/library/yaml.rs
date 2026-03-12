//! SnakeYAML's `Yaml`, as nf-core code uses it: `load` a document and `dumpAsMap` a map.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use saphyr_parser::{Event, Parser, ScalarStyle};

use crate::exception::Exception;
use crate::number::Number;
use crate::value::Value;

/// `yaml.load(text)`: mappings become maps and sequences lists; plain scalars are resolved
/// as YAML 1.1 does, so `1.13` is a number and `yes` a boolean. An empty text gives `null`.
pub(super) fn load(text: &str) -> Result<Value, Exception> {
    let mut stack: Vec<(Vec<Value>, bool)> = Vec::new();
    let mut root = None;
    for event in Parser::new_from_str(text) {
        let (event, _) =
            event.map_err(|error| Exception::new("YAMLException", error.info().to_owned()))?;
        let value = match event {
            Event::SequenceStart(..) => {
                stack.push((Vec::new(), false));
                continue;
            }
            Event::MappingStart(..) => {
                stack.push((Vec::new(), true));
                continue;
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let (items, mapping) = stack.pop().expect("an open collection");
                if mapping {
                    let pairs: Vec<(Value, Value)> = items
                        .chunks(2)
                        .map(|pair| (pair[0].clone(), pair.get(1).cloned().unwrap_or(Value::Null)))
                        .collect();
                    Value::map(pairs)
                } else {
                    Value::list(items)
                }
            }
            Event::Scalar(text, ScalarStyle::Plain, ..) => scalar(&text),
            Event::Scalar(text, ..) => Value::string(text.as_ref()),
            Event::Alias(_) => {
                return Err(Exception::new(
                    "YAMLException",
                    "YAML aliases are not supported yet",
                ));
            }
            _ => continue,
        };
        match stack.last_mut() {
            Some((items, _)) => items.push(value),
            None => root = Some(value),
        }
    }
    Ok(root.unwrap_or(Value::Null))
}

/// A plain scalar as YAML 1.1 resolves it: null, a boolean, a whole number, a decimal
/// number, or else text.
fn scalar(text: &str) -> Value {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" => {
            return Value::Boolean(true);
        }
        "false" | "False" | "FALSE" | "no" | "No" | "NO" | "off" | "Off" | "OFF" => {
            return Value::Boolean(false);
        }
        _ => {}
    }
    let digits = text.trim_start_matches(['+', '-']).replace('_', "");
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        let whole = text.replace('_', "");
        if let Ok(value) = whole.parse::<i32>() {
            return Value::Number(Number::Integer(value));
        }
        if let Ok(value) = whole.parse::<i64>() {
            return Value::Number(Number::Long(value));
        }
    }
    let decimal = digits.contains('.') || digits.contains(['e', 'E']);
    if decimal
        && digits
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '.')
        && let Ok(value) = text.replace('_', "").parse::<f64>()
    {
        return Value::Number(Number::Double(value));
    }
    Value::string(text)
}

/// `yaml.dumpAsMap(map)`: block style with two spaces of indent.
pub(super) fn dump_as_map(value: &Value) -> String {
    let mut out = String::new();
    write_block(value, 0, &mut out);
    out
}

fn write_block(value: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match value {
        Value::Map(map) => {
            if map.borrow().entries.is_empty() {
                out.push_str(&format!("{pad}{{}}\n"));
            }
            for (key, value) in &map.borrow().entries {
                let key = scalar_text(&key.0);
                match value {
                    Value::Map(inner) if !inner.borrow().entries.is_empty() => {
                        out.push_str(&format!("{pad}{key}:\n"));
                        write_block(value, indent + 2, out);
                    }
                    Value::List(list) if !list.borrow().items.is_empty() => {
                        out.push_str(&format!("{pad}{key}:\n"));
                        write_block(value, indent, out);
                    }
                    other => out.push_str(&format!("{pad}{key}: {}\n", scalar_text(other))),
                }
            }
        }
        Value::List(list) => {
            for item in list.borrow().items.iter() {
                out.push_str(&format!("{pad}- {}\n", scalar_text(item)));
            }
        }
        other => out.push_str(&format!("{pad}{}\n", scalar_text(other))),
    }
}

/// A scalar as SnakeYAML writes it: text that would read as another type is quoted.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::String(text) | Value::GString(text) => {
            let plain = !text.is_empty()
                && matches!(scalar(text), Value::String(_))
                && !text.starts_with([
                    ' ', '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>',
                    '\'', '"', '%', '@', '`',
                ])
                && !text.ends_with(' ')
                && !text.contains(": ")
                && !text.contains(" #")
                && !text.contains('\n');
            if plain {
                text.to_string()
            } else {
                format!("'{}'", text.replace('\'', "''"))
            }
        }
        Value::List(list) => {
            let items: Vec<String> = list.borrow().items.iter().map(scalar_text).collect();
            format!("[{}]", items.join(", "))
        }
        other => other.to_string(),
    }
}
