//! Resolves ConfigMaps and Secrets from loaded documents without a cluster.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use argo_parser::{Node, Value as Yaml};

use crate::library::{Kind, Library};
use crate::run::{Failure, Result, Run, scalar_text};

/// A reference to a key of a ConfigMap or a Secret, like `configMapKeyRef` or `secretKeyRef`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KeyRef {
    pub name: String,
    pub key: String,
    pub optional: bool,
}

impl KeyRef {
    pub fn of(node: &Node) -> KeyRef {
        let text = |key: &str| node.get(key).map(scalar_text).unwrap_or_default();
        KeyRef {
            name: text("name"),
            key: text("key"),
            optional: matches!(
                node.get("optional").map(|node| &node.value),
                Some(Yaml::Bool(true))
            ),
        }
    }
}

/// The entries of the ConfigMap of this name among the loaded documents.
pub(crate) fn config_map(
    library: &Library,
    near: &Path,
    name: &str,
) -> Option<Vec<(String, String)>> {
    let document = library.find(Kind::ConfigMap, name, near)?;
    let entries = match document.node.get("data").map(|data| &data.value) {
        Some(Yaml::Mapping(entries)) => entries
            .iter()
            .map(|entry| (entry.key.clone(), scalar_text(&entry.value)))
            .collect(),
        _ => Vec::new(),
    };
    Some(entries)
}

/// The value of a key of a ConfigMap among the loaded documents.
pub(crate) fn config_map_value(
    library: &Library,
    near: &Path,
    reference: &KeyRef,
) -> Option<String> {
    config_map(library, near, &reference.name)?
        .into_iter()
        .find(|(key, _)| *key == reference.key)
        .map(|(_, value)| value)
}

/// A word for a placeholder, like `placeholder_secret_db_password`.
fn word(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// What stands for a ConfigMap key that no loaded document holds: a placeholder in a simulated run,
/// with a warning, or an error in a real run. `what` names the reader, like `parameter x`.
pub(crate) fn missing_config_map(
    what: &str,
    reference: &KeyRef,
    assume_inputs: bool,
    warnings: &mut Vec<String>,
) -> Result<String> {
    let place = format!(
        "key {} of ConfigMap {}, which none of the loaded documents (the workflow file, the configs and the repository) holds",
        reference.key, reference.name
    );
    if !assume_inputs {
        return Err(Failure::error(format!("{what} reads {place}")));
    }
    let placeholder = format!(
        "placeholder_config_map_{}_{}",
        word(&reference.name),
        word(&reference.key)
    );
    warnings.push(format!(
        "{what} reads {place}; a simulated run uses `{placeholder}`"
    ));
    Ok(placeholder)
}

impl Run {
    /// What stands for a key of a Secret: a placeholder in a simulated run, with a warning. A real
    /// run stops, because iunctus has no secret store yet and secret values never enter a task.
    pub(crate) fn secret(&mut self, what: &str, reference: &KeyRef) -> Result<String> {
        let place = format!("key {} of Secret {}", reference.key, reference.name);
        if !self.options.assume_inputs {
            return Err(Failure::error(format!(
                "{what} reads {place}; iunctus has no secret store yet, so a real run cannot give it to a task"
            )));
        }
        let placeholder = format!(
            "placeholder_secret_{}_{}",
            word(&reference.name),
            word(&reference.key)
        );
        self.warn(format!(
            "{what} reads {place}; a simulated run uses `{placeholder}`"
        ));
        Ok(placeholder)
    }

    /// The value of a ConfigMap key, or what stands for it when no loaded document holds it.
    pub(crate) fn config_map_key(&mut self, what: &str, reference: &KeyRef) -> Result<String> {
        if let Some(value) = config_map_value(&self.library, &self.document.path, reference) {
            return Ok(value);
        }
        let mut warnings = Vec::new();
        let value = missing_config_map(what, reference, self.options.assume_inputs, &mut warnings);
        for warning in warnings {
            self.warn(warning);
        }
        value
    }
}
