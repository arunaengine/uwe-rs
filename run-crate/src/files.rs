//! The entities of the files of a run: copied into the crate, on the web, or gone.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;

use run_database::FileRow;
use serde_json::{Value, json};

use crate::data::Placed;
use crate::ids::encode;
use crate::metadata::Record;

impl Record<'_> {
    /// One entity per file in the crate, on the web, or gone.
    pub(crate) fn file_entities(&self) -> Vec<Value> {
        let mut seen = HashSet::new();
        let mut entities = Vec::new();
        for file in self.inputs().chain(self.outputs()) {
            let Some(id) = self.file_id(file) else {
                continue;
            };
            let Some(id) = id["@id"].as_str().map(str::to_owned) else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            let name = file.path.rsplit('/').find(|part| !part.is_empty());
            let mut entity = json!({
                "@id": id,
                "@type": if file.directory { "Dataset" } else { "File" },
                "name": name.unwrap_or(&file.path),
                "alternateName": file.path
            });
            match self.placed_file(file) {
                Some(Placed::Copied { digest, size, .. }) => {
                    entity["sha256"] = json!(digest.hex());
                    entity["contentSize"] = json!(size.to_string());
                }
                Some(Placed::Absent) => {
                    entity["description"] = json!(format!(
                        "Not in this crate: {} was not there when the crate was made.",
                        file.location
                    ));
                }
                _ => {}
            }
            entities.push(entity);
        }
        entities
    }

    pub(crate) fn placed_file(&self, file: &FileRow) -> Option<&Placed> {
        if file.location.is_empty() {
            let digest = file.digest.as_deref()?;
            return self.placed.get(&format!("content:{digest}"));
        }
        self.placed.get(&file.location)
    }

    /// The reference to the entity of a file.
    pub(crate) fn file_id(&self, file: &FileRow) -> Option<Value> {
        let id = match self.placed_file(file)? {
            Placed::Copied { id, .. } => id.clone(),
            Placed::Remote { url } => url.clone(),
            Placed::Absent => format!("#absent/{}", encode(&file.location)),
        };
        Some(json!({"@id": id}))
    }

    pub(crate) fn copied(&self) -> impl Iterator<Item = &str> {
        let mut ids: Vec<&str> = self
            .placed
            .values()
            .filter_map(|placed| match placed {
                Placed::Copied { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
    }

    pub(crate) fn inputs(&self) -> impl Iterator<Item = &FileRow> {
        self.tasks.iter().flat_map(|task| &task.inputs)
    }

    pub(crate) fn outputs(&self) -> impl Iterator<Item = &FileRow> {
        self.tasks.iter().flat_map(|task| &task.outputs)
    }
}
