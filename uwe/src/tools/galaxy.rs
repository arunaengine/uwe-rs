//! Galaxy tool XML and workflows: their tool definitions, with the EDAM formats of common
//! Galaxy datatypes, and the wir document that runs one as a native Galaxy op.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::Value as Json;
use tool_definition::{Definition, SourceKind};
use wir::{ArrayType, Document, Edge, Endpoint, Field, FileType, Graph, Kind, Op, Port, Type};
use workflow_graph::{Driver, Launch};

use super::lowering::name;

/// The definition of the Galaxy tool in the file, with its macro files beside it.
pub(crate) fn definitions(path: &Path) -> Result<Vec<Definition>, String> {
    let tool = galaxy_parser::tool::load(path).map_err(|error| error.to_string())?;
    let source = super::source(SourceKind::Galaxy, path)?;
    let formats = tool_definition::formats::datatypes();
    let definition = galaxy_parser::definition::convert(&tool, &formats, Some(source));
    Ok(vec![definition])
}

/// The wir document of a Galaxy tool or workflow: one native Galaxy op whose ports are the
/// inputs and outputs of its schemas. The source location is relative to `base` below it.
pub fn document(target: &Path, base: &Path) -> Result<Document, String> {
    let launch = Launch {
        target: target.to_owned(),
        ..Launch::default()
    };
    let mut driver = galaxy_runtime::GalaxyDriver::new(&launch, Default::default())
        .map_err(|error| error.message)?;
    let inputs = driver.inputs().map_err(|error| error.message)?;
    let outputs = driver.outputs().map_err(|error| error.message)?;
    let inputs = ports(&inputs.map(|inputs| inputs.schema).unwrap_or_default());
    let outputs = ports(&outputs.unwrap_or_default());
    let title = (target.file_stem()).map_or("galaxy".to_owned(), |stem| {
        stem.to_string_lossy().into_owned()
    });
    let path = name(title.trim_end_matches(".tool"));
    let bytes = std::fs::read(target).map_err(|error| error.to_string())?;
    let absolute = std::path::absolute(target).unwrap_or_else(|_| target.to_owned());
    let base = std::path::absolute(base).unwrap_or_else(|_| base.to_owned());
    let location = match absolute.strip_prefix(&base) {
        Ok(inside) => inside.to_string_lossy().into_owned(),
        Err(_) => absolute.to_string_lossy().into_owned(),
    };
    let source = wir::Source {
        id: "galaxy".to_owned(),
        language: wir::Language::Galaxy,
        location,
        version: None,
        digest: Some(format!(
            "sha256:{}",
            task_model::Digest::sha256(&bytes).hex()
        )),
    };
    let op = Op {
        path: path.clone(),
        kind: Kind::Native(wir::Native {
            source: source.id.clone(),
            language: wir::Language::Galaxy,
            entry: None,
            profiles: Vec::new(),
            configs: Vec::new(),
            targets: Vec::new(),
            config_values: None,
            until: Vec::new(),
            omit_from: Vec::new(),
            forcerun: Vec::new(),
            forceall: false,
        }),
        inputs: inputs.clone(),
        outputs: outputs.clone(),
        after: Vec::new(),
        depends: None,
        handles: Vec::new(),
        effects: Vec::new(),
        attributes: Default::default(),
        origin: Some(wir::Origin {
            source: source.id.clone(),
            at: None,
        }),
        values: Default::default(),
    };
    let wired = |from: Option<&str>, to: Option<&str>, port: &str| Edge {
        from: Endpoint {
            op: from.map(str::to_owned),
            port: port.to_owned(),
        },
        to: Endpoint {
            op: to.map(str::to_owned),
            port: port.to_owned(),
        },
        read: Default::default(),
    };
    let mut edges: Vec<Edge> = (inputs.iter())
        .map(|port| wired(None, Some(&path), &port.name))
        .collect();
    edges.extend((outputs.iter()).map(|port| wired(Some(&path), None, &port.name)));
    let document = Document {
        wir_version: wir::VERSION.to_owned(),
        sources: vec![source],
        types: Vec::new(),
        graphs: vec![Graph {
            name: "main".to_owned(),
            inputs,
            outputs,
            ops: vec![op],
            edges,
        }],
        entry: "main".to_owned(),
        hooks: Vec::new(),
        formats: Default::default(),
    };
    wir::verify(&document).map_err(|problems| {
        let problems: Vec<String> = problems.iter().map(ToString::to_string).collect();
        problems.join("; ")
    })?;
    Ok(document)
}

/// The ports of the properties of an object schema: optional unless required.
fn ports(schema: &Json) -> Vec<Port> {
    let required: Vec<&str> = (schema["required"].as_array().into_iter().flatten())
        .filter_map(Json::as_str)
        .collect();
    let properties = schema["properties"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    (properties.iter())
        .filter_map(|(key, property)| {
            let r#type = schema_type(property)?;
            let needed = required.contains(&key.as_str()) && property.get("default").is_none();
            Some(Port {
                name: name(key),
                r#type: match needed {
                    true => r#type,
                    false => Type::Optional(Box::new(r#type)),
                },
                flow: Default::default(),
                default: property.get("default").cloned().filter(|_| !needed),
            })
        })
        .collect()
}

/// The wir type of the values that a schema takes; none for schemas without a type.
fn schema_type(schema: &Json) -> Option<Type> {
    let kinds: Vec<&str> = match &schema["type"] {
        Json::String(kind) => vec![kind],
        Json::Array(kinds) => kinds.iter().filter_map(Json::as_str).collect(),
        _ => Vec::new(),
    };
    let nullable = kinds.contains(&"null");
    let kind = kinds.iter().find(|kind| **kind != "null").copied();
    let found = match (kind, schema["format"].as_str()) {
        (Some("string"), Some("file-path")) => Type::File(FileType::default()),
        (Some("string"), Some("directory-path")) => Type::Directory,
        (Some("string"), _) => Type::String,
        (Some("integer"), _) => Type::Int,
        (Some("number"), _) => Type::Float,
        (Some("boolean"), _) => Type::Bool,
        (Some("array"), _) => Type::Array(ArrayType {
            items: Box::new(schema_type(&schema["items"]).unwrap_or(Type::String)),
            nonempty: false,
        }),
        (Some("object"), _) => {
            // The params of the cases of a conditional are in its `then` schemas.
            let cases = (schema["allOf"].as_array().into_iter().flatten())
                .filter_map(|case| case["then"]["properties"].as_object());
            let mut fields: Vec<Field> = Vec::new();
            for properties in schema["properties"].as_object().into_iter().chain(cases) {
                for (key, property) in properties {
                    let known = fields.iter().any(|field| field.name == *key);
                    if let (false, Some(inner)) = (known, schema_type(property)) {
                        fields.push(Field {
                            name: key.clone(),
                            r#type: Type::Optional(Box::new(inner)),
                        });
                    }
                }
            }
            Type::Record(fields)
        }
        _ => {
            let symbols = schema["enum"].as_array()?;
            let texts: Vec<String> = symbols
                .iter()
                .filter_map(Json::as_str)
                .map(str::to_owned)
                .collect();
            let optional = symbols.iter().any(Json::is_null);
            let symbols = Type::Enum(texts);
            return Some(match optional {
                true => Type::Optional(Box::new(symbols)),
                false => symbols,
            });
        }
    };
    Some(match nullable {
        true => Type::Optional(Box::new(found)),
        false => found,
    })
}
