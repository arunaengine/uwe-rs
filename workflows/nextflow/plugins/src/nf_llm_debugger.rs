//! nf-llm-debugger 1.0.14: sends the error of a failed run to a language model and prints its
//! diagnosis; off unless `llm.enabled` or the param `llm_enabled` turns it on.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::value::Value;
use serde_json::{Value as Json, json};

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Background, End, Event, Observer};
use crate::service::{self, Sender};

pub(crate) const PLUGIN: Plugin = Plugin {
    scopes: &["llm"],
    observer: Some(activate),
    ..Plugin::new("nf-llm-debugger", Kind::Observer, State::Implemented)
};

/// How long a model may take to answer; the CRG proxy has 15 s more to connect.
const LIMIT: Duration = Duration::from_secs(30);
const PROXY_LIMIT: Duration = Duration::from_secs(75);
/// How long the end of the run waits for the diagnosis.
const WAIT: Duration = Duration::from_secs(90);
const RULE: &str =
    "================================================================================";
const ROBOT: &str = "🤖 [nf-llm-debugger]";

/// uwe's own instructions to the model, in the plugin's three answer styles.
const CONCISE: &str = "You help debug failed bioinformatics workflow runs. Read the Nextflow \
                       error report and state the likely cause and its fix in at most two \
                       sentences, without lists, headings, introductions or summaries.";
const EXTENSIVE: &str = "You help debug failed bioinformatics workflow runs. Read the Nextflow \
                         error report, explain in detail what failed and why, and give \
                         concrete steps to fix it.";
const NORMAL: &str = "You help debug failed bioinformatics workflow runs. Read the Nextflow \
                      error report, explain what failed in plain words, and suggest concrete \
                      fixes.";
const DOCS: &str = "Documentation of the pipeline, as background:";
const REMINDER: &str = "Answer in at most two sentences, without lists or headings.";

/// The services the plugin talks to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    Proxy,
    Gemini,
    Claude,
    OpenAi,
    Ollama,
    Local,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Provider::Proxy => "crg-proxy",
            Provider::Gemini => "gemini",
            Provider::Claude => "claude",
            Provider::OpenAi => "openai",
            Provider::Ollama => "ollama",
            Provider::Local => "local",
        }
    }
}

/// What the diagnosis needs, from the params of the run.
#[derive(Clone, Debug)]
struct Settings {
    endpoint: String,
    provider: Provider,
    /// The address of the service, without the path of the call.
    url: String,
    model: Option<String>,
    key: Option<String>,
    answer: String,
    /// The text of `llm_docs`, trimmed.
    docs: Option<String>,
}

fn activate(
    config: &Config,
    launch: &std::path::Path,
    environment: &crate::catalogue::Environment,
) -> Result<Option<Box<dyn Observer>>, String> {
    let param = |name: &str| {
        (config.get(&["params", name]))
            .filter(|value| !matches!(value, Value::Null))
            .map(|value| value.to_string())
    };
    let enabled = match param("llm_enabled") {
        Some(flag) => flag != "false",
        None => (config.get(&["llm", "enabled"])).is_some_and(|value| value.truth()),
    };
    if !enabled {
        return Ok(None);
    }
    let variable = |name: &str| environment(name).filter(|value| !value.is_empty());
    let launch = launch.to_path_buf();
    let settings = Settings::read(&param, &variable, &launch);
    let mut sender = None;
    let mut done = false;
    let background = Background::start("nf-llm-debugger", WAIT, move |event| {
        if let Event::RunEnded(end) = event
            && !std::mem::replace(&mut done, true)
        {
            ended(&settings, &end, &mut sender);
        }
    })
    .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(background)))
}

impl Settings {
    fn read(
        param: &dyn Fn(&str) -> Option<String>,
        variable: &dyn Fn(&str) -> Option<String>,
        launch: &Path,
    ) -> Settings {
        let given = |name: &str| param(name).filter(|value| !value.is_empty());
        let endpoint = (given("llm_endpoint"))
            .or_else(|| given("llm_address"))
            .unwrap_or_else(|| "local".to_owned());
        let model = given("llm_model");
        let forced = given("llm_provider")
            .is_some_and(|name| matches!(name.as_str(), "crg" | "crg-proxy" | "hpc"));
        let (provider, url) = route(&endpoint, model.as_deref().unwrap_or_default(), forced);
        let default = |name: &str| Some(name.to_owned());
        // A vendor key from the environment goes only to the vendor's own address.
        let vendor =
            |name: &str, home: &str| variable(name).filter(|_| service::same_origin(&url, home));
        let (model, key) = match provider {
            Provider::Proxy => (model, None),
            Provider::Gemini => (
                model.or(default("gemini-1.5-flash")),
                vendor("GEMINI_API_KEY", GEMINI).or_else(|| variable("LLM_API_KEY")),
            ),
            Provider::Claude => (
                model.or(default("claude-3-5-sonnet-20241022")),
                vendor("ANTHROPIC_API_KEY", CLAUDE).or_else(|| variable("LLM_API_KEY")),
            ),
            Provider::OpenAi => (
                model.or(default("gpt-4o-mini")),
                vendor("OPENAI_API_KEY", OPENAI).or_else(|| variable("LLM_API_KEY")),
            ),
            Provider::Ollama => (model.or(default("llama3")), None),
            Provider::Local => (
                model.or(default("LLaMA_CPP")),
                (variable("LLM_API_KEY").or_else(|| variable("OPENAI_API_KEY")))
                    .or(default("ignored")),
            ),
        };
        let docs =
            given("llm_docs").and_then(|path| match std::fs::read_to_string(launch.join(&path)) {
                Ok(text) => Some(text.trim().to_owned()),
                Err(_) => {
                    tracing::warn!("[nf-llm-debugger] Documentation file not found: {path}");
                    None
                }
            });
        Settings {
            endpoint,
            provider,
            url,
            model,
            key: given("llm_api_key").or(key),
            answer: given("llm_answer")
                .unwrap_or_else(|| "concise".to_owned())
                .to_lowercase(),
            docs,
        }
    }

    fn concise(&self) -> bool {
        self.answer == "concise"
    }

    /// The instruction to the model, with the documentation when there is one.
    fn system(&self) -> String {
        let style = match self.answer.as_str() {
            "concise" => CONCISE,
            "extensive" | "extense" => EXTENSIVE,
            _ => NORMAL,
        };
        match &self.docs {
            Some(docs) => format!("{style}\n\n{DOCS}\n{docs}"),
            None => style.to_owned(),
        }
    }

    fn user(&self, report: &str) -> String {
        match self.concise() {
            true => format!("{report}\n\n{REMINDER}"),
            false => report.to_owned(),
        }
    }
}

/// The provider and address of `llm_endpoint`: a preset name, or an address whose provider is
/// guessed from it and the model. uwe sends to the address also for Gemini, Claude and OpenAI.
fn route(endpoint: &str, model: &str, forced: bool) -> (Provider, String) {
    let lower = endpoint.to_lowercase();
    let preset = match lower.as_str() {
        "crg" | "crg-proxy" | "hpc" => Some((Provider::Proxy, PROXY)),
        "gemini" => Some((Provider::Gemini, GEMINI)),
        "claude" | "anthropic" => Some((Provider::Claude, CLAUDE)),
        "chatgpt" | "openai" => Some((Provider::OpenAi, OPENAI)),
        "ollama" => Some((Provider::Ollama, "http://localhost:11434")),
        "local" | "llamafile" => Some((Provider::Local, "http://localhost:8080/v1")),
        _ => None,
    };
    if let Some((provider, url)) = preset {
        return match forced {
            true => (Provider::Proxy, PROXY.to_owned()),
            false => (provider, url.to_owned()),
        };
    }
    let model = model.to_lowercase();
    let url = endpoint.trim_end_matches('/');
    let has = |text: &str| lower.contains(text) || model.contains(text);
    let provider = if forced || lower.contains("llm-proxy") || lower.contains("/debug/nextflow") {
        Provider::Proxy
    } else if lower.contains("googleapis.com") || model.contains("gemini") {
        Provider::Gemini
    } else if has("claude") || lower.contains("anthropic.com") {
        Provider::Claude
    } else if lower.contains("openai.com")
        || model.starts_with("gpt-")
        || model.starts_with("openai/")
    {
        Provider::OpenAi
    } else if has("ollama") || lower.contains(":11434") {
        Provider::Ollama
    } else {
        Provider::Local
    };
    let url = match provider {
        Provider::Proxy if !url.ends_with("/debug/nextflow") => format!("{url}/debug/nextflow"),
        Provider::Proxy | Provider::Gemini | Provider::Claude | Provider::Ollama => url.to_owned(),
        Provider::OpenAi | Provider::Local => url
            .strip_suffix("/chat/completions")
            .unwrap_or(url)
            .to_owned(),
    };
    (provider, url)
}

const PROXY: &str = "https://llm-proxy.hpc.crg.es/debug/nextflow";
const GEMINI: &str = "https://generativelanguage.googleapis.com/v1beta";
const CLAUDE: &str = "https://api.anthropic.com";
const OPENAI: &str = "https://api.openai.com/v1";

/// Diagnoses a failed run once: the failing task when uwe knows it, which Nextflow 26.04.6
/// never sends to the plugin, else the error of the run.
fn ended(settings: &Settings, end: &End, sender: &mut Option<Sender>) {
    tracing::info!("{ROBOT} onFlowComplete - Success: {}", end.success);
    if end.success {
        return;
    }
    let report = match &end.task {
        Some(task) => {
            let field = |name: &str| match task.record.get(name) {
                Some(Json::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => "null".to_owned(),
            };
            let error = (end.error.clone()).unwrap_or_else(|| "No error report available.".into());
            format!(
                "Task {} [{}] failed.\nWork Directory: {}\n\nError report:\n{error}",
                field("name"),
                field("task_id"),
                field("workdir")
            )
        }
        None => {
            tracing::info!(
                "{ROBOT} onFlowComplete - Error: {}",
                end.error.as_deref().unwrap_or_default()
            );
            match &end.error {
                Some(error) => format!("Workflow execution failed.\nError: {error}"),
                None => "Workflow failed (no exception details available in session).".to_owned(),
            }
        }
    };
    tracing::info!(
        "{ROBOT} Debug parameters: endpoint={}, address={}, model={}",
        service::safe_url(&settings.endpoint),
        service::safe_url(&settings.url),
        settings.model.as_deref().unwrap_or("null")
    );
    tracing::info!(
        "{ROBOT} Initializing LLM using provider: {}",
        settings.provider.name()
    );
    let sender = match service::sender(sender) {
        Ok(sender) => sender,
        Err(error) => {
            return tracing::warn!("[nf-llm-debugger] Error running LLM diagnosis: {error}");
        }
    };
    let answer = match settings.provider {
        Provider::Proxy => proxy(settings, &report, sender),
        _ => chat(settings, &report, sender).map(|text| (text, None, None)),
    };
    match answer {
        Ok((text, _, _)) if text.trim().is_empty() => {
            tracing::warn!("[nf-llm-debugger] LLM returned an empty response.");
        }
        Ok((text, model, note)) => show(&text, model.as_deref(), note.as_deref()),
        Err(error) => tracing::warn!("{error}"),
    }
}

/// Asks a chat model with the instruction and the report, in the form of its provider.
fn chat(settings: &Settings, report: &str, sender: &Sender) -> Result<String, String> {
    let name = match settings.provider {
        Provider::Gemini => "Gemini",
        Provider::Claude => "Claude",
        Provider::OpenAi => "ChatGPT",
        Provider::Ollama => "Ollama",
        Provider::Proxy | Provider::Local => "Local/Llamafile",
    };
    let model = settings.model.clone().unwrap_or_default();
    tracing::info!(
        "{ROBOT} Configured {name} Model: {model} at {}",
        service::safe_url(&settings.url)
    );
    tracing::info!("{ROBOT} Generating LLM diagnosis...");
    let (system, user) = (settings.system(), settings.user(report));
    let (url, key) = (&settings.url, settings.key.clone().unwrap_or_default());
    let messages =
        json!([{"role": "system", "content": system}, {"role": "user", "content": user}]);
    let (path, body) = match settings.provider {
        Provider::Gemini => (
            format!("/models/{model}:generateContent"),
            json!({
                "systemInstruction": {"parts": [{"text": system}]},
                "contents": [{"role": "user", "parts": [{"text": user}]}],
                "generationConfig": {"temperature": 0.2},
            }),
        ),
        Provider::Claude => (
            "/v1/messages".to_owned(),
            json!({
                "model": model, "max_tokens": 2048, "system": system, "temperature": 0.2,
                "messages": [{"role": "user", "content": user}],
            }),
        ),
        Provider::Ollama => (
            "/api/chat".to_owned(),
            json!({
                "model": model, "messages": messages, "stream": false,
                "options": {"temperature": 0.2},
            }),
        ),
        Provider::OpenAi | Provider::Local | Provider::Proxy => (
            "/chat/completions".to_owned(),
            json!({"model": model, "messages": messages, "temperature": 0.2}),
        ),
    };
    let mut request = (sender.client().post(format!("{url}{path}")))
        .timeout(LIMIT)
        .header("Content-Type", "application/json")
        .body(body.to_string());
    request = match settings.provider {
        Provider::Gemini => request.header("x-goog-api-key", key),
        Provider::Claude => {
            (request.header("x-api-key", key)).header("anthropic-version", "2023-06-01")
        }
        Provider::Ollama => request,
        Provider::OpenAi | Provider::Local | Provider::Proxy => request.bearer_auth(key),
    };
    let failed = |error: String| format!("[nf-llm-debugger] Error running LLM diagnosis: {error}");
    let answer = sender
        .send(request)
        .map_err(|error| failed(error.without_url().to_string()))?;
    if !(200..300).contains(&answer.status) {
        return Err(failed(format!(
            "HTTP status {}: {}",
            answer.status, answer.body
        )));
    }
    let body: Json =
        serde_json::from_str(&answer.body).map_err(|error| failed(error.to_string()))?;
    let texts = |items: &Json, field: &str| {
        (items.as_array().into_iter().flatten())
            .filter_map(|item| item[field].as_str())
            .collect::<Vec<_>>()
            .join("")
    };
    Ok(match settings.provider {
        Provider::Gemini => texts(&body["candidates"][0]["content"]["parts"], "text"),
        Provider::Claude => texts(&body["content"], "text"),
        Provider::Ollama => body["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        Provider::OpenAi | Provider::Local | Provider::Proxy => {
            (body["choices"][0]["message"]["content"].as_str())
                .unwrap_or_default()
                .to_owned()
        }
    })
}

/// A diagnosis with the model and the note that the CRG proxy may add.
type Diagnosis = (String, Option<String>, Option<String>);

/// Sends the report to the CRG proxy, which answers with its diagnosis.
fn proxy(settings: &Settings, report: &str, sender: &Sender) -> Result<Diagnosis, String> {
    let url = &settings.url;
    tracing::info!(
        "{ROBOT} Sending error report to CRG LLM proxy ({})...",
        service::safe_url(url)
    );
    let log = match &settings.docs {
        None => report.to_owned(),
        Some(docs) => format!(
            "--- Error report ---\n{report}\n--- End of error report ---\n\n--- Pipeline \
             documentation (background, not an error) ---\n{docs}\n--- End of pipeline \
             documentation ---\n\nDiagnose only the error report; the documentation is background."
        ),
    };
    let mut request = (sender.client().post(url))
        .timeout(PROXY_LIMIT)
        .header("Content-Type", "application/json")
        .body(json!({"log": log}).to_string());
    if let Some(key) = &settings.key {
        request = request.bearer_auth(key);
    }
    let answer = sender.send(request).map_err(|error| {
        let error = error.without_url();
        format!("[nf-llm-debugger] Error communicating with CRG LLM proxy: {error}")
    })?;
    if !(200..300).contains(&answer.status) {
        return Err(format!(
            "[nf-llm-debugger] CRG LLM proxy request failed with HTTP status {}: {}",
            answer.status, answer.body
        ));
    }
    let (text, model, note) = match serde_json::from_str::<Json>(&answer.body) {
        Ok(body @ Json::Object(_)) => {
            let field = |name: &str| body[name].as_str().map(str::to_owned);
            (
                field("result").unwrap_or_default(),
                field("model"),
                field("note"),
            )
        }
        _ => (answer.body.clone(), None, None),
    };
    if text.trim().is_empty() {
        return Err("[nf-llm-debugger] CRG LLM proxy returned an empty result.".to_owned());
    }
    Ok((text, model, note))
}

/// Prints the diagnosis between rules on standard output; an observer has no host there.
fn show(text: &str, model: Option<&str>, note: Option<&str>) {
    let header = match model {
        Some(model) => format!("{ROBOT} LLM ERROR DIAGNOSIS (model: {model}):"),
        None => format!("{ROBOT} LLM ERROR DIAGNOSIS:"),
    };
    let note = note
        .map(|note| format!("\nNote: {note}"))
        .unwrap_or_default();
    println!("\n{RULE}\n{header}\n{RULE}\n{text}{note}\n{RULE}\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_endpoints() {
        let route = |endpoint: &str, model: &str| route(endpoint, model, false);
        assert_eq!(
            route("LOCAL", ""),
            (Provider::Local, "http://localhost:8080/v1".into())
        );
        assert_eq!(route("claude", ""), (Provider::Claude, CLAUDE.into()));
        assert_eq!(
            route("http://h:1/llm-proxy/", ""),
            (
                Provider::Proxy,
                "http://h:1/llm-proxy/debug/nextflow".into()
            )
        );
        assert_eq!(
            route("http://h:1/v1", "gemini-2.0-flash").0,
            Provider::Gemini
        );
        assert_eq!(route("http://h:1/v1", "gpt-4o").0, Provider::OpenAi);
        assert_eq!(route("http://h:11434", "llama3").0, Provider::Ollama);
        assert_eq!(
            route("http://h:1/v1/chat/completions", "demo"),
            (Provider::Local, "http://h:1/v1".into())
        );
        assert_eq!(
            super::route("gemini", "", true),
            (Provider::Proxy, PROXY.into())
        );
    }

    #[test]
    fn hides_failed_address() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds a free port");
        let port = listener.local_addr().expect("an address").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });
        let endpoint = format!("http://user:pw-secret@127.0.0.1:{port}/v1?key=query-secret");
        let param = |name: &str| (name == "llm_endpoint").then(|| endpoint.clone());
        let settings = Settings::read(&param, &|_| None, Path::new("."));
        let sender = Sender::new().expect("a sender");
        let error = chat(&settings, "report", &sender).expect_err("the server hangs up");
        let mut proxy = settings.clone();
        proxy.url = settings.endpoint.clone();
        let failed = super::proxy(&proxy, "report", &sender).expect_err("the server hangs up");
        for error in [error, failed] {
            assert!(error.starts_with("[nf-llm-debugger] Error"), "{error}");
            assert!(!error.contains("secret"), "{error}");
        }
    }

    #[test]
    fn vendor_key_stays() {
        let variable = |name: &str| (name == "OPENAI_API_KEY").then(|| "sk-env".to_owned());
        let read = |endpoint: &str| {
            let param = |name: &str| match name {
                "llm_endpoint" => Some(endpoint.to_owned()),
                "llm_model" => Some("gpt-4o".to_owned()),
                _ => None,
            };
            Settings::read(&param, &variable, Path::new("."))
        };
        let custom = read("http://127.0.0.1:9/v1");
        assert_eq!(custom.provider, Provider::OpenAi);
        assert_eq!(
            custom.key, None,
            "the OpenAI key must not go to another host"
        );
        assert_eq!(read("openai").key.as_deref(), Some("sk-env"));
        assert_eq!(
            read("https://api.openai.com/v1/").key.as_deref(),
            Some("sk-env")
        );
    }
}
