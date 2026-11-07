//! nf-gc 0.3.0: frees the intermediate files of a run while it runs, an engine feature of uwe.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{self, Kind, Plugin, State};
use crate::observer::Observer;

// The runtime does the work (`cleanup` of the crate), so the plugin only checks its config.
pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["nfGc"],
    observer: Some(activate),
    ..Plugin::new("nf-gc", Kind::Observer, State::Implemented)
};

/// The `gc_mode` of the config when it names the plugin: `artifact`, the default, or `process`.
/// uwe frees files at the same moments in both modes, once no node of the run can pass them on.
pub(crate) fn mode(config: &Config) -> Result<Option<String>, String> {
    if !catalogue::configured(config)
        .iter()
        .any(|id| id == PLUGIN.id)
    {
        return Ok(None);
    }
    let mode = match config.get(&["nfGc"]) {
        None | Some(Value::Null) => None,
        Some(Value::Map(_)) => config.get(&["nfGc", "gc_mode"]).filter(Value::truth),
        Some(_) => return Err("nf-gc config 'nfGc' must be a configuration scope".into()),
    };
    let Some(mode) = mode else {
        return Ok(Some("artifact".into()));
    };
    match mode.to_string().trim() {
        mode @ ("artifact" | "process") => Ok(Some(mode.to_owned())),
        _ => Err(format!(
            "Unsupported nf-gc gc_mode '{mode}'. Supported values: process, artifact"
        )),
    }
}

fn activate(config: &Config, _: &std::path::Path) -> Result<Option<Box<dyn Observer>>, String> {
    let mode = mode(config)?.unwrap_or_default();
    match config.get(&["nfGc", "gc_mode"]).filter(Value::truth) {
        Some(_) => tracing::info!("nf-gc using gc_mode='{mode}'"),
        None => tracing::info!("nf-gc gc_mode not configured; using default 'artifact'"),
    }
    Ok(None)
}
