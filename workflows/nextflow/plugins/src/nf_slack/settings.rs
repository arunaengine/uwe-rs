//! The `slack` scope of the config: where messages go and which ones the plugin sends.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::report_format::value_json;

/// The Slack Web API, which `slack.bot.apiUrl` replaces for tests and Slack-like servers.
const API: &str = "https://slack.com/api/";

/// The settings of a configured plugin.
pub(super) struct Settings {
    pub(super) mode: Mode,
    /// Later messages answer the start message in its thread.
    pub(super) threads: bool,
    pub(super) validate: bool,
    pub(super) start: Notice,
    pub(super) complete: Notice,
    pub(super) error: Notice,
    /// The least time between progress updates, when they are on.
    pub(super) progress: Option<Duration>,
    pub(super) reactions: Option<Reactions>,
}

pub(super) enum Mode {
    Bot {
        token: String,
        channel: String,
        /// The address of the Web API, ending with `/`.
        api: String,
    },
    Webhook {
        url: String,
    },
}

/// One kind of message: the start, the success or the failure of a run.
pub(super) struct Notice {
    pub(super) enabled: bool,
    pub(super) message: Message,
    pub(super) command_line: bool,
    pub(super) footer: bool,
    /// The standard fields to show; empty for all.
    pub(super) fields: Vec<String>,
    pub(super) resource_usage: bool,
    /// Files uploaded after the message, in bot mode.
    pub(super) files: Vec<String>,
}

pub(super) enum Message {
    Text(String),
    Custom(Custom),
}

/// A message given as a map: its text, the fields it lists and fields of its own.
pub(super) struct Custom {
    pub(super) text: Option<String>,
    pub(super) fields: Vec<String>,
    pub(super) extra: Vec<(String, String)>,
}

pub(super) struct Reactions {
    pub(super) start: String,
    pub(super) success: String,
    pub(super) error: String,
}

impl Notice {
    /// Whether a standard field is shown.
    pub(super) fn shows(&self, field: &str) -> bool {
        self.fields.is_empty() || self.fields.iter().any(|name| name == field)
    }
}

impl Settings {
    /// The settings, `None` when the plugin is off or has nowhere to send to.
    pub(super) fn read(config: &Config) -> Option<Settings> {
        let get = |path: &[&str]| {
            let mut full = vec!["slack"];
            full.extend_from_slice(path);
            config
                .get(&full)
                .filter(|value| !matches!(value, Value::Null))
        };
        let flag = |path: &[&str], default: bool| get(path).map_or(default, |value| value.truth());
        let text = |path: &[&str]| get(path).map(|value| value.to_string());
        if !flag(&["enabled"], true) {
            return None;
        }
        let mode = match (text(&["bot", "token"]), text(&["webhook", "url"])) {
            (Some(token), _) => {
                let Some(channel) = text(&["bot", "channel"]) else {
                    tracing::warn!(
                        "Slack plugin: Bot channel is required when using bot token \u{2014} \
                         plugin will be disabled"
                    );
                    return None;
                };
                let api = text(&["bot", "apiUrl"]).unwrap_or_else(|| API.to_owned());
                let api = format!("{}/", api.trim_end_matches('/'));
                Mode::Bot {
                    token,
                    channel,
                    api,
                }
            }
            (None, Some(url)) => Mode::Webhook { url },
            (None, None) => return None,
        };
        let bot = matches!(mode, Mode::Bot { .. });
        let notice = |scope: &str, default: &str| {
            let message = match get(&[scope, "message"]) {
                Some(value @ Value::Map(_)) => {
                    let map = value_json(&value);
                    let list = |key: &str| {
                        (map[key].as_array().into_iter().flatten())
                            .map(|item| item.as_str().map_or(item.to_string(), str::to_owned))
                            .collect()
                    };
                    let extra = (map["customFields"].as_array().into_iter().flatten())
                        .map(|field| (plain(&field["title"]), plain(&field["value"])))
                        .collect();
                    Message::Custom(Custom {
                        text: map["text"].as_str().map(str::to_owned),
                        fields: list("includeFields"),
                        extra,
                    })
                }
                Some(value) => Message::Text(value.to_string()),
                None => Message::Text(default.to_owned()),
            };
            let fields = get(&[scope, "includeFields"]).map(|value| value_json(&value));
            let files = get(&[scope, "files"]).map(|value| value_json(&value));
            let texts = |value: Option<serde_json::Value>| -> Vec<String> {
                (value
                    .as_ref()
                    .and_then(|value| value.as_array())
                    .into_iter()
                    .flatten())
                .map(plain)
                .collect()
            };
            Notice {
                enabled: flag(&[scope, "enabled"], true),
                message,
                command_line: flag(&[scope, "includeCommandLine"], true),
                footer: flag(&[scope, "showFooter"], true),
                fields: texts(fields),
                resource_usage: flag(&[scope, "includeResourceUsage"], true),
                files: texts(files),
            }
        };
        let progress = (bot && flag(&["onProgress", "enabled"], false))
            .then(|| interval(text(&["onProgress", "interval"]).as_deref()));
        let reactions = (bot && flag(&["reactions", "enabled"], true)).then(|| Reactions {
            start: text(&["reactions", "onStart"]).unwrap_or_else(|| "rocket".into()),
            success: text(&["reactions", "onSuccess"]).unwrap_or_else(|| "white_check_mark".into()),
            error: text(&["reactions", "onError"]).unwrap_or_else(|| "x".into()),
        });
        let threads = match get(&["useThreads"]).or_else(|| get(&["bot", "useThreads"])) {
            Some(value) => value.truth(),
            None => bot,
        };
        Some(Settings {
            mode,
            threads: bot && threads,
            validate: flag(&["validateOnStartup"], true),
            start: notice("onStart", "🚀 *Pipeline started*"),
            complete: notice("onComplete", "✅ *Pipeline completed successfully*"),
            error: notice("onError", "❌ *Pipeline failed*"),
            progress,
            reactions,
        })
    }
}

/// The text of a JSON value, without quotes for text.
fn plain(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `onProgress.interval`: `<n>s`, `<n>m`, `<n>h` or plain milliseconds; else five minutes.
fn interval(text: Option<&str>) -> Duration {
    let fallback = Duration::from_secs(300);
    let Some(text) = text.map(str::trim) else {
        return fallback;
    };
    let (number, factor) = match text.chars().last() {
        Some('s') => (&text[..text.len() - 1], 1_000),
        Some('m') => (&text[..text.len() - 1], 60_000),
        Some('h') => (&text[..text.len() - 1], 3_600_000),
        _ => (text, 1),
    };
    match number.parse::<u64>() {
        Ok(number) => Duration::from_millis(number * factor),
        Err(_) => fallback,
    }
}
