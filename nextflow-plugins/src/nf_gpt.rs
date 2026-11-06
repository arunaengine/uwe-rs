//! nf-gpt 0.4.0: prompts to an OpenAI-compatible chat completions API, as text or as lists of
//! maps of a schema, and the `fromPrompt` factory and `prompt` operator on top of them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, parse_json};
use nextflow_evaluator::units::parse_duration;
use nextflow_evaluator::value::Value;

use crate::Context;
use crate::catalogue::{Kind, Plugin, State};
use crate::nf_schema::entries;
use crate::nf_sqldb::{split_options, thrown};
use crate::report_format::{Json, ordered_json};

pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &[
        "gptPromptForText",
        "gptPromptForData",
        "fromPrompt",
        "prompt",
    ],
    call: Some(call),
    script: Some(SCRIPT),
    factories: &["fromPrompt"],
    scopes: &["gpt"],
    ..Plugin::new("nf-gpt", Kind::Functions, State::Implemented)
};

/// The `prompt` operator, which takes the channel first.
const SCRIPT: &str = "include { gptPromptForData } from 'plugin/nf-gpt'\n\
    def prompt(source, Map opts, template = null) {\n    \
    return source.flatMap { item -> gptPromptForData(opts, template != null ? template.call(item) : item.toString()) }\n}\n";

/// How long one request may take.
const LIMIT: Duration = Duration::from_secs(120);

const TEXT_OPTIONS: [&str; 6] = [
    "model",
    "temperature",
    "maxTokens",
    "numOfChoices",
    "logitBias",
    "debug",
];
const DATA_OPTIONS: [&str; 5] = ["schema", "model", "temperature", "maxTokens", "debug"];

/// The settings of the `gpt` scope.
struct Settings {
    key: String,
    endpoint: String,
    model: String,
    temperature: Option<f64>,
    tokens: Option<i64>,
    retry: Retry,
}

/// The retries of requests that fail on the network or with 429 or 5xx.
struct Retry {
    delay: Duration,
    longest: Duration,
    attempts: u32,
    jitter: f64,
}

fn duration(config: &Config, path: &[&str], default: Duration) -> Duration {
    match config.get(path) {
        Some(Value::Duration(millis)) => Duration::from_millis(u64::try_from(millis).unwrap_or(0)),
        Some(value) if value.truth() => parse_duration(&value.to_string())
            .map(|millis| Duration::from_millis(u64::try_from(millis).unwrap_or(0)))
            .unwrap_or(default),
        _ => default,
    }
}

impl Settings {
    fn of(config: &Config, context: &Context) -> Result<Settings, Error> {
        let text = |key: &str| {
            let found = config.get(&["gpt", key]).filter(|value| value.truth());
            found.map(|value| value.to_string())
        };
        let key = text("apiKey")
            .or_else(|| context.host.environment("OPENAI_API_KEY"))
            .filter(|key| !key.is_empty())
            .ok_or_else(|| thrown("Missing ChatGPT API key"))?;
        let endpoint = text("endpoint").unwrap_or("https://api.openai.com".to_owned());
        if endpoint.ends_with('/') {
            return Err(thrown(format!(
                "Endpoint url must not end with a slash - offending value: {endpoint}"
            )));
        }
        let number = |key: &str| text(key).and_then(|value| value.parse::<f64>().ok());
        let retry = |key: &'static str| ["gpt", "retryPolicy", key];
        Ok(Settings {
            key,
            endpoint,
            model: text("model").unwrap_or("gpt-3.5-turbo".to_owned()),
            temperature: Some(number("temperature").unwrap_or(0.7)),
            tokens: number("maxTokens").map(|tokens| tokens as i64),
            retry: Retry {
                delay: duration(config, &retry("delay"), Duration::from_millis(450)),
                longest: duration(config, &retry("maxDelay"), Duration::from_secs(90)),
                attempts: (config.get(&retry("maxAttempts")))
                    .and_then(|value| value.to_string().parse().ok())
                    .unwrap_or(10),
                jitter: (config.get(&retry("jitter")))
                    .and_then(|value| value.to_string().parse().ok())
                    .unwrap_or(0.25),
            },
        })
    }
}

fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error> {
    let (options, positional) = split_options(arguments);
    let operator = match name {
        "gptPromptForText" => "gptPromptForText",
        "gptPromptForData" | "fromPrompt" => name,
        _ => {
            return Err(Error::Unsupported {
                message: format!("the function `{name}` of the plugin `nf-gpt`"),
                span: Default::default(),
            });
        }
    };
    let allowed: &[&str] = if name == "gptPromptForText" {
        &TEXT_OPTIONS
    } else {
        &DATA_OPTIONS
    };
    for (key, _) in &options {
        if !allowed.contains(&key.as_str()) {
            return Err(thrown(format!(
                "Unknown argument '{key}' for operator '{operator}' -- Possible arguments: {}",
                allowed.join(", ")
            )));
        }
    }
    let option = |key: &str| crate::nf_sqldb::option(&options, key);
    let settings = Settings::of(context.config, context)?;
    let model = option("model").map_or(settings.model.clone(), |model| model.to_string());
    let temperature = option("temperature").and_then(|value| value.to_string().parse::<f64>().ok());
    let temperature = temperature.or(settings.temperature);
    let tokens = (option("maxTokens").and_then(|value| value.to_string().parse::<i64>().ok()))
        .or(settings.tokens);
    let input = positional.into_iter().next().unwrap_or(Value::Null);
    let mut body = Json::object([("model", Json::from(model.as_str()))]);
    if name == "gptPromptForText" {
        let messages = match &input {
            Value::List(_) => ordered_json(&input),
            other => Json::list([Json::object([
                ("role", Json::from("user")),
                ("content", Json::from(other.to_string())),
            ])]),
        };
        body.insert("messages", messages);
        body.insert("max_tokens", tokens);
        let choices =
            option("numOfChoices").and_then(|value| value.to_string().parse::<i64>().ok());
        body.insert("n", choices.unwrap_or(1));
        body.insert("temperature", temperature);
        body.insert(
            "logit_bias",
            option("logitBias").map_or(Json::Null, |bias| ordered_json(&bias)),
        );
        let answer = send(&settings, &body.without_nulls())?;
        let contents: Vec<Value> = (answer["choices"].as_array().into_iter().flatten())
            .map(|choice| Value::string(choice["message"]["content"].as_str().unwrap_or_default()))
            .collect();
        return Ok(match choices {
            Some(_) => Value::list(contents),
            None => contents.into_iter().next().unwrap_or(Value::Null),
        });
    }
    let schema = option("schema").ok_or_else(|| thrown("Missing prompt schema"))?;
    let format = format!(
        "You must answer strictly in the following JSON format: {{\"result\": [{}] }}",
        rendered(&schema)
    );
    body.insert(
        "messages",
        Json::list([
            Json::object([
                ("role", Json::from("system")),
                ("content", Json::from(format)),
            ]),
            Json::object([
                ("role", Json::from("user")),
                ("content", Json::from(input.to_string())),
            ]),
        ]),
    );
    body.insert("max_tokens", tokens);
    body.insert("temperature", temperature);
    body.insert(
        "response_format",
        Json::object([("type", Json::from("json_object"))]),
    );
    let answer = send(&settings, &body.without_nulls())?;
    let content = answer["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    let parsed = parse_json(content).map_err(|error| thrown(error.message.to_string()))?;
    let keys: Vec<String> = entries(&schema).into_iter().map(|(key, _)| key).collect();
    records(&parsed, &keys).ok_or_else(|| {
        thrown(format!(
            "Response does not match expected schema: {schema} - Offending value: {parsed}"
        ))
    })
}

/// A schema as the system message shows it: `{"key": (type: type), ...}`, lists as `[...]`.
fn rendered(schema: &Value) -> String {
    match schema {
        Value::Map(_) => {
            let fields: Vec<String> = (entries(schema).into_iter())
                .map(|(key, value)| format!("\"{key}\": {}", rendered(&value)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::List(list) => {
            let items: Vec<String> = list.borrow().items.iter().map(rendered).collect();
            format!("[{}]", items.join(", "))
        }
        other => format!("(type: {other})"),
    }
}

/// The list of maps in an answer, by the plugin's rules.
fn records(value: &Value, keys: &[String]) -> Option<Value> {
    let has_keys = |value: &Value| match value {
        Value::Map(_) => {
            let found: Vec<String> = entries(value).into_iter().map(|(key, _)| key).collect();
            keys.iter().all(|key| found.contains(key))
        }
        _ => false,
    };
    match value {
        Value::Map(map) => {
            let found = entries(value);
            let exact = found.len() == keys.len() && has_keys(value);
            if exact {
                return Some(Value::list(vec![value.clone()]));
            }
            let numbered = !found.is_empty()
                && found
                    .iter()
                    .all(|(key, item)| key.parse::<i64>().is_ok() && has_keys(item));
            if numbered {
                return Some(Value::list(
                    found.into_iter().map(|(_, item)| item).collect(),
                ));
            }
            if map.borrow().entries.len() == 1 {
                return records(&found[0].1, keys);
            }
            None
        }
        Value::List(list) => {
            let first = list.borrow().items.first().cloned();
            first.filter(has_keys).map(|_| value.clone())
        }
        _ => None,
    }
}

/// Posts a chat completion; network errors, 429 and 5xx are retried with a growing pause.
fn send(settings: &Settings, body: &Json) -> Result<serde_json::Value, Error> {
    let url = format!("{}/v1/chat/completions", settings.endpoint);
    let unreachable = || {
        thrown(format!(
            "Unable to connect ChatGPT service: {}",
            settings.endpoint
        ))
    };
    let client = storage::http_client().map_err(|_| unreachable())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let runtime = runtime.map_err(|_| unreachable())?;
    let mut pause = settings.retry.delay;
    for attempt in 1..=settings.retry.attempts.max(1) {
        let request = (client.post(&url).timeout(LIMIT))
            .bearer_auth(&settings.key)
            .header("Content-Type", "application/json")
            .body(body.compact());
        let answer = runtime.block_on(async {
            let response = request.send().await.map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            Ok::<_, String>((
                status,
                response.text().await.map_err(|error| error.to_string())?,
            ))
        });
        let retried = match &answer {
            Ok((200, text)) => {
                return serde_json::from_str(text).map_err(|error| thrown(error.to_string()));
            }
            Ok((status, _)) => *status == 429 || (500..=504).contains(status),
            Err(_) => true,
        };
        if !retried || attempt == settings.retry.attempts {
            return match answer {
                Ok((status, text)) => Err(thrown(format!(
                    "ChatGPT unexpected response: [{status}] {text}"
                ))),
                Err(_) => Err(unreachable()),
            };
        }
        let jitter = 1.0 + settings.retry.jitter * (f64::from(attempt % 3) - 1.0) / 2.0;
        std::thread::sleep(pause.mul_f64(jitter.max(0.0)));
        pause = (pause * 2).min(settings.retry.longest);
    }
    Err(unreachable())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_schema_answers() {
        let schema = Value::map([
            (Value::string("name"), Value::string("string")),
            (Value::string("age"), Value::string("int")),
        ]);
        assert_eq!(
            rendered(&schema),
            "{\"name\": (type: string), \"age\": (type: int)}"
        );
        let keys = ["name".to_owned(), "age".to_owned()];
        let answer =
            parse_json(r#"{"result": [{"name": "a", "age": 1}, {"name": "b", "age": 2}]}"#)
                .expect("JSON");
        let found = records(&answer, &keys).expect("records");
        assert_eq!(found.to_string(), "[[name:a, age:1], [name:b, age:2]]");
        let single = parse_json(r#"{"name": "a", "age": 1}"#).expect("JSON");
        assert_eq!(
            records(&single, &keys).expect("one").to_string(),
            "[[name:a, age:1]]"
        );
        let wrong = parse_json(r#"{"x": 1, "y": 2}"#).expect("JSON");
        assert!(records(&wrong, &keys).is_none());
    }
}
