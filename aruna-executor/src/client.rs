//! Native jobs contract from Aruna 51c43dba96198542addb53d3375579c308de0b17,
//! api/src/routes/execution/jobs.rs. Submission is realm execution without a workspace.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use reqwest::{Method, Url, header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub use crate::states::{ExecutionState, JobKind, JobState, LogicalState};

const MAX_BODY: usize = 1024 * 1024;
const MAX_ERROR_BODY: usize = 8 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Submit {
    pub group_id: String,
    pub image: String,
    pub name: Option<String>,
    pub entrypoint: Option<Vec<String>>,
    pub command: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub workdir: Option<String>,
    pub cpu_cores: Option<u32>,
    pub ram_bytes: Option<u64>,
    pub max_walltime_ms: Option<u64>,
    pub executor_constraint: Option<String>,
    pub tags: BTreeMap<String, String>,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    /// Sent in the JSON body, scoped by Aruna to the caller. Replays must keep the same plan.
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Input {
    pub bucket: String,
    pub key: String,
    pub version_id: String,
    pub dest_key: String,
    pub container_path: String,
    pub mode: InputMode,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    ExactReference,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Output {
    pub bucket: String,
    pub dest_key: String,
    pub container_path: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Submitted {
    pub job_id: String,
    pub created: bool,
    pub submission_id: Option<String>,
    pub canonical_job_id: String,
    pub state: JobState,
    pub origin_node_url: String,
    pub status_url: String,
}

/// Status is evidence from one response, not a terminal-success decision.
#[derive(Clone, Debug, Deserialize)]
pub struct Status {
    pub job_id: String,
    pub kind: JobKind,
    pub state: JobState,
    pub attempts: u32,
    pub cancel_requested: bool,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
    pub progress: Progress,
    pub error: Option<JobError>,
    pub result: Option<serde_json::Value>,
    pub workspace_bucket: Option<String>,
    pub workspace_mode: String,
    #[serde(default)]
    pub locally_exhausted: bool,
    pub family: Option<Family>,
    #[serde(default)]
    pub awaiting_keys: Vec<AwaitingKey>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AwaitingKey {
    pub node_id: String,
    pub bucket: String,
    pub group_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Progress {
    pub current: u64,
    pub total: Option<u64>,
    pub unit: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct JobError {
    pub message: String,
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Family {
    pub submission_id: String,
    pub request_digest: String,
    pub canonical_job_id: String,
    pub aliases: Vec<String>,
    pub alias_count: u32,
    pub conflict_count: u32,
    pub logical_state: LogicalState,
    pub canonical_execution_id: Option<String>,
    pub executions: u32,
    pub execution_list: Vec<Execution>,
    pub duplicate_successes: u32,
    pub outputs: Vec<Object>,
    pub revision: u64,
    pub projection_digest: String,
    pub responder_node_id: Option<String>,
    pub partial: bool,
    pub locally_exhausted: bool,
    pub cancel_requested: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Execution {
    pub execution_id: String,
    pub executor_node_id: String,
    pub state: ExecutionState,
    pub started_at_ms: Option<u64>,
    pub observed_at_ms: Option<u64>,
    pub canonical: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Object {
    pub bucket: String,
    pub key: String,
    pub version_id: String,
    pub execution_id: String,
    #[serde(default)]
    pub container_path: String,
    pub size: u64,
    #[serde(default)]
    pub content_type: String,
    pub digest: Option<String>,
    /// The output owner's endpoint, not necessarily the submission endpoint.
    pub endpoint_url: Option<String>,
}

/// Bounded response evidence. It is omitted from diagnostics because it may contain secrets.
pub struct ResponseBody {
    bytes: Vec<u8>,
    pub truncated: bool,
}

impl ResponseBody {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for ResponseBody {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("ResponseBody")
            .field("length", &self.bytes.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid Aruna client configuration or job identifier")]
    Configuration,
    #[error("Aruna request failed; submission or cancellation may have been accepted")]
    Transport(#[source] reqwest::Error),
    #[error("Aruna response exceeds the size limit")]
    BodyLimit,
    #[error("Aruna answered HTTP {code}")]
    Status { code: u16, body: ResponseBody },
    #[error("Aruna returned an invalid jobs response")]
    Response(#[source] serde_json::Error),
}

#[derive(Clone)]
pub struct Client {
    base: Url,
    http: reqwest::Client,
    authorization: Option<header::HeaderValue>,
}

impl fmt::Debug for Client {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("Client")
            .field("server", &self.server())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// `server` is the REST API base, including any API prefix. No redirects or automatic retries.
    pub fn new(server: &str, token: Option<String>) -> Result<Self, Error> {
        let mut base = Url::parse(server).map_err(|_| Error::Configuration)?;
        let loopback = base.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || (token.is_some() && base.scheme() != "https" && !loopback)
        {
            return Err(Error::Configuration);
        }
        base.set_path(&format!("{}/", base.path().trim_end_matches('/')));
        let authorization = token
            .map(|token| -> Result<_, Error> {
                let mut value = header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| Error::Configuration)?;
                value.set_sensitive(true);
                Ok(value)
            })
            .transpose()?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| Error::Configuration)?;
        Ok(Self {
            base,
            http,
            authorization,
        })
    }

    /// Canonical configured endpoint for a later journal. Returned routes never replace it.
    pub fn server(&self) -> &str {
        self.base.as_str()
    }

    pub async fn submit(&self, request: &Submit) -> Result<Submitted, Error> {
        let bytes = serde_json::to_vec(request).map_err(|_| Error::Configuration)?;
        self.send(Method::POST, "compute/jobs", Some(bytes), &[200, 201])
            .await
    }

    pub async fn status(&self, id: &str) -> Result<Status, Error> {
        let path = job_path(id)?;
        self.send(Method::GET, &path, None, &[200]).await
    }

    /// A 202 records intent; even a late success remains success with cancellation requested.
    pub async fn cancel(&self, id: &str) -> Result<Status, Error> {
        let path = format!("{}/cancel", job_path(id)?);
        self.send(Method::POST, &path, None, &[200, 202]).await
    }

    // Dropping this future cancels local I/O, not a possibly accepted remote operation.
    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        accepted: &[u16],
    ) -> Result<T, Error> {
        let url = self.base.join(path).map_err(|_| Error::Configuration)?;
        let mut request = self.http.request(method, url);
        if let Some(value) = &self.authorization {
            request = request.header(header::AUTHORIZATION, value.clone());
        }
        if let Some(body) = body {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|error| Error::Transport(error.without_url()))?;
        let code = response.status().as_u16();
        if !accepted.contains(&code) {
            let mut bytes = Vec::new();
            let mut truncated = false;
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| Error::Transport(error.without_url()))?
            {
                let length = chunk.len().min(MAX_ERROR_BODY - bytes.len());
                bytes.extend_from_slice(&chunk[..length]);
                if length < chunk.len() {
                    truncated = true;
                    break;
                }
            }
            return Err(Error::Status {
                code,
                body: ResponseBody { bytes, truncated },
            });
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_BODY as u64)
        {
            return Err(Error::BodyLimit);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| Error::Transport(error.without_url()))?
        {
            if chunk.len() > MAX_BODY - bytes.len() {
                return Err(Error::BodyLimit);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(Error::Response)
    }
}

fn job_path(id: &str) -> Result<String, Error> {
    if id.len() != 26 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(Error::Configuration);
    }
    Ok(format!("compute/jobs/{id}"))
}
