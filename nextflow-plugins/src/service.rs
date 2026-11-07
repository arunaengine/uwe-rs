//! Sends the requests of the plugins that report to network services from their worker
//! threads, with the shared HTTP client that keeps their credentials on the first origin.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::hash::{BuildHasher, RandomState};
use std::sync::atomic::{AtomicU64, Ordering};

use reqwest::header::HeaderMap;

/// The answer to a request.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) headers: HeaderMap,
    pub(crate) body: String,
}

/// Sends requests from one worker thread: a runtime of its own drives the shared client.
pub(crate) struct Sender {
    runtime: tokio::runtime::Runtime,
    client: &'static reqwest::Client,
}

impl Sender {
    /// A sender for the calling thread, which must not run inside an async runtime.
    pub(crate) fn new() -> Result<Sender, String> {
        let client = storage::origin_client()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Sender { runtime, client })
    }

    pub(crate) fn client(&self) -> &'static reqwest::Client {
        self.client
    }

    /// Sends a request and reads the whole answer.
    pub(crate) fn send(&self, request: reqwest::RequestBuilder) -> Result<Answer, reqwest::Error> {
        self.runtime.block_on(async {
            let response = request.send().await?;
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let body = response.text().await?;
            Ok(Answer {
                status,
                headers,
                body,
            })
        })
    }
}

/// The sender of a worker, made on its first use in the worker thread, so that its runtime
/// is never made or dropped inside the runtime of the caller.
pub(crate) fn sender(slot: &mut Option<Sender>) -> Result<&Sender, String> {
    if slot.is_none() {
        *slot = Some(Sender::new()?);
    }
    slot.as_ref().ok_or_else(|| "no HTTP client".to_owned())
}

/// Random lower-case hex digits, for ids like those of a `traceparent` header and for the
/// spread of retry delays; not for secrets.
pub(crate) fn random_hex(digits: usize) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut text = String::new();
    while text.len() < digits {
        let value = RandomState::new().hash_one(COUNTER.fetch_add(1, Ordering::Relaxed));
        text.push_str(&format!("{value:016x}"));
    }
    text.truncate(digits);
    text
}

/// A random number in `[0, 1)`.
pub(crate) fn random_unit() -> f64 {
    let digits = random_hex(13);
    u64::from_str_radix(&digits, 16).unwrap_or(0) as f64 / (1u64 << 52) as f64
}

/// Text for a form body: letters, digits and `-._*` stay, a space is `+`, the rest `%XX`.
pub(crate) fn form_encode(text: &str) -> String {
    let mut encoded = String::new();
    for byte in text.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// An address as messages show it: without user name, password, query and fragment, which
/// may hold credentials.
pub(crate) fn safe_url(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or_default();
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (format!("{scheme}://"), rest),
        None => (String::new(), url),
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}{host}{path}")
}

/// Whether a setting holds a credential, by a word in its name in any case, as in `apiKey`,
/// `api_key`, `privateKey`, `accessToken` or `password`.
pub(crate) fn secret(name: &str) -> bool {
    let name = name.to_lowercase();
    let words = [
        "key",
        "secret",
        "password",
        "token",
        "credential",
        "private",
    ];
    words.iter().any(|word| name.contains(word))
}

/// A config as JSON with the values of credential settings replaced by `[secret]`.
pub(crate) fn redacted(config: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value as Json;
    match config {
        Json::Object(map) => Json::Object(
            (map.iter())
                .map(|(key, value)| match value {
                    Json::Object(_) => (key.clone(), redacted(value)),
                    _ if secret(key) => (key.clone(), Json::String("[secret]".into())),
                    _ => (key.clone(), redacted(value)),
                })
                .collect(),
        ),
        Json::Array(items) => Json::Array(items.iter().map(redacted).collect()),
        Json::String(text) if leaks(text) => Json::String(safe_url(text)),
        other => other.clone(),
    }
}

/// Whether an address holds a user name or password, or a query value of a credential, as in
/// `jdbc:postgresql://db/app?password=secret`.
fn leaks(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let query = rest.split_once('?').map_or("", |(_, query)| query);
    let named = |pair: &str| secret(pair.split('=').next().unwrap_or_default());
    authority.contains('@') || query.split(['&', ';']).any(named)
}

/// Whether two addresses share scheme, host and port.
pub(crate) fn same_origin(url: &str, other: &str) -> bool {
    match (reqwest::Url::parse(url), reqwest::Url::parse(other)) {
        (Ok(url), Ok(other)) => url.origin() == other.origin(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_ids_differ() {
        let (first, second) = (random_hex(32), random_hex(32));
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|digit| digit.is_ascii_hexdigit()));
        assert_ne!(first, second);
        assert!((0.0..1.0).contains(&random_unit()));
    }

    /// Addresses with credentials lose them in a redacted config; others stay whole.
    #[test]
    fn redacts_addresses() {
        let config = serde_json::json!({
            "sql": {"db": {"default": {"url": "jdbc:postgresql://db/app?password=secret"}}},
            "work": ["s3://user:pw@bucket/run", "https://example.org/a?page=2"],
        });
        let hidden = redacted(&config);
        assert_eq!(
            hidden["sql"]["db"]["default"]["url"],
            "jdbc:postgresql://db/app"
        );
        assert_eq!(hidden["work"][0], "s3://bucket/run");
        assert_eq!(hidden["work"][1], "https://example.org/a?page=2");
    }

    #[test]
    fn hides_credentials() {
        let url = "https://user:secret@push.example.org:9091/a/b?token=abc#part";
        assert_eq!(safe_url(url), "https://push.example.org:9091/a/b");
        assert_eq!(
            safe_url("http://127.0.0.1:9/trace?workspaceId=1"),
            "http://127.0.0.1:9/trace"
        );
        assert_eq!(safe_url("user:pw@host/path"), "host/path");
    }
}
