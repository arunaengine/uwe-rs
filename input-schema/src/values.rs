//! The values of a run as the check sees them: the texts of the command line read as the
//! numbers and booleans that the schema asks for.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;

use crate::properties::resolve;

/// The values as the check sees them: texts turned into the numbers and booleans that the schema
/// asks for, as values from the command line are texts.
pub(crate) fn prepared(root: &Json, schema: &Json, value: Json) -> Json {
    let (schema, _) = resolve(root, schema);
    let mut value = value;
    if let Some(Json::Array(parts)) = schema.get("allOf") {
        for part in parts {
            value = prepared(root, part, value);
        }
    }
    match value {
        Json::String(text) => typed(root, schema, text),
        Json::Object(entries) => {
            let properties = schema.get("properties");
            let entries = entries.into_iter().map(|(name, value)| {
                match properties.and_then(|p| p.get(&name)) {
                    Some(property) => {
                        let value = prepared(root, property, value);
                        (name, value)
                    }
                    None => (name, value),
                }
            });
            Json::Object(entries.collect())
        }
        Json::Array(items) => match schema.get("items") {
            Some(item @ Json::Object(_)) => Json::Array(
                items
                    .into_iter()
                    .map(|value| prepared(root, item, value))
                    .collect(),
            ),
            _ => Json::Array(items),
        },
        other => other,
    }
}

/// A text as the number or boolean that the schema asks for, when it reads as one.
fn typed(root: &Json, schema: &Json, text: String) -> Json {
    let types = types(root, schema);
    if types.is_empty() || types.contains(&"string") {
        return Json::String(text);
    }
    let trimmed = text.trim();
    if types.contains(&"integer")
        && let Ok(number) = trimmed.parse::<i64>()
    {
        return Json::from(number);
    }
    if types.contains(&"number")
        && let Some(number) = trimmed
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
    {
        return Json::Number(number);
    }
    if types.contains(&"boolean") {
        match trimmed.to_ascii_lowercase().as_str() {
            "true" => return Json::Bool(true),
            "false" => return Json::Bool(false),
            _ => {}
        }
    }
    Json::String(text)
}

/// The JSON types that a schema allows: its `type`, else those of its `anyOf` and `oneOf`.
fn types<'a>(root: &'a Json, schema: &'a Json) -> Vec<&'a str> {
    match schema.get("type") {
        Some(Json::String(kind)) => vec![kind.as_str()],
        Some(Json::Array(kinds)) => kinds.iter().filter_map(Json::as_str).collect(),
        _ => ["anyOf", "oneOf"]
            .iter()
            .filter_map(|keyword| schema.get(*keyword).and_then(Json::as_array))
            .flatten()
            .flat_map(|branch| types(root, resolve(root, branch).0))
            .collect(),
    }
}
