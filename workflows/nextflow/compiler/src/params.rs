//! The entry ports of a compiled pipeline: one port per param, typed by `nextflow_schema.json`
//! where it describes the param, with the value of the run as its default.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Map, Value as Json, json};

/// The inputs of the entry graph for the params of a run. A param whose name is no port name
/// goes into the one port `params`, with the others of its kind.
pub(crate) fn ports(params: &Json, schema: Option<&Json>) -> Vec<Json> {
    let properties = schema.map(described).unwrap_or_default();
    let mut ports = Vec::new();
    let mut rest = Map::new();
    let Json::Object(params) = params else {
        return ports;
    };
    for (name, value) in params {
        if !port_name(name) || name == "params" {
            rest.insert(name.clone(), value.clone());
            continue;
        }
        let r#type = properties.get(name).map_or(json!("dynamic"), kind);
        let (r#type, value) = fitting(r#type, value);
        ports.push(json!({"name": name, "type": r#type, "default": value}));
    }
    if !rest.is_empty() {
        ports.push(json!({"name": "params", "type": "dynamic", "default": rest}));
    }
    ports
}

/// The properties that a schema describes, from its own `properties` and those of the groups
/// in `$defs` or `definitions`.
fn described(schema: &Json) -> Map<String, Json> {
    let mut properties = Map::new();
    let groups = ["$defs", "definitions"]
        .into_iter()
        .filter_map(|key| schema.get(key));
    let groups = groups
        .filter_map(Json::as_object)
        .flat_map(|groups| groups.values());
    for holder in std::iter::once(schema).chain(groups) {
        if let Some(Json::Object(own)) = holder.get("properties") {
            properties.extend(
                own.iter()
                    .map(|(name, value)| (name.clone(), value.clone())),
            );
        }
    }
    properties
}

/// The wir type of a schema property. A path param takes a File or Directory, or its text, as
/// a direct run of the pipeline does, which reads the text from the launch folder.
fn kind(property: &Json) -> Json {
    let format = property.get("format").and_then(Json::as_str);
    match (property.get("type").and_then(Json::as_str), format) {
        (_, Some("file-path")) => json!({"union": [{"file": {}}, "string"]}),
        (_, Some("directory-path")) => json!({"union": ["directory", "string"]}),
        (Some("string"), _) => match property.get("enum").and_then(Json::as_array) {
            Some(symbols) if symbols.iter().all(Json::is_string) => json!({"enum": symbols}),
            _ => json!("string"),
        },
        (Some("integer"), _) => json!("int"),
        (Some("number"), _) => json!("float"),
        (Some("boolean"), _) => json!("bool"),
        _ => json!("dynamic"),
    }
}

/// The type and default of a port for the value of a param: the value as the run has it, a
/// null value as optional, and a value that does not fit the type as `dynamic`.
fn fitting(r#type: Json, value: &Json) -> (Json, Json) {
    let value = value.clone();
    let parsed: Option<wir::Type> = serde_json::from_value(r#type.clone()).ok();
    match parsed {
        _ if value.is_null() => (json!({"optional": r#type}), value),
        Some(parsed) if wir::fits(&value, &parsed, &[]) => (r#type, value),
        _ => (json!("dynamic"), value),
    }
}

/// Whether a param name is a port name: letters, digits, `_`, `-` and `.`.
fn port_name(name: &str) -> bool {
    let allowed =
        |letter: char| letter.is_ascii_alphanumeric() || matches!(letter, '_' | '-' | '.');
    !name.is_empty() && name.chars().all(allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_schema_params() {
        let schema = json!({
            "$defs": {"input_output_options": {"properties": {
                "input": {"type": "string", "format": "file-path"},
                "outdir": {"type": "string", "format": "directory-path"},
                "mode": {"type": "string", "enum": ["fast", "slow"]},
                "max_cpus": {"type": "integer"},
                "skip": {"type": "boolean"}
            }}},
            "properties": {"email": {"type": "string"}}
        });
        let params = json!({
            "input": "sheet.csv", "outdir": "s3://bucket/out", "mode": "other", "max_cpus": 4,
            "skip": false, "email": null, "genomes": {"GRCh38": {}}, "with space": 1
        });
        let ports = ports(&params, Some(&schema));
        let by = |name: &str| {
            ports
                .iter()
                .find(|port| port["name"] == name)
                .unwrap()
                .clone()
        };
        let path = json!({"union": [{"file": {}}, "string"]});
        assert_eq!(
            by("input")["type"],
            path,
            "a File or the text of a direct run"
        );
        assert_eq!(
            by("input")["default"],
            "sheet.csv",
            "the text as the run has it"
        );
        assert_eq!(by("outdir")["default"], "s3://bucket/out");
        assert_eq!(by("mode")["type"], "dynamic", "a value outside the enum");
        assert_eq!(
            (by("max_cpus")["type"].clone(), by("skip")["type"].clone()),
            (json!("int"), json!("bool"))
        );
        assert_eq!(by("email")["type"], json!({"optional": "string"}));
        assert_eq!(by("genomes")["type"], "dynamic", "no schema property");
        assert_eq!(by("params")["default"], json!({"with space": 1}));
    }
}
