//! Brings a schema of an older draft, like the draft 7 of nf-validation or the drafts 4 to 7 of
//! Snakemake workflows, to draft 2020-12: `definitions` become `$defs` with their references,
//! `dependencies` become `dependentRequired` and `dependentSchemas`, a list of `items` becomes
//! `prefixItems`, and the boolean `exclusiveMinimum` and `exclusiveMaximum` of draft 4 and the
//! boolean `required` of draft 3 take the forms of draft 2020-12.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Map, Value as Json};

use crate::DRAFT;

/// The schema in draft 2020-12, with `$schema` naming that draft.
pub fn upgrade(schema: Json) -> Json {
    let mut schema = schema;
    upgrade_schema(&mut schema);
    if let Json::Object(map) = &mut schema {
        map.insert("$schema".into(), Json::String(DRAFT.into()));
    }
    schema
}

/// The keywords whose value is a map of schemas.
const SCHEMA_MAPS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "dependentSchemas",
];

/// The keywords whose value is a list of schemas.
const SCHEMA_LISTS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];

/// The keywords whose value is one schema.
const SCHEMAS: &[&str] = &[
    "items",
    "additionalProperties",
    "not",
    "if",
    "then",
    "else",
    "contains",
    "propertyNames",
    "unevaluatedItems",
    "unevaluatedProperties",
];

fn upgrade_schema(schema: &mut Json) {
    let Json::Object(map) = schema else {
        return;
    };
    // An embedded schema follows the draft of the whole.
    map.remove("$schema");
    if let Some(Json::Object(definitions)) = map.remove("definitions") {
        match map.get_mut("$defs") {
            Some(Json::Object(defs)) => {
                for (name, definition) in definitions {
                    defs.entry(name).or_insert(definition);
                }
            }
            _ => {
                map.insert("$defs".into(), Json::Object(definitions));
            }
        }
    }
    if let Some(Json::String(reference)) = map.get_mut("$ref") {
        *reference = reference.replace("#/definitions/", "#/$defs/");
    }
    for (exclusive, bound) in [
        ("exclusiveMinimum", "minimum"),
        ("exclusiveMaximum", "maximum"),
    ] {
        if let Some(Json::Bool(flag)) = map.get(exclusive) {
            let flag = *flag;
            map.remove(exclusive);
            if flag && let Some(limit) = map.remove(bound) {
                map.insert(exclusive.into(), limit);
            }
        }
    }
    if matches!(map.get("items"), Some(Json::Array(_))) {
        if let Some(items) = map.remove("items") {
            map.insert("prefixItems".into(), items);
        }
        if let Some(additional) = map.remove("additionalItems") {
            map.insert("items".into(), additional);
        }
    } else {
        map.remove("additionalItems");
    }
    if matches!(map.get("dependencies"), Some(Json::Object(_)))
        && let Some(Json::Object(dependencies)) = map.remove("dependencies")
    {
        let mut required = Map::new();
        let mut schemas = Map::new();
        for (name, dependency) in dependencies {
            match dependency {
                Json::Array(_) => required.insert(name, dependency),
                other => schemas.insert(name, other),
            };
        }
        if !required.is_empty() {
            map.insert("dependentRequired".into(), Json::Object(required));
        }
        if !schemas.is_empty() {
            map.insert("dependentSchemas".into(), Json::Object(schemas));
        }
    }
    required_properties(map);
    for keyword in SCHEMA_MAPS {
        if let Some(Json::Object(schemas)) = map.get_mut(*keyword) {
            schemas.values_mut().for_each(upgrade_schema);
        }
    }
    for keyword in SCHEMA_LISTS {
        if let Some(Json::Array(schemas)) = map.get_mut(*keyword) {
            schemas.iter_mut().for_each(upgrade_schema);
        }
    }
    for keyword in SCHEMAS {
        if let Some(schema) = map.get_mut(*keyword) {
            upgrade_schema(schema);
        }
    }
}

/// Draft 3 marks a required property with `required: true` inside it; later drafts list the
/// required properties in the object.
fn required_properties(map: &mut Map<String, Json>) {
    if matches!(map.get("required"), Some(Json::Bool(_))) {
        map.remove("required");
    }
    let Some(Json::Object(properties)) = map.get_mut("properties") else {
        return;
    };
    let mut names = Vec::new();
    for (name, property) in properties.iter_mut() {
        if let Json::Object(property) = property
            && let Some(Json::Bool(required)) = property.get("required")
        {
            if *required {
                names.push(Json::String(name.clone()));
            }
            property.remove("required");
        }
    }
    if names.is_empty() {
        return;
    }
    match map.get_mut("required") {
        Some(Json::Array(required)) => required.extend(names),
        _ => {
            map.insert("required".into(), Json::Array(names));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn draft_7_of_nf_validation() {
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema",
            "definitions": {"io": {"properties": {"input": {"type": "string"}}}},
            "allOf": [{"$ref": "#/definitions/io"}]
        });
        assert_eq!(
            upgrade(schema),
            json!({
                "$schema": DRAFT,
                "$defs": {"io": {"properties": {"input": {"type": "string"}}}},
                "allOf": [{"$ref": "#/$defs/io"}]
            })
        );
    }

    #[test]
    fn draft_4_and_3_keywords() {
        let schema = json!({
            "$schema": "http://json-schema.org/draft-04/schema#",
            "properties": {
                "ratio": {"type": "number", "minimum": 0, "exclusiveMinimum": true},
                "pair": {"type": "array", "items": [{"type": "string"}], "additionalItems": false},
                "name": {"type": "string", "required": true}
            },
            "dependencies": {"a": ["b"], "c": {"required": ["d"]}}
        });
        let upgraded = upgrade(schema);
        assert_eq!(
            upgraded,
            json!({
                "$schema": DRAFT,
                "properties": {
                    "ratio": {"type": "number", "exclusiveMinimum": 0},
                    "pair": {"type": "array", "prefixItems": [{"type": "string"}], "items": false},
                    "name": {"type": "string"}
                },
                "required": ["name"],
                "dependentRequired": {"a": ["b"]},
                "dependentSchemas": {"c": {"required": ["d"]}}
            })
        );
        assert_eq!(crate::valid(&upgraded), Ok(()));
    }
}
