//! The values of a run as the check sees them: the texts of the command line read as the
//! numbers and booleans that the schema asks for.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;

use crate::properties::resolve;

/// The values as the check sees them: texts turned into the numbers and booleans that the schema
/// asks for, as values from the command line are texts.
pub(crate) fn prepared(root: &Json, schema: &Json, value: Json) -> Json {
    prepare(root, schema, value, &mut Vec::new())
}

fn prepare<'a>(root: &'a Json, schema: &'a Json, value: Json, active: &mut Vec<&'a Json>) -> Json {
    let (schema, _) = resolve(root, schema);
    if active.iter().any(|seen| std::ptr::eq(*seen, schema)) {
        return value;
    }
    active.push(schema);
    let mut value = value;
    if let Some(Json::Array(parts)) = schema.get("allOf") {
        for part in parts {
            value = prepare(root, part, value, active);
        }
    }
    active.pop();
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
    let types = types(root, schema, &mut Vec::new());
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
fn types<'a>(root: &'a Json, schema: &'a Json, active: &mut Vec<&'a Json>) -> Vec<&'a str> {
    if active.iter().any(|seen| std::ptr::eq(*seen, schema)) {
        return Vec::new();
    }
    active.push(schema);
    let types = match schema.get("type") {
        Some(Json::String(kind)) => vec![kind.as_str()],
        Some(Json::Array(kinds)) => kinds.iter().filter_map(Json::as_str).collect(),
        _ => ["anyOf", "oneOf"]
            .iter()
            .filter_map(|keyword| schema.get(*keyword).and_then(Json::as_array))
            .flatten()
            .flat_map(|branch| types(root, resolve(root, branch).0, active))
            .collect(),
    };
    active.pop();
    types
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn coerces_recursive_applicators() {
        let schema = json!({
            "allOf": [{"$ref": "#"}],
            "properties": {"count": {"type": "integer"}}
        });
        assert_eq!(
            prepared(&schema, &schema, json!({"count": "7"})),
            json!({"count": 7})
        );
        let schema = json!({"anyOf": [{"$ref": "#"}, {"type": "integer"}]});
        assert_eq!(prepared(&schema, &schema, json!("7")), json!(7));
    }

    #[test]
    fn prepares_recursive_values() {
        let schema = json!({
            "properties": {"count": {"type": "integer"}, "child": {"$ref": "#"}}
        });
        assert_eq!(
            prepared(
                &schema,
                &schema,
                json!({"count": "7", "child": {"count": "8"}})
            ),
            json!({"count": 7, "child": {"count": 8}})
        );
    }
}
