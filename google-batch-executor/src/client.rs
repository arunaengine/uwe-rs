//! Calls the Google Batch and Cloud Logging REST APIs with an access token, at a pace, and
//! again after a pause while Google is busy or failing.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::token::Credentials;

/// How often a call is tried in all, as Nextflow's `google.batch.retryPolicy.maxAttempts`.
const TRIES: u32 = 5;

/// The first pause before a retry; it doubles each time, up to [`LONGEST_PAUSE`].
const FIRST_PAUSE: Duration = Duration::from_millis(350);
const LONGEST_PAUSE: Duration = Duration::from_secs(90);

/// How long one call may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// A client of the jobs of one project and location, and of Cloud Logging.
#[derive(Debug)]
pub struct Client {
    pub project: String,
    pub location: String,
    /// The address of Batch, like `https://batch.googleapis.com`.
    pub batch: String,
    /// The address of Cloud Logging, like `https://logging.googleapis.com`.
    pub logging: String,
    credentials: Credentials,
    /// The least time between two calls: 20 ms for Nextflow's default of 50 calls a second.
    pace: Duration,
    next: tokio::sync::Mutex<Instant>,
}

/// Why a call failed.
#[derive(Debug, thiserror::Error)]
pub enum GoogleError {
    /// No answer came, or no token.
    #[error("{operation} failed: {message}")]
    Request { operation: String, message: String },
    /// Google answered with an error status, like 409 `ALREADY_EXISTS`.
    #[error("{operation} answered {status} {code}: {message}")]
    Status {
        operation: String,
        status: u16,
        code: String,
        message: String,
    },
}

impl GoogleError {
    /// Whether Google failed rather than the request, so that a later try may work.
    pub fn retryable(&self) -> bool {
        match self {
            GoogleError::Request { .. } => true,
            GoogleError::Status { status, .. } => *status >= 500 || *status == 429,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            GoogleError::Request { .. } => None,
            GoogleError::Status { status, .. } => Some(*status),
        }
    }
}

impl Client {
    pub fn new(
        project: String,
        location: String,
        credentials: Credentials,
        settings: &crate::Settings,
        pace: Duration,
    ) -> Client {
        Client {
            project,
            location,
            batch: settings.batch_endpoint.clone(),
            logging: settings.logging_endpoint.clone(),
            credentials,
            pace,
            next: tokio::sync::Mutex::new(Instant::now()),
        }
    }

    /// Creates the job `id`. When a try that got no answer made the job, a later try finds it
    /// there and gives it back.
    pub async fn create(&self, id: &str, body: &Value) -> Result<Value, GoogleError> {
        let url = format!("{}?job_id={id}", self.jobs());
        let (mut pause, mut tries) = (FIRST_PAUSE, 1);
        loop {
            self.wait_turn().await;
            match self
                .once("create", reqwest::Method::POST, &url, Some(body))
                .await
            {
                Err(error) if error.status() == Some(409) && tries > 1 => {
                    let found = self.job(id).await?;
                    return found.ok_or(error);
                }
                Err(error) if error.retryable() && tries < TRIES => {
                    tracing::debug!(%error, "[GOOGLE BATCH] trying to create the job again");
                    tokio::time::sleep(pause).await;
                    pause = (pause * 2).min(LONGEST_PAUSE);
                    tries += 1;
                }
                result => return result,
            }
        }
    }

    /// The job `id`; `None` when Google does not know it.
    pub async fn job(&self, id: &str) -> Result<Option<Value>, GoogleError> {
        let url = format!("{}/{id}", self.jobs());
        match self.send("get", reqwest::Method::GET, &url, None).await {
            Err(error) if error.status() == Some(404) => Ok(None),
            answer => answer.map(Some),
        }
    }

    /// Deletes the job `id`, which stops its tasks; a job that is gone already is no error.
    pub async fn delete(&self, id: &str) -> Result<(), GoogleError> {
        let url = format!("{}/{id}", self.jobs());
        match self
            .send("delete", reqwest::Method::DELETE, &url, None)
            .await
        {
            Err(error) if error.status() == Some(404) => Ok(()),
            answer => answer.map(|_| ()),
        }
    }

    /// The last `limit` lines that the task of the job with `uid` wrote to Cloud Logging.
    pub async fn log_tail(&self, uid: &str, limit: u32) -> Result<Vec<String>, GoogleError> {
        let project = &self.project;
        let filter = format!(
            "(resource.type=\"generic_task\" OR resource.type=\"batch.googleapis.com/Job\") \
             AND logName=\"projects/{project}/logs/batch_task_logs\" \
             AND labels.job_uid=\"{uid}\""
        );
        let body = json!({
            "resourceNames": [format!("projects/{project}")],
            "filter": filter,
            "orderBy": "timestamp desc",
            "pageSize": limit,
        });
        let url = format!("{}/v2/entries:list", self.logging);
        let answer = (self.send("entries.list", reqwest::Method::POST, &url, Some(&body))).await?;
        let entries = answer["entries"].as_array().cloned().unwrap_or_default();
        let lines = entries.iter().rev().filter_map(|entry| {
            (entry["textPayload"].as_str()).or_else(|| entry["jsonPayload"]["message"].as_str())
        });
        Ok(lines.map(|line| line.trim_end().to_owned()).collect())
    }

    fn jobs(&self) -> String {
        format!(
            "{}/v1/projects/{}/locations/{}/jobs",
            self.batch, self.project, self.location
        )
    }

    /// Sends a call at the pace of the client, again after a pause while it fails for a reason
    /// that may pass.
    async fn send(
        &self,
        operation: &str,
        method: reqwest::Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Value, GoogleError> {
        let (mut pause, mut tries) = (FIRST_PAUSE, 1);
        loop {
            self.wait_turn().await;
            match self.once(operation, method.clone(), url, body).await {
                Err(error) if error.retryable() && tries < TRIES => {
                    tracing::debug!(operation, %error, "[GOOGLE BATCH] trying the call again");
                    tokio::time::sleep(pause).await;
                    pause = (pause * 2).min(LONGEST_PAUSE);
                    tries += 1;
                }
                result => return result,
            }
        }
    }

    async fn wait_turn(&self) {
        let mut next = self.next.lock().await;
        let now = Instant::now();
        if *next > now {
            tokio::time::sleep(*next - now).await;
        }
        *next = Instant::now() + self.pace;
    }

    async fn once(
        &self,
        operation: &str,
        method: reqwest::Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Value, GoogleError> {
        let failed = |message: String| GoogleError::Request {
            operation: operation.to_owned(),
            message,
        };
        let token = (self.credentials.token().await).map_err(|error| failed(error.to_string()))?;
        let client = storage::http_client().map_err(failed)?;
        let mut request = client
            .request(method, url)
            .bearer_auth(token)
            .timeout(TIMEOUT);
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let answer = (request.send().await).map_err(|error| failed(error.to_string()))?;
        let status = answer.status();
        let text = answer.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(body);
        }
        // Google names the error in `error.status`, like `NOT_FOUND`.
        let error = &body["error"];
        Err(GoogleError::Status {
            operation: operation.to_owned(),
            status: status.as_u16(),
            code: error["status"].as_str().unwrap_or_default().to_owned(),
            message: (error["message"].as_str())
                .unwrap_or(status.canonical_reason().unwrap_or_default())
                .to_owned(),
        })
    }
}
