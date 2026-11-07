//! nf-drs 0.1.0: `drs://` paths, read by the DRS provider of storage with the `drs` scope.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::Observer;

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["drs"],
    observer: Some(activate),
    ..Plugin::new("nf-drs", Kind::FileSystem, State::Implemented)
};

/// Hands the `drs` scope to the DRS provider that serves `drs://` paths; the plugin hears no
/// run events.
fn activate(config: &Config, _: &std::path::Path) -> Result<Option<Box<dyn Observer>>, String> {
    let text = |key| {
        (config.get(&["drs", key]))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
    };
    storage::drs::use_scope(storage::drs::Options {
        authentication: text("authProvider"),
        credential: None,
        variable: text("authSecret"),
        token_path: text("tokenEndpointPath"),
        protocol: text("accessProtocol"),
        plain_http: Vec::new(),
    });
    Ok(None)
}
