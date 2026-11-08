//! The inputs of a lowered document from a WDL input file: keys without the name of the
//! workflow, Files and Directories as wir values, and pairs as tuples.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Map, Value as Json, json};
use wdl_runtime::value::located;
use wir::Type;

use crate::Lowering;

impl Lowering {
    /// The input values of the entry graph from the object of a WDL input file, like
    /// `{"hello.name": "x"}`. Relative paths start at `base`, the folder of the file.
    pub fn inputs(&self, json: &Json, base: &Path) -> Result<Json, String> {
        let Json::Object(fields) = json else {
            return Err("a WDL input file holds a JSON object".into());
        };
        let entry = (self.document.graphs.iter()).find(|graph| graph.name == self.document.entry);
        let ports = entry.map_or(&[][..], |graph| &graph.inputs);
        let prefix = format!("{}.", self.name);
        let mut values = Map::new();
        for (key, value) in fields {
            let name = key.strip_prefix(&prefix).unwrap_or(key);
            let found = ports.iter().find(|port| port.name == name);
            let port = found.ok_or_else(|| format!("{} has no input {name}", self.name))?;
            values.insert(name.to_owned(), value_of(value, &port.r#type, base));
        }
        Ok(Json::Object(values))
    }

    /// The input values of the entry graph from command-line parameters like `--hello.name x`,
    /// read as uwe's WDL driver reads them: repeated values make an array, a String, File or
    /// Directory is the text, and other values are JSON. Relative paths start at `base`.
    pub fn parameters(&self, params: &[(String, String)], base: &Path) -> Result<Json, String> {
        let prefix = format!("{}.", self.name);
        let mut texts: Vec<(&str, Vec<&str>)> = Vec::new();
        for (name, text) in params {
            let name = name.strip_prefix(&prefix).unwrap_or(name);
            match texts.iter_mut().find(|(known, _)| *known == name) {
                Some((_, list)) => list.push(text),
                None => texts.push((name, vec![text])),
            }
        }
        let entry = (self.document.graphs.iter()).find(|graph| graph.name == self.document.entry);
        let ports = entry.map_or(&[][..], |graph| &graph.inputs);
        let mut values = Map::new();
        for (name, list) in texts {
            let found = ports.iter().find(|port| port.name == name);
            let port = found.ok_or_else(|| format!("{} has no input {name}", self.name))?;
            values.insert(name.to_owned(), parameter(&port.r#type, &list));
        }
        self.inputs(&Json::Object(values), base)
    }
}

/// The WDL input JSON of a parameter given one or more times.
fn parameter(r#type: &Type, texts: &[&str]) -> Json {
    let text = texts.last().copied().unwrap_or_default();
    match r#type {
        Type::Optional(inner) => parameter(inner, texts),
        Type::Array(array) => Json::Array(
            (texts.iter())
                .map(|text| parameter(&array.items, &[text]))
                .collect(),
        ),
        Type::String | Type::File(_) | Type::Directory => Json::String(text.to_owned()),
        _ => serde_json::from_str(text).unwrap_or_else(|_| Json::String(text.to_owned())),
    }
}

/// A value of WDL input JSON as a wir value of a type.
fn value_of(json: &Json, r#type: &Type, base: &Path) -> Json {
    let entry = |class: &str, text: &str| json!({"class": class, "location": located(text, base)});
    match (r#type, json) {
        (Type::Optional(inner), _) => value_of(json, inner, base),
        (Type::File(_), Json::String(text)) => entry("File", text),
        (Type::Directory, Json::String(text)) => entry("Directory", text),
        (Type::Array(array), Json::Array(items)) => Json::Array(
            (items.iter())
                .map(|item| value_of(item, &array.items, base))
                .collect(),
        ),
        (Type::Tuple(types), Json::Object(pair)) if types.len() == 2 => Json::Array(vec![
            value_of(pair.get("left").unwrap_or(&Json::Null), &types[0], base),
            value_of(pair.get("right").unwrap_or(&Json::Null), &types[1], base),
        ]),
        (Type::Map(map), Json::Object(object)) => Json::Object(
            (object.iter())
                .map(|(key, item)| (key.clone(), value_of(item, &map.value, base)))
                .collect(),
        ),
        (Type::Record(fields), Json::Object(object)) => Json::Object(
            (object.iter())
                .map(|(key, item)| {
                    let field = fields.iter().find(|field| field.name == *key);
                    let want = field.map_or(Type::Dynamic, |field| field.r#type.clone());
                    (key.clone(), value_of(item, &want, base))
                })
                .collect(),
        ),
        _ => json.clone(),
    }
}
