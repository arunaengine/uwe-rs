//! nf-synapse 0.1.3: `syn://` paths, read and published by the Synapse provider of storage.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["synapse"],
    ..Plugin::new("nf-synapse", Kind::FileSystem, State::Implemented)
};

/// The options of the `synapse` scope for the Synapse provider that serves `syn://` paths of
/// the run; the plugin hears no run events.
pub(crate) fn options(config: &Config) -> storage::synapse::Options {
    let text = |key| {
        (config.get(&["synapse", key]))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
    };
    storage::synapse::Options {
        endpoint: text("endpoint"),
        token: text("authToken"),
    }
}
