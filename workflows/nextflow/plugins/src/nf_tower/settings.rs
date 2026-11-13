//! The `tower` scope of the config and the environment variables that stand in for it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use super::client::{Api, Retry};

const DEFAULT_ENDPOINT: &str = "https://api.cloud.seqera.io";

/// The `tower` scope with the environment variables that stand in for it.
pub(super) struct Settings {
    pub(super) api: Api,
    /// `TOWER_WORKFLOW_ID`, set when Seqera Platform launched the run.
    pub(super) launched: Option<String>,
    /// The config as `nextflow config` prints it, secrets hidden.
    pub(super) config_text: String,
    /// The run's own files, sent when `TOWER_ALLOW_NEXTFLOW_LOGS` is `true`.
    pub(super) logs: Option<Logs>,
    pub(super) reports_file: Option<String>,
}

pub(super) struct Logs {
    pub(super) operation: String,
    pub(super) log_file: Option<String>,
    pub(super) out_file: Option<String>,
}

impl Settings {
    pub(super) fn read(
        config: &Config,
        variable: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<Settings>, String> {
        let setting = |key: &str| {
            config
                .get(&["tower", key])
                .filter(|value| !matches!(value, Value::Null))
        };
        let launched = variable("TOWER_WORKFLOW_ID").filter(|id| !id.is_empty());
        let enabled = setting("enabled").is_some_and(|value| value.truth());
        if !enabled && launched.is_none() {
            return Ok(None);
        }
        // A run that Seqera Platform launched takes its credentials only from the environment.
        let from_config = |key: &str| match launched {
            Some(_) => None,
            None => setting(key).map(|value| value.to_string()),
        };
        let endpoint = (setting("endpoint").map(|value| value.to_string()))
            .or_else(|| variable("TOWER_API_ENDPOINT"))
            .filter(|endpoint| !endpoint.is_empty() && endpoint != "-")
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());
        let endpoint = endpoint.trim_end_matches('/').to_owned();
        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            return Err(format!(
                "Only http and https are supported -- The given URL was: {}",
                crate::service::safe_url(&endpoint)
            ));
        }
        if endpoint.contains("https://api.tower.nf") {
            tracing::warn!(
                "The endpoint `https://api.tower.nf` is deprecated - Please use `https://api.cloud.seqera.io` instead"
            );
        }
        let token = match (&launched, config.get(&["tower", "accessToken"])) {
            (None, Some(token)) if !matches!(token, Value::Null) => token.to_string(),
            _ => variable("TOWER_ACCESS_TOKEN").unwrap_or_default(),
        };
        if token.is_empty() {
            return Err(
                "Missing Seqera Platform access token -- Make sure there's a variable \
                 TOWER_ACCESS_TOKEN in your environment"
                    .to_owned(),
            );
        }
        let refresh = from_config("refreshToken").or_else(|| variable("TOWER_REFRESH_TOKEN"));
        let workspace = (from_config("workspaceId"))
            .or_else(|| variable("TOWER_WORKSPACE_ID"))
            .filter(|workspace| !workspace.is_empty());
        if let Some(workspace) = &workspace
            && !workspace.chars().all(|digit| digit.is_ascii_digit())
        {
            return Err(format!(
                "Invalid Seqera Platform workspace ID: '{workspace}' \u{2014} workspace ID must \
                 be a numeric value. Check the 'tower.workspaceId' setting in your Nextflow \
                 configuration or the TOWER_WORKSPACE_ID environment variable"
            ));
        }
        let duration = |path: &[&str], default: u64| {
            let mut full = vec!["tower"];
            full.extend_from_slice(path);
            Duration::from_millis(millis(config.get(&full)).unwrap_or(default))
        };
        let number = |paths: [&[&str]; 2]| {
            (paths.iter()).find_map(|path| {
                let mut full = vec!["tower"];
                full.extend_from_slice(path);
                config.get(&full)?.to_string().parse::<f64>().ok()
            })
        };
        let delay = match config.get(&["tower", "retryPolicy", "delay"]) {
            Some(_) => duration(&["retryPolicy", "delay"], 350),
            None => duration(&["backOffDelay"], 350),
        };
        let retry = Retry {
            delay,
            max_delay: duration(&["retryPolicy", "maxDelay"], 90_000),
            attempts: number([&["retryPolicy", "maxAttempts"], &["maxRetries"]])
                .map_or(10, |attempts| attempts.max(1.0) as u32),
            jitter: number([&["retryPolicy", "jitter"], &["retryPolicy", "jitter"]])
                .unwrap_or(0.25),
            multiplier: number([&["retryPolicy", "multiplier"], &["backOffBase"]]).unwrap_or(2.0),
        };
        let logs = (variable("TOWER_ALLOW_NEXTFLOW_LOGS").as_deref() == Some("true")).then(|| {
            let operation = match variable("AWS_BATCH_JOB_ID") {
                Some(job) => format!("aws-batch::{job}"),
                None => format!("local-platform::{}", std::process::id()),
            };
            Logs {
                operation,
                log_file: variable("NXF_LOG_FILE"),
                out_file: variable("NXF_OUT_FILE"),
            }
        });
        Ok(Some(Settings {
            api: Api {
                endpoint,
                workspace,
                token,
                refresh,
                timeout: duration(&["httpReadTimeout"], 60_000),
                retry,
            },
            launched,
            config_text: config_text(&config.tree),
            logs,
            reports_file: variable("TOWER_REPORTS_FILE"),
        }))
    }
}

/// Milliseconds of a duration setting: a duration, its text, or a plain number of them.
fn millis(value: Option<Value>) -> Option<u64> {
    match value? {
        Value::Duration(millis) => u64::try_from(millis).ok(),
        Value::Number(number) => u64::try_from(number.to_i64()).ok(),
        other => {
            let parsed = nextflow_evaluator::units::parse_duration(&other.to_string());
            u64::try_from(parsed.ok()?).ok()
        }
    }
}

/// The config as `nextflow config` prints it, with the values of secret settings hidden.
fn config_text(tree: &Value) -> String {
    let mut text = String::new();
    write_block(tree, 0, &mut text);
    text
}

fn write_block(tree: &Value, depth: usize, text: &mut String) {
    let Value::Map(map) = tree else {
        return;
    };
    let indent = "   ".repeat(depth);
    let mut after_block = false;
    let entries: Vec<(String, Value)> = (map.borrow().entries.iter())
        .map(|(key, value)| (key.0.to_string(), value.clone()))
        .filter(|(_, value)| !matches!(value, Value::Map(map) if map.borrow().entries.is_empty()))
        .collect();
    for (place, (key, value)) in entries.iter().enumerate() {
        let block = matches!(value, Value::Map(_));
        if depth == 0 && place > 0 && (block || after_block) {
            text.push('\n');
        }
        if block {
            text.push_str(&format!("{indent}{key} {{\n"));
            write_block(value, depth + 1, text);
            text.push_str(&format!("{indent}}}\n"));
        } else if crate::service::secret(key) {
            text.push_str(&format!("{indent}{key} = '[secret]'\n"));
        } else {
            text.push_str(&format!("{indent}{key} = {}\n", render(value)));
        }
        after_block = block;
    }
}

fn render(value: &Value) -> String {
    let items = |items: Vec<String>| format!("[{}]", items.join(", "));
    match value {
        Value::String(text) | Value::GString(text) => {
            format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
        }
        Value::List(list) => items(list.borrow().items.iter().map(render).collect()),
        Value::Set(set) => items(
            set.borrow()
                .items
                .iter()
                .map(|key| render(&key.0))
                .collect(),
        ),
        Value::Duration(_) | Value::MemoryUnit(_) | Value::Path(_) => {
            format!("'{value}'")
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        let directory = tempfile::tempdir().expect("a folder");
        let file = directory.path().join("nextflow.config");
        std::fs::write(&file, text).expect("writes the config");
        let options = nextflow_config::Options {
            profiles: None,
            params: Value::map([]),
            project_dir: "/project".into(),
            launch_dir: "/launch".into(),
            work_dir: "/work".into(),
        };
        nextflow_config::load(&[file], &options, &mut Quiet).expect("loads")
    }

    struct Quiet;

    impl nextflow_evaluator::interpreter::Host for Quiet {
        fn print(&mut self, _: &str) {}
    }

    #[test]
    fn checks_settings() {
        let none = |_: &str| None;
        let read = |text: &str| Settings::read(&config(text), &none).map(|read| read.is_some());
        assert_eq!(read("tower.accessToken = 'x'"), Ok(false));
        let workspace = read("tower { enabled = true; accessToken = 'x'; workspaceId = 'abc' }");
        assert!(
            workspace.is_err_and(|error| error.starts_with("Invalid Seqera Platform workspace"))
        );
        let endpoint = read("tower { enabled = true; accessToken = 'x'; endpoint = 'ftp://a' }");
        assert_eq!(
            endpoint,
            Err("Only http and https are supported -- The given URL was: ftp://a".into())
        );
        let token = read("tower.enabled = true");
        assert!(token.is_err_and(|error| error.starts_with("Missing Seqera Platform access")));
        let launched = |name: &str| (name == "TOWER_WORKFLOW_ID").then(|| "abc".to_owned());
        let from_env = Settings::read(&config("tower.accessToken = 'x'"), &launched);
        assert!(
            from_env.is_err(),
            "a launched run takes the token from the environment"
        );
    }

    #[test]
    fn hides_secrets() {
        let text = config_text(
            &config("plugins { id 'nf-tower' }\ntower { enabled = true\n accessToken = 'abc' }\n")
                .tree,
        );
        assert_eq!(
            text,
            "plugins = ['nf-tower']\n\ntower {\n   enabled = true\n   accessToken = '[secret]'\n}\n"
        );
        let keys =
            "gpt.apiKey = 'a'\nlamin.api_key = 'b'\noci.privateKey = 'c'\naws.region = 'd'\n";
        let text = config_text(&config(keys).tree);
        for hidden in ["apiKey", "api_key", "privateKey"] {
            assert!(text.contains(&format!("{hidden} = '[secret]'")), "{text}");
        }
        assert!(text.contains("region = 'd'"), "{text}");
    }
}
