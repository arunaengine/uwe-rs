//! nf-jev 0.2.1: typed questions to the Jev judgment model of TypeSafe or OpenRouter, one JSON
//! request per state, with an optional cache of the answers on disk.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, parse_json};
use nextflow_evaluator::value::Value;
use sha2::{Digest, Sha256};

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_schema::entries;
use crate::nf_sqldb::thrown;
use crate::report_format::{Json, ordered_json};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["jev", "noul", "choice", "score"],
    call: Some(call),
    scopes: &["jev"],
    ..Plugin::new("nf-jev", Kind::Functions, State::Implemented)
};

/// The statuses that a request retries, as the plugin does.
const RETRIED: [u16; 5] = [429, 502, 503, 524, 529];

/// The waits before the second and the third attempt.
const WAITS: [Duration; 2] = [Duration::from_millis(250), Duration::from_millis(500)];

/// One of the two services of the same API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    TypeSafe,
    OpenRouter,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Provider::TypeSafe => "typesafe",
            Provider::OpenRouter => "openrouter",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Provider::TypeSafe => "TypeSafe",
            Provider::OpenRouter => "OpenRouter",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Provider::TypeSafe => "https://api.typesafe.ai/v1/systemone",
            Provider::OpenRouter => "https://openrouter.ai/api/alpha/decisions",
        }
    }

    fn model(self) -> &'static str {
        match self {
            Provider::TypeSafe => "jev-latest",
            Provider::OpenRouter => "typesafe/jev-1.13",
        }
    }

    fn variable(self) -> &'static str {
        match self {
            Provider::TypeSafe => "TYPESAFE_API_KEY",
            Provider::OpenRouter => "OPENROUTER_API_KEY",
        }
    }

    /// The provider whose host the address names, or a subdomain of it.
    fn of_host(endpoint: &str) -> Option<Provider> {
        let rest = endpoint
            .split_once("://")
            .map_or(endpoint, |(_, rest)| rest);
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
        let host = host
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let under = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
        if under("openrouter.ai") {
            Some(Provider::OpenRouter)
        } else if under("api.typesafe.ai") {
            Some(Provider::TypeSafe)
        } else {
            None
        }
    }
}

/// The settings of the `jev` scope after the plugin's checks.
#[derive(Clone, Debug)]
struct Settings {
    provider: Provider,
    endpoint: String,
    model: String,
    key: Option<String>,
    timeout: Duration,
    cache: Option<String>,
}

impl Settings {
    fn of(config: &Config, variable: &dyn Fn(&str) -> Option<String>) -> Result<Settings, String> {
        let text = |key: &str| {
            let found = config
                .get(&["jev", key])
                .filter(|value| !matches!(value, Value::Null));
            found
                .map(|value| value.to_string().trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let endpoint = text("endpoint");
        let provider = match text("provider").map(|name| name.to_lowercase()) {
            Some(name) => {
                let provider = [Provider::TypeSafe, Provider::OpenRouter]
                    .into_iter()
                    .find(|provider| provider.name() == name)
                    .ok_or_else(|| {
                        format!(
                            "Unknown Jev provider `{name}` - `jev.provider` must be one of: \
                             typesafe, openrouter"
                        )
                    })?;
                if let Some(endpoint) = &endpoint
                    && let Some(other) =
                        Provider::of_host(endpoint).filter(|other| *other != provider)
                {
                    return Err(format!(
                        "Conflicting Jev settings - `jev.provider` is `{name}` but `jev.endpoint` \
                         points at {} ({endpoint}); remove one of them",
                        other.title()
                    ));
                }
                provider
            }
            None => match endpoint.as_deref().and_then(Provider::of_host) {
                Some(Provider::OpenRouter) => Provider::OpenRouter,
                _ => Provider::TypeSafe,
            },
        };
        let seconds = text("timeout")
            .and_then(|seconds| seconds.parse().ok())
            .unwrap_or(30);
        Ok(Settings {
            provider,
            endpoint: endpoint.unwrap_or(provider.endpoint().to_owned()),
            model: text("model").unwrap_or(provider.model().to_owned()),
            key: text("apiKey")
                .or_else(|| variable(provider.variable()).filter(|key| !key.is_empty())),
            timeout: Duration::from_secs(seconds),
            cache: text("cacheDir"),
        })
    }
}

/// Whether a model id names one snapshot: it ends in `x.y.z` or in `-` and eight digits.
fn pinned(model: &str) -> bool {
    let last = model.rsplit(['-', '/']).next().unwrap_or_default();
    let dated = model.contains('-') && last.len() == 8 && last.chars().all(|c| c.is_ascii_digit());
    let version = model
        .rsplit(['-', '/'])
        .next()
        .unwrap_or_default()
        .split('.')
        .collect::<Vec<_>>();
    let triple = version.len() == 3
        && version
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
    dated || triple
}

/// When the run starts: the settings checks of the plugin's include, and the warning about
/// caching answers of a floating model.
pub(crate) fn start(context: &mut Context) -> Result<(), Error> {
    let settings =
        Settings::of(context.config, &|name| context.host.environment(name)).map_err(thrown)?;
    if settings.cache.is_some() && !pinned(&settings.model) {
        context.host.print(&format!(
            "WARN: Jev caching is enabled with the floating model id `{}` - cached answers will keep \
             being replayed after it moves to a newer snapshot; pin `jev.model` to the snapshot the \
             response reports, such as `jev-1.13.0` on TypeSafe or `typesafe/jev-1.13-20260917` on \
             OpenRouter\n",
            settings.model
        ));
    }
    Ok(())
}

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let argument = |place: usize| arguments.get(place).cloned().unwrap_or(Value::Null);
    let text = |key: &str| Value::string(key);
    let question = |kind: &str, criteria: Option<Value>| -> Result<Value, Error> {
        let instructions = argument(0);
        if matches!(instructions, Value::Null) || instructions.to_string().trim().is_empty() {
            return Err(thrown(format!(
                "The `instructions` of a {kind} question cannot be empty"
            )));
        }
        let mut entries = vec![
            (text("type"), text(kind)),
            (text("instructions"), instructions),
        ];
        if let Some(criteria) = criteria {
            entries.push((text("criteria"), criteria));
        }
        Ok(Value::map(entries))
    };
    let count = |value: &Value| match value {
        Value::Map(map) => map.borrow().entries.len(),
        Value::List(list) => list.borrow().items.len(),
        _ => 0,
    };
    match name {
        "noul" if arguments.len() < 2 => question("noul", None),
        "noul" => {
            let criteria = argument(1);
            let keys: Vec<String> = entries(&criteria).into_iter().map(|(key, _)| key).collect();
            if !(keys.contains(&"true".to_owned()) && keys.contains(&"false".to_owned())) {
                return Err(thrown(format!(
                    "The criteria of a noul question need a `true` and a `false` entry - got [{}]",
                    keys.join(", ")
                )));
            }
            question("noul", Some(criteria))
        }
        "choice" | "score" => {
            let criteria = argument(1);
            let found = count(&criteria);
            let least = if name == "choice" {
                "options"
            } else {
                "levels"
            };
            if found < 2 {
                let article = if name == "choice" {
                    "A choice"
                } else {
                    "A score"
                };
                return Err(thrown(format!(
                    "{article} question needs at least two {least} - got {found}"
                )));
            }
            question(name, Some(criteria))
        }
        "jev" => jev(&argument(0), &argument(1), context),
        _ => Err(Error::Unsupported {
            message: format!("the function `{name}` of the plugin `nf-jev`"),
            span: Default::default(),
        }),
    }
}

/// `jev(state, questions)`: the `answers` of the service or of the cache.
fn jev(state: &Value, questions: &Value, context: &mut Context) -> Result<Value, Error> {
    if count_entries(questions) == 0 {
        return Err(thrown("At least one question is required to call Jev"));
    }
    let settings =
        Settings::of(context.config, &|name| context.host.environment(name)).map_err(thrown)?;
    let Some(key) = settings.key.clone() else {
        let provider = settings.provider;
        return Err(thrown(format!(
            "Missing {} credential - set `jev.apiKey` in the Nextflow configuration, or the {} \
             environment variable",
            provider.title(),
            provider.variable()
        )));
    };
    let state = match state {
        Value::Map(_) | Value::List(_) => ordered_json(state),
        other => Json::from(other.to_string()),
    };
    let body = Json::object([
        ("model", Json::from(settings.model.as_str())),
        ("state", state),
        ("questions", ordered_json(questions)),
    ])
    .compact();
    let launch = PathBuf::from(context.host.launch_dir());
    let cached = settings.cache.as_ref().map(|folder| {
        let hex: String = Sha256::digest(body.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        launch
            .join(folder)
            .join(&hex[..2])
            .join(format!("{}.json", &hex[2..]))
    });
    if let Some(file) = &cached
        && let Ok(text) = std::fs::read_to_string(file)
        && let Ok(value) = parse_json(&text)
        && let Some(answers) = crate::nf_schema::get(&value, "answers")
    {
        return Ok(answers);
    }
    let text = send(&settings, &key, &body).map_err(thrown)?;
    let parsed = parse_json(&text).map_err(|_| {
        let shown: String = text.chars().take(200).collect();
        let more = if text.chars().count() > 200 {
            "..."
        } else {
            ""
        };
        thrown(format!("Unable to parse the Jev response - {shown}{more}"))
    })?;
    let Some(answers) = crate::nf_schema::get(&parsed, "answers") else {
        let keys: Vec<String> = entries(&parsed).into_iter().map(|(key, _)| key).collect();
        return Err(thrown(format!(
            "Unexpected Jev response - expected an `answers` object, got: [{}]",
            keys.join(", ")
        )));
    };
    if let Some(file) = cached {
        store(&file, &text);
    }
    Ok(answers)
}

fn count_entries(value: &Value) -> usize {
    match value {
        Value::Map(map) => map.borrow().entries.len(),
        _ => 0,
    }
}

/// Posts the body, retrying the statuses and network errors of [`RETRIED`] up to 3 attempts.
fn send(settings: &Settings, key: &str, body: &str) -> Result<String, String> {
    let client = storage::http_client()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let runtime = runtime.map_err(|error| error.to_string())?;
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(WAITS[attempt - 1]);
        }
        let request = (client.post(&settings.endpoint).timeout(settings.timeout))
            .bearer_auth(key)
            .header("Content-Type", "application/json")
            .body(body.to_owned());
        let answer = runtime.block_on(async {
            let response = request.send().await.map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let text = response.text().await.map_err(|error| error.to_string())?;
            Ok::<_, String>((status, text))
        });
        match answer {
            Ok((200, text)) => return Ok(text),
            Ok((status, _)) if RETRIED.contains(&status) => last = format!("status {status}"),
            Ok((status, text)) => {
                return Err(format!("Jev request failed with status {status} - {text}"));
            }
            Err(error) => last = error,
        }
    }
    Err(format!("Jev request failed after 3 attempts - {last}"))
}

/// Writes the answer to the cache through a temporary file in the same folder; a failure is
/// ignored, as in the plugin.
fn store(file: &Path, text: &str) {
    let Some(folder) = file.parent() else {
        return;
    };
    let temporary = folder.join(format!(".jev-{}.tmp", std::process::id()));
    let written = std::fs::create_dir_all(folder)
        .and_then(|()| std::fs::write(&temporary, text))
        .and_then(|()| std::fs::rename(&temporary, file));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_pinned_models() {
        assert!(pinned("jev-1.13.0"));
        assert!(pinned("typesafe/jev-1.13-20260917"));
        assert!(!pinned("jev-latest"));
        assert!(!pinned("typesafe/jev-1.13"));
        assert_eq!(
            Provider::of_host("https://eu.openrouter.ai/x"),
            Some(Provider::OpenRouter)
        );
        assert_eq!(Provider::of_host("http://127.0.0.1:9/x"), None);
    }
}
