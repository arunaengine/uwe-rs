//! Calls the Nomad HTTP API with the ACL token of the settings: registers, reads and stops jobs,
//! and reads the logs of tasks, again after a pause while Nomad is busy or failing.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use serde_json::{Value, json};

/// How often a call is tried in all, as nf-nomad's `retryConfig.maxAttempts`.
const TRIES: u32 = 10;

/// The first pause before a retry; it doubles each time, up to [`LONGEST_PAUSE`].
const FIRST_PAUSE: Duration = Duration::from_millis(500);
const LONGEST_PAUSE: Duration = Duration::from_secs(90);

/// How long one call may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A client of the jobs of one namespace and region of a Nomad cluster.
pub struct Client {
    /// The address of the agent, like `http://127.0.0.1:4646`.
    pub address: String,
    pub namespace: Option<String>,
    pub region: Option<String>,
    token: Option<String>,
    /// The pause before the first retry, shorter in tests.
    pub pause: Duration,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("address", &self.address)
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

/// Why a call failed.
#[derive(Debug, thiserror::Error)]
pub enum NomadError {
    /// No answer came.
    #[error("{operation} failed: {message}")]
    Request { operation: String, message: String },
    /// Nomad answered with an error status, like 403 for a token without the right policy.
    #[error("{operation} answered {status}: {message}")]
    Status {
        operation: String,
        status: u16,
        message: String,
    },
}

impl NomadError {
    /// Whether Nomad failed rather than the request, so that a later try may work.
    pub fn retryable(&self) -> bool {
        match self {
            NomadError::Request { .. } => true,
            NomadError::Status { status, .. } => matches!(status, 408 | 429 | 500..),
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            NomadError::Request { .. } => None,
            NomadError::Status { status, .. } => Some(*status),
        }
    }
}

impl Client {
    pub fn new(
        address: &str,
        token: Option<String>,
        namespace: Option<String>,
        region: Option<String>,
    ) -> Client {
        Client {
            address: address.trim_end_matches('/').to_owned(),
            namespace,
            region,
            token: token.filter(|token| !token.is_empty()),
            pause: FIRST_PAUSE,
        }
    }

    /// Registers the job, which starts it; registering the same id again updates the job.
    pub async fn register(&self, job: &Value) -> Result<Value, NomadError> {
        let body = json!({"Job": job});
        let path = "/v1/jobs".to_owned();
        self.send("register", reqwest::Method::POST, path, &[], Some(&body))
            .await
    }

    /// The allocations of the job `id`, the newest last by Nomad's index.
    pub async fn allocations(&self, id: &str) -> Result<Vec<Value>, NomadError> {
        let path = format!("/v1/job/{id}/allocations");
        let answer = (self.send("allocations", reqwest::Method::GET, path, &[], None)).await?;
        let mut list = answer.as_array().cloned().unwrap_or_default();
        list.sort_by_key(|allocation| allocation["CreateIndex"].as_u64().unwrap_or(0));
        Ok(list)
    }

    /// The evaluations of the job `id`, the latest last by Nomad's index.
    pub async fn evaluations(&self, id: &str) -> Result<Vec<Value>, NomadError> {
        let path = format!("/v1/job/{id}/evaluations");
        let answer = (self.send("evaluations", reqwest::Method::GET, path, &[], None)).await?;
        let mut list = answer.as_array().cloned().unwrap_or_default();
        list.sort_by_key(|evaluation| evaluation["ModifyIndex"].as_u64().unwrap_or(0));
        Ok(list)
    }

    /// Stops the job `id`, and with `purge` removes it from Nomad; a job that is gone already
    /// is no error.
    pub async fn stop(&self, id: &str, purge: bool) -> Result<(), NomadError> {
        let path = format!("/v1/job/{id}");
        let query = [("purge", if purge { "true" } else { "false" })];
        match self
            .send("deregister", reqwest::Method::DELETE, path, &query, None)
            .await
        {
            Err(error) if error.status() == Some(404) => Ok(()),
            answer => answer.map(|_| ()),
        }
    }

    /// The last `bytes` of the standard error of the task `task` of an allocation.
    pub async fn log_tail(
        &self,
        alloc: &str,
        task: &str,
        bytes: u64,
    ) -> Result<String, NomadError> {
        let path = format!("/v1/client/fs/logs/{alloc}");
        let offset = bytes.to_string();
        let query = [
            ("task", task),
            ("type", "stderr"),
            ("origin", "end"),
            ("offset", offset.as_str()),
            ("plain", "true"),
        ];
        let answer = (self.raw("logs", reqwest::Method::GET, &path, &query, None)).await?;
        Ok(answer)
    }

    /// Sends a call, again after a pause while it fails for a reason that may pass.
    async fn send(
        &self,
        operation: &str,
        method: reqwest::Method,
        path: String,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, NomadError> {
        let text = self.raw(operation, method, &path, query, body).await?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn raw(
        &self,
        operation: &str,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<String, NomadError> {
        let (mut pause, mut tries) = (self.pause, 1);
        loop {
            match self
                .once(operation, method.clone(), path, query, body)
                .await
            {
                Err(error) if error.retryable() && tries < TRIES => {
                    tracing::debug!(operation, %error, "[NOMAD] trying the call again");
                    tokio::time::sleep(pause).await;
                    pause = (pause * 2).min(LONGEST_PAUSE);
                    tries += 1;
                }
                result => return result,
            }
        }
    }

    async fn once(
        &self,
        operation: &str,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<String, NomadError> {
        let failed = |message: String| NomadError::Request {
            operation: operation.to_owned(),
            message,
        };
        let client = storage::http_client().map_err(failed)?;
        let mut words: Vec<(&str, &str)> = query.to_vec();
        if let Some(namespace) = &self.namespace {
            words.push(("namespace", namespace));
        }
        if let Some(region) = &self.region {
            words.push(("region", region));
        }
        let address = format!("{}{path}", self.address);
        let url = match words.is_empty() {
            true => reqwest::Url::parse(&address),
            false => reqwest::Url::parse_with_params(&address, &words),
        };
        let url = url.map_err(|error| failed(error.to_string()))?;
        let mut request = client.request(method, url).timeout(TIMEOUT);
        if let Some(token) = &self.token {
            request = request.header("X-Nomad-Token", token);
        }
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let answer = (request.send().await).map_err(|error| failed(cause(&error)))?;
        let status = answer.status();
        let text = answer.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(text);
        }
        Err(NomadError::Status {
            operation: operation.to_owned(),
            status: status.as_u16(),
            message: text.trim().chars().take(2000).collect(),
        })
    }
}

/// What went wrong with a request, in the words of its deepest cause.
fn cause(error: &reqwest::Error) -> String {
    let mut cause: &dyn std::error::Error = error;
    while let Some(source) = cause.source() {
        cause = source;
    }
    cause.to_string()
}
