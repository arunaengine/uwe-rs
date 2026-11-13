//! The wir values of CWL input defaults, with File and Directory fields as wir keeps them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_parser::node::{Node, Value};
use serde_json::{Map, Value as Json};
use wir::Port;

/// Sets the default of a port from a CWL default, when the value fits the port type. A
/// location below `base` becomes relative to it, as the wir document is meant for `base`.
pub fn set(port: &mut Port, default: &Node, base: &Path) -> Result<(), String> {
    let value = convert(default, base).ok_or("a default that wir cannot hold")?;
    match wir::fits(&value, &port.r#type, &[]) {
        true => {
            port.default = Some(value);
            Ok(())
        }
        false => Err("a default that does not fit its type".into()),
    }
}

fn convert(node: &Node, base: &Path) -> Option<Json> {
    Some(match &node.value {
        Value::Null => Json::Null,
        Value::Bool(flag) => Json::Bool(*flag),
        Value::Integer(number) => Json::from(*number),
        Value::BigInteger(_) => return None,
        Value::Float(number) => Json::from(serde_json::Number::from_f64(*number)?),
        Value::String(text) => Json::String(text.clone()),
        Value::List(items) => Json::Array(
            (items.iter())
                .map(|item| convert(item, base))
                .collect::<Option<_>>()?,
        ),
        Value::Map(entries) => {
            let class = node.get("class").and_then(Node::as_str);
            let keys: &[&str] = match class {
                Some("File") => &["location", "basename", "format", "contents", "size"],
                Some("Directory") => &["location", "basename"],
                _ => &[],
            };
            let mut object = Map::new();
            for entry in entries {
                let key = match (class, entry.key.as_str()) {
                    (None, key) => key,
                    (Some(_), "class") => "class",
                    (Some(_), "path") if node.get("location").is_none() => "location",
                    (Some("File"), "secondaryFiles") => "secondary_files",
                    (Some("Directory"), "listing") => "listing",
                    (Some(_), key) if keys.contains(&key) => key,
                    // Fields that CWL computes, like `nameroot`, are not part of a wir value.
                    _ => continue,
                };
                let mut value = convert(&entry.value, base)?;
                if key == "location"
                    && let Json::String(text) = &value
                {
                    value = Json::String(place(text, base));
                }
                object.insert(key.to_owned(), value);
            }
            Json::Object(object)
        }
    })
}

/// A location as a path relative to `base` when it is a local file below it, else as a path
/// or address.
fn place(location: &str, base: &Path) -> String {
    match cwl_parser::iri::file_path(location) {
        Some(path) => match path.strip_prefix(base) {
            Ok(relative) => relative.to_string_lossy().into_owned(),
            Err(_) => path.to_string_lossy().into_owned(),
        },
        None => location.to_owned(),
    }
}
