//! The values of the entry graph's inputs, from the inputs file and the parameters of a
//! launch.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Map, Value as Json};
use wir::{Port, Type, TypeDef, resolve};
use workflow_graph::{DriverError, Launch};

use crate::state::widen;

/// The input values of a launch by port name: the JSON or YAML inputs file, with File and Directory
/// locations taken from its folder, then the parameters, read by the type of their port.
pub fn read(
    launch: &Launch,
    ports: &[Port],
    defs: &[TypeDef],
) -> Result<Map<String, Json>, DriverError> {
    let mut values = Map::new();
    if let Some(path) = &launch.inputs {
        let shown = path.display();
        let text = std::fs::read_to_string(path)
            .map_err(|error| DriverError::new(format!("cannot read {shown}: {error}")))?;
        // A CWL job may be YAML, which the CWL reader of values takes.
        let mut json: Json = match serde_json::from_str(&text) {
            Ok(json) => json,
            Err(_) => cwl_runtime::Value::parse(&text)
                .ok()
                .and_then(|value| serde_json::from_str(&value.to_json()).ok())
                .ok_or_else(|| DriverError::new(format!("{shown} is neither JSON nor YAML")))?,
        };
        let base = std::path::absolute(path).unwrap_or_else(|_| path.clone());
        absolute(&mut json, base.parent().unwrap_or(Path::new("/")));
        let Json::Object(object) = json else {
            return Err(DriverError::new(format!("{shown} must hold an object")));
        };
        values = object;
    }
    let mut given: Vec<(&str, Vec<&str>)> = Vec::new();
    for (name, text) in &launch.params {
        match given.iter_mut().find(|(known, _)| known == name) {
            Some((_, texts)) => texts.push(text),
            None => given.push((name, vec![text])),
        }
    }
    for (name, texts) in given {
        let Some(port) = ports.iter().find(|port| port.name == name) else {
            return Err(DriverError::new(format!("the graph has no input {name}")));
        };
        values.insert(name.to_owned(), parameter(&texts, &port.r#type, defs));
    }
    for port in ports {
        let given = match values.remove(&port.name) {
            None | Some(Json::Null) if port.default.is_some() => port.default.clone(),
            given => given,
        };
        let value = match given {
            Some(mut value) => {
                discover(&mut value, &port.r#type, (defs, launch.simulated));
                widen(value, &port.r#type, defs)
            }
            None if port.r#type.is_optional(defs) => Json::Null,
            None => {
                let message = format!("the input {} has no value", port.name);
                return Err(DriverError::new(message));
            }
        };
        if !wir::fits(&value, &port.r#type, defs) {
            let message = format!("the value of the input {} does not fit its type", port.name);
            return Err(DriverError::new(message));
        }
        values.insert(port.name.clone(), value);
    }
    if let Some(name) = values
        .keys()
        .find(|name| !ports.iter().any(|port| port.name == **name))
    {
        return Err(DriverError::new(format!("the graph has no input {name}")));
    }
    Ok(values)
}

/// Adds to each local File that lists no secondary files those beside it that its type asks
/// for, as CWL finds the secondary files of a job. A simulated run takes required ones as
/// present, as its files are made up.
fn discover(value: &mut Json, r#type: &Type, (defs, simulated): (&[TypeDef], bool)) {
    let inner = (defs, simulated);
    match (value, resolve(r#type, defs)) {
        (Json::Array(items), Some(Type::Array(array))) => {
            (items.iter_mut()).for_each(|item| discover(item, &array.items, inner));
        }
        (value, Some(Type::Optional(inner_type))) => discover(value, inner_type, inner),
        (Json::Object(file), Some(Type::File(wanted))) if !file.contains_key("secondary_files") => {
            let location = file
                .get("location")
                .and_then(Json::as_str)
                .unwrap_or_default();
            if location.contains("://") || wanted.secondary_files.is_empty() {
                return;
            }
            let primary = Path::new(location);
            let (Some(folder), Some(name)) = (primary.parent(), primary.file_name()) else {
                return;
            };
            let mut found = Vec::new();
            for item in &wanted.secondary_files {
                let path = folder.join(wir::secondary_name(&name.to_string_lossy(), &item.pattern));
                let class = match std::fs::metadata(&path) {
                    Ok(metadata) if metadata.is_dir() => "Directory",
                    Ok(_) => "File",
                    Err(_) if simulated && item.required => "File",
                    Err(_) => continue,
                };
                let location = path.to_string_lossy().into_owned();
                found.push(serde_json::json!({"class": class, "location": location}));
            }
            file.insert("secondary_files".into(), Json::Array(found));
        }
        _ => {}
    }
}

/// Makes the relative `location` or `path` of Files and Directories absolute, as a
/// `location`, the one key that wir values use.
pub fn absolute(value: &mut Json, base: &Path) {
    match value {
        Json::Array(items) => items.iter_mut().for_each(|item| absolute(item, base)),
        Json::Object(object) => {
            let class = object.get("class").and_then(Json::as_str);
            if matches!(class, Some("File" | "Directory")) {
                let path = object.remove("path");
                if let Some(Json::String(text)) = object.get("location").or(path.as_ref()) {
                    let location = match text.contains("://") || text.starts_with('/') {
                        true => text.clone(),
                        false => base.join(text).to_string_lossy().into_owned(),
                    };
                    object.insert("location".into(), Json::String(location));
                }
            }
            object.values_mut().for_each(|item| absolute(item, base));
        }
        _ => {}
    }
}

/// The value of a parameter given on the command line, by the type of its port; repeated
/// values make an array.
fn parameter(texts: &[&str], r#type: &Type, defs: &[TypeDef]) -> Json {
    let last = texts.last().copied().unwrap_or_default();
    match resolve(r#type, defs) {
        Some(Type::Optional(inner)) => parameter(texts, inner, defs),
        Some(Type::Array(array)) => Json::Array(
            (texts.iter())
                .map(|text| parameter(&[text], &array.items, defs))
                .collect(),
        ),
        Some(Type::String | Type::Enum(_)) => Json::from(last),
        Some(Type::File(_) | Type::Directory) => {
            let class = match resolve(r#type, defs) {
                Some(Type::Directory) => "Directory",
                _ => "File",
            };
            let location = match last.contains("://") {
                true => last.to_owned(),
                false => std::path::absolute(last)
                    .map_or(last.to_owned(), |path| path.to_string_lossy().into_owned()),
            };
            serde_json::json!({"class": class, "location": location})
        }
        _ => serde_json::from_str(last).unwrap_or_else(|_| Json::from(last)),
    }
}
