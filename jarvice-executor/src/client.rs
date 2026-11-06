//! Calls the JARVICE API as one user: submits batch jobs, reads their status and output, and
//! shuts them down. The API key goes in bodies and query strings, so no URL is ever logged.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use serde_json::{Value, json};

/// How long one call may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The line of 79 `#` that ends the header of the platform in the output of a job.
const HEADER_END: &str =
    "###############################################################################";

/// A client of the JARVICE API for one user.
pub struct Client {
    /// The base of the API, like `https://cloud.nimbix.net/api`.
    pub address: String,
    pub username: String,
    api_key: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("address", &self.address)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// Why a call failed.
#[derive(Debug, thiserror::Error)]
pub enum JarviceError {
    /// No answer came.
    #[error("{operation} failed: {message}")]
    Request { operation: String, message: String },
    /// JARVICE answered with an error status, or with an error in its body.
    #[error("{operation} answered {status}: {message}")]
    Status {
        operation: String,
        status: u16,
        message: String,
    },
}

/// The job that a submission started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submitted {
    pub name: String,
    pub number: u64,
}

impl Client {
    pub fn new(address: &str, username: String, api_key: String) -> Client {
        Client {
            address: address.trim_end_matches('/').to_owned(),
            username,
            api_key,
        }
    }

    /// Submits a batch job: `body` with the user of this client.
    pub async fn submit(&self, body: &Value) -> Result<Submitted, JarviceError> {
        let mut body = body.clone();
        body["user"] = json!({"username": self.username, "apikey": self.api_key});
        let answer = self.call("submit", reqwest::Method::POST, "batch", &[], Some(&body));
        let answer: Value = serde_json::from_str(&answer.await?).unwrap_or(Value::Null);
        match (answer["name"].as_str(), answer["number"].as_u64()) {
            (Some(name), Some(number)) => Ok(Submitted {
                name: name.to_owned(),
                number,
            }),
            _ => Err(JarviceError::Status {
                operation: "submit".into(),
                status: 200,
                message: format!("no job name and number in the answer: {answer}"),
            }),
        }
    }

    /// The `job_status` of the job, like `PROCESSING STARTING`; `None` when JARVICE names none.
    pub async fn status(&self, job: &Submitted) -> Result<Option<String>, JarviceError> {
        let answer = self.job("status", "status", &job.name, &[]).await?;
        let answer: Value = serde_json::from_str(&answer).unwrap_or(Value::Null);
        let entry = match answer.get(job.number.to_string()) {
            Some(entry) => entry,
            None => (answer.as_object())
                .and_then(|jobs| jobs.values().next())
                .unwrap_or(&Value::Null),
        };
        Ok(entry["job_status"].as_str().map(str::to_owned))
    }

    /// All lines of the output of a job that ended.
    pub async fn output(&self, job: &Submitted) -> Result<String, JarviceError> {
        self.job("output", "output", &job.name, &[("lines", "0")])
            .await
    }

    /// Asks the job to end, which works only while it runs.
    pub async fn shutdown(&self, job: &Submitted) -> Result<(), JarviceError> {
        self.job("shutdown", "shutdown", &job.name, &[]).await?;
        Ok(())
    }

    /// Ends the job at once, in any state.
    pub async fn terminate(&self, job: &Submitted) -> Result<(), JarviceError> {
        self.job("terminate", "terminate", &job.name, &[]).await?;
        Ok(())
    }

    async fn job(
        &self,
        operation: &str,
        path: &str,
        name: &str,
        more: &[(&str, &str)],
    ) -> Result<String, JarviceError> {
        let mut query = vec![
            ("username", self.username.as_str()),
            ("apikey", self.api_key.as_str()),
            ("name", name),
        ];
        query.extend_from_slice(more);
        (self.call(operation, reqwest::Method::GET, path, &query, None)).await
    }

    async fn call(
        &self,
        operation: &str,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<String, JarviceError> {
        let failed = |message: String| JarviceError::Request {
            operation: operation.to_owned(),
            message,
        };
        let client = storage::http_client().map_err(failed)?;
        let address = format!("{}/jarvice/{path}", self.address);
        let url = reqwest::Url::parse_with_params(&address, query)
            .map_err(|error| failed(error.to_string()))?;
        let mut request = client.request(method, url).timeout(TIMEOUT);
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        // The URL holds the key, so errors leave it out.
        let answer =
            (request.send().await).map_err(|error| failed(error.without_url().to_string()))?;
        let status = answer.status();
        let text =
            (answer.text().await).map_err(|error| failed(error.without_url().to_string()))?;
        let error = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|answer| answer["error"].as_str().map(str::to_owned));
        match (status.is_success(), error) {
            (true, None) => Ok(text),
            (_, error) => Err(JarviceError::Status {
                operation: operation.to_owned(),
                status: status.as_u16(),
                message: error.unwrap_or_else(|| text.trim().chars().take(2000).collect()),
            }),
        }
    }
}

/// The lines of the output of a job after the header of the platform, which ends with a line of
/// 79 `#`; all lines without that header.
pub fn output_lines(text: &str) -> &str {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end_matches(['\r', '\n']) == HEADER_END {
            return &text[offset..];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_output_header() {
        let text = format!("JARVICE job\n{HEADER_END}\nhello\nworld\n");
        assert_eq!(output_lines(&text), "hello\nworld\n");
        assert_eq!(output_lines("no header\n"), "no header\n");
        assert_eq!(HEADER_END.len(), 79);
    }
}
