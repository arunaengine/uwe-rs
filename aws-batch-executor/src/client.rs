//! Calls the AWS Batch and CloudWatch Logs JSON APIs, signed with the shared SigV4 signer, with
//! a pace for the calls and retries for throttling and server errors.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use storage::aws::{self, Credentials};

/// How often a throttled or failed call is tried again before it fails, as Nextflow does.
const RETRIES: u32 = 10;

/// The first pause before a retry; it doubles each time, up to [`LONGEST_PAUSE`].
const FIRST_PAUSE: Duration = Duration::from_millis(250);
const LONGEST_PAUSE: Duration = Duration::from_secs(10);

/// The log group that AWS Batch writes to when the job definition names none.
pub const LOG_GROUP: &str = "/aws/batch/job";

/// A client of one region of AWS Batch and of CloudWatch Logs.
#[derive(Debug)]
pub struct Client {
    pub region: String,
    /// The address of Batch, like `https://batch.eu-west-1.amazonaws.com`.
    pub batch: String,
    /// The address of CloudWatch Logs, like `https://logs.eu-west-1.amazonaws.com`.
    pub logs: String,
    credentials: Credentials,
    /// The least time between two calls: 20 ms for Nextflow's default of 50 calls a second.
    pace: Duration,
    next: tokio::sync::Mutex<Instant>,
}

/// Why a call failed.
#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    /// The service did not answer, or the request could not be signed.
    #[error("{operation} failed: {message}")]
    Request { operation: String, message: String },
    /// The service answered with an error status.
    #[error("{operation} answered {status} {code}: {message}")]
    Status {
        operation: String,
        status: u16,
        code: String,
        message: String,
    },
}

impl BatchError {
    /// Whether the service failed rather than the request, so that a later try may work.
    pub fn retryable(&self) -> bool {
        match self {
            BatchError::Request { .. } => true,
            BatchError::Status { status, code, .. } => {
                *status >= 500 || *status == 429 || code == "TooManyRequestsException"
            }
        }
    }

    /// The error code of AWS, like `ResourceNotFoundException`; empty without an answer.
    pub fn code(&self) -> &str {
        match self {
            BatchError::Request { .. } => "",
            BatchError::Status { code, .. } => code,
        }
    }
}

impl Client {
    /// A client of `region` with the addresses of AWS, unless `AWS_ENDPOINT_URL_BATCH`,
    /// `AWS_ENDPOINT_URL_CLOUDWATCH_LOGS` or `AWS_ENDPOINT_URL` name others.
    pub fn new(region: &str, credentials: Credentials, pace: Duration) -> Client {
        let variable = |name: &str| std::env::var(name).ok();
        let batch = format!("https://batch.{region}.amazonaws.com");
        let logs = format!("https://logs.{region}.amazonaws.com");
        Client {
            region: region.to_owned(),
            batch: aws::endpoint("BATCH", batch, variable),
            logs: aws::endpoint("CLOUDWATCH_LOGS", logs, variable),
            credentials,
            pace,
            next: tokio::sync::Mutex::new(Instant::now()),
        }
    }

    /// Calls the Batch operation `operation`, like `submitjob`, with a JSON body.
    pub async fn batch(&self, operation: &str, body: &Value) -> Result<Value, BatchError> {
        let request = aws::Request {
            service: "batch".into(),
            region: self.region.clone(),
            method: "POST".into(),
            url: format!("{}/v1/{operation}", self.batch),
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.to_string().into_bytes(),
        };
        self.send(operation, request).await
    }

    /// The last `limit` lines of a CloudWatch Logs stream; empty when the stream is not there.
    pub async fn log_tail(
        &self,
        group: &str,
        stream: &str,
        limit: u32,
    ) -> Result<Vec<String>, BatchError> {
        let body = json!({
            "logGroupName": group,
            "logStreamName": stream,
            "startFromHead": false,
            "limit": limit,
        });
        let request = aws::Request {
            service: "logs".into(),
            region: self.region.clone(),
            method: "POST".into(),
            url: format!("{}/", self.logs),
            headers: vec![
                ("content-type".into(), "application/x-amz-json-1.1".into()),
                ("x-amz-target".into(), "Logs_20140328.GetLogEvents".into()),
            ],
            body: body.to_string().into_bytes(),
        };
        let answer = match self.send("GetLogEvents", request).await {
            Err(error) if error.code() == "ResourceNotFoundException" => return Ok(Vec::new()),
            answer => answer?,
        };
        let events = answer["events"].as_array().cloned().unwrap_or_default();
        Ok(events
            .iter()
            .filter_map(|event| event["message"].as_str())
            .map(|line| line.trim_end().to_owned())
            .collect())
    }

    /// Sends a request at the pace of the client, again after a pause while it fails for a
    /// reason that may pass.
    async fn send(&self, operation: &str, request: aws::Request) -> Result<Value, BatchError> {
        let mut pause = FIRST_PAUSE;
        let mut tries = 0;
        loop {
            self.wait_turn().await;
            // A submission without an answer may have made a job, which a retry would double.
            let unanswered = |error: &BatchError| {
                operation == "submitjob" && matches!(error, BatchError::Request { .. })
            };
            match self.once(operation, request.clone()).await {
                Err(error) if error.retryable() && !unanswered(&error) && tries < RETRIES => {
                    tracing::debug!(operation, %error, "trying the AWS call again");
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

    /// One signed request; the signer and its HTTP client wait for the answer on a thread of
    /// their own.
    async fn once(&self, operation: &str, request: aws::Request) -> Result<Value, BatchError> {
        let credentials = self.credentials.clone();
        let sent = tokio::task::spawn_blocking(move || aws::send(request, &credentials)).await;
        let failed = |message: String| BatchError::Request {
            operation: operation.to_owned(),
            message,
        };
        let answer = sent
            .map_err(|error| failed(error.to_string()))?
            .map_err(|error| failed(error.to_string()))?;
        let body: Value = serde_json::from_slice(&answer.body).unwrap_or(Value::Null);
        if answer.status < 300 {
            return Ok(body);
        }
        // AWS names the error in a header, or in the `__type` of the body, like
        // `com.amazonaws...#ResourceNotFoundException`.
        let header = (answer.headers.iter())
            .find(|(name, _)| name.eq_ignore_ascii_case("x-amzn-errortype"))
            .map(|(_, value)| value.clone());
        let code = header
            .or_else(|| body["__type"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let code = code.split(':').next().unwrap_or_default();
        let code = code.rsplit('#').next().unwrap_or_default().to_owned();
        let message = (body["message"].as_str())
            .or_else(|| body["Message"].as_str())
            .unwrap_or(&answer.reason)
            .to_owned();
        Err(BatchError::Status {
            operation: operation.to_owned(),
            status: answer.status,
            code,
            message,
        })
    }
}
