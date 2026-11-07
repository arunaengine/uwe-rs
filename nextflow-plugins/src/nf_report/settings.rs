//! The `nfreport` scope of nf-report and nf-crg-report: the plugin switch, the defaults of the
//! three reports and each report's own settings.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

/// One of the three reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Execution,
    Task,
    Sample,
}

impl Kind {
    pub(crate) const ALL: [Kind; 3] = [Kind::Execution, Kind::Task, Kind::Sample];

    /// The config key of the report, like `taskStatusReport`.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Kind::Execution => "executionReport",
            Kind::Task => "taskStatusReport",
            Kind::Sample => "sampleStatusReport",
        }
    }

    /// The `report_type` of the report and its default file name, like `task-status-report`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Execution => "execution-report",
            Kind::Task => "task-status-report",
            Kind::Sample => "sample-status-report",
        }
    }
}

/// The settings of one enabled report.
#[derive(Clone, Debug)]
pub(crate) struct Report {
    pub kind: Kind,
    pub formats: Vec<String>,
    /// Relative to the launch directory.
    pub folder: String,
    pub prefix: String,
    /// `None` when neither the report nor the scope sets one.
    pub suffix: Option<String>,
    pub name: String,
    pub fields: Vec<String>,
    pub template: Option<String>,
    pub link: bool,
    /// Sample names from the meta map of the inputs instead of the tag.
    pub from_meta: bool,
    pub tag_pattern: Option<String>,
    pub key_pattern: Option<String>,
    pub summary: bool,
}

/// The settings of the scope.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub enabled: bool,
    pub reports: Vec<Report>,
    pub email: bool,
}

/// A setting of the scope or of a report; closures, which uwe cannot call here, count as unset.
fn get(config: &Config, path: &[&str]) -> Option<Value> {
    let full: Vec<&str> = std::iter::once("nfreport")
        .chain(path.iter().copied())
        .collect();
    match config.get(&full) {
        None | Some(Value::Null) => None,
        Some(Value::Closure(_)) => {
            let key = path.join(".");
            tracing::warn!("Failed to evaluate closure for config key '{key}': not supported");
            None
        }
        Some(value) => Some(value),
    }
}

/// A boolean setting; another type warns and keeps the default.
fn flag(config: &Config, path: &[&str], default: bool) -> bool {
    match get(config, path) {
        None => default,
        Some(Value::Boolean(value)) => value,
        Some(_) => {
            let key = path.join(".");
            tracing::warn!(
                "Invalid value for 'nfreport.{key}', expected a Boolean. Using default: {default}"
            );
            default
        }
    }
}

/// A text setting; another type warns and keeps the default.
fn text(config: &Config, path: &[&str]) -> Option<String> {
    match get(config, path) {
        None => None,
        Some(Value::String(text) | Value::GString(text)) => Some(text.to_string()),
        Some(other) => {
            let key = path.join(".");
            tracing::warn!(
                "Invalid value for 'nfreport.{key}', expected a String. Using default: {other}"
            );
            None
        }
    }
}

fn texts(value: Option<Value>) -> Vec<String> {
    match value {
        Some(Value::List(list)) => list.borrow().items.iter().map(Value::to_string).collect(),
        Some(other) => vec![other.to_string()],
        None => Vec::new(),
    }
}

impl Settings {
    pub(crate) fn of(config: &Config) -> Settings {
        let enabled = flag(config, &["enabled"], true);
        let format = text(config, &["format"])
            .unwrap_or("json".into())
            .to_lowercase();
        let folder = text(config, &["outputDir"]).unwrap_or("./reports".into());
        let prefix = text(config, &["prefix"]).unwrap_or_default();
        let suffix = text(config, &["suffix"]);
        let link = flag(config, &["createLinkToLatestReport"], false);
        let mut reports = Vec::new();
        for kind in Kind::ALL {
            let key = kind.key();
            if !flag(config, &[key, "enabled"], false) {
                continue;
            }
            let formats = match get(config, &[key, "format"]) {
                Some(Value::List(list)) => {
                    list.borrow().items.iter().map(Value::to_string).collect()
                }
                Some(other) => vec![other.to_string().to_lowercase()],
                None => vec![format.clone()],
            };
            let own = |name: &str| text(config, &[key, name]);
            reports.push(Report {
                kind,
                formats,
                folder: own("outputDir").unwrap_or(folder.clone()),
                prefix: own("prefix").unwrap_or(prefix.clone()),
                suffix: own("suffix").or(suffix.clone()),
                name: own("outputFilename").unwrap_or(kind.name().into()),
                fields: texts(get(config, &[key, "fields"])),
                template: own("htmlTemplatePath"),
                link: flag(config, &[key, "createLinkToLatestReport"], link),
                from_meta: own("extractSampleNameFrom").as_deref() == Some("meta_map"),
                tag_pattern: own("sampleNameTagPattern"),
                key_pattern: own("sampleNameMetaKeyPattern"),
                summary: flag(config, &[key, "printCompletionSummary"], false),
            });
        }
        Settings {
            enabled,
            reports,
            email: flag(config, &["email", "enabled"], false),
        }
    }

    pub(crate) fn report(&self, kind: Kind) -> Option<&Report> {
        self.reports.iter().find(|report| report.kind == kind)
    }
}

/// The `nfreport` scope as a map, empty when the config has none.
pub(crate) fn scope(config: &Config) -> Value {
    match config.get(&["nfreport"]) {
        Some(value @ Value::Map(_)) => value,
        _ => Value::map(Vec::new()),
    }
}
