//! Calls the Snowflake SQL API: submits statements, asks about them and cancels them, signed
//! with a key-pair JWT or a token, again after a pause while Snowflake is busy or failing.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use crate::jwt::KeyPair;

/// How often a call is tried in all.
const TRIES: u32 = 6;

/// The first pause before a retry; it doubles each time, up to [`LONGEST_PAUSE`].
const FIRST_PAUSE: Duration = Duration::from_millis(500);
const LONGEST_PAUSE: Duration = Duration::from_secs(30);

/// How long one call may take; Snowflake answers a statement that runs longer with 202.
const TIMEOUT: Duration = Duration::from_secs(60);

/// How the client signs in.
#[derive(Clone)]
pub enum Auth {
    /// A JWT signed with the user's key, `KEYPAIR_JWT`.
    KeyPair(Box<KeyPair>),
    /// A programmatic access token, or an OAuth token inside Snowpark Container Services.
    Token { kind: &'static str, token: String },
}

/// The kinds of tokens, as `X-Snowflake-Authorization-Token-Type` names them.
pub const ACCESS_TOKEN: &str = "PROGRAMMATIC_ACCESS_TOKEN";
pub const OAUTH: &str = "OAUTH";

/// What a statement is doing, by the status code of the SQL API.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    /// 202: it still runs.
    Running { handle: String },
    /// 200: it ended, with its first rows.
    Done { handle: String, data: Value },
    /// 422: it failed, with Snowflake's message.
    Failed { handle: String, message: String },
}

/// Why a call failed.
#[derive(Debug, thiserror::Error)]
pub enum SqlError {
    /// No answer came.
    #[error("{operation} failed: {message}")]
    Request { operation: String, message: String },
    /// Snowflake answered with an error status, like 401 for a token it does not take.
    #[error("{operation} answered {status}: {message}")]
    Status {
        operation: String,
        status: u16,
        message: String,
    },
}

impl SqlError {
    /// Whether Snowflake failed rather than the request, so that a later try may work.
    pub fn retryable(&self) -> bool {
        match self {
            SqlError::Request { .. } => true,
            SqlError::Status { status, .. } => matches!(status, 408 | 429 | 500..),
        }
    }
}

/// The context of the statements: where the job services go, and who runs them.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub warehouse: Option<String>,
    pub role: Option<String>,
}

/// A client of the SQL API of one account.
pub struct Client {
    /// The address of the account, like `https://myorg-lab.snowflakecomputing.com`.
    pub address: String,
    auth: Auth,
    context: Context,
    /// The pause before the first retry, shorter in tests.
    pub pause: Duration,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("address", &self.address)
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(address: &str, auth: Auth, context: Context) -> Client {
        Client {
            address: address.trim_end_matches('/').to_owned(),
            auth,
            context,
            pause: FIRST_PAUSE,
        }
    }

    /// Submits a statement that runs on after the answer, with the request id `request`, which
    /// makes a retried submission run once; `seconds` limits how long it may run.
    pub async fn submit(
        &self,
        statement: &str,
        request: &str,
        seconds: Option<u64>,
    ) -> Result<Answer, SqlError> {
        let mut body = Map::new();
        body.insert("statement".into(), json!(statement));
        if let Some(seconds) = seconds {
            body.insert("timeout".into(), json!(seconds));
        }
        let context = &self.context;
        for (key, value) in [
            ("database", &context.database),
            ("schema", &context.schema),
            ("warehouse", &context.warehouse),
            ("role", &context.role),
        ] {
            if let Some(value) = value {
                body.insert(key.into(), json!(value));
            }
        }
        let body = Value::Object(body);
        let path = format!("/api/v2/statements?async=true&requestId={request}");
        self.send("submit", reqwest::Method::POST, &path, Some(&body))
            .await
    }

    /// What the statement `handle` is doing.
    pub async fn status(&self, handle: &str) -> Result<Answer, SqlError> {
        let path = format!("/api/v2/statements/{handle}");
        self.send("status", reqwest::Method::GET, &path, None).await
    }

    /// Cancels the statement `handle`.
    pub async fn cancel(&self, handle: &str) -> Result<(), SqlError> {
        let path = format!("/api/v2/statements/{handle}/cancel");
        (self
            .send("cancel", reqwest::Method::POST, &path, None)
            .await)
            .map(|_| ())
    }

    /// Runs a short statement and waits for its end, asking every `poll`.
    pub async fn execute(
        &self,
        statement: &str,
        request: &str,
        poll: Duration,
    ) -> Result<Answer, SqlError> {
        let mut answer = self.submit(statement, request, Some(120)).await?;
        while let Answer::Running { handle } = &answer {
            tokio::time::sleep(poll).await;
            answer = self.status(handle).await?;
        }
        Ok(answer)
    }

    /// Sends a call, again after a pause while it fails for a reason that may pass. A retried
    /// submission says so, as the SQL API asks.
    async fn send(
        &self,
        operation: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Answer, SqlError> {
        let (mut pause, mut tries) = (self.pause, 1);
        let mut path = path.to_owned();
        loop {
            match self.once(operation, method.clone(), &path, body).await {
                Err(error) if error.retryable() && tries < TRIES => {
                    tracing::debug!(operation, %error, "[SNOWFLAKE] trying the call again");
                    tokio::time::sleep(pause).await;
                    pause = (pause * 2).min(LONGEST_PAUSE);
                    tries += 1;
                    if path.contains("requestId=") && !path.contains("retry=true") {
                        path.push_str("&retry=true");
                    }
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
        body: Option<&Value>,
    ) -> Result<Answer, SqlError> {
        let failed = |message: String| SqlError::Request {
            operation: operation.to_owned(),
            message,
        };
        let client = storage::http_client().map_err(failed)?;
        let (kind, token) = match &self.auth {
            Auth::KeyPair(pair) => {
                let now = (SystemTime::now().duration_since(SystemTime::UNIX_EPOCH))
                    .map_or(0, |elapsed| elapsed.as_secs());
                ("KEYPAIR_JWT", pair.token(now).map_err(failed)?)
            }
            Auth::Token { kind, token } => (*kind, token.clone()),
        };
        let mut request = client
            .request(method, format!("{}{path}", self.address))
            .timeout(TIMEOUT)
            .header("authorization", format!("Bearer {token}"))
            .header("x-snowflake-authorization-token-type", kind)
            .header("accept", "application/json")
            .header("user-agent", concat!("uwe/", env!("CARGO_PKG_VERSION")));
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let answer = (request.send().await).map_err(|error| failed(cause(&error)))?;
        let status = answer.status().as_u16();
        let text = answer.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or_default();
        let handle = body["statementHandle"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let message = match body["message"].as_str() {
            Some(message) => message.to_owned(),
            None => text.trim().chars().take(2000).collect(),
        };
        match status {
            200 => Ok(Answer::Done {
                handle,
                data: body["data"].clone(),
            }),
            202 => Ok(Answer::Running { handle }),
            422 => Ok(Answer::Failed { handle, message }),
            _ => Err(SqlError::Status {
                operation: operation.to_owned(),
                status,
                message,
            }),
        }
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
