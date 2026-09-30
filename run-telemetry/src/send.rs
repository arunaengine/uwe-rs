//! Posts trace export requests to configured OTLP HTTP endpoints with their headers.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};

use crate::otlp::Request;

/// The path of traces below the base address of an OTLP HTTP endpoint.
const TRACES: &str = "/v1/traces";
const SEND_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
const RESPONSE_LIMIT: usize = 64 << 10;

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
    send_with_limit(client, endpoint, headers, requests, SEND_LIMIT).await
}

async fn send_with_limit(
    client: &reqwest::Client,
    endpoint: &str,
    headers: &[(String, String)],
    requests: &[Request],
    limit: std::time::Duration,
) -> Result<Sent, TelemetryError> {
    tokio::time::timeout(limit, send_requests(client, endpoint, headers, requests))
        .await
        .map_err(|_| TelemetryError::Send {
            address: traces_address(endpoint),
            message: format!(
                "the total send deadline of {} seconds elapsed",
                limit.as_secs_f64()
            ),
        })?
}

async fn send_requests(
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
        tracing::debug!(spans = request.spans().count(), "sending telemetry request");
        let body = serde_json::to_vec(request)?;
        let mut response = client
            .post(&address)
            .headers(map.clone())
            .body(body)
            .send()
            .await
            .map_err(failed)?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(failed)? {
            if bytes.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
                if !status.is_success() {
                    let needed = 1200usize.saturating_sub(bytes.len()).min(chunk.len());
                    bytes.extend_from_slice(&chunk[..needed]);
                    break;
                }
                return Err(TelemetryError::Send {
                    address,
                    message: format!("the response exceeds {RESPONSE_LIMIT} bytes"),
                });
            }
            bytes.extend_from_slice(&chunk);
            if !status.is_success() && bytes.len() >= 1200 {
                break;
            }
        }
        let text = String::from_utf8_lossy(&bytes);
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
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    use super::*;

    fn collector(trickle: bool, status: &str, advertised: usize, initial: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let status = status.to_owned();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            let framing = if trickle {
                "Transfer-Encoding: chunked".to_owned()
            } else {
                format!("Content-Length: {advertised}")
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\n{framing}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            if trickle {
                for _ in 0..200 {
                    if stream.write_all(b"1\r\nx\r\n").is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            } else {
                let _ = stream.write_all(&vec![b'x'; initial]);
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        endpoint
    }

    #[tokio::test]
    async fn enforces_total_deadline() {
        let endpoint = collector(true, "200 OK", 0, 0);
        let requests = [Request {
            resource_spans: Vec::new(),
        }];
        let started = std::time::Instant::now();
        let error = send_with_limit(
            &reqwest::Client::new(),
            &endpoint,
            &[],
            &requests,
            Duration::from_millis(50),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, TelemetryError::Send { message, .. } if message.contains("total send deadline"))
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn bounds_collector_responses() {
        let requests = [Request {
            resource_spans: Vec::new(),
        }];
        for (status, initial) in [("500 Failed", 2048), ("200 OK", 128 << 10)] {
            let endpoint = collector(false, status, 100_000_000, initial);
            let result = tokio::time::timeout(
                Duration::from_millis(500),
                send(&reqwest::Client::new(), &endpoint, &[], &requests),
            )
            .await
            .expect("bounded reply");
            if status.starts_with("500") {
                assert!(
                    matches!(result, Err(TelemetryError::Refused { body, status: 500, .. }) if body.len() == 300)
                );
            } else {
                assert!(
                    matches!(result, Err(TelemetryError::Send { message, .. }) if message.contains("exceeds"))
                );
            }
        }
    }

    #[test]
    fn appends_traces_path() {
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
    fn reports_rejected_spans() {
        assert_eq!(rejected(""), 0);
        assert_eq!(rejected("{}"), 0);
        assert_eq!(rejected(r#"{"partialSuccess":{"rejectedSpans":"3"}}"#), 3);
        assert_eq!(rejected(r#"{"partialSuccess":{"rejectedSpans":2}}"#), 2);
    }
}
