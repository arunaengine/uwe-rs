//! nf-cloudcache 0.6.2: `cloudcache.path` and `NXF_CLOUDCACHE_PATH` become the place of the
//! store, where uwe keeps its cache of task results.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["cloudcache"],
    ..Plugin::new("nf-cloudcache", Kind::FileSystem, State::Implemented)
};

/// Where the config puts the cache: `cloudcache.path`, else `variable` (the value of
/// `NXF_CLOUDCACHE_PATH`), else `workDir`; `None` unless `cloudcache.enabled` is `true`. A
/// place on this machine is refused, as in Nextflow.
pub fn location(config: &Config, variable: Option<&str>) -> Result<Option<String>, String> {
    let text = |path: &[&str]| {
        (config.get(path))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
            .filter(|text| !text.is_empty())
    };
    let path = text(&["cloudcache", "path"]);
    let enabled = matches!(
        config.get(&["cloudcache", "enabled"]),
        Some(Value::Boolean(true))
    );
    if !enabled {
        if path.is_some() {
            tracing::warn!(
                "Cloud cache is NOT enabled: `cloudcache.path` is set but `cloudcache.enabled` \
                 is not `true`, so this run uses the default cache"
            );
        }
        return Ok(None);
    }
    let path = path
        .or_else(|| variable.filter(|text| !text.is_empty()).map(str::to_owned))
        .or_else(|| text(&["workDir"]))
        .ok_or(
            "Cloud-cache path not defined - use either -with-cloudcache run option or \
                NXF_CLOUDCACHE_PATH environment variable",
        )?;
    match storage::address::scheme(&path) {
        Some(scheme) if scheme != "file" => Ok(Some(path)),
        _ => Err(format!(
            "Storage path not supported by Cloud-cache - offending value: '{path}'"
        )),
    }
}
