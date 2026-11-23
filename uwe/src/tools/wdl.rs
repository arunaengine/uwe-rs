//! Tool definitions of WDL tasks, from their typed wir lowering: File and Directory ports,
//! other inputs as parameters, and the container and resources of the task.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Map, Value as Json, json};
use tool_definition::edam::term_id;
use tool_definition::{
    Cardinality, Curation, Definition, Image, InputPort, OutputFormat, OutputPort, PortKind,
    Resources, SourceKind, ValueType,
};
use wir::{Kind, Type, TypeDef, resolve};

/// The definitions of the tasks of a WDL document, one per task.
pub(crate) fn definitions(path: &Path) -> Result<Vec<Definition>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let names = task_names(&text);
    if names.is_empty() {
        return Err("the WDL document has no task".to_owned());
    }
    let base = path.parent().unwrap_or(Path::new("."));
    let source = super::source(SourceKind::Wdl, path)?;
    let mut found = Vec::new();
    for name in names {
        let lowering = wdl_lowering::lower(path, Some(&name), base)?;
        found.push(definition(&lowering.document, &name, source.clone()));
    }
    Ok(found)
}

/// The names of the tasks of a document, by their `task <name> {` lines.
fn task_names(text: &str) -> Vec<String> {
    (text.lines())
        .filter_map(|line| line.trim_start().strip_prefix("task "))
        .filter_map(|rest| {
            let name = rest.split(|c: char| c.is_whitespace() || c == '{').next()?;
            (!name.is_empty()).then(|| name.to_owned())
        })
        .collect()
}

fn definition(document: &wir::Document, name: &str, source: tool_definition::Source) -> Definition {
    let defs = &document.types;
    let entry = (document.graphs.iter()).find(|graph| graph.name == document.entry);
    let (ports, results) = entry.map_or((&[][..], &[][..]), |graph| {
        (graph.inputs.as_slice(), graph.outputs.as_slice())
    });
    let mut inputs = Vec::new();
    let mut properties = Map::new();
    let mut required = Vec::new();
    // `given.<input>` flags say whether an optional input was given; they are no inputs.
    for port in ports.iter().filter(|port| !port.name.starts_with("given.")) {
        let optional = port.r#type.is_optional(defs) || port.default.is_some();
        match file_port(&port.r#type, defs) {
            Some((kind, cardinality, formats)) => inputs.push(InputPort {
                name: port.name.clone(),
                title: None,
                description: None,
                kind,
                cardinality,
                formats,
                unit: None,
                required: !optional,
                default: port.default.clone(),
                examples: Vec::new(),
                conditions: Vec::new(),
                active_when: None,
            }),
            None => {
                let mut schema = value_schema(&port.r#type, defs);
                if let Some(default) = &port.default {
                    schema["default"] = default.clone();
                }
                if !optional {
                    required.push(json!(port.name));
                }
                properties.insert(port.name.clone(), schema);
            }
        }
    }
    let outputs = (results.iter())
        .map(|port| {
            let (kind, cardinality, formats) = file_port(&port.r#type, defs).unwrap_or((
                PortKind::Value(value_type(&port.r#type, defs)),
                Cardinality::One,
                Vec::new(),
            ));
            OutputPort {
                name: port.name.clone(),
                title: None,
                description: None,
                kind,
                cardinality,
                format: OutputFormat::Terms(formats),
                guarantees: Vec::new(),
                created_when: None,
                optional: false,
                schema: Json::Null,
            }
        })
        .collect();
    let ops = document.graphs.iter().flat_map(|graph| &graph.ops);
    let task = ops
        .clone()
        .find(|op| matches!(&op.kind, Kind::Task(task) if task.image.is_some()));
    let image = task.and_then(|op| match &op.kind {
        Kind::Task(task) => task.image.clone(),
        _ => None,
    });
    let mut resources = Resources::default();
    if let Some(op) = task {
        if let Some(asked) = &op.attributes.resources {
            resources.cpus = asked.cpus;
            resources.memory = asked.memory;
            resources.gpus = asked.gpus;
        }
        resources.time = op.attributes.time;
    }
    let mut parameters = Json::Null;
    if !properties.is_empty() {
        parameters = json!({"type": "object", "properties": properties});
        if !required.is_empty() {
            parameters["required"] = Json::Array(required);
        }
    }
    let version = (image.as_deref())
        .and_then(super::tag)
        .unwrap_or_else(|| "1.0".to_owned());
    Definition {
        id: name.to_owned(),
        version,
        title: name.to_owned(),
        description: None,
        source: Some(source),
        image: image.map(|name| Image { name, digest: None }),
        license: None,
        authors: Vec::new(),
        citations: Vec::new(),
        biotools: None,
        operations: Vec::new(),
        topics: Vec::new(),
        keywords: Vec::new(),
        inputs,
        parameters,
        outputs,
        resources,
        tests: Vec::new(),
        curation: Curation::Unchecked,
    }
}

/// The port kind, cardinality and formats of a File or Directory type, or an array of them.
pub(crate) fn file_port(
    r#type: &Type,
    defs: &[TypeDef],
) -> Option<(PortKind, Cardinality, Vec<String>)> {
    match resolve(r#type, defs)? {
        Type::Optional(inner) => file_port(inner, defs),
        Type::File(file) => {
            let formats = (file.format.iter()).map(|format| term_id(format).to_owned());
            Some((PortKind::File, Cardinality::One, formats.collect()))
        }
        Type::Directory => Some((PortKind::Directory, Cardinality::One, Vec::new())),
        Type::Array(array) => match file_port(&array.items, defs)? {
            (kind, Cardinality::One, formats) => Some((kind, Cardinality::List, formats)),
            _ => None,
        },
        _ => None,
    }
}

fn value_type(r#type: &Type, defs: &[TypeDef]) -> ValueType {
    match resolve(r#type, defs) {
        Some(Type::Optional(inner)) => value_type(inner, defs),
        Some(Type::Bool) => ValueType::Boolean,
        Some(Type::Int) => ValueType::Integer,
        Some(Type::Float) => ValueType::Number,
        Some(Type::Array(_) | Type::Tuple(_)) => ValueType::Array,
        Some(Type::Record(_) | Type::Map(_)) => ValueType::Object,
        _ => ValueType::String,
    }
}

/// The JSON Schema of the values of a wir type.
pub(crate) fn value_schema(r#type: &Type, defs: &[TypeDef]) -> Json {
    match resolve(r#type, defs) {
        Some(Type::Optional(inner)) => value_schema(inner, defs),
        Some(Type::Enum(symbols)) => json!({"type": "string", "enum": symbols}),
        Some(Type::Array(array)) => {
            json!({"type": "array", "items": value_schema(&array.items, defs)})
        }
        Some(Type::Record(fields)) => {
            let properties: Map<String, Json> = (fields.iter())
                .map(|field| (field.name.clone(), value_schema(&field.r#type, defs)))
                .collect();
            json!({"type": "object", "properties": properties})
        }
        Some(Type::Map(map)) => json!({"type": "object",
            "additionalProperties": value_schema(&map.value, defs)}),
        Some(Type::Bool | Type::Int | Type::Float | Type::String) => {
            let kind = serde_json::to_value(value_type(r#type, defs)).unwrap_or_default();
            json!({"type": kind})
        }
        _ => json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_task_names() {
        let text = "version 1.1\ntask count_lines {\n  input { File f }\n}\n  task greet{\n}\n";
        assert_eq!(task_names(text), ["count_lines", "greet"]);
    }
}
