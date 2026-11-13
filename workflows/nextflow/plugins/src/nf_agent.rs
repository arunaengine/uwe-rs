//! nf-agent 0.2.0: the `langchain4j` runner of the `agent` primitive, which sends one OpenAI
//! chat completions request per agent task, with the credential the `agent` scope resolves.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use nextflow_config::Config;
use nextflow_evaluator::lowered::{AnswerType, Structured};
use nextflow_evaluator::value::Value;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::service::{self, Sender};

/// The run-wide options of the `agent` scope; the key is a secret and never printed.
pub struct Options {
    provider: Option<String>,
    key: Option<String>,
    base_url: Option<String>,
    /// How long one model request may take, `agent.requestTimeout`.
    pub timeout: Duration,
}

/// Where the requests of one model go, with the credential they carry.
pub struct Endpoint {
    /// The base address, like `https://api.openai.com/v1`.
    pub url: String,
    key: String,
    /// What the resolution warns about, with addresses shown without credentials.
    pub warnings: Vec<String>,
}

/// Why a model call cannot start: a wrong model id fails its task, while a missing or withheld
/// credential stops the run, as Nextflow does.
#[derive(Debug)]
pub enum Refused {
    Task(String),
    Run(String),
}

/// One request of an agent task: the messages for the model and the schema of its answer.
pub struct Question {
    pub agent: String,
    /// The model as the wire names it, without the `openai/` prefix.
    pub model: String,
    pub system: String,
    pub user: String,
    /// The JSON schema of a structured answer, as it is stored, before strict mode.
    pub schema: Option<Json>,
}

const PROVIDERS: &[&str] = &[
    "anthropic",
    "azure",
    "gemini",
    "google",
    "mistral",
    "openai",
    "openrouter",
];
const HOSTS: &[(&str, &str)] = &[
    ("api.openai.com", "openai"),
    ("api.anthropic.com", "anthropic"),
    ("openrouter.ai", "openrouter"),
    ("api.mistral.ai", "mistral"),
];
const DEFAULT_URL: &str = "https://api.openai.com/v1";
/// The key sent to a local server when no credential resolves, which it ignores.
const NO_KEY: &str = "nxf-no-credential";
/// Two retries after the first attempt, like the default of the langchain4j client.
const ATTEMPTS: u32 = 3;

impl Options {
    /// The options of the config, with Nextflow's check of `agent.apiProvider`.
    pub fn of(config: &Config) -> Result<Options, String> {
        let text = |key: &str| {
            (config.get(&["agent", key]))
                .filter(Value::truth)
                .map(|value| value.to_string())
        };
        let provider = text("apiProvider").map(|provider| provider.to_lowercase());
        if let Some(provider) = provider
            .as_ref()
            .filter(|p| !PROVIDERS.contains(&p.as_str()))
        {
            return Err(format!(
                "Invalid `agent.apiProvider` value `{provider}` - expected one of: {}",
                PROVIDERS.join(", ")
            ));
        }
        let millis = match config.get(&["agent", "requestTimeout"]) {
            None | Some(Value::Null) => 120_000,
            Some(Value::Duration(millis)) => millis,
            Some(value) => nextflow_evaluator::units::parse_duration(&value.to_string())
                .map_err(|exception| exception.message)?,
        };
        Ok(Options {
            provider,
            key: text("apiKey"),
            base_url: text("baseUrl").map(|url| url.trim_end_matches('/').to_owned()),
            timeout: Duration::from_millis(millis.max(1) as u64),
        })
    }
}

/// Why uwe cannot run the agents of this config at all, before any task starts.
pub fn refusal(config: &Config) -> Option<String> {
    let runner = config.get(&["agent", "runner"]).filter(Value::truth)?;
    match runner.to_string().as_str() {
        "langchain4j" => None,
        "pi" => Some(
            "`agent.runner = 'pi'`: the pi runner runs the pi agent program in a container that \
             talks gRPC with the driver, which uwe does not have yet; use `agent.runner = \
             'langchain4j'`"
                .into(),
        ),
        other => Some(format!(
            "the agent runner `{other}`: uwe has the `langchain4j` runner only"
        )),
    }
}

/// The endpoint and credential of a model, resolved as the `agent` scope says: the options,
/// then `NXF_AGENT_*`, then the variables of the provider, which go only to its endpoints.
pub fn endpoint(
    options: &Options,
    model: &str,
    variable: &dyn Fn(&str) -> Option<String>,
) -> Result<Endpoint, Refused> {
    let Some((prefix, _)) = model
        .split_once('/')
        .filter(|(p, m)| !p.is_empty() && !m.is_empty())
    else {
        return Err(Refused::Task(format!(
            "Invalid model id `{model}` - expected `provider/model`"
        )));
    };
    let prefix = prefix.to_lowercase();
    let mut url = (options.base_url.clone()).or_else(|| variable("NXF_AGENT_BASE_URL"));
    let namespace = (options.provider.clone())
        .or_else(|| url.as_deref().and_then(known_host).map(str::to_owned))
        .unwrap_or_else(|| prefix.clone());
    let (keys, endpoint_variable) = variables(&namespace);
    let mut own_endpoint = false;
    if url.is_none()
        && let Some(found) = endpoint_variable.and_then(variable)
    {
        url = Some(found.trim_end_matches('/').to_owned());
        own_endpoint = true;
    }
    let host = url.as_deref().and_then(known_host);
    let shown = url.as_deref().map(service::safe_url);
    let mut warnings = Vec::new();
    if let (Some(provider), Some(other)) = (&options.provider, host)
        && provider != other
    {
        let url = shown.as_deref().unwrap_or_default();
        warnings.push(format!(
            "Agent config sets `agent.apiProvider = '{provider}'` but the endpoint {url} is a \
             known `{other}` endpoint - no `{provider}` credential will be sent to it; set \
             `agent.apiKey` to the credential this endpoint accepts"
        ));
    }
    let mut key = (options.key.clone()).or_else(|| variable("NXF_AGENT_API_KEY"));
    let mut withheld = false;
    if key.is_none()
        && let Some((name, found)) = (keys.iter()).find_map(|name| Some((*name, variable(name)?)))
    {
        let owned = host == Some(namespace.as_str())
            || own_endpoint
            || (options.provider.is_some() && host.is_none_or(|host| host == namespace))
            || (url.is_none() && namespace == prefix);
        match owned {
            true => key = Some(found),
            false => {
                withheld = true;
                warnings.push(withheld_warning(name, model, &namespace, &prefix, &shown));
            }
        }
    }
    if prefix != "openai" {
        return Err(Refused::Task(format!(
            "Unsupported agent model provider `{prefix}` - only the `openai` wire protocol is \
             supported; to reach an OpenAI-compatible endpoint keep the `openai/` prefix and \
             set `agent.baseUrl`"
        )));
    }
    let key = match (key, &shown, host) {
        (Some(key), _, _) => key,
        (None, _, _) if withheld => {
            let target = match &shown {
                Some(url) => format!("the endpoint {url}"),
                None => "the default `openai` endpoint".into(),
            };
            return Err(Refused::Run(format!(
                "The `{namespace}` credential resolved for agent model `{model}` was withheld \
                 from {target} - that endpoint is not one `{namespace}` owns; set \
                 `agent.apiKey` in the Nextflow configuration, or the `NXF_AGENT_API_KEY` \
                 environment variable, to the credential it does accept, or \
                 `agent.apiProvider` to name the namespace it belongs to"
            )));
        }
        (None, Some(url), Some(provider)) => {
            let name = variables(provider)
                .0
                .first()
                .copied()
                .unwrap_or("NXF_AGENT_API_KEY");
            return Err(Refused::Run(format!(
                "Missing `{provider}` credential for the agent endpoint {url} - set \
                 `agent.apiKey` in the Nextflow configuration, or the `{name}` environment \
                 variable; the `{provider}` namespace was inferred from the endpoint host, so \
                 set `agent.apiProvider` to name a different one"
            )));
        }
        (None, Some(_), None) => NO_KEY.into(),
        (None, None, _) => {
            let names = match keys.is_empty() {
                true => "NXF_AGENT_API_KEY".into(),
                false => keys.join(" or "),
            };
            let unknown = match PROVIDERS.contains(&namespace.as_str()) {
                true => "",
                false => ", and name the credential namespace with `agent.apiProvider`",
            };
            return Err(Refused::Run(format!(
                "Missing LLM provider credential for agent model `{model}` - set \
                 `agent.apiKey` in the Nextflow configuration, or the `{names}` environment \
                 variable; the credential namespace is `{namespace}`, set \
                 `agent.apiProvider` to name a different one{unknown}"
            )));
        }
    };
    Ok(Endpoint {
        url: url.unwrap_or_else(|| DEFAULT_URL.into()),
        key,
        warnings,
    })
}

/// The warning for a provider's key that its endpoint does not get.
fn withheld_warning(
    name: &str,
    model: &str,
    provider: &str,
    prefix: &str,
    url: &Option<String>,
) -> String {
    let because = match url {
        Some(url) => format!("the resolved endpoint {url} is not a known `{provider}` endpoint"),
        None if PROVIDERS.contains(&prefix) => format!(
            "no endpoint resolved, so the request goes to the default endpoint of the model's \
             own `{prefix}` provider and not to `{provider}`"
        ),
        None => "no endpoint resolved and the model id names no provider, so there is no \
                 endpoint this credential is known to belong to"
            .into(),
    };
    let remedy = match url {
        Some(_) => format!(
            "`agent.apiProvider = '{provider}'` to confirm it accepts `{provider}` credentials"
        ),
        None => format!("`agent.baseUrl` to the `{provider}` endpoint this credential belongs to"),
    };
    format!(
        "Not using the {name} credential for agent model `{model}`: {because} - set \
         `agent.apiKey` to the credential that endpoint accepts, or {remedy}"
    )
}

/// The key variables of a provider in order, and its endpoint variable.
fn variables(provider: &str) -> (&'static [&'static str], Option<&'static str>) {
    match provider {
        "anthropic" => (&["ANTHROPIC_API_KEY"], Some("ANTHROPIC_BASE_URL")),
        "azure" => (&["AZURE_OPENAI_API_KEY"], Some("AZURE_OPENAI_ENDPOINT")),
        "gemini" => (&["GEMINI_API_KEY", "GOOGLE_API_KEY"], None),
        "google" => (&["GOOGLE_API_KEY", "GEMINI_API_KEY"], None),
        "mistral" => (&["MISTRAL_API_KEY"], None),
        "openai" => (&["OPENAI_API_KEY"], Some("OPENAI_BASE_URL")),
        "openrouter" => (&["OPENROUTER_API_KEY"], None),
        _ => (&[], None),
    }
}

/// The provider whose endpoint this is: its host, or a host below it, never a substring.
fn known_host(url: &str) -> Option<&'static str> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_lowercase();
    (HOSTS.iter())
        .find(|(known, _)| host == *known || host.ends_with(&format!(".{known}")))
        .map(|(_, provider)| *provider)
}

/// The system message: the instruction, then the goal with the sentence that ends the run.
pub fn system(instruction: &str, goal: &str) -> String {
    let mut text = instruction.trim().to_owned();
    if !goal.trim().is_empty() {
        let goal = goal.trim();
        text = format!(
            "{text}\n\nGoal:\n{goal}\nYou are done when the goal is met; produce your final \
             answer as plain text."
        );
    }
    text.trim().to_owned()
}

/// The user message: the prompt, then the input JSON after a blank line.
pub fn user(prompt: &str, input: Option<&Json>) -> String {
    match input {
        Some(input) => format!("{prompt}\n\nInput (JSON):\n{input}"),
        None => prompt.to_owned(),
    }
}

/// The JSON schema of a typed agent output: a record as it is, any other type wrapped in an
/// object with one property named after the output.
pub fn schema(structured: &Structured) -> Json {
    match &structured.kind {
        AnswerType::Record(_) => type_schema(&structured.kind),
        other => json!({
            "type": "object",
            "properties": {structured.name.as_ref(): type_schema(other)},
            "required": [structured.name.as_ref()],
            "additionalProperties": false,
        }),
    }
}

fn type_schema(kind: &AnswerType) -> Json {
    match kind {
        AnswerType::Text => json!({"type": "string"}),
        AnswerType::Integer => json!({"type": "integer"}),
        AnswerType::Float => json!({"type": "number"}),
        AnswerType::Boolean => json!({"type": "boolean"}),
        AnswerType::Path => {
            json!({"type": "string", "description": "Absolute path of an existing file"})
        }
        AnswerType::List(element) => json!({"type": "array", "items": type_schema(element)}),
        AnswerType::Record(fields) => {
            let mut properties = serde_json::Map::new();
            for field in fields {
                properties.insert(field.name.to_string(), type_schema(&field.kind));
            }
            let required: Vec<&str> = (fields.iter())
                .filter(|field| !field.optional)
                .map(|field| &*field.name)
                .collect();
            json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            })
        }
    }
}

/// The schema for strict mode, where every property is required: an optional one may be
/// null instead.
fn strict(schema: &Json) -> Json {
    let mut schema = schema.clone();
    if let Some(items) = schema.get("items") {
        schema["items"] = strict(items);
    }
    let Some(properties) = schema.get("properties").and_then(Json::as_object).cloned() else {
        return schema;
    };
    let mut required: Vec<Json> = schema["required"].as_array().cloned().unwrap_or_default();
    for (name, property) in properties {
        let mut property = strict(&property);
        if !required.contains(&Json::from(name.as_str())) {
            let kind = property["type"].take();
            property["type"] = json!([kind, "null"]);
            required.push(Json::from(name.as_str()));
        }
        schema["properties"][&name] = property;
    }
    schema["required"] = Json::Array(required);
    schema
}

/// The key of an agent run in the cache: the SHA-256 of what decides its answer.
pub fn cache_key(parts: &Json) -> String {
    let digest = Sha256::digest(parts.to_string().as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Asks the model, with retries of failed connections and of busy or failing servers. The
/// caller's thread must not run inside an async runtime.
pub fn ask(endpoint: &Endpoint, question: &Question, timeout: Duration) -> Result<String, String> {
    let sender = Sender::new()?;
    let mut messages = Vec::new();
    if !question.system.is_empty() {
        messages.push(json!({"role": "system", "content": question.system}));
    }
    messages.push(json!({"role": "user", "content": question.user}));
    let mut body = json!({"model": question.model, "messages": messages});
    if let Some(schema) = &question.schema {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {"name": "Output", "strict": true, "schema": strict(schema)},
        });
    }
    let address = format!("{}/chat/completions", endpoint.url);
    let shown = service::safe_url(&address);
    let agent = &question.agent;
    let mut attempt = 1;
    let answer = loop {
        let request = (sender.client().post(&address))
            .timeout(timeout)
            .bearer_auth(&endpoint.key)
            .header("Content-Type", "application/json")
            .body(body.to_string());
        let failure = match sender.send(request) {
            Ok(answer) if answer.status == 429 || answer.status >= 500 => {
                format!("answered HTTP status {}", answer.status)
            }
            Ok(answer) => break answer,
            Err(error) => format!("cannot be reached: {}", error.without_url()),
        };
        tracing::debug!(agent, endpoint = %shown, attempt, "model request failed: {failure}");
        if attempt >= ATTEMPTS {
            return Err(format!(
                "Agent `{agent}`: the model endpoint {shown} {failure}"
            ));
        }
        std::thread::sleep(Duration::from_secs(u64::from(attempt)));
        attempt += 1;
    };
    let reply: Json = serde_json::from_str(&answer.body).unwrap_or(Json::Null);
    if !(200..300).contains(&answer.status) {
        let detail = (reply["error"]["message"].as_str())
            .map_or(String::new(), |message| format!(": {message}"));
        return Err(format!(
            "Agent `{agent}`: the model endpoint {shown} answered HTTP status {}{detail}",
            answer.status
        ));
    }
    let message = &reply["choices"][0]["message"];
    let content = message["content"].as_str().filter(|text| !text.is_empty());
    let filtered = reply["choices"][0]["finish_reason"] == "content_filter";
    match content {
        Some(text) if !filtered && message["refusal"].is_null() => Ok(text.to_owned()),
        _ if question.schema.is_some() => Err(format!(
            "Agent `{agent}`: the model refused or returned no structured content for the \
             required output schema"
        )),
        _ => Err(format!("Agent `{agent}`: the model returned no answer")),
    }
}

#[cfg(test)]
#[path = "nf_agent_tests.rs"]
mod tests;
