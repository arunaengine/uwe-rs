//! The optional AI section of nf-runinsights: one Anthropic Messages API call on the findings.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use crate::report_format::Json;

/// The longest wait for the answer.
const LIMIT: Duration = Duration::from_secs(120);
/// The length of the data text, after which it is cut.
const DATA_MAX: usize = 12_000;

/// uwe's instruction to the model, with the intent of the plugin's own.
const INSTRUCTION: &str = "You read resource statistics of one run of a Nextflow pipeline, \
compared with earlier runs of the same pipeline. Explain the findings in the data below to the \
people who run the pipeline: what most likely changed and what to check first. Use only the \
numbers in the data and invent none. Write 120 to 200 words of plain prose, without headings.";

/// Where and how to reach the API: the address, and the key or token of the environment.
pub(super) struct Access {
    pub(super) base: String,
    pub(super) key: Option<String>,
    pub(super) token: Option<String>,
}

impl Access {
    /// From `ANTHROPIC_BASE_URL`, `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN`; `None`
    /// without a key or a token.
    pub(super) fn from_environment(variable: &crate::catalogue::Environment) -> Option<Access> {
        let read = |name: &str| variable(name).filter(|value| !value.is_empty());
        let access = Access {
            base: read("ANTHROPIC_BASE_URL").unwrap_or_else(|| "https://api.anthropic.com".into()),
            key: read("ANTHROPIC_API_KEY"),
            token: read("ANTHROPIC_AUTH_TOKEN"),
        };
        (access.key.is_some() || access.token.is_some()).then_some(access)
    }
}

/// The data that the model explains, as compact JSON cut at its limit.
pub(super) fn data(payload: &Json) -> String {
    let text = payload.compact();
    match text.char_indices().nth(DATA_MAX) {
        Some((end, _)) => format!("{}...truncated", &text[..end]),
        None => text,
    }
}

/// Asks the model to explain the data, and gives its text: the text blocks of the answer,
/// joined and trimmed. An empty text, an error status or a failed request is an error.
pub(super) fn narrate(access: &Access, model: &str, data: &str) -> Result<String, String> {
    let body = Json::Object(Vec::new())
        .with("model", model)
        .with("max_tokens", 1500u64)
        .with(
            "messages",
            vec![
                Json::Object(Vec::new())
                    .with("role", "user")
                    .with("content", format!("{INSTRUCTION}\n\nDATA:\n{data}")),
            ],
        );
    let client = storage::origin_client()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let url = format!("{}/v1/messages", access.base.trim_end_matches('/'));
    let answer: serde_json::Value = runtime.block_on(async {
        let mut request = (client.post(url).timeout(LIMIT))
            .header("anthropic-version", "2026-10-21")
            .header("content-type", "application/json")
            .body(body.compact());
        if let Some(key) = &access.key {
            request = request.header("x-api-key", key);
        } else if let Some(token) = &access.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|error| error.to_string())?;
        let status = response.status();
        let text = response.text().await.map_err(|error| error.to_string())?;
        if !status.is_success() {
            return Err(format!("HTTP {}: {}", status.as_u16(), text.trim()));
        }
        serde_json::from_str(&text).map_err(|error| error.to_string())
    })?;
    let blocks = answer["content"].as_array().into_iter().flatten();
    let texts = blocks.filter(|block| block["type"] == "text");
    let text: String = texts.filter_map(|block| block["text"].as_str()).collect();
    match text.trim() {
        "" => Err("the answer has no text".into()),
        text => Ok(text.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    use super::*;

    /// Answers one request with this status and body, and gives back the request it read.
    fn serve(
        status: &'static str,
        body: &'static str,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        let served = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("a request");
            let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
            let mut head = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("reads");
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().expect("a length");
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut content = vec![0; length];
            reader.read_exact(&mut content).expect("reads the body");
            let answer = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            (&stream).write_all(answer.as_bytes()).expect("answers");
            head + &String::from_utf8_lossy(&content)
        });
        (base, served)
    }

    #[test]
    fn asks_messages_api() {
        let answer = r#"{"content":[{"type":"text","text":" CRUNCH slowed down "},{"type":"text","text":"twice."}]}"#;
        let (base, served) = serve("200 OK", answer);
        let access = Access {
            base,
            key: Some("test-key".into()),
            token: None,
        };
        let text = narrate(&access, "claude-opus-5", "{\"pipeline\":\"ri/main.nf\"}");
        assert_eq!(text.as_deref(), Ok("CRUNCH slowed down twice."));
        let request = served.join().expect("served");
        assert!(
            request.starts_with("POST /v1/messages HTTP/1.1"),
            "{request}"
        );
        let lower = request.to_lowercase();
        assert!(lower.contains("x-api-key: test-key"), "{request}");
        assert!(lower.contains("anthropic-version: 2026-10-21"), "{request}");
        let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
        let body: serde_json::Value = serde_json::from_str(body).expect("a JSON body");
        assert_eq!(body["model"], "claude-opus-5");
        assert_eq!(body["max_tokens"], 1500);
        let content = body["messages"][0]["content"].as_str().unwrap_or_default();
        assert!(
            content.ends_with("DATA:\n{\"pipeline\":\"ri/main.nf\"}"),
            "{content}"
        );
    }

    #[test]
    fn reports_failures() {
        let (base, served) = serve("529 Overloaded", r#"{"type":"error"}"#);
        let access = Access {
            base,
            key: None,
            token: Some("token".into()),
        };
        let failed = narrate(&access, "claude-opus-5", "{}");
        assert_eq!(failed, Err("HTTP 529: {\"type\":\"error\"}".into()));
        let request = served.join().expect("served").to_lowercase();
        assert!(request.contains("authorization: bearer token"), "{request}");
        let (base, _served) = serve("200 OK", r#"{"content":[]}"#);
        let empty = Access {
            base,
            key: Some("k".into()),
            token: None,
        };
        assert_eq!(
            narrate(&empty, "m", "{}"),
            Err("the answer has no text".into())
        );
        let long = data(&Json::from("x".repeat(DATA_MAX + 10)));
        assert!(long.ends_with("...truncated") && long.chars().count() == DATA_MAX + 12);
    }
}
