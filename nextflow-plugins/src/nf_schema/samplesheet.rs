//! `samplesheetToList`: the rows of a CSV, TSV or JSON samplesheet as lists, converted and
//! checked by the schema of its items.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::{Error, parse_json};
use nextflow_evaluator::number::{BigDecimal, Number};
use nextflow_evaluator::value::{PathValue, Value};

use super::{entries, errors, get, json, path, plain, read, schema, thrown};
use crate::Context;

/// Each row becomes a list: a map of the fields with `meta` keys first, if the schema has
/// any, then the other fields in schema order. A missing field is its default, else `[]`.
pub(super) fn to_list(
    samplesheet: &Value,
    schema_path: &Value,
    _options: Option<&Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    let launch = context.host.launch_dir();
    let file = path(samplesheet, &launch);
    let schema = schema(schema_path, context)?;
    let text = read(&file, context)?;
    let rows = rows(&file, &text)?;
    let items = get(&schema.tree, "items").unwrap_or_else(|| Value::map([]));
    let properties = get(&items, "properties").map_or_else(Vec::new, |p| entries(&p));
    // Values of the types of the schema, which are checked before they become paths.
    let mut typed_rows = Vec::new();
    for row in &rows {
        let mut typed = Vec::new();
        for (name, definition) in &properties {
            let text = row
                .iter()
                .find(|(header, _)| header == name)
                .map(|(_, v)| v.as_str());
            if let Some(value) = text.filter(|text| !text.is_empty()) {
                typed.push((name.clone(), typed_value(value, definition)));
            }
        }
        typed_rows.push(typed);
    }
    check(&schema.json, &typed_rows, &file)?;
    let has_meta = properties
        .iter()
        .any(|(_, definition)| get(definition, "meta").is_some());
    let mut result = Vec::new();
    for typed in typed_rows {
        let mut meta = Vec::new();
        let mut fields = Vec::new();
        for (name, definition) in &properties {
            let value = match typed.iter().find(|(field, _)| field == name) {
                Some((_, value)) => file_value(value, definition, &launch),
                None => get(definition, "default").unwrap_or_else(|| Value::list(Vec::new())),
            };
            match get(definition, "meta") {
                Some(Value::List(keys)) => {
                    for key in keys.borrow().items.iter() {
                        meta.push((key.clone(), value.clone()));
                    }
                }
                Some(key) => meta.push((key, value)),
                None => fields.push(value),
            }
        }
        if has_meta {
            fields.insert(0, Value::map(meta));
        }
        result.push(Value::list(fields));
    }
    Ok(Value::list(result))
}

/// A field's text as the type of its schema: `integer`, `number` or `boolean`, else text.
fn typed_value(text: &str, definition: &Value) -> Value {
    let kind = get(definition, "type").map(|kind| kind.to_string());
    match kind.as_deref() {
        Some("integer") => match text.parse::<i32>() {
            Ok(value) => Value::Number(Number::Integer(value)),
            Err(_) => text
                .parse::<i64>()
                .map_or_else(|_| Value::string(text), |v| Value::Number(Number::Long(v))),
        },
        Some("number") => BigDecimal::parse(text).map_or_else(
            || Value::string(text),
            |v| Value::Number(Number::BigDecimal(v)),
        ),
        Some("boolean") if text.eq_ignore_ascii_case("true") => Value::Boolean(true),
        Some("boolean") if text.eq_ignore_ascii_case("false") => Value::Boolean(false),
        _ => Value::string(text),
    }
}

/// A field with a path format becomes a path, which relative text starts at `launch`.
fn file_value(value: &Value, definition: &Value, launch: &str) -> Value {
    let format = get(definition, "format").map(|format| format.to_string());
    match (format.as_deref(), value) {
        (Some("file-path" | "directory-path" | "path"), Value::String(text)) => {
            Value::Path(std::rc::Rc::new(PathValue::parse(text, launch)))
        }
        _ => value.clone(),
    }
}

/// Checks the rows against the schema, as an array of objects.
fn check(
    schema: &serde_json::Value,
    rows: &[Vec<(String, Value)>],
    file: &PathValue,
) -> Result<(), Error> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|error| thrown(format!("Invalid samplesheet schema: {error}")))?;
    let instance: serde_json::Value = rows
        .iter()
        .map(|row| {
            serde_json::Value::Object(
                row.iter()
                    .map(|(name, value)| (name.clone(), json(value)))
                    .collect(),
            )
        })
        .collect();
    let errors: Vec<String> = errors(&validator, &instance, "field")
        .into_iter()
        .map(|(location, message)| {
            let mut parts = location.trim_start_matches('/').splitn(2, '/');
            let entry = parts.next().and_then(|index| index.parse::<usize>().ok());
            match (entry, parts.next()) {
                (Some(index), Some(field)) => {
                    let value = instance[index].get(field).map(plain).unwrap_or_default();
                    format!(
                        "-> Entry {}: Error for field '{field}' ({value}): {message}",
                        index + 1
                    )
                }
                (Some(index), None) => format!("-> Entry {}: {message}", index + 1),
                _ => format!("-> {message}"),
            }
        })
        .collect();
    if errors.is_empty() {
        return Ok(());
    }
    Err(thrown(format!(
        "The following errors have been detected in {}:\n\n{}\n",
        file.uri(),
        errors.join("\n")
    )))
}

/// The rows of a samplesheet as pairs of header and text. JSON files hold a list of objects.
fn rows(file: &PathValue, text: &str) -> Result<Vec<Vec<(String, String)>>, Error> {
    let name = file.name().to_ascii_lowercase();
    if name.ends_with(".json") {
        let parsed = parse_json(text).map_err(|exception| thrown(exception.message))?;
        let Value::List(list) = parsed else {
            return Err(thrown(format!(
                "The samplesheet {} is not a list",
                file.uri()
            )));
        };
        let items = list.borrow().items.clone();
        return Ok(items
            .iter()
            .map(|row| {
                entries(row)
                    .into_iter()
                    .map(|(key, value)| {
                        (
                            key,
                            if let Value::Null = value {
                                String::new()
                            } else {
                                value.to_string()
                            },
                        )
                    })
                    .collect()
            })
            .collect());
    }
    let separator = match () {
        _ if name.ends_with(".csv") => ',',
        _ if name.ends_with(".tsv") => '\t',
        _ => {
            return Err(Error::Unsupported {
                message: format!(
                    "samplesheets like {} that are not CSV, TSV or JSON",
                    file.uri()
                ),
                span: Default::default(),
            });
        }
    };
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let headers = lines
        .next()
        .map(|line| fields(line, separator))
        .unwrap_or_default();
    Ok(lines
        .map(|line| {
            headers
                .iter()
                .cloned()
                .zip(fields(line, separator))
                .collect()
        })
        .collect())
}

/// The fields of one line; double quotes group text with separators and `""` is a quote.
fn fields(line: &str, separator: char) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut quoted = false;
    let mut characters = line.trim_end_matches('\r').chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '"' if quoted && characters.peek() == Some(&'"') => {
                characters.next();
                fields.last_mut().expect("a field").push('"');
            }
            '"' => quoted = !quoted,
            c if c == separator && !quoted => fields.push(String::new()),
            other => fields.last_mut().expect("a field").push(other),
        }
    }
    fields
        .into_iter()
        .map(|field| field.trim().to_owned())
        .collect()
}
