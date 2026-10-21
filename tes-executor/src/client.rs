//! Creates, reads and cancels tasks through the GA4GH TES v1.1 API.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A request to the server failed.
#[derive(Debug, thiserror::Error)]
pub enum TesError {
    #[error("the TES server at {url} cannot be reached: {message}")]
    Request { url: String, message: String },
    #[error("the TES server answered {status} to {url}: {body}")]
    Status {
        url: String,
        status: u16,
        body: String,
    },
    #[error("the TES server gave an answer that is not a TES document: {0}")]
    Answer(String),
}

/// A task as uwe sends it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub name: String,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    pub executors: Vec<ExecutorSpec>,
    /// Directories that the executors share, made empty when the task starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
    pub tags: BTreeMap<String, String>,
}

/// `FILE` or `DIRECTORY`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum FileType {
    #[default]
    File,
    Directory,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Input {
    /// The absolute path in the container.
    pub path: String,
    pub url: String,
    #[serde(rename = "type")]
    pub kind: FileType,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub path: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    #[serde(rename = "type")]
    pub kind: FileType,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Resources {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram_gb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_gb: Option<f64>,
}

/// One executor of a task: a command in an image.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExecutorSpec {
    pub image: String,
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// The state of a task on the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum State {
    Unknown,
    Queued,
    Initializing,
    Running,
    Paused,
    Complete,
    ExecutorError,
    SystemError,
    Canceled,
    Canceling,
    Preempted,
}

impl State {
    /// Whether the task will not change any more.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            State::Complete
                | State::ExecutorError
                | State::SystemError
                | State::Canceled
                | State::Preempted
        )
    }
}

/// What the server tells of a task.
#[derive(Clone, Debug, Deserialize)]
pub struct Status {
    pub id: String,
    pub state: State,
    #[serde(default)]
    pub logs: Vec<TaskLog>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TaskLog {
    #[serde(default)]
    pub system_logs: Vec<String>,
    #[serde(default)]
    pub logs: Vec<ExecutorLog>,
    #[serde(default)]
    pub outputs: Vec<FileLog>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ExecutorLog {
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FileLog {
    pub path: String,
    pub url: String,
    pub size_bytes: String,
}

#[derive(Deserialize)]
struct Created {
    id: String,
}

/// A task in a list of the server, with its tags.
#[derive(Clone, Debug, Deserialize)]
pub struct Listed {
    pub id: String,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    tasks: Vec<Listed>,
    #[serde(default)]
    next_page_token: Option<String>,
}

/// The most pages of tasks that [`Client::list`] reads.
const MOST_PAGES: usize = 100;

/// A client of one TES server.
#[derive(Clone)]
pub struct Client {
    /// The address of the API, like `http://localhost:8000/ga4gh/tes/v1`.
    base: String,
    http: &'static reqwest::Client,
    /// A bearer token for the server, when it needs one.
    token: Option<String>,
    basic: Option<(String, String)>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client of the server at `server`, like `http://localhost:8000`; the API path
    /// `/ga4gh/tes/v1` is added unless the address has it.
    pub fn new(server: &str, token: Option<String>) -> Result<Client, TesError> {
        let address = reqwest::Url::parse(server)
            .map_err(|_| TesError::Answer("TES server must be an HTTP address".into()))?;
        if !matches!(address.scheme(), "http" | "https")
            || !address.username().is_empty()
            || address.password().is_some()
            || address.query().is_some()
            || address.fragment().is_some()
        {
            return Err(TesError::Answer(
                "TES server must be an HTTP address without credentials, query or fragment".into(),
            ));
        }
        let server = server.trim_end_matches('/');
        let base = match server.ends_with("/ga4gh/tes/v1") {
            true => server.to_owned(),
            false => format!("{server}/ga4gh/tes/v1"),
        };
        let http = executor::http::client().map_err(|message| TesError::Request {
            url: base.clone(),
            message,
        })?;
        Ok(Client {
            base,
            http,
            token,
            basic: None,
        })
    }

    pub fn with_basic(mut self, user: String, password: String) -> Self {
        self.basic = Some((user, password));
        self
    }

    pub fn redact(&self, text: &str) -> String {
        let mut text = text.to_owned();
        if let Some((user, password)) = &self.basic
            && let Ok(request) = self
                .http
                .get(&self.base)
                .basic_auth(user, Some(password))
                .build()
            && let Some(value) = request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
        {
            text = text.replace(value, "[secret]");
        }
        for secret in self.token.iter().chain(
            self.basic
                .iter()
                .flat_map(|(user, password)| [user, password]),
        ) {
            if !secret.is_empty() {
                text = text.replace(secret, "[secret]");
            }
        }
        text
    }

    pub fn server(&self) -> &str {
        &self.base
    }

    pub(crate) fn identity(&self) -> String {
        let bytes =
            serde_json::to_vec(&(&self.token, &self.basic)).expect("authentication strings encode");
        task_model::Digest::sha256(&bytes).hex().to_owned()
    }

    /// Creates a task and gives its id.
    pub async fn create(&self, task: &Task) -> Result<String, TesError> {
        let url = format!("{}/tasks", self.base);
        let body =
            serde_json::to_string(task).map_err(|error| TesError::Answer(error.to_string()))?;
        let request = self
            .http
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        let created: Created = self.send(request, &url).await?;
        Ok(created.id)
    }

    /// The state of a task, with the logs of the system when it failed.
    pub async fn status(&self, id: &str) -> Result<Status, TesError> {
        let url = format!("{}/tasks/{id}?view=FULL", self.base);
        let mut status: Status = self.send(self.http.get(&url), &url).await?;
        for log in &mut status.logs {
            for text in &mut log.system_logs {
                *text = self.redact(text);
            }
            for executor in &mut log.logs {
                executor.stdout = executor.stdout.as_deref().map(|text| self.redact(text));
                executor.stderr = executor.stderr.as_deref().map(|text| self.redact(text));
            }
        }
        Ok(status)
    }

    /// The tasks whose name starts with `name` and that have the tag `key` with `value`, with
    /// their tags. A server may ignore these filters, so the caller checks the tags.
    pub async fn list(&self, name: &str, key: &str, value: &str) -> Result<Vec<Listed>, TesError> {
        let mut tasks = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MOST_PAGES {
            let mut query = vec![
                ("name_prefix", name),
                ("tag_key", key),
                ("tag_value", value),
                ("view", "BASIC"),
            ];
            if let Some(token) = &token {
                query.push(("page_token", token));
            }
            let url = reqwest::Url::parse_with_params(&format!("{}/tasks", self.base), &query)
                .map_err(|error| TesError::Answer(error.to_string()))?;
            let page: Page = self.send(self.http.get(url.as_str()), url.as_str()).await?;
            tasks.extend(page.tasks);
            token = page.next_page_token.filter(|token| !token.is_empty());
            if token.is_none() {
                return Ok(tasks);
            }
        }
        Err(TesError::Answer(format!(
            "the list of tasks has more than {MOST_PAGES} pages"
        )))
    }

    /// Asks the server about itself (`GET /service-info`), which tells whether it answers.
    pub async fn service_info(&self) -> Result<serde_json::Value, TesError> {
        let url = format!("{}/service-info", self.base);
        self.send(self.http.get(&url), &url).await
    }

    /// Asks the server to cancel a task.
    pub async fn cancel(&self, id: &str) -> Result<(), TesError> {
        let url = format!("{}/tasks/{id}:cancel", self.base);
        let _: serde_json::Value = self.send(self.http.post(&url).body("{}"), &url).await?;
        Ok(())
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        mut request: reqwest::RequestBuilder,
        url: &str,
    ) -> Result<T, TesError> {
        request = request.timeout(Duration::from_secs(120));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        } else if let Some((user, password)) = &self.basic {
            request = request.basic_auth(user, Some(password));
        }
        let failed = |error: reqwest::Error| TesError::Request {
            url: url.to_owned(),
            message: self.redact(&cause(&error)),
        };
        let response = request.send().await.map_err(failed)?;
        let status = response.status();
        let text = response.text().await.map_err(failed)?;
        if !status.is_success() {
            return Err(TesError::Status {
                url: url.to_owned(),
                status: status.as_u16(),
                body: self.redact(&text),
            });
        }
        serde_json::from_str(&text)
            .map_err(|error| TesError::Answer(self.redact(&format!("{error}: {text}"))))
    }
}

/// What went wrong with a request, in the words of its deepest cause, like `Connection
/// refused`, which says more than `error sending request for url`.
fn cause(error: &reqwest::Error) -> String {
    let mut cause: &dyn std::error::Error = error;
    while let Some(source) = cause.source() {
        cause = source;
    }
    cause.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_authentication_secrets() {
        let client = Client::new("http://localhost:1", Some("private-token".into()))
            .unwrap()
            .with_basic("private-user".into(), "private-password".into());
        let printed = format!("{client:?}");
        assert!(!printed.contains("private"));
        assert_eq!(
            client.redact("private-token private-user private-password"),
            "[secret] [secret] [secret]"
        );
    }

    #[test]
    fn reads_terminal_logs() {
        let status: Status = serde_json::from_value(serde_json::json!({
            "id": "x", "state": "COMPLETE", "logs": [{
                "logs": [{"exit_code": 0, "stdout": "tail", "stderr": null}],
                "outputs": [{"path": "/work/x", "url": "s3://bucket/x", "size_bytes": "4"}]
            }]
        }))
        .unwrap();
        assert_eq!(status.logs[0].logs[0].exit_code, Some(0));
        assert_eq!(status.logs[0].outputs[0].size_bytes, "4");
    }

    #[test]
    fn preserves_specification_names() {
        let task = Task {
            name: "a".into(),
            inputs: vec![Input {
                path: "/uwe/work/in".into(),
                url: "file:///s/in".into(),
                kind: FileType::Directory,
            }],
            executors: vec![ExecutorSpec {
                image: "debian".into(),
                command: vec!["bash".into()],
                workdir: None,
                env: BTreeMap::new(),
            }],
            resources: Some(Resources {
                cpu_cores: Some(2),
                ..Resources::default()
            }),
            ..Task::default()
        };
        let text = serde_json::to_string(&task).expect("writes");
        assert!(text.contains(r#""type":"DIRECTORY""#), "{text}");
        assert!(text.contains(r#""cpu_cores":2"#), "{text}");
        assert!(
            !text.contains("volumes") && !text.contains("workdir"),
            "{text}"
        );
        let status: Status =
            serde_json::from_str(r#"{"id":"x","state":"EXECUTOR_ERROR"}"#).expect("reads");
        assert!(status.state.is_final() && status.logs.is_empty());
    }
}
