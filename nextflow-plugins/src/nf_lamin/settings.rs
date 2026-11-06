//! The `lamin` scope of nf-lamin: the instance, the hub and the records to link, with the
//! plugin's environment variables.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use super::client::{Hub, Retry};

const HUB: &str = "https://hub.lamin.ai";
const WEB: &str = "https://lamin.ai";

/// Reads an environment variable; empty values count as unset.
pub(super) type Variable<'a> = &'a dyn Fn(&str) -> Option<String>;

/// What the run tracking needs, read once when the run starts.
pub(super) struct Settings {
    pub(super) hub: Hub,
    /// The base of the links in log messages.
    pub(super) web: String,
    pub(super) dry_run: bool,
    pub(super) transform_uid: Option<String>,
    pub(super) run_uid: Option<String>,
    pub(super) projects: Vec<String>,
    pub(super) labels: Vec<String>,
    pub(super) run_labels: Vec<String>,
    pub(super) transform_labels: Vec<String>,
    pub(super) space: Option<String>,
    pub(super) branch: Option<String>,
}

impl Settings {
    /// The settings, `None` when no instance is set, which leaves tracking off.
    pub(super) fn read(config: &Config, variable: Variable) -> Result<Option<Settings>, String> {
        let Some(slug) = instance(config, variable) else {
            return Ok(None);
        };
        let (owner, name) = (slug.split_once('/'))
            .filter(|(owner, name)| !owner.is_empty() && !name.is_empty())
            .ok_or_else(|| format!("lamin.instance must be 'owner/name', not '{slug}'"))?;
        let scope = Scope(config);
        let number = |key: &str, default: i64| scope.number(&["api", key]).unwrap_or(default);
        let positive = |value: i64| u64::try_from(value).unwrap_or(0);
        let hub = Hub {
            url: (scope.text(&["api", "supabase_api_url"]))
                .or_else(|| variable("SUPABASE_API_URL"))
                .unwrap_or_else(|| HUB.to_owned()),
            anon_key: (scope.text(&["api", "supabase_anon_key"]))
                .or_else(|| variable("SUPABASE_ANON_KEY")),
            api_key: scope
                .text(&["api_key"])
                .or_else(|| variable("LAMIN_API_KEY")),
            owner: owner.to_owned(),
            name: name.to_owned(),
            retry: Retry {
                attempts: u32::try_from(number("max_retries", 3)).unwrap_or(0),
                delay: positive(number("retry_delay", 100)),
                most: positive(number("max_retry_delay", 30_000)),
            },
        };
        let dry_run = match scope.get(&["dry_run"]) {
            Some(value) => value.truth() && value.to_string() != "false",
            None => variable("LAMIN_DRY_RUN").is_some_and(|value| value == "true"),
        };
        let projects = match scope.get(&["project_uids"]) {
            Some(value) => texts(&value),
            None => {
                (variable("LAMIN_CURRENT_PROJECT").map(|text| split(&text))).unwrap_or_default()
            }
        };
        Ok(Some(Settings {
            hub,
            web: scope
                .text(&["api", "web_url"])
                .unwrap_or_else(|| WEB.to_owned()),
            dry_run,
            transform_uid: scope
                .text(&["transform_uid"])
                .or_else(|| variable("LAMIN_TRANSFORM_UID")),
            run_uid: scope
                .text(&["run_uid"])
                .or_else(|| variable("LAMIN_RUN_UID")),
            projects,
            labels: scope.list(&["ulabel_uids"]),
            run_labels: scope.list(&["run", "ulabel_uids"]),
            transform_labels: scope.list(&["transform", "ulabel_uids"]),
            space: scope.text(&["space_uid"]),
            branch: scope.text(&["branch_uid"]),
        }))
    }
}

/// The instance `owner/name` of the scope or `LAMIN_CURRENT_INSTANCE`.
pub(super) fn instance(config: &Config, variable: Variable) -> Option<String> {
    Scope(config)
        .text(&["instance"])
        .or_else(|| variable("LAMIN_CURRENT_INSTANCE"))
}

/// Texts of a list, or of comma-separated text.
pub(super) fn texts(value: &Value) -> Vec<String> {
    match value {
        Value::Null => Vec::new(),
        Value::List(list) => (list.borrow().items.iter())
            .filter(|item| !matches!(item, Value::Null))
            .map(Value::to_string)
            .collect(),
        other => split(&other.to_string()),
    }
}

/// Adds the labels that the list lacks.
pub(super) fn merge(labels: &mut Vec<String>, more: &[String]) {
    for label in more {
        if !labels.contains(label) {
            labels.push(label.clone());
        }
    }
}

fn split(text: &str) -> Vec<String> {
    (text.split(','))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The `lamin` scope of a config.
struct Scope<'a>(&'a Config);

impl Scope<'_> {
    fn get(&self, path: &[&str]) -> Option<Value> {
        let mut full = vec!["lamin"];
        full.extend_from_slice(path);
        self.0
            .get(&full)
            .filter(|value| !matches!(value, Value::Null))
    }

    fn text(&self, path: &[&str]) -> Option<String> {
        self.get(path)
            .map(|value| value.to_string())
            .filter(|text| !text.is_empty())
    }

    fn list(&self, path: &[&str]) -> Vec<String> {
        self.get(path)
            .map(|value| texts(&value))
            .unwrap_or_default()
    }

    fn number(&self, path: &[&str]) -> Option<i64> {
        self.text(path)?.trim().parse().ok()
    }
}
