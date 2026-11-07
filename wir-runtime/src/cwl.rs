//! CWL native blocks: the CWL driver of uwe as a child, and its values in the forms of CWL
//! and wir.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Map, Value as Json};
use workflow_graph::{Driver, DriverError, Launch};

use crate::child::Block;

/// The keys that a wir File value has; CWL adds others, like `path` and `nameroot`.
const FILE_KEYS: [&str; 6] = [
    "location", "basename", "format", "size", "checksum", "contents",
];

/// A CWL driver for the launch of one native block. A child never prints its output object.
pub fn block(
    launch: &Launch,
    options: &cwl_runtime::Options,
) -> Result<Box<dyn Block>, DriverError> {
    let options = cwl_runtime::Options {
        simulated: launch.simulated,
        print: false,
        ..options.clone()
    };
    Ok(Box::new(cwl_runtime::CwlDriver::new(launch, options)?))
}

impl Block for cwl_runtime::CwlDriver {
    /// The output object as delivered by a real run, else the one of a simulated run, whose
    /// files are made up.
    fn exported(&self) -> Result<Option<Json>, DriverError> {
        let object = self.results().or_else(|| {
            let outputs = cwl_runtime::Value::Map(self.outputs()?.clone());
            serde_json::from_str(&outputs.to_json()).ok()
        });
        Ok(object.map(to_wir))
    }
}

/// A CWL value as a wir value: Files and Directories keep the fields wir knows, with
/// `secondaryFiles` as `secondary_files`.
pub fn to_wir(value: Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.into_iter().map(to_wir).collect()),
        Json::Object(object) => match object.get("class").and_then(Json::as_str) {
            Some("File") => {
                let mut file = entry("File", &object, &FILE_KEYS);
                if let Some(Json::Array(items)) = object.get("secondaryFiles") {
                    let items = items.iter().cloned().map(to_wir).collect();
                    file.insert("secondary_files".into(), Json::Array(items));
                }
                Json::Object(file)
            }
            Some("Directory") => {
                let mut folder = entry("Directory", &object, &["location", "basename"]);
                if let Some(Json::Array(items)) = object.get("listing") {
                    let items = items.iter().cloned().map(to_wir).collect();
                    folder.insert("listing".into(), Json::Array(items));
                }
                Json::Object(folder)
            }
            _ => Json::Object((object.into_iter()).map(|(k, v)| (k, to_wir(v))).collect()),
        },
        other => other,
    }
}

/// A wir value as a CWL value of a job file.
pub fn to_cwl(value: Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.into_iter().map(to_cwl).collect()),
        Json::Object(object) => {
            let file = object.get("class").and_then(Json::as_str) == Some("File");
            let converted = object.into_iter().map(|(key, item)| match key.as_str() {
                "secondary_files" if file => ("secondaryFiles".to_owned(), to_cwl(item)),
                _ => (key, to_cwl(item)),
            });
            Json::Object(converted.collect())
        }
        other => other,
    }
}

/// A File or Directory object with the class and those of `keys` that `object` has.
fn entry(class: &str, object: &Map<String, Json>, keys: &[&str]) -> Map<String, Json> {
    let mut entry = Map::new();
    entry.insert("class".into(), Json::from(class));
    for key in keys {
        if let Some(value) = object.get(*key).filter(|value| !value.is_null()) {
            entry.insert((*key).into(), value.clone());
        }
    }
    entry
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn converts_file_values() {
        let cwl = json!({
            "out": {
                "class": "File", "location": "file:///o/a.txt", "path": "/o/a.txt",
                "basename": "a.txt", "nameroot": "a", "nameext": ".txt", "size": 3,
                "checksum": "sha1$00", "format": null,
                "secondaryFiles": [{"class": "File", "location": "file:///o/a.txt.fai", "path": "/o/a.txt.fai"}]
            },
            "folder": {"class": "Directory", "location": "file:///o/d", "path": "/o/d", "listing": []},
            "count": [1, 2]
        });
        let wir = to_wir(cwl);
        let expected = json!({
            "out": {
                "class": "File", "location": "file:///o/a.txt", "basename": "a.txt", "size": 3,
                "checksum": "sha1$00",
                "secondary_files": [{"class": "File", "location": "file:///o/a.txt.fai"}]
            },
            "folder": {"class": "Directory", "location": "file:///o/d", "listing": []},
            "count": [1, 2]
        });
        assert_eq!(wir, expected);
        let types: wir::Type = serde_json::from_value(json!({"file": {}})).unwrap();
        assert!(
            wir::fits(&wir["out"], &types, &[]),
            "a converted file is a wir File"
        );
        let back = to_cwl(wir);
        assert_eq!(
            back["out"]["secondaryFiles"][0]["location"],
            "file:///o/a.txt.fai"
        );
        assert!(back["out"].get("secondary_files").is_none());
    }
}
