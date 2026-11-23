//! nf-irods 0.1.2: `irods://` paths, read and written by the iRODS provider of storage.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::Observer;

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["irods"],
    observer: Some(activate),
    ..Plugin::new("nf-irods", Kind::FileSystem, State::Implemented)
};

/// Checks the `irods` scope, which reaches the iRODS provider of the run through
/// [`crate::catalogue::scopes`]; the plugin hears no run events.
fn activate(
    config: &Config,
    _: &std::path::Path,
    _: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    options(config)?;
    Ok(None)
}

/// The options of the `irods` scope for the iRODS provider that serves `irods://` paths.
pub(crate) fn options(config: &Config) -> Result<storage::irods::Options, String> {
    let text = |key| {
        (config.get(&["irods", key]))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
    };
    let port = match text("port") {
        Some(port) => Some(
            (port.parse())
                .map_err(|_| format!("irods.port must be a port number, like 1247, not {port}"))?,
        ),
        None => None,
    };
    Ok(storage::irods::Options {
        host: text("host"),
        port,
        user: text("username"),
        zone: text("zone"),
        password: text("password"),
        authentication: text("authenticationScheme"),
        negotiation: text("sslNegotiationPolicy"),
    })
}
