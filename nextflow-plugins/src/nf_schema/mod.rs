//! nf-schema: parameter validation and summaries, and samplesheets as lists, from JSON
//! schemas (JSON Schema 2020-12, through the `jsonschema` crate).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod params;
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
    // Named arguments come first as a map; nf-schema also takes a map of options as the last
    // argument of `samplesheetToList`.
    match (name, arguments.as_slice()) {
        ("samplesheetToList", [sheet, schema]) => {
            samplesheet::to_list(sheet, schema, None, context)
        }
        ("samplesheetToList", [options @ Value::Map(_), sheet, schema])
        | ("samplesheetToList", [sheet, schema, options]) => {
            samplesheet::to_list(sheet, schema, Some(options), context)
        }
        ("validateParameters", []) => params::validate(None, context),
        ("validateParameters", [options]) => params::validate(Some(options), context),
        ("paramsSummaryMap", [workflow]) => params::summary(None, workflow, context),
        ("paramsSummaryMap", [options, workflow]) => {
            params::summary(Some(options), workflow, context)
        }
        ("paramsSummaryLog", [workflow]) => params::summary_log(None, workflow, context),
        ("paramsSummaryLog", [options, workflow]) => {
            params::summary_log(Some(options), workflow, context)
        }
        ("paramsHelp", []) => params::help(None, context),
        ("paramsHelp", [options @ Value::Map(_), ..]) => params::help(Some(options), context),
        _ => Err(Error::Unsupported {
            message: format!(
                "the function `{name}` of the plugin `nf-schema` with these arguments"
            ),
            span: Default::default(),
        }),
    }
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

/// The errors of an instance, in nf-schema's words, by the path of the value they concern.
/// Missing required fields become one error of the object, like `Missing required field(s):
/// a, b`; `what` names the fields, like `field` or `parameter`.
fn errors(
    validator: &jsonschema::Validator,
    instance: &serde_json::Value,
    what: &str,
) -> Vec<(String, String)> {
    use jsonschema::error::{TypeKind, ValidationErrorKind as Kind};
    let mut errors: Vec<(String, String)> = Vec::new();
    let mut missing: Vec<(String, Vec<String>)> = Vec::new();
    for error in validator.iter_errors(instance) {
        let path = error.instance_path().to_string();
        let value = error.instance();
        let message = match error.kind() {
            Kind::Required { property } => {
                let property = property.as_str().unwrap_or_default().to_owned();
                match missing.iter_mut().find(|(at, _)| *at == path) {
                    Some((_, names)) => names.push(property),
                    None => missing.push((path, vec![property])),
                }
                continue;
            }
            Kind::Type { kind } => {
                let expected = match kind {
                    TypeKind::Single(kind) => kind.to_string(),
                    TypeKind::Multiple(kinds) => {
                        let kinds: Vec<String> =
                            kinds.iter().map(|kind| kind.to_string()).collect();
                        kinds.join(", ")
                    }
                };
                format!("Value is [{}] but should be [{expected}]", json_type(value))
            }
            Kind::Pattern { pattern } => {
                format!("{value} does not match regular expression [{pattern}]")
            }
            Kind::Enum { options } => {
                let options: Vec<String> = options
                    .as_array()
                    .map(|items| items.iter().map(plain).collect())
                    .unwrap_or_default();
                format!("Expected any of [[{}]]", options.join(", "))
            }
            _ => error.to_string(),
        };
        errors.push((path, message));
    }
    for (path, mut names) in missing {
        names.sort();
        errors.push((
            path,
            format!("Missing required {what}(s): {}", names.join(", ")),
        ));
    }
    errors
}

/// The JSON type of a value, as nf-schema names it.
fn json_type(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(number) if number.is_f64() => "number",
        serde_json::Value::Number(_) => "integer",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// A JSON value as Groovy prints it: text without quotes.
fn plain(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// nf-schema's `SchemaValidationException`.
fn thrown(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::new("SchemaValidationException", message)),
        span: Default::default(),
    }
}
