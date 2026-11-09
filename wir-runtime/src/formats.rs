//! The formats of Files in a run: the prefixes of the formats of input values, and the
//! ontologies by which a format fits another one, which load when an exact match fails.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;

use cwl_parser::source::Remote;
use cwl_runtime::ontology::Ontology;
use serde_json::Value as Json;

/// How the formats of a document match, as CWL matches them with its `$schemas`.
#[derive(Debug)]
pub struct FormatCheck {
    prefixes: BTreeMap<String, String>,
    ontology: Ontology,
}

impl FormatCheck {
    /// The check of the formats of a document in the folder `base`, which reads remote
    /// ontologies through `remote`.
    pub fn new(formats: &wir::Formats, base: &Path, remote: Remote) -> FormatCheck {
        let schemas = (formats.ontologies.iter())
            .map(|location| match location.contains("://") {
                true => location.clone(),
                false => cwl_parser::iri::file_url(&base.join(location)),
            })
            .collect();
        FormatCheck {
            prefixes: formats.prefixes.clone(),
            ontology: Ontology::new(schemas, remote),
        }
    }

    /// Whether a File of the format `found` fits the `wanted` one by the ontologies: a
    /// subclass or an equivalent class of it, also through others.
    pub fn matches(&self, found: &str, wanted: &str) -> bool {
        match self.ontology.matches(found, &[wanted.to_owned()]) {
            Ok(matched) => matched,
            Err(why) => {
                tracing::warn!(%why, "the ontologies of the formats cannot be read");
                false
            }
        }
    }

    /// Expands the prefixes of the formats of the Files in a value, as CWL expands those of
    /// a job.
    pub fn expand(&self, value: &mut Json) {
        match value {
            Json::Array(items) => items.iter_mut().for_each(|item| self.expand(item)),
            Json::Object(object) => {
                if object.get("class").and_then(Json::as_str) == Some("File")
                    && let Some(Json::String(format)) = object.get_mut("format")
                    && let Some((prefix, rest)) = format.split_once(':')
                    && let Some(iri) = self.prefixes.get(prefix)
                {
                    *format = format!("{iri}{rest}");
                }
                object.values_mut().for_each(|item| self.expand(item));
            }
            _ => {}
        }
    }
}
