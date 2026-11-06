//! The `fairscape` scope of nf-fairscape, read when a run starts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use super::Fairscape;
use crate::observer::Observer;
use crate::report_format::Json;

/// The `fairscape` scope.
pub(super) struct Settings {
    pub(super) file: PathBuf,
    pub(super) patterns: Vec<String>,
    pub(super) workflow: bool,
    pub(super) param_inputs: bool,
    pub(super) author: Option<String>,
    pub(super) description: Option<String>,
    pub(super) keywords: Vec<String>,
    pub(super) license: Option<String>,
    pub(super) organization: Option<String>,
    pub(super) naan: String,
    pub(super) tool_versions: bool,
    pub(super) metadata: Vec<(String, Json)>,
    pub(super) evidence: bool,
    pub(super) inverses: bool,
    pub(super) checksums: bool,
    pub(super) sizes: bool,
}

/// Reads the config, checks the crate file as the plugin does at the start of a run, and warns
/// once about the parts of the plugin that uwe leaves out.
pub(super) fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let value = |key: &str| config.get(&["fairscape", key]);
    let flag = |key: &str, default: bool| match value(key) {
        Some(Value::Boolean(flag)) => flag,
        Some(Value::Null) | None => default,
        Some(other) => other.truth(),
    };
    let text = |key: &str| {
        value(key)
            .filter(Value::truth)
            .map(|value| value.to_string())
    };
    let list = |key: &str| match value(key) {
        Some(Value::List(items)) => {
            Some(items.borrow().items.iter().map(Value::to_string).collect())
        }
        Some(Value::String(text)) => Some(vec![text.to_string()]),
        _ => None,
    };
    if !flag("enabled", true) {
        return Ok(None);
    }
    let launch = std::env::current_dir().unwrap_or_default();
    let file = launch.join(text("file").unwrap_or_else(|| "ro-crate-metadata.json".into()));
    if file.exists() {
        if !flag("overwrite", false) {
            return Err(format!(
                "Provenance file already exists: {}",
                file.display()
            ));
        }
        std::fs::remove_file(&file).map_err(|_| {
            format!(
                "Unable to overwrite existing provenance file: {}",
                file.display()
            )
        })?;
    }
    let mut metadata = Vec::new();
    if let Some(Value::Map(map)) = value("metadata") {
        for (key, item) in &map.borrow().entries {
            let key = key.0.to_string();
            if ["@id", "@type", "conformsTo", "hasPart"].contains(&key.as_str()) {
                tracing::warn!(
                    "nf-fairscape: fairscape.metadata key '{key}' is managed by the plugin and \
                     cannot be overridden; ignoring"
                );
            } else if !matches!(item, Value::Null) {
                metadata.push((key, json_of(item)));
            }
        }
    }
    let mut extras = Vec::new();
    if flag("datasheet", true) {
        extras.push("ro-crate-datasheet.html and ai_ready_score.json (fairscape.datasheet)");
    }
    if flag("evidenceGraph", true) {
        extras.push("provenance-graph.html (fairscape.evidenceGraph)");
    }
    if flag("linkml", true) {
        extras.push("ro-crate-linkml.yaml (fairscape.linkml)");
    }
    for (key, what) in [
        ("schemas", "schemas (fairscape.schemas)"),
        (
            "containerProvenance",
            "container digests (fairscape.containerProvenance)",
        ),
        (
            "expandDirectories",
            "files inside directories (fairscape.expandDirectories)",
        ),
    ] {
        if flag(key, false) {
            extras.push(what);
        }
    }
    if !extras.is_empty() {
        tracing::warn!(
            "nf-fairscape: uwe writes the RO-Crate and provenance-graph.json, but not {}",
            extras.join(", ")
        );
    }
    let settings = Settings {
        file,
        patterns: list("patterns").unwrap_or_default(),
        workflow: flag("includeWorkflow", true),
        param_inputs: flag("paramInputs", true),
        author: text("author"),
        description: text("description"),
        keywords: list("keywords").unwrap_or_else(|| vec!["nextflow".into(), "workflow".into()]),
        license: text("license"),
        organization: text("organization"),
        naan: text("naan").unwrap_or_else(|| "59853".into()),
        tool_versions: flag("toolVersions", true),
        metadata,
        evidence: flag("evidenceGraph", true),
        inverses: flag("linkInverses", true),
        checksums: flag("checksums", false),
        sizes: flag("contentSizes", false),
    };
    Ok(Some(Box::new(Fairscape {
        settings,
        start: None,
        tasks: Vec::new(),
        published: Vec::new(),
    })))
}

/// A config value as JSON.
fn json_of(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Boolean(flag) => Json::Bool(*flag),
        Value::Number(number) => match serde_json::from_str(&number.to_string()) {
            Ok(serde_json::Value::Number(number)) => Json::Number(number),
            _ => Json::Text(number.to_string()),
        },
        Value::List(items) => Json::List(items.borrow().items.iter().map(json_of).collect()),
        Value::Map(map) => Json::Object(
            (map.borrow().entries.iter())
                .map(|(key, item)| (key.0.to_string(), json_of(item)))
                .collect(),
        ),
        other => Json::Text(other.to_string()),
    }
}
