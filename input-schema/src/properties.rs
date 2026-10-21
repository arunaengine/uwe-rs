//! The inputs at the top of a schema, whether they sit in its `properties` or in groups under
//! `$defs` that `allOf` joins, as nf-schema lays out the parameters of nf-core pipelines.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;

/// An input at the top of a schema.
#[derive(Clone, Debug, PartialEq)]
pub struct Property<'a> {
    pub name: &'a str,
    pub schema: &'a Json,
    pub required: bool,
    /// The title of the group that holds the input, if any.
    pub group: Option<&'a str>,
}

/// The inputs of a schema, each once: those of the groups that `allOf` joins, in that order,
/// then those of its own `properties`.
pub fn properties(schema: &Json) -> Vec<Property<'_>> {
    let mut found = Vec::new();
    if let Some(Json::Array(parts)) = schema.get("allOf") {
        for part in parts {
            let (group, name) = resolve(schema, part);
            let title = group.get("title").and_then(Json::as_str).or(name);
            collect(group, title, &mut found);
        }
    }
    collect(schema, None, &mut found);
    found
}

/// The schema that a local `$ref` like `#/$defs/name` points to, with the last part of the
/// reference; a schema without a reference is itself.
pub(crate) fn resolve<'a>(root: &'a Json, schema: &'a Json) -> (&'a Json, Option<&'a str>) {
    let Some(reference) = schema.get("$ref").and_then(Json::as_str) else {
        return (schema, None);
    };
    let Some(pointer) = reference.strip_prefix('#') else {
        return (schema, None);
    };
    match root.pointer(pointer) {
        Some(target) => (target, pointer.rsplit('/').next()),
        None => (schema, None),
    }
}

fn collect<'a>(schema: &'a Json, group: Option<&'a str>, found: &mut Vec<Property<'a>>) {
    let Some(Json::Object(properties)) = schema.get("properties") else {
        return;
    };
    let required: Vec<&str> = match schema.get("required") {
        Some(Json::Array(names)) => names.iter().filter_map(Json::as_str).collect(),
        _ => Vec::new(),
    };
    for (name, definition) in properties {
        if found.iter().any(|known| known.name == name) {
            continue;
        }
        found.push(Property {
            name,
            schema: definition,
            required: required.contains(&name.as_str()),
            group,
        });
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn orders_group_properties() {
        let schema = json!({
            "$defs": {
                "io": {
                    "title": "Input/output options",
                    "required": ["input"],
                    "properties": {"input": {"type": "string"}, "outdir": {"type": "string"}}
                },
                "hidden": {"properties": {"help": {"type": "boolean"}}}
            },
            "allOf": [{"$ref": "#/$defs/io"}, {"$ref": "#/$defs/hidden"}],
            "properties": {"extra": {"type": "integer"}, "input": {"type": "number"}}
        });
        let found: Vec<(&str, bool, Option<&str>)> = properties(&schema)
            .iter()
            .map(|property| (property.name, property.required, property.group))
            .collect();
        assert_eq!(
            found,
            [
                ("input", true, Some("Input/output options")),
                ("outdir", false, Some("Input/output options")),
                ("help", false, Some("hidden")),
                ("extra", false, None),
            ]
        );
    }
}
