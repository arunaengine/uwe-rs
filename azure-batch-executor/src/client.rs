//! Calls the Azure Batch REST API for pools, jobs, tasks and task files, authorized by Shared Key
//! or Entra ID, and again after a pause while Azure is busy or failing.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Method, Url};
use serde_json::Value;

use crate::auth::Credentials;

/// The retry policy of Nextflow's `azure.retryPolicy`: a first pause that doubles up to a
/// longest one, a share of it as jitter, and the tries in all.
const FIRST_PAUSE: Duration = Duration::from_millis(250);
const LONGEST_PAUSE: Duration = Duration::from_secs(90);
const JITTER: f64 = 0.25;
const TRIES: u32 = 10;

/// The answers that a later try may not get.
const PASSING: [u16; 6] = [408, 429, 500, 502, 503, 504];

/// How long one call may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The content type of the bodies that Azure Batch takes.
const JSON: &str = "application/json; odata=minimalmetadata";

/// A client of one Batch account.
#[derive(Debug)]
pub struct Client {
    /// The address of the account, like `https://lab.westeurope.batch.azure.com`.
    pub endpoint: String,
    pub account: String,
    pub api_version: String,
    credentials: Credentials,
}

/// Why a call failed.
#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    /// No answer came, or no token.
    #[error("{operation} failed: {message}")]
    Request { operation: String, message: String },
    /// Azure answered with an error status and its error code, like 404 `PoolNotFound`.
    #[error("{operation} answered {status} {code}: {message}")]
    Status {
        operation: String,
        status: u16,
        code: String,
        message: String,
    },
}

/// A successful answer: its JSON body, or its text for a file, and its length.
#[derive(Debug, Default)]
pub struct Answer {
    pub body: Value,
    pub text: String,
    pub length: Option<u64>,
}

impl BatchError {
    /// Whether Azure failed rather than the request, so that a later try may work.
    pub fn retryable(&self) -> bool {
        match self {
            BatchError::Request { .. } => true,
            BatchError::Status { status, .. } => PASSING.contains(status),
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            BatchError::Request { .. } => None,
            BatchError::Status { status, .. } => Some(*status),
        }
    }

    /// Whether Azure named `code` as the reason, in its code or its message.
    pub fn names(&self, code: &str) -> bool {
        match self {
            BatchError::Request { .. } => false,
            BatchError::Status {
                code: own, message, ..
            } => own == code || message.contains(code),
        }
    }
}

impl Client {
    pub fn new(
        endpoint: String,
        account: String,
        api_version: String,
        credentials: Credentials,
    ) -> Client {
        Client {
            endpoint,
            account,
            api_version,
            credentials,
        }
    }

    /// The pool `id`; `None` when it does not exist.
    pub async fn pool(&self, id: &str) -> Result<Option<Value>, BatchError> {
        let path = format!("/pools/{id}");
        found(self.send("get pool", Method::GET, &path, None).await)
    }

    /// Creates a pool; one that exists already, as another run made it, is no error.
    pub async fn create_pool(&self, body: &Value) -> Result<(), BatchError> {
        match self
            .send("create pool", Method::POST, "/pools", Some(body))
            .await
        {
            Err(error) if error.status() == Some(409) && error.names("PoolExists") => Ok(()),
            answer => answer.map(|_| ()),
        }
    }

    /// Every image that pools of the account can use, page by page.
    pub async fn supported_images(&self) -> Result<Vec<Value>, BatchError> {
        let mut images = Vec::new();
        let mut next = Some("/supportedimages".to_owned());
        while let Some(path) = next.take() {
            let answer = self.send("list images", Method::GET, &path, None).await?;
            let page = answer.body["value"].as_array().cloned().unwrap_or_default();
            images.extend(page);
            if let Some(link) = answer.body["odata.nextLink"].as_str() {
                let url = Url::parse(link).map_err(|error| BatchError::Request {
                    operation: "list images".into(),
                    message: format!("a next page that is not a URL: {error}"),
                })?;
                // The next page names its own api-version, which `send` adds again.
                let pairs = url.query_pairs().filter(|(name, _)| name != "api-version");
                let query: Vec<String> = pairs
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect();
                next = Some(format!("{}?{}", url.path(), query.join("&")));
            }
        }
        Ok(images)
    }

    pub async fn create_job(&self, body: &Value) -> Result<(), BatchError> {
        match self
            .send("create job", Method::POST, "/jobs", Some(body))
            .await
        {
            Err(error) if error.status() == Some(409) && error.names("JobExists") => Ok(()),
            answer => answer.map(|_| ()),
        }
    }

    pub async fn job(&self, id: &str) -> Result<Option<Value>, BatchError> {
        found(
            self.send("get job", Method::GET, &format!("/jobs/{id}"), None)
                .await,
        )
    }

    pub async fn patch_job(&self, id: &str, body: &Value) -> Result<(), BatchError> {
        let path = format!("/jobs/{id}");
        (self
            .send("update job", Method::PATCH, &path, Some(body))
            .await)
            .map(|_| ())
    }

    /// Deletes a job or a pool by its path, like `/jobs/<id>`; one that is gone is no error.
    pub async fn delete(&self, path: &str) -> Result<(), BatchError> {
        match self.send("delete", Method::DELETE, path, None).await {
            Err(error) if error.status() == Some(404) => Ok(()),
            answer => answer.map(|_| ()),
        }
    }

    /// Adds a task to a job. When a try that got no answer added it, a later try finds it.
    pub async fn create_task(&self, job: &str, body: &Value) -> Result<(), BatchError> {
        let path = format!("/jobs/{job}/tasks");
        match self.send("add task", Method::POST, &path, Some(body)).await {
            Err(error) if error.status() == Some(409) && error.names("TaskExists") => Ok(()),
            answer => answer.map(|_| ()),
        }
    }

    pub async fn task(&self, job: &str, task: &str) -> Result<Option<Value>, BatchError> {
        let path = format!("/jobs/{job}/tasks/{task}");
        found(self.send("get task", Method::GET, &path, None).await)
    }

    /// Stops a task; one that ended or is gone is no error.
    pub async fn terminate(&self, job: &str, task: &str) -> Result<(), BatchError> {
        let path = format!("/jobs/{job}/tasks/{task}/terminate");
        let empty = Value::Null;
        match self
            .send("terminate task", Method::POST, &path, Some(&empty))
            .await
        {
            Err(error) if matches!(error.status(), Some(404 | 409)) => Ok(()),
            answer => answer.map(|_| ()),
        }
    }

    /// The last `bytes` of the file `name` of a task, like `stderr.txt`; empty when it has none.
    pub async fn file_tail(
        &self,
        job: &str,
        task: &str,
        name: &str,
        bytes: u64,
    ) -> Result<String, BatchError> {
        let path = format!("/jobs/{job}/tasks/{task}/files/{name}");
        let head = match self
            .send("file properties", Method::HEAD, &path, None)
            .await
        {
            Err(error) if error.status() == Some(404) => return Ok(String::new()),
            head => head?,
        };
        let length = head.length.unwrap_or_default();
        if length == 0 {
            return Ok(String::new());
        }
        let range = format!("bytes={}-{}", length.saturating_sub(bytes), length - 1);
        let answer = self
            .request("get file", Method::GET, &path, None, Some(range))
            .await?;
        Ok(answer.text)
    }

    async fn send(
        &self,
        operation: &str,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Answer, BatchError> {
        self.request(operation, method, path, body, None).await
    }

    /// Sends a call, again after a pause while it fails for a reason that may pass.
    async fn request(
        &self,
        operation: &str,
        method: Method,
        path: &str,
        body: Option<&Value>,
        range: Option<String>,
    ) -> Result<Answer, BatchError> {
        let mut pause = FIRST_PAUSE;
        for tries in 1.. {
            match self
                .once(operation, method.clone(), path, body, range.as_deref())
                .await
            {
                Err(error) if error.retryable() && tries < TRIES => {
                    tracing::debug!(operation, %error, "[AZURE BATCH] trying the call again");
                    tokio::time::sleep(jittered(pause)).await;
                    pause = (pause * 2).min(LONGEST_PAUSE);
                }
                result => return result,
            }
        }
        unreachable!("the loop returns")
    }

    async fn once(
        &self,
        operation: &str,
        method: Method,
        path: &str,
        body: Option<&Value>,
        range: Option<&str>,
    ) -> Result<Answer, BatchError> {
        let failed = |message: String| BatchError::Request {
            operation: operation.to_owned(),
            message,
        };
        let joiner = if path.contains('?') { '&' } else { '?' };
        let address = format!(
            "{}{path}{joiner}api-version={}",
            self.endpoint, self.api_version
        );
        let url = Url::parse(&address).map_err(|error| failed(error.to_string()))?;
        let date = jiff::Timestamp::now().strftime("%a, %d %b %Y %H:%M:%S GMT");
        let text = match body {
            Some(Value::Null) => Some(String::new()),
            Some(body) => Some(body.to_string()),
            None => None,
        };
        let mut headers = vec![("ocp-date", date.to_string())];
        if let Some(text) = &text {
            headers.push(("content-length", text.len().to_string()));
            if !text.is_empty() {
                headers.push(("content-type", JSON.to_owned()));
            }
        }
        if let Some(range) = range {
            headers.push(("ocp-range", range.to_owned()));
        }
        let authorization = (self
            .credentials
            .authorize(method.as_str(), &url, &headers)
            .await)
            .map_err(failed)?;
        let client = storage::http_client().map_err(failed)?;
        let mut request =
            (client.request(method, url).timeout(TIMEOUT)).header("authorization", authorization);
        for (name, value) in headers.iter().filter(|(name, _)| *name != "content-length") {
            request = request.header(*name, value);
        }
        if let Some(text) = text {
            request = request.body(text);
        }
        // The error of reqwest names the address, which holds no secret.
        let answer = (request.send().await).map_err(|error| failed(error.to_string()))?;
        let status = answer.status();
        // A HEAD answer has no body, so its length is only in the header.
        let length = (answer.headers().get("content-length"))
            .and_then(|value| value.to_str().ok()?.parse().ok());
        let text = answer.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(Answer { body, text, length });
        }
        // Azure Batch names the error in `code`, and explains it in `message.value`.
        let message = (body["message"]["value"].as_str())
            .or(body["message"].as_str())
            .unwrap_or(status.canonical_reason().unwrap_or_default());
        Err(BatchError::Status {
            operation: operation.to_owned(),
            status: status.as_u16(),
            code: body["code"].as_str().unwrap_or_default().to_owned(),
            message: message.lines().next().unwrap_or_default().to_owned(),
        })
    }
}

/// The body of an answer, or `None` when Azure does not know the object.
fn found(answer: Result<Answer, BatchError>) -> Result<Option<Value>, BatchError> {
    match answer {
        Err(error) if error.status() == Some(404) => Ok(None),
        answer => answer.map(|answer| Some(answer.body)),
    }
}

/// A pause with up to a quarter more or less, so that many clients do not try together.
fn jittered(pause: Duration) -> Duration {
    let nanos = (SystemTime::now().duration_since(UNIX_EPOCH))
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    let share = f64::from(nanos % 1000) / 1000.0 * 2.0 - 1.0;
    pause.mul_f64(1.0 + share * JITTER)
}
