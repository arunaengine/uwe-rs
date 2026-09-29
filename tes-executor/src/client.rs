//! A small client of the GA4GH TES v1.1 API: the parts of a task that iunctus sends, and the
//! calls to create a task, read its state and cancel it. Field names are those of the OpenAPI
//! file, in snake case.
// Copyright (c) 2026 The iunctus Contributors
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

/// A task as iunctus sends it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub name: String,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    pub executors: Vec<ExecutorSpec>,
    /// Directories that the executors share, made empty when the task starts.
    #[serde(skip_serializing_if = "Vec::is_empty")]
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
}

#[derive(Deserialize)]
struct Created {
    id: String,
}

/// A client of one TES server.
#[derive(Clone, Debug)]
pub struct Client {
    /// The address of the API, like `http://localhost:8000/ga4gh/tes/v1`.
    base: String,
    http: &'static reqwest::Client,
    /// A bearer token for the server, when it needs one.
    token: Option<String>,
}

impl Client {
    /// A client of the server at `server`, like `http://localhost:8000`; the API path
    /// `/ga4gh/tes/v1` is added unless the address has it.
    pub fn new(server: &str, token: Option<String>) -> Result<Client, TesError> {
        let server = server.trim_end_matches('/');
        let base = match server.ends_with("/ga4gh/tes/v1") {
            true => server.to_owned(),
            false => format!("{server}/ga4gh/tes/v1"),
        };
        let http = executor::http::client().map_err(|message| TesError::Request {
            url: base.clone(),
            message,
        })?;
        Ok(Client { base, http, token })
    }

    pub fn server(&self) -> &str {
        &self.base
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
        self.send(self.http.get(&url), &url).await
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
        }
        let failed = |error: reqwest::Error| TesError::Request {
            url: url.to_owned(),
            message: cause(&error),
        };
        let response = request.send().await.map_err(failed)?;
        let status = response.status();
        let text = response.text().await.map_err(failed)?;
        if !status.is_success() {
            return Err(TesError::Status {
                url: url.to_owned(),
                status: status.as_u16(),
                body: text,
            });
        }
        serde_json::from_str(&text).map_err(|error| TesError::Answer(format!("{error}: {text}")))
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
    fn preserves_specification_names() {
        let task = Task {
            name: "a".into(),
            inputs: vec![Input {
                path: "/iunctus/work/in".into(),
                url: "file:///s/in".into(),
                kind: FileType::Directory,
            }],
            executors: vec![ExecutorSpec {
                image: "debian".into(),
                command: vec!["bash".into()],
                workdir: None,
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
