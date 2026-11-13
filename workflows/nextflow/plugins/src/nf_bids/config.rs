//! The YAML config of `fromBIDS`: the loop entities and how each config key groups its files.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::parse_yaml;
use nextflow_evaluator::value::Value;

use super::files::column;
use crate::nf_schema::{entries, get};

/// Entity values by column, like `direction: dir-AP`.
pub(super) type Selector = Vec<(String, String)>;

/// How a config key groups the files it matches.
#[derive(Debug)]
pub(super) enum Set {
    /// One file per extension.
    Plain,
    /// Named groups of files, by entity values.
    Named(Vec<(String, Selector)>),
    /// Ordered lists over the values of entities.
    Sequential { by: Vec<String>, hierarchical: bool },
    /// Named groups, each holding a sequence.
    Mixed {
        sequence: String,
        groups: Vec<(String, Selector)>,
    },
}

#[derive(Debug)]
pub(super) struct Key {
    pub name: String,
    pub suffix: String,
    /// The extensions it collects, without the dot.
    pub extensions: Vec<String>,
    /// Entity columns that do not tell its files apart.
    pub excluded: Vec<String>,
    pub cross: Vec<String>,
    pub required: Vec<String>,
    pub set: Set,
}

#[derive(Debug)]
pub(super) struct Config {
    /// The entity columns that items are grouped by.
    pub loop_over: Vec<String>,
    pub keys: Vec<Key>,
}

/// Reads and checks the config; the error is the message the run fails with.
pub(super) fn read(text: &str, shown: &str) -> Result<Config, String> {
    let failed =
        |message: &str| format!("Failed to load BIDS configuration from {shown}: {message}");
    let document = parse_yaml(text).map_err(|error| failed(&error.message))?;
    // SnakeYAML, which the plugin reads with, refuses a key without a name.
    if let Value::Map(map) = &document
        && map
            .borrow()
            .entries
            .keys()
            .any(|key| matches!(key.0, Value::Null))
    {
        return Err(failed("a mapping key is empty"));
    }
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let loop_over = match get(&document, "loop_over") {
        None | Some(Value::Null) => ["subject", "session", "run", "task"]
            .map(String::from)
            .to_vec(),
        Some(Value::List(list)) => list.borrow().items.iter().map(entity).collect(),
        Some(_) => {
            errors.push("Global 'loop_over' must be a list of entity names".to_owned());
            Vec::new()
        }
    };
    let mut keys = Vec::new();
    for (name, value) in entries(&document) {
        if name == "loop_over" {
            continue;
        }
        if name == "meta" {
            return Err("Configuration error: Reserved key 'meta' cannot be used as suffix name. Please rename this suffix in your configuration file.".into());
        }
        match key(&name, &value, &mut errors, &mut warnings) {
            Some(key) => keys.push(key),
            None => errors.push(format!("Suffix '{name}': configuration is null or empty")),
        }
    }
    if !errors.is_empty() {
        let mut message = format!("Configuration validation failed for {shown}:\nERRORS:");
        for error in &errors {
            message.push_str(&format!("\n  {error}"));
        }
        if !warnings.is_empty() {
            message.push_str("\n\nWARNINGS:");
            for warning in &warnings {
                message.push_str(&format!("\n  {warning}"));
            }
        }
        return Err(message);
    }
    for warning in &warnings {
        tracing::warn!("[nf-bids] {warning}");
    }
    Ok(Config { loop_over, keys })
}

/// An entity column from a name or key of the config.
fn entity(value: &Value) -> String {
    let text = value.to_string();
    column(&text).map_or(text, str::to_owned)
}

fn texts(value: Option<Value>) -> Vec<String> {
    match value {
        Some(Value::List(list)) => list.borrow().items.iter().map(Value::to_string).collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![other.to_string()],
    }
}

/// The selectors of named groups; a `description` is documentation.
fn selectors(groups: &Value) -> Vec<(String, Selector)> {
    (entries(groups).into_iter())
        .map(|(name, spec)| {
            let selector = (entries(&spec).into_iter())
                .filter(|(key, _)| key != "description")
                .map(|(key, value)| (column(&key).map_or(key, str::to_owned), value.to_string()))
                .collect();
            (name, selector)
        })
        .collect()
}

fn key(
    name: &str,
    value: &Value,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Option<Key> {
    let kinds = ["plain_set", "named_set", "sequential_set", "mixed_set"];
    let (kind, settings) = kinds
        .iter()
        .find_map(|kind| get(value, kind).map(|settings| (*kind, settings)))?;
    // Options stand at the key or inside the set; the key's win.
    let option = |option: &str| get(value, option).or_else(|| get(&settings, option));
    let set = match kind {
        "plain_set" => Set::Plain,
        "named_set" => {
            let groups = selectors(&settings);
            for required in texts(option("required")) {
                if !groups.iter().any(|(group, _)| *group == required) {
                    errors.push(format!(
                        "Suffix '{name}' named_set: required group '{required}' is not defined"
                    ));
                }
            }
            Set::Named(groups)
        }
        "sequential_set" => {
            let by = match get(&settings, "by_entity") {
                Some(single) => vec![entity(&single)],
                None => texts(get(&settings, "by_entities"))
                    .iter()
                    .map(|e| entity(&Value::string(e.as_str())))
                    .collect(),
            };
            if get(&settings, "by_entities").is_some() && by.len() == 1 {
                warnings.push(format!(
                    "Suffix '{name}' sequential_set: 'by_entities' has only one entity, consider using 'by_entity' instead"
                ));
            }
            let order = get(&settings, "order").map(|order| order.to_string());
            if let Some(order) = order
                .as_deref()
                .filter(|o| !matches!(*o, "hierarchical" | "flat"))
            {
                errors.push(format!(
                    "Suffix '{name}' sequential_set: 'order' must be 'hierarchical' or 'flat', got '{order}'"
                ));
            }
            Set::Sequential {
                by,
                hierarchical: order.as_deref() == Some("hierarchical"),
            }
        }
        _ => Set::Mixed {
            sequence: get(&settings, "sequential_dimension")
                .map(|e| entity(&e))
                .unwrap_or_default(),
            groups: selectors(&get(&settings, "named_groups").unwrap_or(Value::Null)),
        },
    };
    let mut extensions: Vec<String> = ["nii.gz", "nii", "json"].map(String::from).to_vec();
    for extra in texts(option("additional_extensions")) {
        let extra = extra.trim_start_matches('.').to_owned();
        if !extensions.contains(&extra) {
            extensions.push(extra);
        }
    }
    Some(Key {
        name: name.to_owned(),
        suffix: option("suffix_maps_to").map_or_else(|| name.to_owned(), |s| s.to_string()),
        extensions,
        excluded: texts(option("exclude_entities"))
            .iter()
            .map(|e| entity(&Value::string(e.as_str())))
            .collect(),
        cross: texts(option("include_cross_modal")),
        required: texts(option("required")),
        set,
    })
}
