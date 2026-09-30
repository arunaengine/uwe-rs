//! Describes workflow inputs as Bioschemas formal parameters in an exported crate.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use input_schema::{DIRECTORY_PATH, FILE_PATH, FILE_PATH_PATTERN, PATH, Property};
use serde_json::{Value, json};

use crate::entities::one_or_many;
use crate::ids::encode;

/// The file of the crate that holds the JSON Schema of the inputs.
pub(crate) const INPUT_SCHEMA: &str = "inputs.schema.json";

/// The profile that every formal parameter conforms to.
pub(crate) const FORMAL_PARAMETER: &str =
    "https://bioschemas.org/profiles/FormalParameter/1.0-RELEASE";

/// The id of the formal parameter of an input.
pub(crate) fn input_id(name: &str) -> String {
    format!("#input/{}", encode(name))
}

/// The entity of the schema file.
pub(crate) fn schema_file(workflow: &str) -> Value {
    json!({
        "@id": INPUT_SCHEMA,
        "@type": "File",
        "name": "The JSON Schema of the inputs of the workflow",
        "encodingFormat": "application/schema+json",
        "about": {"@id": workflow}
    })
}

/// The formal parameter of an input: its name, its type in the words of the Workflow Run
/// Crate, whether it needs a value, its default, its description and the formats of a file.
pub(crate) fn formal_parameter(property: &Property) -> Value {
    let schema = property.schema;
    let mut parameter = json!({
        "@id": input_id(property.name),
        "@type": "FormalParameter",
        "conformsTo": {"@id": FORMAL_PARAMETER},
        "name": property.name,
        "additionalType": additional_type(schema),
        "valueRequired": property.required
    });
    if let Some(default) = schema.get("default") {
        parameter["defaultValue"] = json!(match default {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        });
    }
    let description = schema.get("description").or_else(|| schema.get("title"));
    if let Some(description) = description.and_then(Value::as_str) {
        parameter["description"] = json!(description);
    }
    let formats: Vec<&str> = ["mimetype", "fileFormat"]
        .iter()
        .filter_map(|keyword| schema.get(*keyword))
        .flat_map(|formats| match formats {
            Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
            other => other.as_str().into_iter().collect::<Vec<_>>(),
        })
        .collect();
    if !formats.is_empty() {
        parameter["encodingFormat"] = one_or_many(formats.into_iter().map(Value::from));
    }
    parameter
}

/// The input whose value names a file of the run: its location, or a relative path that the
/// location ends with, also inside a list.
pub(crate) fn input_of_file<'a>(values: &'a Value, location: &str) -> Option<&'a str> {
    let Value::Object(values) = values else {
        return None;
    };
    values
        .iter()
        .find(|(_, value)| names_file(value, location))
        .map(|(name, _)| name.as_str())
}

fn names_file(value: &Value, location: &str) -> bool {
    match value {
        Value::String(text) if !text.is_empty() => {
            let relative = text.trim_start_matches("./");
            text == location
                || (!text.starts_with('/') && location.ends_with(&format!("/{relative}")))
        }
        Value::Array(items) => items.iter().any(|item| names_file(item, location)),
        _ => false,
    }
}

/// The type of an input as the Workflow Run Crate names it: `File`, `Dataset` for a directory,
/// `Collection` for a list, `Text`, `Integer`, `Float`, `Boolean`, or `PropertyValue` for an
/// object or a value of several types.
fn additional_type(schema: &Value) -> &'static str {
    match schema.get("format").and_then(Value::as_str) {
        Some(FILE_PATH | PATH | FILE_PATH_PATTERN) => return "File",
        Some(DIRECTORY_PATH) => return "Dataset",
        _ => {}
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => "Text",
        Some("integer") => "Integer",
        Some("number") => "Float",
        Some("boolean") => "Boolean",
        Some("array") => "Collection",
        _ => "PropertyValue",
    }
}
