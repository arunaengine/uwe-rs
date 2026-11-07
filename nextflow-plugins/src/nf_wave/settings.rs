//! Reads the `wave` scope and the Seqera Platform account of a Nextflow config for nf-wave.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::units::parse_duration;
use nextflow_evaluator::value::Value;
use serde_json::{Value as Json, json};

use crate::service::safe_url;

/// The sources of a task that `wave.strategy` can name.
const SOURCES: [&str; 3] = ["container", "dockerfile", "conda"];

pub(super) struct Settings {
    /// Whether uwe asks the service; else it keeps its own images.
    pub(super) service: bool,
    pub(super) endpoint: String,
    pub(super) freeze: bool,
    pub(super) mirror: bool,
    pub(super) strategy: Vec<String>,
    pub(super) repository: Option<String>,
    pub(super) cache_repository: Option<String>,
    pub(super) template: Option<String>,
    pub(super) conda: Json,
    pub(super) compression: Json,
    pub(super) scan_mode: Option<String>,
    pub(super) scan_levels: Option<Vec<String>>,
    pub(super) tower: Tower,
    /// How long uwe waits for a build.
    pub(super) build_limit: Duration,
    /// How long an answer serves the same request again.
    pub(super) reuse: Duration,
    /// The pause between requests, from `httpClient.maxRate`.
    pub(super) spacing: Duration,
    pub(super) retry: Retry,
}

/// The retries of requests that fail on the network or with 429 or 5xx.
pub(super) struct Retry {
    pub(super) delay: Duration,
    pub(super) longest: Duration,
    pub(super) attempts: u32,
    pub(super) jitter: f64,
    pub(super) factor: f64,
}

/// The Seqera Platform account that Wave knows the user by.
pub(super) struct Tower {
    pub(super) token: Option<String>,
    pub(super) refresh: Option<String>,
    pub(super) workspace: Option<String>,
    pub(super) endpoint: String,
}

/// The settings of a config; `None` unless `wave.enabled` is set and the environment variable
/// `NXF_DISABLE_WAVE_SERVICE` is not.
pub(super) fn read(
    config: &Config,
    variable: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<Settings>, String> {
    let set = |path: &[&str]| config.get(path).filter(Value::truth);
    let text = |path: &[&str]| set(path).map(|value| value.to_string());
    let flag = |key: &str| set(&["wave", key]).is_some();
    if !flag("enabled") || variable("NXF_DISABLE_WAVE_SERVICE").is_some() {
        return Ok(None);
    }
    let given = text(&["wave", "endpoint"]).or_else(|| variable("WAVE_API_ENDPOINT"));
    let endpoint = given.clone().unwrap_or("https://wave.seqera.io".into());
    if !endpoint.starts_with("http:") && !endpoint.starts_with("https:") {
        return Err(format!(
            "Endpoint URL should start with 'http:' or 'https:' protocol prefix - offending value: '{}'",
            safe_url(&endpoint)
        ));
    }
    let repository = |key: &str| -> Result<Option<String>, String> {
        match text(&["wave", "build", key]) {
            Some(value) if value.contains("://") => Err(format!(
                "Config setting 'wave.build.{key}' should not include any protocol prefix - offending value: '{value}'"
            )),
            value => Ok(value),
        }
    };
    let strategy = match set(&["wave", "strategy"]) {
        Some(value) => list(&value),
        None => SOURCES.map(str::to_owned).to_vec(),
    };
    if let Some(wrong) = (strategy.iter()).find(|item| !SOURCES.contains(&item.as_str())) {
        return Err(format!(
            "Invalid value for 'wave.strategy' configuration attribute - offending value: {wrong}"
        ));
    }
    let (freeze, mirror) = (flag("freeze"), flag("mirror"));
    let target = repository("repository")?;
    if mirror && freeze {
        return Err(
            "Wave configuration setting 'wave.mirror' and 'wave.freeze' conflicts each other"
                .into(),
        );
    }
    if mirror && target.is_none() {
        return Err("Wave configuration setting 'wave.mirror' requires the use of 'wave.build.repository' to define the target registry".into());
    }
    let scan_mode = text(&["wave", "scan", "mode"]);
    let number = |path: &[&str]| text(path).and_then(|value| value.parse::<f64>().ok());
    let conda =
        |key: &str, default: &str| text(&["wave", "build", "conda", key]).unwrap_or(default.into());
    let compression = match text(&["wave", "build", "compression", "mode"]) {
        None => Json::Null,
        Some(mode) => json!({
            "mode": mode,
            "level": number(&["wave", "build", "compression", "level"]).map(|level| level as i64),
            "force": set(&["wave", "build", "compression", "force"]).map(|_| true),
        }),
    };
    let levels = set(&["wave", "scan", "allowedLevels"]).map(|value| list(&value));
    let tower = |key: &str, name: &str| text(&["tower", key]).or_else(|| variable(name));
    let time = |path: &[&str], default: Duration| duration(config, path, default);
    let retry = |key: &'static str| ["wave", "retryPolicy", key];
    Ok(Some(Settings {
        service: given.is_some() || freeze || mirror || scan_mode.is_some() || target.is_some(),
        endpoint: endpoint.trim_end_matches('/').to_owned(),
        freeze,
        mirror,
        strategy,
        repository: target,
        cache_repository: repository("cacheRepository")?,
        template: text(&["wave", "build", "template"]),
        conda: json!({
            "mambaImage": conda("mambaImage", "mambaorg/micromamba:1.5.10-noble"),
            "basePackages": conda("basePackages", "conda-forge::procps-ng"),
            "commands": set(&["wave", "build", "conda", "commands"]).map(|value| list(&value)),
            "baseImage": conda("baseImage", "ubuntu:24.04"),
        }),
        compression,
        scan_mode,
        scan_levels: levels.map(|levels| levels.iter().map(|level| level.to_uppercase()).collect()),
        tower: Tower {
            token: tower("accessToken", "TOWER_ACCESS_TOKEN"),
            refresh: tower("refreshToken", "TOWER_REFRESH_TOKEN"),
            workspace: tower("workspaceId", "TOWER_WORKSPACE_ID"),
            endpoint: tower("endpoint", "TOWER_API_ENDPOINT")
                .unwrap_or("https://api.cloud.seqera.io".into()),
        },
        build_limit: time(&["wave", "build", "maxDuration"], Duration::from_secs(2400)),
        reuse: time(
            &["wave", "tokens", "cache", "maxDuration"],
            Duration::from_secs(1800),
        ),
        spacing: rate(text(&["wave", "httpClient", "maxRate"]).as_deref())?,
        retry: Retry {
            delay: time(&retry("delay"), Duration::from_millis(450)),
            longest: time(&retry("maxDelay"), Duration::from_secs(15)),
            attempts: number(&retry("maxAttempts")).map_or(10, |count| count as u32),
            jitter: number(&retry("jitter")).unwrap_or(0.25),
            factor: number(&retry("multiplier")).unwrap_or(2.0),
        },
    }))
}

fn duration(config: &Config, path: &[&str], default: Duration) -> Duration {
    let millis = match config.get(path) {
        Some(Value::Duration(millis)) => Some(millis),
        Some(value) if value.truth() => parse_duration(&value.to_string()).ok(),
        _ => None,
    };
    let millis = millis.map(|millis| u64::try_from(millis).unwrap_or(0));
    millis.map_or(default, Duration::from_millis)
}

/// The pause between requests for a rate like `1/sec` or `10/min`; one second by default.
fn rate(text: Option<&str>) -> Result<Duration, String> {
    let Some(text) = text else {
        return Ok(Duration::from_secs(1));
    };
    let wrong = || format!("Invalid rate value: {text}");
    let (count, unit) = text.split_once('/').ok_or_else(wrong)?;
    let count: f64 = count.trim().parse().map_err(|_| wrong())?;
    let seconds = match unit.trim() {
        "s" | "sec" | "second" => 1.0,
        "m" | "min" | "minute" => 60.0,
        "h" | "hour" => 3600.0,
        _ => return Err(wrong()),
    };
    if count <= 0.0 {
        return Err(wrong());
    }
    Ok(Duration::from_secs_f64(seconds / count))
}

/// The items of a list or of comma-separated text.
fn list(value: &Value) -> Vec<String> {
    match value {
        Value::List(list) => list.borrow().items.iter().map(Value::to_string).collect(),
        other => (other.to_string().split(','))
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect(),
    }
}
