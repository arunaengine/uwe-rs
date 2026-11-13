//! Calls Flyte's Connect API with JSON messages: unary calls and server streams, with an access
//! token from the API key, again after a pause while Flyte is busy or failing.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::auth::Token;
use crate::settings::ApiKey;

/// How often a call is tried in all.
const TRIES: u32 = 6;

/// The first pause before a retry; it doubles each time, up to [`LONGEST_PAUSE`].
const FIRST_PAUSE: Duration = Duration::from_millis(500);
const LONGEST_PAUSE: Duration = Duration::from_secs(30);

/// How long one unary call may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The Connect codes of failures that may pass, as the Connect protocol lists them.
const PASSING: [&str; 5] = [
    "unavailable",
    "resource_exhausted",
    "deadline_exceeded",
    "aborted",
    "internal",
];

/// A client of one Flyte control plane.
pub struct Client {
    /// The address of the control plane, like `https://flyte.example.org`.
    pub address: String,
    token: Option<Token>,
    /// The pause before the first retry, shorter in tests.
    pub pause: Duration,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

/// Why a call failed.
#[derive(Debug, thiserror::Error)]
pub enum FlyteError {
    /// No answer came.
    #[error("{operation} failed: {message}")]
    Request { operation: String, message: String },
    /// Flyte answered with an error, like the Connect code `permission_denied`.
    #[error("{operation} answered {code}: {message}")]
    Status {
        operation: String,
        status: u16,
        code: String,
        message: String,
    },
}

impl FlyteError {
    /// Whether Flyte failed rather than the request, so that a later try may work.
    pub fn retryable(&self) -> bool {
        match self {
            FlyteError::Request { .. } => true,
            FlyteError::Status { status, code, .. } => {
                PASSING.contains(&code.as_str()) || matches!(status, 408 | 429 | 502..=504)
            }
        }
    }

    fn unauthenticated(&self) -> bool {
        matches!(self, FlyteError::Status { status: 401, .. })
            || matches!(self, FlyteError::Status { code, .. } if code == "unauthenticated")
    }
}

impl Client {
    /// A client that signs its calls with tokens for `key`, or sends none without one.
    pub fn new(address: &str, key: Option<ApiKey>) -> Client {
        let address = address.trim_end_matches('/').to_owned();
        Client {
            token: key.map(|key| Token::new(&address, key)),
            address,
            pause: FIRST_PAUSE,
        }
    }

    /// A unary call of `method`, like `flyteidl2.workflow.RunService/CreateRun`.
    pub async fn call(&self, method: &str, body: &Value) -> Result<Value, FlyteError> {
        let (mut pause, mut tries, mut renewed) = (self.pause, 1, false);
        loop {
            match self.once(method, body).await {
                Err(error) if error.unauthenticated() && !renewed => {
                    // A token may end before its time; one new token is worth a try.
                    if let Some(token) = &self.token {
                        token.forget().await;
                    }
                    renewed = true;
                }
                Err(error) if error.retryable() && tries < TRIES => {
                    tracing::debug!(method, %error, "[FLYTE] trying the call again");
                    tokio::time::sleep(pause).await;
                    pause = (pause * 2).min(LONGEST_PAUSE);
                    tries += 1;
                }
                result => return result,
            }
        }
    }

    /// The messages of the server stream of `method` that come within `wait`; a stream that
    /// goes on longer is cut there.
    pub async fn stream(
        &self,
        method: &str,
        body: &Value,
        wait: Duration,
    ) -> Result<Vec<Value>, FlyteError> {
        let failed = |message: String| FlyteError::Request {
            operation: method.to_owned(),
            message,
        };
        let text = body.to_string();
        let mut envelope = vec![0];
        let length = u32::try_from(text.len()).map_err(|error| failed(error.to_string()))?;
        envelope.extend(length.to_be_bytes());
        envelope.extend(text.as_bytes());
        let request = self
            .request(method, "application/connect+json", wait)
            .await?
            .body(envelope);
        let mut answer = (request.send().await).map_err(|error| failed(cause(&error)))?;
        let status = answer.status().as_u16();
        if status != 200 {
            let text = answer.text().await.unwrap_or_default();
            return Err(status_error(method, status, &text));
        }
        let (start, mut bytes, mut messages) = (Instant::now(), Vec::new(), Vec::new());
        loop {
            let left = wait.saturating_sub(start.elapsed());
            let chunk = match tokio::time::timeout(left, answer.chunk()).await {
                Ok(chunk) => chunk.map_err(|error| failed(cause(&error)))?,
                Err(_) => break,
            };
            let Some(chunk) = chunk else { break };
            bytes.extend_from_slice(&chunk);
            while bytes.len() >= 5 {
                let size = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
                if bytes.len() < 5 + size {
                    break;
                }
                let flags = bytes[0];
                let message: Value =
                    serde_json::from_slice(&bytes[5..5 + size]).unwrap_or_default();
                bytes.drain(..5 + size);
                // The last message of a stream says how it ended.
                if flags & 2 != 0 {
                    if message["error"].is_object() {
                        return Err(status_error(method, 200, &message["error"].to_string()));
                    }
                    return Ok(messages);
                }
                messages.push(message);
            }
        }
        Ok(messages)
    }

    async fn once(&self, method: &str, body: &Value) -> Result<Value, FlyteError> {
        let failed = |message: String| FlyteError::Request {
            operation: method.to_owned(),
            message,
        };
        let request =
            (self.request(method, "application/json", TIMEOUT).await?).body(body.to_string());
        let answer = (request.send().await).map_err(|error| failed(cause(&error)))?;
        let status = answer.status().as_u16();
        let text = answer.text().await.unwrap_or_default();
        if status == 200 {
            return Ok(serde_json::from_str(&text).unwrap_or(Value::Null));
        }
        Err(status_error(method, status, &text))
    }

    async fn request(
        &self,
        method: &str,
        kind: &str,
        timeout: Duration,
    ) -> Result<reqwest::RequestBuilder, FlyteError> {
        let client = storage::http_client().map_err(|message| FlyteError::Request {
            operation: method.to_owned(),
            message,
        })?;
        let millis = timeout.as_millis().to_string();
        let mut request = client
            .post(format!("{}/{method}", self.address))
            .timeout(timeout + Duration::from_secs(5))
            .header("content-type", kind)
            .header("connect-protocol-version", "1")
            .header("connect-timeout-ms", millis);
        if let Some(token) = &self.token {
            let (name, value) = token.header().await?;
            request = request.header(name, value);
        }
        Ok(request)
    }
}

/// The field `camel` of a message, or its `snake_case` form, as protobuf JSON allows both.
pub fn field<'a>(message: &'a Value, camel: &str) -> &'a Value {
    match &message[camel] {
        Value::Null => {
            let mut snake = String::new();
            for letter in camel.chars() {
                if letter.is_ascii_uppercase() {
                    snake.push('_');
                }
                snake.push(letter.to_ascii_lowercase());
            }
            &message[snake.as_str()]
        }
        value => value,
    }
}

/// An error from its HTTP status and a Connect error body, `{"code": ..., "message": ...}`.
fn status_error(operation: &str, status: u16, text: &str) -> FlyteError {
    let body: Value = serde_json::from_str(text).unwrap_or_default();
    let code = body["code"].as_str().unwrap_or(match status {
        401 => "unauthenticated",
        403 => "permission_denied",
        404 => "not_found",
        _ => "unknown",
    });
    let message = match body["message"].as_str() {
        Some(message) => message.to_owned(),
        None => text.trim().chars().take(2000).collect(),
    };
    FlyteError::Status {
        operation: operation.to_owned(),
        status,
        code: code.to_owned(),
        message,
    }
}

/// What went wrong with a request, in the words of its deepest cause.
pub(crate) fn cause(error: &reqwest::Error) -> String {
    let mut cause: &dyn std::error::Error = error;
    while let Some(source) = cause.source() {
        cause = source;
    }
    cause.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_connect_errors() {
        let error = status_error("Get", 503, r#"{"code": "unavailable", "message": "busy"}"#);
        assert!(error.retryable());
        assert_eq!(error.to_string(), "Get answered unavailable: busy");
        let error = status_error("Get", 404, "no such run");
        assert!(!error.retryable());
        assert!(error.to_string().contains("not_found: no such run"));
        assert!(status_error("Get", 401, "").unauthenticated());
        let message = json!({"errorInfo": {"message": "a"}, "abort_info": {"reason": "b"}});
        assert_eq!(field(&message, "errorInfo")["message"], "a");
        assert_eq!(field(&message, "abortInfo")["reason"], "b");
    }
}
