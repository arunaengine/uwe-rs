//! The JSON Schemas (draft 2020-12) of the inputs and outputs of the entry graph of a wir
//! document, from the types of its ports, for `uwe inputs`, forms and strict admission.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json, json};
use wir::{Port, Type, TypeDef, resolve};

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

/// How deep named types nest at most before a schema takes any value.
const DEPTH: usize = 32;

/// The schema of the inputs of a graph: one property per port, required unless its type is
/// optional or it has a default. Files and Directories are paths that must exist.
pub(crate) fn inputs(
    title: &str,
    ports: &[Port],
    defs: &[TypeDef],
    annotations: &BTreeMap<String, Map<String, Json>>,
) -> Json {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for port in ports {
        let mut schema = type_schema(&port.r#type, defs, true, 0);
        if let Some(default) = &port.default {
            schema["default"] = plain(default.clone());
        } else if !port.r#type.is_optional(defs) {
            required.push(Json::String(port.name.clone()));
        }
        annotated(&mut schema, annotations.get(&port.name));
        properties.insert(port.name.clone(), schema);
    }
    object(title, properties, required)
}

/// The schema of the outputs of a graph, as the paths of the files a run makes.
pub(crate) fn outputs(
    title: &str,
    ports: &[Port],
    defs: &[TypeDef],
    annotations: &BTreeMap<String, Map<String, Json>>,
) -> Json {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for port in ports {
        let mut schema = type_schema(&port.r#type, defs, false, 0);
        if !port.r#type.is_optional(defs) {
            required.push(Json::String(port.name.clone()));
        }
        annotated(&mut schema, annotations.get(&port.name));
        properties.insert(port.name.clone(), schema);
    }
    object(title, properties, required)
}

/// A wir value in the form of the schema: each File and Directory as its path, or as its
/// address when it is not local.
pub(crate) fn plain(value: Json) -> Json {
    match value {
        Json::Object(object) if matches!(class(&object), Some("File" | "Directory")) => {
            match object.get("location").or_else(|| object.get("path")) {
                Some(Json::String(location)) => Json::String(
                    location
                        .strip_prefix("file://")
                        .unwrap_or(location)
                        .to_owned(),
                ),
                _ => Json::Object(object),
            }
        }
        Json::Object(object) => {
            Json::Object(object.into_iter().map(|(k, v)| (k, plain(v))).collect())
        }
        Json::Array(items) => Json::Array(items.into_iter().map(plain).collect()),
        other => other,
    }
}

fn class(object: &Map<String, Json>) -> Option<&str> {
    object.get("class").and_then(Json::as_str)
}

fn annotated(schema: &mut Json, keywords: Option<&Map<String, Json>>) {
    if let (Some(keywords), Json::Object(fields)) = (keywords, schema) {
        fields.extend(keywords.clone());
    }
}

fn object(title: &str, properties: Map<String, Json>, required: Vec<Json>) -> Json {
    let mut schema = json!({"$schema": DRAFT, "title": title, "type": "object",
        "properties": properties, "additionalProperties": false});
    if !required.is_empty() {
        schema["required"] = Json::Array(required);
    }
    schema
}

/// The schema of a type; `input` asks files to exist.
fn type_schema(r#type: &Type, defs: &[TypeDef], input: bool, depth: usize) -> Json {
    if depth > DEPTH {
        return json!({});
    }
    let inner = |r#type: &Type| type_schema(r#type, defs, input, depth + 1);
    let Some(resolved) = resolve(r#type, defs) else {
        return json!({});
    };
    match resolved {
        Type::Bool => json!({"type": "boolean"}),
        Type::Int => json!({"type": "integer"}),
        Type::Float => json!({"type": "number"}),
        Type::String => json!({"type": "string"}),
        Type::File(file) => {
            let mut schema = json!({"type": "string", "format": "file-path"});
            if input {
                schema["exists"] = Json::Bool(true);
            }
            if let Some(format) = &file.format {
                schema["fileFormat"] = Json::String(format.clone());
            }
            schema
        }
        Type::Directory if input => {
            json!({"type": "string", "format": "directory-path", "exists": true})
        }
        Type::Directory => json!({"type": "string", "format": "directory-path"}),
        Type::Record(fields) => {
            let mut properties = Map::new();
            let mut required = Vec::new();
            for field in fields {
                if !field.r#type.is_optional(defs) {
                    required.push(Json::String(field.name.clone()));
                }
                properties.insert(field.name.clone(), inner(&field.r#type));
            }
            json!({"type": "object", "properties": properties, "required": required,
                "additionalProperties": false})
        }
        Type::Array(array) => {
            let mut schema = json!({"type": "array", "items": inner(&array.items)});
            if array.nonempty {
                schema["minItems"] = json!(1);
            }
            schema
        }
        Type::Map(map) => json!({"type": "object", "additionalProperties": inner(&map.value)}),
        Type::Optional(item) => json!({"anyOf": [inner(item), {"type": "null"}]}),
        Type::Union(members) => json!({"anyOf": members.iter().map(inner).collect::<Vec<_>>()}),
        Type::Enum(symbols) => json!({"type": "string", "enum": symbols}),
        Type::Tuple(items) => json!({"type": "array",
            "prefixItems": items.iter().map(inner).collect::<Vec<_>>(),
            "minItems": items.len(), "maxItems": items.len()}),
        Type::Named(_) | Type::Dynamic => json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_ports() {
        let ports: Vec<Port> = serde_json::from_value(json!([
            {"name": "reads", "type": {"array": {"items": {"file": {"format": "edam:format_1930"}}}}},
            {"name": "lines", "type": "int", "default": 2},
            {"name": "label", "type": {"optional": "string"}},
            {"name": "mode", "type": {"enum": ["fast", "exact"]}}
        ]))
        .expect("ports");
        let keywords = Map::from_iter([("description".to_owned(), json!("Lines to keep"))]);
        let annotations = BTreeMap::from([("lines".to_owned(), keywords)]);
        let schema = inputs("main", &ports, &[], &annotations);
        assert_eq!(schema["required"], json!(["reads", "mode"]));
        let reads = &schema["properties"]["reads"]["items"];
        assert_eq!(
            (reads["format"].as_str(), reads["exists"].as_bool()),
            (Some("file-path"), Some(true))
        );
        assert_eq!(schema["properties"]["lines"]["default"], 2);
        assert_eq!(
            schema["properties"]["lines"]["description"],
            "Lines to keep"
        );
        assert_eq!(
            schema["properties"]["mode"]["enum"],
            json!(["fast", "exact"])
        );
        let given = workflow_graph::Inputs {
            schema: schema.clone(),
            values: json!({"reads": ["a.fq"], "mode": "fast"}),
            ..Default::default()
        };
        let validation = input_schema::validate_json(&given, true);
        let errors = (validation.diagnostics.iter())
            .filter(|found| found.severity == input_schema::Severity::Error);
        assert_eq!(errors.count(), 0, "{:?}", validation.diagnostics);
        let file = json!({"class": "File", "location": "file:///data/a.fq"});
        assert_eq!(plain(json!([file])), json!(["/data/a.fq"]));
    }

    #[test]
    fn closes_records() {
        let ports: Vec<Port> =
            serde_json::from_value(json!([{"name": "sample", "type": {"record": [
            {"name": "reads", "type": {"file": {}}},
            {"name": "pair", "type": {"tuple": [{"file": {}}, "directory"]}},
            {"name": "more", "type": {"map": {"key": "string", "value": {"file": {}}}}}]}}]))
            .expect("ports");
        let schema = inputs("main", &ports, &[], &BTreeMap::new());
        let sample = &schema["properties"]["sample"];
        assert_eq!(sample["additionalProperties"], false);
        let mut value = json!({"sample": {"reads": "a.fq", "pair": ["b.fq", "c"],
            "more": {"x": "d.fq"}}});
        crate::inputs::named(&mut value, &ports, &[]);
        let file = |path: &str| json!({"class": "File", "path": path});
        assert_eq!(value["sample"]["reads"], file("a.fq"));
        assert_eq!(
            value["sample"]["pair"],
            json!([file("b.fq"), {"class": "Directory", "path": "c"}])
        );
        assert_eq!(value["sample"]["more"]["x"], file("d.fq"));
    }
}
