//! Describes the wir document of a run, its version and its declared sources, as provenance.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use run_database::Run;
use serde_json::{Value, json};

use crate::{CrateError, Options, ids};

/// The id of the entity of the wir document of a run.
const DOCUMENT: &str = "#wir-document";

/// Adds the wir document that the run compiled its workflow into, or ran, to `graph`: its
/// `wir_version` and digest, and each source it declares with its location and digest. The
/// root names it among what it mentions. A run without a readable document adds nothing.
pub(crate) fn provenance(
    graph: &mut Vec<Value>,
    run: &Run,
    options: &Options,
) -> Result<(), CrateError> {
    let path = match (&run.document, run.language.as_str()) {
        (Some(document), _) => document.clone(),
        (None, "wir") => run.workflow.clone(),
        (None, _) => return Ok(()),
    };
    // The copy that the run kept wins over the file as it is now, which may have changed.
    let kept = (run.document_digest.as_ref()).and_then(|digest| options.sources.get(digest));
    let Ok(bytes) = std::fs::read(kept.map_or(Path::new(&path), |kept| kept.as_path())) else {
        return Ok(());
    };
    let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(());
    };
    let digest = task_model::Digest::sha256(&bytes);
    if (run.document_digest.as_ref()).is_some_and(|recorded| *recorded != digest.to_string()) {
        return Ok(());
    }
    let mut sources = Vec::new();
    for source in document["sources"].as_array().into_iter().flatten() {
        let Some(id) = source["id"].as_str() else {
            continue;
        };
        let mut entity = json!({
            "@id": format!("#wir-source/{}", ids::encode(id)),
            "@type": "SoftwareSourceCode",
            "name": id,
        });
        for (key, field) in [
            ("programmingLanguage", "language"),
            ("version", "version"),
            ("codeRepository", "location"),
        ] {
            if let Some(text) = source[field].as_str() {
                entity[key] = json!(text);
            }
        }
        if let Some(hex) = source["digest"]
            .as_str()
            .and_then(|digest| digest.strip_prefix("sha256:"))
        {
            entity["sha256"] = json!(hex);
        }
        sources.push(entity);
    }
    let mut entity = json!({
        "@id": DOCUMENT,
        "@type": ["File", "SoftwareSourceCode"],
        "name": Path::new(&path).file_name().map_or(path.clone(), |name| {
            name.to_string_lossy().into_owned()
        }),
        "description": "The wir document that the run ran, as uwe compiled the workflow into it.",
        "encodingFormat": "application/json",
        "programmingLanguage": "wir",
        "sha256": digest.hex(),
        "contentSize": bytes.len().to_string(),
    });
    if let Some(version) = document["wir_version"].as_str() {
        entity["version"] = json!(version);
    }
    if !sources.is_empty() {
        let ids: Vec<Value> = (sources.iter())
            .map(|source| json!({"@id": source["@id"]}))
            .collect();
        entity["isBasedOn"] = json!(ids);
    }
    if let Some(root) = graph.iter_mut().find(|entity| entity["@id"] == "./") {
        let mentions = root["mentions"].take();
        let mut mentions = match mentions {
            Value::Array(items) => items,
            Value::Null => Vec::new(),
            other => vec![other],
        };
        mentions.push(json!({"@id": DOCUMENT}));
        root["mentions"] = json!(mentions);
    }
    graph.push(entity);
    graph.extend(sources);
    Ok(())
}
