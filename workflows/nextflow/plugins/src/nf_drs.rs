//! nf-drs 0.1.0: `drs://` paths, read by the DRS provider of storage with the `drs` scope.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["drs"],
    ..Plugin::new("nf-drs", Kind::FileSystem, State::Implemented)
};

/// The options of the `drs` scope for the DRS provider that serves `drs://` paths of the run;
/// the plugin hears no run events.
pub(crate) fn options(config: &Config) -> storage::drs::Options {
    let text = |key| {
        (config.get(&["drs", key]))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
    };
    storage::drs::Options {
        authentication: text("authProvider"),
        credential: None,
        variable: text("authSecret"),
        token_path: text("tokenEndpointPath"),
        protocol: text("accessProtocol"),
        plain_http: Vec::new(),
    }
}
