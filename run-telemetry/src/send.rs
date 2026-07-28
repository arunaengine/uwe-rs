//! Sending traces to an OTLP HTTP endpoint, like an OpenTelemetry Collector or a tracing
//! backend that takes OTLP: each export request as `POST <endpoint>/v1/traces` with JSON, one
//! after the other, with the headers of the config file, like a token.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};

use crate::otlp::Request;

/// The path of traces below the base address of an OTLP HTTP endpoint.
const TRACES: &str = "/v1/traces";

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("the header `{0}` of the telemetry settings is not a valid header")]
    Header(String),
    #[error("cannot send the traces to {address}: {message}")]
    Send { address: String, message: String },
    #[error("{address} refused the traces with {status}: {body}")]
    Refused {
        address: String,
        status: u16,
        body: String,
    },
    #[error("cannot write the traces as JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot write the traces: {0}")]
    Write(#[from] std::io::Error),
}

/// What an endpoint took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sent {
    pub requests: usize,
    pub spans: usize,
    /// The spans that the endpoint said it rejected, in a partial success.
    pub rejected: u64,
}

/// The address that traces go to: the base address of the endpoint with `/v1/traces`, or the
/// address as it is when it ends with that path already.
pub fn traces_address(endpoint: &str) -> String {
    let endpoint = endpoint.trim_end_matches('/');
    if endpoint.ends_with(TRACES) {
        endpoint.to_owned()
    } else {
        format!("{endpoint}{TRACES}")
    }
}

/// Sends the requests to an OTLP HTTP endpoint with these headers, whose values may be
/// secrets: they are marked as sensitive, so that no log shows them. It stops at the first
/// request that the endpoint does not take.
pub async fn send(
    client: &reqwest::Client,
    endpoint: &str,
    headers: &[(String, String)],
    requests: &[Request],
) -> Result<Sent, TelemetryError> {
    let address = traces_address(endpoint);
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let header = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| TelemetryError::Header(name.clone()))?;
        let mut value =
            HeaderValue::from_str(value).map_err(|_| TelemetryError::Header(name.clone()))?;
        value.set_sensitive(true);
        map.insert(header, value);
    }
    map.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let failed = |error: reqwest::Error| TelemetryError::Send {
        address: address.clone(),
        message: error.to_string(),
    };
    let mut sent = Sent::default();
    for request in requests {
        let body = serde_json::to_vec(request)?;
        let response = client
            .post(&address)
            .headers(map.clone())
            .body(body)
            .send()
            .await
            .map_err(failed)?;
        let status = response.status();
        let text = response.text().await.map_err(failed)?;
        if !status.is_success() {
            return Err(TelemetryError::Refused {
                address,
                status: status.as_u16(),
                body: text.chars().take(300).collect(),
            });
        }
        sent.requests += 1;
        sent.spans += request.spans().count();
        sent.rejected += rejected(&text);
    }
    Ok(sent)
}

/// The spans that the answer of an endpoint says it rejected: `partialSuccess.rejectedSpans`,
/// a number or its decimal text; none in an empty answer.
fn rejected(answer: &str) -> u64 {
    let Ok(answer) = serde_json::from_str::<serde_json::Value>(answer) else {
        return 0;
    };
    match &answer["partialSuccess"]["rejectedSpans"] {
        serde_json::Value::Number(number) => number.as_u64().unwrap_or(0),
        serde_json::Value::String(text) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traces_go_below_the_base_address() {
        assert_eq!(
            traces_address("http://localhost:4318"),
            "http://localhost:4318/v1/traces"
        );
        assert_eq!(traces_address("http://c:4318/"), "http://c:4318/v1/traces");
        assert_eq!(
            traces_address("https://c/otlp/v1/traces"),
            "https://c/otlp/v1/traces"
        );
    }

    #[test]
    fn partial_successes_name_rejected_spans() {
        assert_eq!(rejected(""), 0);
        assert_eq!(rejected("{}"), 0);
        assert_eq!(rejected(r#"{"partialSuccess":{"rejectedSpans":"3"}}"#), 3);
        assert_eq!(rejected(r#"{"partialSuccess":{"rejectedSpans":2}}"#), 2);
    }
}
