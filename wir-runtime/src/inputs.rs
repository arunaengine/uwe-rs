//! The values of the entry graph's inputs, from the inputs file and the parameters of a
//! launch.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Map, Value as Json};
use wir::{Port, Type, TypeDef, resolve};
use workflow_graph::{DriverError, Launch};

use cwl_runtime::files::Access;

use crate::state::widen;

/// The input values of a launch by port name: the JSON or YAML inputs file, with File and Directory
/// locations taken from its folder, then the parameters, read by the type of their port.
pub fn read(
    launch: &Launch,
    ports: &[Port],
    defs: &[TypeDef],
    remote: &cwl_parser::source::Remote,
    formats: Option<&crate::formats::FormatCheck>,
) -> Result<Map<String, Json>, DriverError> {
    let access = cwl_runtime::files::Access {
        assume_present: launch.simulated,
        remote: remote.clone(),
    };
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
        filled(&mut json, launch.simulated);
        if let Some(check) = formats {
            check.expand(&mut json);
        }
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
                discover(&mut value, &port.r#type, (defs, &access));
                widen(value, &port.r#type, defs)
            }
            None if port.r#type.is_optional(defs) => Json::Null,
            None => {
                let message = format!("the input {} has no value", port.name);
                return Err(DriverError::new(message));
            }
        };
        let fits = match formats {
            None => wir::fits(&value, &port.r#type, defs),
            Some(check) => {
                let formats = |found: &str, wanted: &str| check.matches(found, wanted);
                wir::fits_with(&value, &port.r#type, defs, &formats)
            }
        };
        if !fits {
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

/// Adds to each File that lists no secondary files those beside it that its type asks for, as
/// CWL finds the secondary files of a job: on disk, or at an address that its provider finds.
/// A simulated run takes required ones as present, as its files are made up.
fn discover(value: &mut Json, r#type: &Type, (defs, access): (&[TypeDef], &Access)) {
    let inner = (defs, access);
    let simulated = access.assume_present;
    match (value, resolve(r#type, defs)) {
        (Json::Array(items), Some(Type::Array(array))) => {
            (items.iter_mut()).for_each(|item| discover(item, &array.items, inner));
        }
        (value, Some(Type::Optional(inner_type))) => discover(value, inner_type, inner),
        (Json::Object(object), Some(Type::Record(fields))) => {
            for field in fields {
                if let Some(item) = object.get_mut(&field.name) {
                    discover(item, &field.r#type, inner);
                }
            }
        }
        (Json::Object(file), Some(Type::File(wanted))) if !file.contains_key("secondary_files") => {
            let location = file
                .get("location")
                .and_then(Json::as_str)
                .unwrap_or_default();
            if wanted.secondary_files.is_empty() {
                return;
            }
            if location.contains("://") {
                let (folder, name) = location.rsplit_once('/').unwrap_or(("", location));
                let mut found = Vec::new();
                for item in &wanted.secondary_files {
                    let beside = format!("{folder}/{}", wir::secondary_name(name, &item.pattern));
                    if access.exists_next_to(&beside, location) {
                        found.push(serde_json::json!({"class": "File", "location": beside}));
                    }
                }
                file.insert("secondary_files".into(), Json::Array(found));
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
                let given = object.get("location").is_some();
                if let Some(Json::String(text)) = object.get("location").or(path.as_ref()) {
                    let location = match text.contains("://") || text.starts_with('/') {
                        true => text.clone(),
                        // A relative location is a URI reference, whose escapes stay escapes.
                        false if given => {
                            let folder = format!("{}/", cwl_parser::iri::file_url(base));
                            let uri = cwl_runtime::files::resolve_uri(&folder, text);
                            (cwl_parser::iri::file_path(&uri))
                                .map_or(uri, |path| path.to_string_lossy().into_owned())
                        }
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

/// Gives each File the basename of a literal and the size of its file or contents that uwe's
/// CWL driver gives it; a simulated run takes a missing file as empty.
pub(crate) fn filled(value: &mut Json, simulated: bool) {
    match value {
        Json::Array(items) => items.iter_mut().for_each(|item| filled(item, simulated)),
        Json::Object(object) => {
            object.values_mut().for_each(|item| filled(item, simulated));
            if object.get("class").and_then(Json::as_str) != Some("File") {
                return;
            }
            let literal = !object.contains_key("location") && !object.contains_key("basename");
            if let Some(name) = literal.then(|| literal_name(object)).flatten() {
                object.insert("basename".into(), Json::String(name));
            }
            if object.contains_key("size") {
                return;
            }
            let location = object.get("location").and_then(Json::as_str);
            let path = location.map(|text| cwl_parser::iri::file_path(text).unwrap_or(text.into()));
            let size = match (path.map(std::fs::metadata), object.get("contents")) {
                (Some(Ok(metadata)), _) => Some(metadata.len()),
                (_, Some(Json::String(text))) => Some(text.len() as u64),
                (Some(Err(_)), _) if simulated => Some(0),
                _ => None,
            };
            if let Some(size) = size {
                object.insert("size".into(), Json::from(size));
            }
        }
        _ => {}
    }
}

/// The basename that uwe's CWL driver gives a File literal: a digest of its fields.
fn literal_name(object: &Map<String, Json>) -> Option<String> {
    let text = crate::cwl::to_cwl(Json::Object(object.clone())).to_string();
    let mut value = cwl_runtime::Value::parse(&text).ok()?;
    cwl_runtime::files::normalize(&mut value).ok()?;
    value.text("basename").map(str::to_owned)
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
