//! nf-synapse 0.1.3: `syn://` paths, read and published by the Synapse provider of storage.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::Observer;

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["synapse"],
    observer: Some(activate),
    ..Plugin::new("nf-synapse", Kind::FileSystem, State::Implemented)
};

/// Hands the `synapse` scope to the Synapse provider that serves `syn://` paths; the plugin
/// hears no run events.
fn activate(config: &Config, _: &std::path::Path) -> Result<Option<Box<dyn Observer>>, String> {
    let text = |key| {
        (config.get(&["synapse", key]))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
    };
    storage::synapse::use_scope(storage::synapse::Options {
        endpoint: text("endpoint"),
        token: text("authToken"),
    });
    Ok(None)
}
