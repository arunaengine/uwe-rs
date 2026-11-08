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
