//! Small entities and values of the metadata: action status, container images, languages.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value, json};

use crate::ids::time;
use crate::image::ImageReference;

/// One value as itself and several as a list, as RO-Crate 1.1 recommends.
pub(crate) fn one_or_many(values: impl IntoIterator<Item = Value>) -> Value {
    let mut values: Vec<Value> = values.into_iter().collect();
    match values.len() {
        1 => values.remove(0),
        _ => Value::Array(values),
    }
}

pub(crate) fn push_unique(values: &mut Vec<Option<Value>>, value: Option<Value>) {
    if value.is_some() && !values.contains(&value) {
        values.push(value);
    }
}

/// Sets the end time, the status and the error of an action.
pub(crate) fn status(action: &mut Value, ended: Option<i64>, state: &str, error: Option<&str>) {
    if let Some(ended) = ended {
        action["endTime"] = json!(time(ended));
    }
    match state {
        "succeeded" => action["actionStatus"] = json!("http://schema.org/CompletedActionStatus"),
        "running" => action["actionStatus"] = json!("http://schema.org/ActiveActionStatus"),
        _ => {
            action["actionStatus"] = json!("http://schema.org/FailedActionStatus");
            if let Some(error) = error {
                action["error"] = json!(error);
            }
        }
    }
}

pub(crate) fn image_entity(id: &str, reference: &str) -> Value {
    let image = ImageReference::parse(reference);
    let mut entity = json!({
        "@id": id,
        "@type": "ContainerImage",
        "additionalType": {"@id": "https://w3id.org/ro/terms/workflow-run#DockerImage"},
        "registry": image.registry,
        "name": image.name
    });
    if let Some(tag) = image.tag {
        entity["tag"] = json!(tag);
    }
    if let Some(sha256) = image.sha256 {
        entity["sha256"] = json!(sha256);
    }
    entity
}

/// The id of a language as Workflow RO-Crate names it; Argo has none there.
pub(crate) fn language_id(language: &str) -> String {
    match language {
        "argo" => "https://argoproj.github.io/workflows/".into(),
        other => format!("https://w3id.org/workflowhub/workflow-ro-crate#{other}"),
    }
}

pub(crate) fn language(language: &str) -> Value {
    let (name, url) = match language {
        "nextflow" => ("Nextflow", "https://www.nextflow.io/"),
        "snakemake" => ("Snakemake", "https://snakemake.readthedocs.io/"),
        "cwl" => ("Common Workflow Language", "https://www.commonwl.org/"),
        _ => ("Argo Workflows", "https://argoproj.github.io/workflows/"),
    };
    json!({
        "@id": language_id(language),
        "@type": "ComputerLanguage",
        "name": name,
        "identifier": {"@id": url},
        "url": {"@id": url}
    })
}
