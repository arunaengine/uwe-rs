//! The data entities of the RO-Crate of nf-prov: the params with their values and files, the
//! workflow inputs, and the workflow outputs with the published files.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use super::wrroc::{Builder, reference};
use super::{file_name, media_type, normalise};
use crate::report_format::Json;

const FORMAL: &str = "https://bioschemas.org/profiles/FormalParameter/1.0-RELEASE";

impl Builder<'_> {
    /// The input FormalParameters, the PropertyValues of their values, and the entities of
    /// the param files, by param name.
    pub(super) fn params(&mut self) -> std::io::Result<(Vec<Json>, Vec<Json>, Vec<ParamFile>)> {
        let start = self.run.start;
        let schema = schema_properties(Path::new(&start.script));
        let (mut formals, mut values, mut files) = (Vec::new(), Vec::new(), Vec::new());
        let Some(params) = start.params.as_object() else {
            return Ok((formals, values, files));
        };
        for (name, value) in params.iter().filter(|(_, value)| !value.is_null()) {
            let property = schema.get(name);
            let kind = kind_of(property, value);
            if kind.is_none() {
                tracing::warn!(
                    "Could not determine type of parameter `{name}` for Workflow Run RO-Crate -- \
                     the resulting crate will be invalid"
                );
            }
            let path = value.as_str().filter(|path| !path.is_empty());
            let format = path.filter(|_| kind == Some("File")).and_then(media_type);
            formals.push(Json::object([
                ("@id", Json::from(format!("#param/{name}"))),
                ("@type", "FormalParameter".into()),
                ("additionalType", kind.into()),
                ("conformsTo", reference(FORMAL)),
                ("encodingFormat", format.into()),
                ("name", name.as_str().into()),
                (
                    "defaultValue",
                    property
                        .map(|property| Json::from(&property["default"]))
                        .into(),
                ),
                (
                    "description",
                    property
                        .map(|property| Json::from(&property["description"]))
                        .into(),
                ),
            ]));
            let globbed = path.is_some_and(nextflow_evaluator::interpreter::glob::is_glob);
            let file = path.filter(|_| matches!(kind, Some("File" | "Dataset")) && !globbed);
            if let Some(path) = file {
                let local = path.starts_with('/');
                if local && Path::new(path).is_file() {
                    let relative = format!("{name}/{}", file_name(path));
                    self.copy(Path::new(path), &relative)?;
                    self.ids.insert(path.to_owned(), relative.clone());
                    files.push(ParamFile {
                        name: name.clone(),
                        path: path.to_owned(),
                        id: relative,
                    });
                    continue;
                }
                if !local || Path::new(path).is_dir() {
                    let directory = !local && kind == Some("Dataset") || Path::new(path).is_dir();
                    let mut id = normalise(path);
                    if directory && !id.ends_with('/') {
                        id.push('/');
                    }
                    self.ids.insert(path.to_owned(), id.clone());
                    files.push(ParamFile {
                        name: name.clone(),
                        path: path.to_owned(),
                        id,
                    });
                }
            }
            let text = match value {
                Value::String(text) => Json::from(normalise(text)),
                Value::Bool(_) | Value::Number(_) => Json::from(value),
                other => Json::from(other.to_string()),
            };
            values.push(Json::object([
                ("@id", Json::from(format!("#param/{name}/value"))),
                ("@type", "PropertyValue".into()),
                ("exampleOfWork", reference(&format!("#param/{name}"))),
                ("name", name.as_str().into()),
                ("value", text),
            ]));
        }
        Ok((formals, values, files))
    }

    /// The entities of the workflow inputs: the files of `collectFile` apart, and the param
    /// files that no task read.
    pub(super) fn inputs(&mut self, params: &[ParamFile]) -> Inputs {
        let start = self.run.start;
        let temporary_folder = format!("{}/tmp/", start.work_dir.trim_end_matches('/'));
        let project = format!("{}/", start.project_dir.trim_end_matches('/'));
        let mut inputs = Inputs::default();
        let paths = self.run.inputs();
        for path in &paths {
            if path.starts_with(&temporary_folder) {
                let id = format!("#tmp/{}", file_name(path));
                self.ids.insert(path.clone(), id.clone());
                inputs.temporary.push(Json::object([
                    ("@id", Json::from(id)),
                    ("@type", "CreativeWork".into()),
                    ("name", file_name(path).into()),
                    ("encodingFormat", media_type(path).into()),
                ]));
                continue;
            }
            let id = match self.ids.get(path) {
                Some(id) => id.clone(),
                None => match path.strip_prefix(&project) {
                    Some(relative) if start.repository.is_none() => {
                        if self.copy(Path::new(path), relative).is_err() {
                            tracing::debug!(path, "an input was not copied into the crate");
                        }
                        relative.to_owned()
                    }
                    _ => normalise(path),
                },
            };
            self.ids.insert(path.clone(), id.clone());
            inputs
                .entities
                .push(input_entity(&id, path, params, &paths, &self.ids, None));
        }
        for param in params {
            if !paths.contains(&param.path) {
                let description = match Path::new(&param.path).is_dir() {
                    true => format!("Input directory specified by params.{}", param.name),
                    false => format!("Input file specified by params.{}", param.name),
                };
                let entity = input_entity(
                    &param.id,
                    &param.path,
                    params,
                    &paths,
                    &self.ids,
                    Some(description),
                );
                inputs.entities.push(entity);
            }
        }
        inputs
    }

    /// The output FormalParameters, the PropertyValues of outputs without files in the crate,
    /// and the entities of the published files inside the crate.
    pub(super) fn outputs(&self) -> (Vec<Json>, Vec<Json>, Vec<Json>) {
        let (mut formals, mut values, mut files) = (Vec::new(), Vec::new(), Vec::new());
        let mut added = Vec::new();
        for (target, _) in self.run.published {
            match self.relative(target) {
                Some(id) => {
                    files.push(file_entity(&id, target, None));
                    added.push(target.clone());
                }
                None => tracing::warn!(
                    "Excluding workflow output {target} because it is outside of the RO-Crate \
                     directory -- make sure that the workflow output directory and RO-Crate \
                     directory are the same"
                ),
            }
        }
        for (name, paths) in &self.run.outputs {
            let first = paths.first().map(String::as_str).unwrap_or_default();
            let kind = match Path::new(first) {
                path if path.is_dir() => Some("Dataset"),
                path if path.is_file() => Some("File"),
                _ => None,
            };
            if kind.is_none() {
                tracing::warn!(
                    "Could not determine type of workflow output `{name}` for Workflow Run \
                     RO-Crate -- the resulting crate will be invalid"
                );
            }
            let single = (paths.len() == 1).then(|| media_type(first)).flatten();
            formals.push(Json::object([
                ("@id", Json::from(format!("#output/{name}"))),
                ("@type", "FormalParameter".into()),
                ("additionalType", kind.into()),
                ("conformsTo", reference(FORMAL)),
                ("encodingFormat", single.into()),
                ("name", name.as_str().into()),
            ]));
            let mut inside = false;
            for path in paths {
                if let Some(id) = self.relative(path) {
                    inside = true;
                    if !added.contains(path) {
                        files.push(file_entity(&id, path, Some(name)));
                        added.push(path.clone());
                    }
                }
            }
            if !inside {
                let relative: Vec<Json> = (paths.iter())
                    .filter_map(
                        |path| match Path::new(path).strip_prefix(self.run.output_dir) {
                            Ok(relative) => {
                                Some(Json::from(relative.to_string_lossy().into_owned()))
                            }
                            Err(_) => {
                                tracing::warn!(
                                    "Cannot get relative path for workflow output '{path}'"
                                );
                                None
                            }
                        },
                    )
                    .collect();
                values.push(Json::object([
                    ("@id", Json::from(format!("#output/{name}/value"))),
                    ("@type", "PropertyValue".into()),
                    ("exampleOfWork", reference(&format!("#output/{name}"))),
                    ("name", name.as_str().into()),
                    ("value", Json::List(relative)),
                ]));
            }
        }
        (formals, values, files)
    }
}

/// A local file param, with the entity id it has in the crate.
pub(super) struct ParamFile {
    pub(super) name: String,
    pub(super) path: String,
    pub(super) id: String,
}

#[derive(Default)]
pub(super) struct Inputs {
    pub(super) entities: Vec<Json>,
    /// The files of `collectFile` in the work directory.
    pub(super) temporary: Vec<Json>,
}

/// The entity of an input file, with the params that name it, and for a folder param the
/// inputs inside it.
fn input_entity(
    id: &str,
    path: &str,
    params: &[ParamFile],
    inputs: &[String],
    ids: &HashMap<String, String>,
    description: Option<String>,
) -> Json {
    let directory = Path::new(path).is_dir() || id.ends_with('/');
    let examples: Vec<Json> = (params.iter())
        .filter(|param| param.path == path)
        .map(|param| reference(&format!("#param/{}", param.name)))
        .collect();
    let mut parts: Vec<String> = Vec::new();
    if directory {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        parts = (inputs.iter())
            .filter(|input| input.starts_with(&prefix))
            .map(|input| ids.get(input).cloned().unwrap_or_else(|| normalise(input)))
            .collect();
        parts.sort();
    }
    Json::object([
        ("@id", Json::from(id)),
        (
            "@type",
            Json::from(if directory { "Dataset" } else { "File" }),
        ),
        ("name", file_name(path).into()),
        ("description", description.into()),
        ("encodingFormat", media_type(path).into()),
        (
            "exampleOfWork",
            (!examples.is_empty())
                .then_some(Json::List(examples))
                .into(),
        ),
        (
            "hasPart",
            (!parts.is_empty())
                .then(|| Json::list(parts.iter().map(|part| reference(part))))
                .into(),
        ),
    ])
}

fn file_entity(id: &str, path: &str, output: Option<&str>) -> Json {
    let directory = Path::new(path).is_dir();
    Json::object([
        ("@id", Json::from(id)),
        (
            "@type",
            Json::from(if directory { "Dataset" } else { "File" }),
        ),
        ("name", file_name(path).into()),
        ("encodingFormat", media_type(path).into()),
        (
            "exampleOfWork",
            output
                .map(|name| reference(&format!("#output/{name}")))
                .into(),
        ),
    ])
}

/// The type of a param: from its schema, else from its value.
fn kind_of(property: Option<&Value>, value: &Value) -> Option<&'static str> {
    if let Some(kind) = property.and_then(|property| property["type"].as_str()) {
        return match (
            kind,
            property.and_then(|property| property["format"].as_str()),
        ) {
            ("boolean", _) => Some("Boolean"),
            ("integer" | "number", _) => Some("Number"),
            ("string", Some("file-path")) => Some("File"),
            ("string", Some("directory-path")) => Some("Dataset"),
            ("string", _) => Some("Text"),
            _ => None,
        };
    }
    match value {
        Value::Bool(_) => Some("Boolean"),
        Value::Number(_) => Some("Number"),
        Value::String(_) | Value::Array(_) | Value::Object(_) => Some("Text"),
        Value::Null => None,
    }
}

/// The properties of the parameter schema next to the main script, by name: those of the
/// root and of each group in `$defs`, `defs` or `definitions`.
fn schema_properties(script: &Path) -> serde_json::Map<String, Value> {
    let path = script.with_file_name("nextflow_schema.json");
    let schema: Value = (std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let mut properties = serde_json::Map::new();
    let mut add = |group: &Value| {
        for (name, property) in group["properties"].as_object().into_iter().flatten() {
            properties.insert(name.clone(), property.clone());
        }
    };
    add(&schema);
    for key in ["$defs", "defs", "definitions"] {
        for group in schema[key].as_object().into_iter().flatten() {
            add(group.1);
        }
    }
    properties
}
