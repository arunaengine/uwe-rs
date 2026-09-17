//! Config files and config values from the command line, merged as Snakemake merges them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use python_evaluator::modules::data::{load_yaml, parse_json};
use python_evaluator::{Dict, Error, Interpreter, Key, Value};

use crate::workflow::workflow_error;

/// Reads a config file as JSON, else as YAML; it must hold a mapping.
pub fn load(interpreter: &mut Interpreter, path: &str, kind: &str) -> Result<Value, Error> {
    let resolved = interpreter.host.resolve(path);
    let data = interpreter
        .host
        .read(&resolved)
        .map_err(|_| workflow_error(format!("{kind} file {path} not found.")))?;
    let text = String::from_utf8_lossy(&data);
    let value = match parse_json(&text) {
        Ok(value) => value,
        Err(_) => load_yaml(&text).map_err(|error| {
            workflow_error(format!(
                "{kind} file is not valid JSON or YAML. In case of YAML, make sure to not mix whitespace and tab indentation. ({})",
                error.message()
            ))
        })?,
    };
    // Snakemake renders the YTE templates of a document that asks for it.
    python_evaluator::modules::render_yte(interpreter, value, None, true)
}

/// Loads a config file that must hold a mapping at the top.
pub fn load_config(interpreter: &mut Interpreter, path: &str) -> Result<Value, Error> {
    let value = load(interpreter, path, "Config")?;
    if !matches!(value.plain(), Value::Dict(_)) {
        return Err(workflow_error(
            "Config file must be given as JSON or YAML with keys at top level.",
        ));
    }
    Ok(value)
}

/// `snakemake.utils.update_config`: merges mappings recursively, other values replace.
pub fn update(
    interpreter: &mut Interpreter,
    config: &Value,
    overwrite: &Value,
) -> Result<(), Error> {
    let Value::Dict(target) = config.plain() else {
        return Ok(());
    };
    for (key, value) in interpreter.mapping_items(overwrite)? {
        let key = Key::new(key)?;
        let is_mapping = matches!(value.plain(), Value::Dict(_));
        if is_mapping {
            let existing = target.borrow().entries.get(&key).cloned();
            let sub = match existing {
                Some(existing) if matches!(existing.plain(), Value::Dict(_)) => existing,
                _ => {
                    let fresh = Value::dict(Dict::default());
                    target
                        .borrow_mut()
                        .entries
                        .insert(key.clone(), fresh.clone());
                    fresh
                }
            };
            update(interpreter, &sub, &value)?;
        } else {
            target.borrow_mut().entries.insert(key, value);
        }
    }
    Ok(())
}

/// A value of `--config name=value`: an integer, a float, `True` or `False`, a YAML list or
/// mapping of text, or else text, as Snakemake's `parse_config` tries them.
pub fn parse_value(text: &str) -> Value {
    if text.is_empty() {
        return Value::None;
    }
    if let Ok(number) = text.trim().parse::<i64>() {
        return Value::Int(number);
    }
    if let Ok(number) = python_evaluator::number::parse_float(text) {
        return Value::Float(number);
    }
    match text {
        "True" => return Value::Bool(true),
        "False" => return Value::Bool(false),
        _ => {}
    }
    if text.starts_with(['[', '{'])
        && let Ok(value) = load_yaml(text)
    {
        return stringify(value);
    }
    Value::str(text)
}

/// Makes every scalar text, as YAML's base loader reads them.
fn stringify(value: Value) -> Value {
    match value.plain() {
        Value::List(items) => Value::list(items.borrow().iter().cloned().map(stringify).collect()),
        Value::Dict(dict) => {
            let mut result = Dict::default();
            for (key, item) in dict.borrow().entries.iter() {
                let key = Key(Value::str(python_evaluator::format::plain_str(&key.0)));
                result.entries.insert(key, stringify(item.clone()));
            }
            Value::dict(result)
        }
        Value::Str(_) => value,
        Value::None => Value::str(""),
        other => Value::str(python_evaluator::format::plain_str(other)),
    }
}
