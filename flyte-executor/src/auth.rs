//! Gets access tokens for Flyte from an API key: the token endpoint and header name from the
//! server's public auth metadata, then the OAuth 2.0 client-credentials grant.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::client::{FlyteError, cause, field};
use crate::settings::ApiKey;

/// The calls that tell a client how to sign in; they need no token.
const CLIENT_CONFIG: &str = "flyteidl2.auth.AuthMetadataService/GetPublicClientConfig";
const OAUTH_METADATA: &str = "flyteidl2.auth.AuthMetadataService/GetOAuth2Metadata";

/// How long before its end a token is replaced.
const MARGIN: Duration = Duration::from_secs(60);

const TIMEOUT: Duration = Duration::from_secs(30);

/// The tokens of one API key, kept until shortly before they end.
pub struct Token {
    address: String,
    key: ApiKey,
    granted: tokio::sync::Mutex<Option<Granted>>,
}

/// A token, and the header that carries it.
struct Granted {
    header: String,
    value: String,
    until: Instant,
}

impl Token {
    pub fn new(address: &str, key: ApiKey) -> Token {
        Token {
            address: address.to_owned(),
            key,
            granted: tokio::sync::Mutex::new(None),
        }
    }

    /// The name and value of the header for a call, with a new token when the last one ends
    /// soon.
    pub async fn header(&self) -> Result<(String, String), FlyteError> {
        let mut granted = self.granted.lock().await;
        let fresh = (granted.as_ref()).is_some_and(|token| token.until > Instant::now() + MARGIN);
        if !fresh {
            *granted = Some(self.grant().await?);
        }
        let token = granted.as_ref().ok_or_else(|| failed("no token".into()))?;
        Ok((token.header.clone(), token.value.clone()))
    }

    /// Drops the token, after Flyte refused it.
    pub async fn forget(&self) {
        *self.granted.lock().await = None;
    }

    async fn grant(&self) -> Result<Granted, FlyteError> {
        let config = self.metadata(CLIENT_CONFIG).await?;
        let header = (field(&config, "authorizationMetadataKey").as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("authorization")
            .to_ascii_lowercase();
        let scopes: Vec<&str> = (field(&config, "scopes").as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .collect();
        let audience = field(&config, "audience").as_str().unwrap_or_default();
        let metadata = self.metadata(OAUTH_METADATA).await?;
        let endpoint = (field(&metadata, "tokenEndpoint").as_str())
            .filter(|endpoint| !endpoint.is_empty())
            .ok_or_else(|| failed("Flyte names no token endpoint in its auth metadata".into()))?;
        let scope = scopes.join(" ");
        let mut pairs = vec![("grant_type", "client_credentials")];
        if !scope.is_empty() {
            pairs.push(("scope", &scope));
        }
        if !audience.is_empty() {
            pairs.push(("audience", audience));
        }
        let client = storage::http_client().map_err(failed)?;
        let answer = client
            .post(endpoint)
            .timeout(TIMEOUT)
            .basic_auth(&self.key.client_id, Some(&self.key.secret))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept", "application/json")
            .body(form(&pairs))
            .send()
            .await
            .map_err(|error| failed(cause(&error)))?;
        let status = answer.status().as_u16();
        let body: Value =
            serde_json::from_str(&answer.text().await.unwrap_or_default()).unwrap_or_default();
        let Some(token) = body["access_token"].as_str().filter(|_| status == 200) else {
            // The error fields of OAuth 2.0 never hold the secret.
            let reason = body["error_description"]
                .as_str()
                .or(body["error"].as_str());
            return Err(FlyteError::Status {
                operation: "the token request".into(),
                status,
                code: "unauthenticated".into(),
                message: reason.unwrap_or("Flyte gave no access token").to_owned(),
            });
        };
        let seconds = body["expires_in"].as_u64().unwrap_or(3600);
        Ok(Granted {
            header,
            value: format!("Bearer {token}"),
            until: Instant::now() + Duration::from_secs(seconds),
        })
    }

    /// One of the auth metadata calls.
    async fn metadata(&self, method: &str) -> Result<Value, FlyteError> {
        let client = storage::http_client().map_err(failed)?;
        let answer = client
            .post(format!("{}/{method}", self.address))
            .timeout(TIMEOUT)
            .header("content-type", "application/json")
            .header("connect-protocol-version", "1")
            .body("{}")
            .send()
            .await
            .map_err(|error| failed(cause(&error)))?;
        let status = answer.status().as_u16();
        let text = answer.text().await.unwrap_or_default();
        if status != 200 {
            return Err(FlyteError::Status {
                operation: method.to_owned(),
                status,
                code: "unknown".into(),
                message: text.chars().take(2000).collect(),
            });
        }
        serde_json::from_str(&text).map_err(|error| failed(error.to_string()))
    }
}

fn failed(message: String) -> FlyteError {
    FlyteError::Request {
        operation: "the token request".into(),
        message,
    }
}

/// A form body, with every byte but letters, digits and `-._~` percent-encoded.
fn form(pairs: &[(&str, &str)]) -> String {
    let encode = |text: &str| {
        let mut encoded = String::new();
        for byte in text.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    encoded.push(char::from(byte))
                }
                _ => encoded.push_str(&format!("%{byte:02X}")),
            }
        }
        encoded
    };
    let pairs = pairs
        .iter()
        .map(|(name, value)| format!("{name}={}", encode(value)));
    pairs.collect::<Vec<_>>().join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_forms() {
        let body = form(&[
            ("grant_type", "client_credentials"),
            ("scope", "all offline"),
        ]);
        assert_eq!(body, "grant_type=client_credentials&scope=all%20offline");
    }
}
