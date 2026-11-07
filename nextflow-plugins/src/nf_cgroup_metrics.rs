//! nf-cgroup-metrics 1.0.1: the memory fields of the trace from the cgroup of each container task.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{self, Kind, Plugin, State};
use crate::observer::Observer;

// The task wrapper samples the cgroup of every container task, so the plugin needs no observer
// of its own; the trace reads its switch through `enabled`.
pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["cgroup"],
    observer: Some(activate),
    ..Plugin::new("nf-cgroup-metrics", Kind::Observer, State::Implemented)
};

/// Whether the trace takes `%mem`, `vmem`, `rss`, `peak_vmem` and `peak_rss` from the samples
/// of the cgroup: the config names the plugin and `cgroup.metrics.enabled` is not `false`.
pub(crate) fn enabled(config: &Config) -> bool {
    let named = catalogue::configured(config)
        .iter()
        .any(|id| id == PLUGIN.id);
    named
        && !matches!(
            config.get(&["cgroup", "metrics", "enabled"]),
            Some(Value::Boolean(false))
        )
}

fn activate(config: &Config, _: &std::path::Path) -> Result<Option<Box<dyn Observer>>, String> {
    match enabled(config) {
        true => tracing::info!("Initializing CgroupMetricsObserver for session"),
        false => tracing::info!("Cgroup metrics plugin is disabled via configuration"),
    }
    Ok(None)
}
