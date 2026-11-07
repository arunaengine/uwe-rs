//! Nextflow native blocks: the Nextflow driver of uwe as a child in a launch folder of its
//! own, its params file, and its emits and published files as op outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::driver::{Nextflow, Settings};
use serde_json::{Map, Value as Json, json};
use wir::{Port, Type, TypeDef};
use workflow_graph::{DriverError, Launch};

use crate::child::Block;

/// Makes the settings of the driver of a block from its launch and its launch folder.
type Making = dyn Fn(&Launch, &Path) -> Settings;

/// How a run makes the settings of the Nextflow driver of each block, from the launch of the
/// block and its launch folder. The host of the settings must start in that folder.
#[derive(Clone)]
pub struct Options {
    settings: Arc<Making>,
}

impl Options {
    pub fn new(settings: impl Fn(&Launch, &Path) -> Settings + 'static) -> Options {
        Options {
            settings: Arc::new(settings),
        }
    }
}

impl Default for Options {
    /// A host that prints to standard output and reads local files.
    fn default() -> Self {
        Options::new(|_, folder| Settings {
            host: Box::new(Console(folder.to_string_lossy().into_owned())),
            ..Settings::default()
        })
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Options { .. }")
    }
}

/// A host in a launch folder that prints to standard output.
struct Console(String);

impl Host for Console {
    fn print(&mut self, text: &str) {
        print!("{text}");
    }

    fn launch_dir(&self) -> String {
        self.0.clone()
    }
}

/// A Nextflow driver for the launch of one block, launched in `folder`.
pub(crate) fn block(
    launch: &Launch,
    folder: &Path,
    options: &Options,
) -> Result<Box<dyn Block>, DriverError> {
    let settings = (options.settings)(launch, folder);
    let started = PathBuf::from(settings.host.launch_dir());
    if started != folder {
        let message = format!(
            "the Nextflow host starts in {}, not in the launch folder {} of the block",
            started.display(),
            folder.display()
        );
        return Err(DriverError::new(message));
    }
    Ok(Box::new(Nextflow::new(launch, settings)?))
}

impl Block for Nextflow {
    /// What the entry workflow emitted, which a simulated run makes up too.
    fn exported(&self) -> Result<Option<Json>, DriverError> {
        self.emitted()
    }
}

/// The op inputs as Nextflow params: a File or a Directory becomes its path, and a null
/// input stays out, so that the default of the pipeline holds.
pub(crate) fn params(bound: &Map<String, Json>) -> Json {
    let given = bound.iter().filter(|(_, value)| !value.is_null());
    Json::Object(
        given
            .map(|(name, value)| (name.clone(), param(value)))
            .collect(),
    )
}

fn param(value: &Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.iter().map(param).collect()),
        Json::Object(object) => match entry_path(value) {
            Some(path) => Json::String(path),
            None => Json::Object(
                (object.iter())
                    .map(|(k, v)| (k.clone(), param(v)))
                    .collect(),
            ),
        },
        other => other.clone(),
    }
}

/// The path of a File or Directory value: a local path, else its address.
fn entry_path(value: &Json) -> Option<String> {
    let class = value.get("class").and_then(Json::as_str);
    let location = value.get("location").and_then(Json::as_str);
    let (Some("File" | "Directory"), Some(location)) = (class, location) else {
        return None;
    };
    Some(match cwl_parser::iri::file_path(location) {
        Some(path) => path.to_string_lossy().into_owned(),
        None => location.to_owned(),
    })
}

/// The op outputs of a finished block, by port: the emit of the entry workflow with the
/// port's name, else the files published below the folder that the op input of that name
/// names. docs/wir.md describes the mapping.
pub(crate) fn outputs(
    emitted: Json,
    targets: &[String],
    ports: &[Port],
    bound: &Map<String, Json>,
    folder: &Path,
    defs: &[TypeDef],
) -> Result<Map<String, Json>, String> {
    let emitted = match emitted {
        Json::Object(object) => object,
        _ => Map::new(),
    };
    let mut values = Map::new();
    for port in ports {
        let value = match emitted.get(&port.name) {
            Some(value) => shape(value.clone(), &port.r#type, defs),
            None => published(port, targets, bound.get(&port.name), folder, defs)?,
        };
        values.insert(port.name.clone(), value);
    }
    Ok(values)
}

/// A File of an emit becomes a Directory where the port type wants one: Nextflow paths do
/// not say which they are.
fn shape(value: Json, r#type: &Type, defs: &[TypeDef]) -> Json {
    let Some(r#type) = wir::resolve(r#type, defs) else {
        return value;
    };
    match (r#type, value) {
        (Type::Optional(inner), value) => shape(value, inner, defs),
        (Type::Directory, Json::Object(mut object))
            if object.get("class") == Some(&json!("File")) =>
        {
            object.insert("class".into(), json!("Directory"));
            Json::Object(object)
        }
        (Type::Array(array), Json::Array(items)) => Json::Array(
            (items.into_iter())
                .map(|item| shape(item, &array.items, defs))
                .collect(),
        ),
        (Type::Tuple(types), Json::Array(items)) => Json::Array(
            (items.into_iter().enumerate())
                .map(|(index, item)| match types.get(index) {
                    Some(r#type) => shape(item, r#type, defs),
                    None => item,
                })
                .collect(),
        ),
        (Type::Record(fields), Json::Object(object)) => Json::Object(
            (object.into_iter())
                .map(
                    |(name, item)| match fields.iter().find(|field| field.name == name) {
                        Some(field) => {
                            let item = shape(item, &field.r#type, defs);
                            (name, item)
                        }
                        None => (name, item),
                    },
                )
                .collect(),
        ),
        (_, value) => value,
    }
}

/// The files published below the folder of an input: the folder itself for a Directory, each
/// file for an array, the only one for a File.
fn published(
    port: &Port,
    targets: &[String],
    input: Option<&Json>,
    folder: &Path,
    defs: &[TypeDef],
) -> Result<Json, String> {
    let name = &port.name;
    let base = input.and_then(|value| match value {
        Json::String(text) => Some(text.clone()),
        other => entry_path(other),
    });
    let Some(base) = base else {
        return Err(format!(
            "output {name} is no emit of the entry workflow and no input names its folder"
        ));
    };
    let base = match base.contains("://") {
        true => PathBuf::from(base),
        false => folder.join(base),
    };
    let mut files: Vec<&String> = (targets.iter())
        .filter(|target| Path::new(target).starts_with(&base) && Path::new(target) != base)
        .collect();
    files.sort();
    files.dedup();
    let entry = |class: &str, location: &str| {
        let basename = location.rsplit('/').next().unwrap_or(location);
        json!({"class": class, "location": location, "basename": basename})
    };
    let shown = base.to_string_lossy();
    let mut r#type = wir::resolve(&port.r#type, defs);
    let optional = matches!(r#type, Some(Type::Optional(_)));
    if let Some(Type::Optional(inner)) = r#type {
        r#type = wir::resolve(inner, defs);
    }
    match r#type {
        _ if files.is_empty() && optional => Ok(Json::Null),
        Some(Type::Directory) if files.is_empty() => Err(format!(
            "output {name}: the block published nothing below {shown}"
        )),
        Some(Type::Directory) => Ok(entry("Directory", &shown)),
        Some(Type::Array(_) | Type::Dynamic) => Ok(Json::Array(
            files.iter().map(|file| entry("File", file)).collect(),
        )),
        Some(Type::File(_)) if files.len() == 1 => Ok(entry("File", files[0])),
        Some(Type::File(_)) => Err(format!(
            "output {name}: the block published {} files below {shown}, the port takes one",
            files.len()
        )),
        _ => Err(format!(
            "output {name} takes the files published below {shown}, so it needs a File, a \
             Directory or an array type"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(name: &str, r#type: Json) -> Port {
        serde_json::from_value(json!({"name": name, "type": r#type})).unwrap()
    }

    #[test]
    fn writes_params_values() {
        let bound = json!({
            "input": {"class": "File", "location": "file:///data/a%20b.csv"},
            "samples": [{"class": "File", "location": "/data/s1.fq"}],
            "outdir": "results",
            "skip": null,
            "depth": 3
        });
        let params = params(bound.as_object().unwrap());
        let expected = json!({
            "input": "/data/a b.csv", "samples": ["/data/s1.fq"], "outdir": "results", "depth": 3
        });
        assert_eq!(params, expected);
    }

    #[test]
    fn maps_block_outputs() {
        let folder = Path::new("/out/block");
        let targets = [
            "/out/block/results/b.txt".to_owned(),
            "/out/block/results/a.txt".to_owned(),
            "/out/block/pipeline_info/trace.txt".to_owned(),
        ];
        let bound = json!({"outdir": "results"});
        let bound = bound.as_object().unwrap();
        let file = json!({"class": "File", "location": "/store/x", "basename": "x"});
        let emitted = json!({"folder": file, "count": 2});
        let ports = [
            port("folder", json!("directory")),
            port("count", json!("int")),
            port("outdir", json!({"array": {"items": {"file": {}}}})),
        ];
        let values = outputs(emitted.clone(), &targets, &ports, bound, folder, &[]).unwrap();
        assert_eq!(
            values["folder"]["class"], "Directory",
            "a path as a Directory"
        );
        assert_eq!(values["count"], 2);
        let names: Vec<&str> = (values["outdir"].as_array().unwrap().iter())
            .map(|file| file["basename"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["a.txt", "b.txt"],
            "published below the folder, sorted"
        );
        let ports = [port("outdir", json!("directory"))];
        let values = outputs(emitted.clone(), &targets, &ports, bound, folder, &[]).unwrap();
        assert_eq!(values["outdir"]["location"], "/out/block/results");
        let ports = [port("outdir", json!({"file": {}}))];
        let error = outputs(emitted.clone(), &targets, &ports, bound, folder, &[]).unwrap_err();
        assert!(error.contains("published 2 files"), "{error}");
        let ports = [port("missing", json!({"file": {}}))];
        let error = outputs(emitted, &targets, &ports, bound, folder, &[]).unwrap_err();
        assert!(error.contains("no emit of the entry workflow"), "{error}");
    }
}
