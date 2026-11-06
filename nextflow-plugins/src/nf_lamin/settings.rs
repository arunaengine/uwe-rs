//! The `lamin` scope of nf-lamin: the instance, the hub, the records to link and the rules
//! that pick the files to track, with the plugin's environment variables.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::regex::Pattern;
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
    pub(super) inputs: Tracking,
    pub(super) outputs: Tracking,
    /// The `report.file` of a run with `report.enabled`.
    pub(super) report: Option<String>,
}

/// Which files of one direction become artifacts, and how.
#[derive(Clone, Debug)]
pub(super) struct Tracking {
    pub(super) enabled: bool,
    pub(super) skip_work: bool,
    pub(super) skip_assets: bool,
    pub(super) include: Option<Pattern>,
    pub(super) exclude: Option<Pattern>,
    pub(super) labels: Vec<String>,
    pub(super) kind: Option<String>,
    /// Paths registered without the rules: the block's own, then those of its rules.
    pub(super) paths: Vec<Listed>,
    /// The rules of this direction, by `order`.
    pub(super) rules: Vec<Rule>,
}

/// A path of `include_paths`, with the kind and labels of its block or rule.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Listed {
    pub(super) path: String,
    pub(super) kind: Option<String>,
    pub(super) labels: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct Rule {
    pub(super) pattern: Option<Pattern>,
    pub(super) include: bool,
    pub(super) order: i64,
    pub(super) labels: Vec<String>,
    pub(super) kind: Option<String>,
}

/// How a tracked file becomes an artifact.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Choice {
    pub(super) kind: Option<String>,
    pub(super) labels: Vec<String>,
}

/// The direction of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Input,
    Output,
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
        let single = scope.get(&["artifacts"]).is_some();
        if single
            && (scope.get(&["input_artifacts"]).is_some()
                || scope.get(&["output_artifacts"]).is_some())
        {
            return Err(
                "Cannot use both 'artifacts' and 'input_artifacts'/'output_artifacts' \
                 configurations."
                    .to_owned(),
            );
        }
        let block = |own: &str, direction| match single {
            true => Tracking::read(&scope, "artifacts", direction),
            false => Tracking::read(&scope, own, direction),
        };
        let report = (config.get(&["report", "enabled"]))
            .filter(Value::truth)
            .and_then(|_| config.get(&["report", "file"]))
            .map(|file| file.to_string());
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
            inputs: block("input_artifacts", Direction::Input)?,
            outputs: block("output_artifacts", Direction::Output)?,
            report,
        }))
    }
}

impl Tracking {
    fn read(scope: &Scope, block: &str, direction: Direction) -> Result<Tracking, String> {
        let flag = |key: &str| scope.get(&[block, key]).is_none_or(|value| value.truth());
        let pattern = |path: &[&str]| scope.text(path).map(|text| compile(&text)).transpose();
        let kind = scope.text(&[block, "kind"]);
        let labels = scope.list(&[block, "ulabel_uids"]);
        let mut paths = listed(scope, &[block, "include_paths"], &kind, &labels);
        let mut rules = Vec::new();
        let entries = scope
            .get(&[block, "rules"])
            .map(|rules| crate::nf_schema::entries(&rules));
        for (name, _) in entries.unwrap_or_default() {
            let rule = |key: &'static str| [block, "rules", name.as_str(), key];
            let wanted = match scope.text(&rule("direction")).as_deref() {
                Some("input") => direction == Direction::Input,
                Some("output") => direction == Direction::Output,
                _ => true,
            };
            if !wanted
                || scope
                    .get(&rule("enabled"))
                    .is_some_and(|value| !value.truth())
            {
                continue;
            }
            let (kind, labels) = (scope.text(&rule("kind")), scope.list(&rule("ulabel_uids")));
            paths.extend(listed(scope, &rule("include_paths"), &kind, &labels));
            rules.push(Rule {
                pattern: pattern(&rule("pattern"))?,
                include: scope
                    .text(&rule("type"))
                    .is_none_or(|kind| kind != "exclude"),
                order: scope.number(&rule("order")).unwrap_or(100),
                labels,
                kind,
            });
        }
        rules.sort_by_key(|rule| rule.order);
        Ok(Tracking {
            enabled: flag("enabled"),
            skip_work: flag("exclude_work_dir"),
            skip_assets: flag("exclude_assets_dir"),
            include: pattern(&[block, "include_pattern"])?,
            exclude: pattern(&[block, "exclude_pattern"])?,
            labels,
            kind,
            paths,
            rules,
        })
    }

    /// How a file found in the run becomes an artifact, `None` when it is not tracked. The
    /// patterns find their match anywhere in the path; later rules override the kind.
    pub(super) fn decide(&self, path: &str, skipped: &[&str]) -> Option<Choice> {
        if !self.enabled || skipped.iter().any(|folder| inside(path, folder)) {
            return None;
        }
        let found = |pattern: &Pattern| pattern.find(path).unwrap_or(false);
        if self.include.as_ref().is_some_and(|pattern| !found(pattern))
            || self.exclude.as_ref().is_some_and(found)
        {
            return None;
        }
        let mut choice = Choice {
            kind: self.kind.clone(),
            labels: self.labels.clone(),
        };
        for rule in &self.rules {
            if !rule.pattern.as_ref().is_some_and(found) {
                continue;
            }
            if !rule.include {
                return None;
            }
            if rule.kind.is_some() {
                choice.kind = rule.kind.clone();
            }
            merge(&mut choice.labels, &rule.labels);
        }
        Some(choice)
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

fn compile(text: &str) -> Result<Pattern, String> {
    Pattern::new(text)
        .map_err(|error| format!("nf-lamin: the pattern '{text}' is invalid: {error}"))
}

fn inside(path: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    !folder.is_empty() && (path == folder || path.starts_with(&format!("{folder}/")))
}

/// The paths of an `include_paths` setting. A closure needs the script's interpreter, which an
/// observer does not have, so it is skipped with a warning.
fn listed(scope: &Scope, path: &[&str], kind: &Option<String>, labels: &[String]) -> Vec<Listed> {
    let value = scope.get(path);
    if let Some(Value::Closure(_)) = value {
        tracing::warn!("nf-lamin: uwe skips lamin.{}, a closure", path.join("."));
        return Vec::new();
    }
    (value
        .map(|value| texts(&value))
        .unwrap_or_default()
        .into_iter())
    .map(|path| Listed {
        path,
        kind: kind.clone(),
        labels: labels.to_vec(),
    })
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
