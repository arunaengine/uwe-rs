//! The LaminHub functions and the instance REST API that the run tracking of nf-lamin calls,
//! with the plugin's retries.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use reqwest::Method;
use serde_json::{Value, json};

use crate::service::{self, Sender};

/// How long a call to a LaminHub function may take: 5 s to connect, 15 s to read.
const HUB_LIMIT: Duration = Duration::from_secs(20);
/// How long a call to the instance API may take.
const API_LIMIT: Duration = Duration::from_secs(30);

/// Where LaminHub is and what the client signs in with.
#[derive(Clone, Debug)]
pub(super) struct Hub {
    pub(super) url: String,
    pub(super) anon_key: Option<String>,
    pub(super) api_key: Option<String>,
    pub(super) owner: String,
    pub(super) name: String,
    pub(super) retry: Retry,
}

/// The retries of the instance API: delays in milliseconds.
#[derive(Clone, Debug)]
pub(super) struct Retry {
    pub(super) attempts: u32,
    pub(super) delay: u64,
    pub(super) most: u64,
}

/// A signed-in client of one instance.
pub(super) struct Client {
    sender: Option<Sender>,
    hub: Hub,
    token: String,
    /// The base address of the instance API, from the instance settings.
    api_url: String,
    instance_id: String,
}

impl Client {
    /// Signs in, reads the settings of the instance and the account, as the run begins.
    pub(super) fn connect(hub: &Hub) -> Result<Client, String> {
        let mut client = Client {
            sender: None,
            hub: hub.clone(),
            token: String::new(),
            api_url: String::new(),
            instance_id: String::new(),
        };
        let slug = format!("{}/{}", hub.owner, hub.name);
        let connected = client.sign_in().and_then(|()| {
            let account = client.call(Method::GET, "/account", None, "GET account")?;
            Ok(account.unwrap_or_default())
        });
        // The sender's runtime ends in this thread, never in the runtime of a caller.
        client.sender = None;
        match connected {
            Ok(account) => {
                let handle = account["handle"].as_str().unwrap_or_default();
                tracing::info!("→ connected lamindb: '{slug}' as '{handle}'");
                Ok(client)
            }
            Err(error) => {
                tracing::error!("✗ Could not connect lamindb: '{slug}'!");
                tracing::error!("API call failed: {error}");
                Err(error)
            }
        }
    }

    /// Fetches the token and the settings of the instance.
    fn sign_in(&mut self) -> Result<(), String> {
        self.refresh()?;
        let body = json!({"owner": self.hub.owner, "name": self.hub.name});
        let settings = match self.function("get-instance-settings-v1", &body) {
            Err(Denied) => {
                self.refresh()?;
                self.function("get-instance-settings-v1", &body)
            }
            other => other,
        };
        let settings =
            settings.map_err(|Denied| "the instance settings were refused".to_owned())?;
        let slug = format!("{}/{}", self.hub.owner, self.hub.name);
        let settings = settings?;
        if let (Some(code), Some(message)) = (settings.get("code"), settings.get("message")) {
            return Err(format!(
                "Failed to get instance settings for {slug}: {} (code: {})",
                text(message),
                text(code)
            ));
        }
        match (settings["id"].as_str(), settings["api_url"].as_str()) {
            (Some(id), Some(url)) => {
                self.instance_id = id.to_owned();
                self.api_url = url.trim_end_matches('/').to_owned();
                Ok(())
            }
            _ => Err(format!("LaminDB instance {slug} could not be found.")),
        }
    }

    /// A new access token for the API key, or the anon key without one.
    fn refresh(&mut self) -> Result<(), String> {
        let anon = self.hub.anon_key.clone().ok_or_else(|| {
            "the LaminHub anon key is missing: set lamin.api.supabase_anon_key or \
             SUPABASE_ANON_KEY"
                .to_owned()
        })?;
        let Some(key) = self.hub.api_key.clone() else {
            self.token = anon;
            return Ok(());
        };
        self.token = anon;
        let answer = self.function("get-jwt-v1", &json!({"api_key": key}));
        let answer =
            answer.map_err(|Denied| "Failed to fetch access token: denied".to_owned())??;
        if let Some(error) = answer.get("error") {
            return Err(format!("Failed to fetch access token: {}", text(error)));
        }
        match answer["accessToken"].as_str() {
            Some(token) => {
                self.token = token.to_owned();
                Ok(())
            }
            None => Err("Failed to fetch access token: no token in the answer".to_owned()),
        }
    }

    /// Calls a LaminHub function; a 401 or 403 answer is [`Denied`].
    fn function(&mut self, name: &str, body: &Value) -> Result<Result<Value, String>, Denied> {
        let url = format!("{}/functions/v1/{name}", self.hub.url.trim_end_matches('/'));
        let token = self.token.clone();
        let sender = match service::sender(&mut self.sender) {
            Ok(sender) => sender,
            Err(error) => return Ok(Err(error)),
        };
        let request = (sender.client().post(&url))
            .timeout(HUB_LIMIT)
            .bearer_auth(token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body.to_string());
        match sender.send(request) {
            Ok(answer) if matches!(answer.status, 401 | 403) => Err(Denied),
            Ok(answer) => Ok(Ok(serde_json::from_str(&answer.body).unwrap_or(Value::Null))),
            Err(error) => Ok(Err(format!("{name}: {error}"))),
        }
    }

    /// Calls the instance API with the plugin's retries: a 401 fetches a new token once, a 404
    /// is `None`, client errors fail, and other failures are tried again after a random delay.
    pub(super) fn call(
        &mut self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        operation: &str,
    ) -> Result<Option<Value>, String> {
        let id = service::random_hex(8);
        let mut refreshed = false;
        let mut attempt = 0;
        loop {
            let url = format!("{}{path}", self.api_url);
            let token = self.token.clone();
            let sender = service::sender(&mut self.sender)?;
            let mut request = (sender.client().request(method.clone(), &url))
                .timeout(API_LIMIT)
                .bearer_auth(token)
                .header("Accept", "application/json");
            if let Some(body) = body {
                request = (request.header("Content-Type", "application/json; charset=utf-8"))
                    .body(body.to_string());
            }
            let (status, answer) = match sender.send(request) {
                Ok(answer) => (answer.status, answer.body),
                Err(error) => (0, error.to_string()),
            };
            match status {
                200..=299 => return Ok(Some(serde_json::from_str(&answer).unwrap_or_default())),
                401 if !refreshed => {
                    refreshed = true;
                    self.refresh()?;
                    continue;
                }
                404 => return Ok(None),
                400 | 401 | 409 => return Err(format!("{operation} failed ({status}): {answer}")),
                403 => return Err(format!("{operation} was not permitted: {answer}")),
                500.. if final_error(&answer) => {
                    return Err(format!("{operation} failed ({status}): {answer}"));
                }
                _ => {}
            }
            if attempt >= self.hub.retry.attempts {
                return Err(format!("{operation} failed ({status}): {answer}"));
            }
            attempt += 1;
            let delay = self.hub.retry.delay(attempt);
            tracing::warn!(
                "[{id}] {operation} - status {status} - Retrying ({attempt}/{}) in {}ms... \
                 Response: {answer}",
                self.hub.retry.attempts,
                delay.as_millis()
            );
            std::thread::sleep(delay);
        }
    }

    fn module(&self, model: &str) -> String {
        format!("/instances/{}/modules/core/{model}", self.instance_id)
    }

    /// The records of a model that match a filter.
    pub(super) fn find(
        &mut self,
        model: &str,
        filter: Value,
        foreign: bool,
    ) -> Result<Vec<Value>, String> {
        let path = format!(
            "{}?limit=50&offset=0&limit_to_many=10&include_foreign_keys={foreign}",
            self.module(model)
        );
        let body = json!({"filter": filter});
        let found = self.call(Method::POST, &path, Some(&body), &format!("POST {model}"))?;
        Ok(found
            .and_then(|found| found.as_array().cloned())
            .unwrap_or_default())
    }

    /// Creates a record and gives it back with its `id` and `uid`.
    pub(super) fn create(&mut self, model: &str, body: &Value) -> Result<Value, String> {
        let path = self.module(model);
        let created = self.call(Method::PUT, &path, Some(body), &format!("PUT {model}"))?;
        first(created).ok_or_else(|| format!("PUT {model} gave no record"))
    }

    pub(super) fn update(&mut self, model: &str, uid: &str, body: &Value) -> Result<(), String> {
        let path = format!("{}/{uid}", self.module(model));
        self.call(Method::PATCH, &path, Some(body), &format!("PATCH {model}"))?;
        Ok(())
    }

    /// Creates a transform from its key, kind, source code and the other fields in `kwargs`.
    pub(super) fn transform(&mut self, body: &Value) -> Result<Value, String> {
        let path = format!("/instances/{}/transforms", self.instance_id);
        let created = self.call(Method::POST, &path, Some(body), "POST transforms")?;
        let transform = created.map(|created| created["body"]["transform"].clone());
        transform
            .filter(Value::is_object)
            .ok_or_else(|| "POST transforms gave no transform".to_owned())
    }
}

/// A LaminHub function refused the token.
struct Denied;

impl Retry {
    /// A random delay up to `delay × 2^attempt`, at most `most`.
    fn delay(&self, attempt: u32) -> Duration {
        let grown = self.delay.saturating_mul(1u64 << attempt.min(30));
        let bound = grown.min(self.most) as f64;
        Duration::from_millis((service::random_unit() * bound) as u64)
    }
}

/// Server errors that another attempt does not fix.
fn final_error(body: &str) -> bool {
    [
        "UnknownStorageLocation",
        "BlobHashNotFound",
        "PermissionError",
        "AccessDenied",
        "Forbidden",
        "FileNotFoundError",
        "permission denied",
    ]
    .iter()
    .any(|word| body.contains(word))
}

/// The first record of a list answer, or the answer itself.
fn first(answer: Option<Value>) -> Option<Value> {
    match answer? {
        Value::Array(items) => items.into_iter().next(),
        record @ Value::Object(_) => Some(record),
        _ => None,
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_retry_delays() {
        let retry = Retry {
            attempts: 3,
            delay: 100,
            most: 300,
        };
        for attempt in 1..10 {
            assert!(retry.delay(attempt) <= Duration::from_millis(300));
        }
        assert!(final_error("boom: PermissionError at x"));
        assert!(!final_error("Bad gateway"));
        assert_eq!(first(Some(json!([{"id": 1}]))), Some(json!({"id": 1})));
    }
}
