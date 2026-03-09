//! nf-schema: parameter validation and summaries, and samplesheets as lists, from JSON
//! schemas (JSON Schema 2020-12, through the `jsonschema` crate).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod samplesheet;

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, parse_json};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{Key, PathValue, Value};

use crate::Context;

pub(crate) fn call(
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    let (options, positional) = split(arguments);
    match (name, positional.as_slice()) {
        ("samplesheetToList", [samplesheet, schema]) => {
            samplesheet::to_list(samplesheet, schema, options.as_ref(), context)
        }
        _ => Err(Error::Unsupported {
            message: format!(
                "the function `{name}` of the plugin `nf-schema` with these arguments"
            ),
            span: Default::default(),
        }),
    }
}

/// Named arguments, which come first as a map, and the others.
fn split(arguments: Vec<Value>) -> (Option<Value>, Vec<Value>) {
    let mut arguments = arguments.into_iter().peekable();
    let options = arguments.next_if(|argument| matches!(argument, Value::Map(_)));
    (options, arguments.collect())
}

/// The value of a key of a map.
fn get(map: &Value, key: &str) -> Option<Value> {
    match map {
        Value::Map(map) => map.borrow().entries.get(&Key(Value::string(key))).cloned(),
        _ => None,
    }
}

/// The entries of a map, with the keys as text.
fn entries(map: &Value) -> Vec<(String, Value)> {
    match map {
        Value::Map(map) => map
            .borrow()
            .entries
            .iter()
            .map(|(key, value)| (key.0.to_string(), value.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

/// A path from a path or its text; relative text starts at `base`.
fn path(value: &Value, base: &str) -> PathValue {
    match value {
        Value::Path(path) => (**path).clone(),
        other => PathValue::parse(&other.to_string(), base),
    }
}

/// A JSON schema: its text and its tree with keys in file order.
struct Schema {
    json: serde_json::Value,
    tree: Value,
}

/// Reads a schema. A relative path starts at the project directory, as nf-schema does.
fn schema(value: &Value, context: &mut Context) -> Result<Schema, Error> {
    let path = path(value, context.project_dir);
    let text = read(&path, context)?;
    let tree = parse_json(&text).map_err(|exception| thrown(exception.message))?;
    let json = serde_json::from_str(&text)
        .map_err(|error| thrown(format!("Invalid JSON schema {}: {error}", path.uri())))?;
    Ok(Schema { json, tree })
}

fn read(path: &PathValue, context: &mut Context) -> Result<String, Error> {
    let bytes = context.host.read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// A value as JSON, for validation.
fn json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(value) => serde_json::Value::Bool(*value),
        Value::Number(Number::Integer(value)) => (*value).into(),
        Value::Number(Number::Long(value)) => (*value).into(),
        Value::Number(number) => serde_json::Number::from_f64(number.to_f64())
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::List(list) => list.borrow().items.iter().map(json).collect(),
        Value::Map(_) => serde_json::Value::Object(
            entries(value)
                .into_iter()
                .map(|(key, value)| (key, json(&value)))
                .collect(),
        ),
        other => serde_json::Value::String(other.to_string()),
    }
}

/// nf-schema's `SchemaValidationException`.
fn thrown(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::new("SchemaValidationException", message)),
        span: Default::default(),
    }
}
