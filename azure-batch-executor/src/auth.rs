//! Authorizes Azure Batch requests: signs them with the Shared Key of the account, or gets Entra
//! ID tokens with a client secret or from the managed identity of the machine.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use reqwest::Url;
use serde_json::Value;
use sha2::Sha256;

use crate::settings::{Identity, Settings};

/// The scope of Entra ID tokens for Azure Batch.
pub const SCOPE: &str = "https://batch.core.windows.net/.default";

/// The resource of Azure Batch, as the instance metadata service names it.
const RESOURCE: &str = "https://batch.core.windows.net/";

/// How long before its end a token is no longer used.
const MARGIN: Duration = Duration::from_secs(300);

/// The headers of a request, in the order of the string to sign, before its `ocp-` headers.
const SIGNED: [&str; 11] = [
    "content-encoding",
    "content-language",
    "content-length",
    "content-md5",
    "content-type",
    "date",
    "if-modified-since",
    "if-match",
    "if-none-match",
    "if-unmodified-since",
    "range",
];

/// How the requests of a client are authorized.
pub enum Credentials {
    /// The account name and its decoded Shared Key.
    SharedKey { account: String, key: Vec<u8> },
    /// Entra ID tokens, kept until shortly before they end.
    Token {
        source: Source,
        cached: tokio::sync::Mutex<Option<(String, Instant)>>,
    },
}

/// Where Entra ID tokens come from.
#[derive(Clone)]
pub enum Source {
    /// A service principal with a client secret, at the token address of its tenant.
    Secret {
        url: String,
        client_id: String,
        secret: String,
    },
    /// The instance metadata service, for the system identity or a user identity.
    Identity {
        url: String,
        client_id: Option<String>,
    },
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Credentials::SharedKey { account, .. } => format!("Shared Key of {account}"),
            Credentials::Token {
                source: Source::Secret { client_id, .. },
                ..
            } => format!("service principal {client_id}"),
            Credentials::Token { .. } => "managed identity".to_owned(),
        };
        formatter.write_str(&kind)
    }
}

impl Credentials {
    /// The credentials of the settings and the environment, first match wins: a managed
    /// identity, a service principal, then the Shared Key of the account.
    pub fn find(
        settings: &Settings,
        account: &str,
        variable: impl Fn(&str) -> Option<String>,
    ) -> Result<Credentials, String> {
        let set = |name: &str| variable(name).filter(|value| !value.is_empty());
        let identity = (settings.managed_identity.clone())
            .or_else(|| set("AZURE_MANAGED_IDENTITY_USER").map(Identity::User))
            .or_else(|| {
                let system = set("AZURE_MANAGED_IDENTITY_SYSTEM");
                (system.as_deref() == Some("true")).then_some(Identity::System)
            });
        if let Some(identity) = identity {
            let client_id = match identity {
                Identity::System => None,
                Identity::User(id) => Some(id),
            };
            let url = settings.identity_endpoint.clone();
            return Ok(Credentials::tokens(Source::Identity { url, client_id }));
        }
        let tenant = settings
            .tenant_id
            .clone()
            .or_else(|| set("AZURE_TENANT_ID"));
        let client = settings
            .client_id
            .clone()
            .or_else(|| set("AZURE_CLIENT_ID"));
        let secret = match &settings.client_secret {
            Some(secret) => Some(secret.reveal()?),
            None => set("AZURE_CLIENT_SECRET"),
        };
        if let (Some(tenant), Some(client_id), Some(secret)) = (tenant, client, secret) {
            let url = format!("{}/{tenant}/oauth2/v2.0/token", settings.authority_endpoint);
            return Ok(Credentials::tokens(Source::Secret {
                url,
                client_id,
                secret,
            }));
        }
        let key = match &settings.account_key {
            Some(key) => Some(key.reveal()?),
            None => set("AZURE_BATCH_ACCOUNT_KEY"),
        };
        let key = key.ok_or(
            "Azure Batch needs credentials: set account_key, a service principal (tenant_id, \
             client_id, client_secret) or managed_identity in [executor.azure-batch], or \
             azure.batch.accountKey in the Nextflow config",
        )?;
        let key = (STANDARD.decode(key.trim()))
            .map_err(|_| "the Azure Batch account key is not Base64".to_owned())?;
        Ok(Credentials::SharedKey {
            account: account.to_owned(),
            key,
        })
    }

    pub fn tokens(source: Source) -> Credentials {
        Credentials::Token {
            source,
            cached: tokio::sync::Mutex::new(None),
        }
    }

    /// The `authorization` header of a request with the `headers` that it sends, which hold
    /// `ocp-date` for Shared Key.
    pub async fn authorize(
        &self,
        method: &str,
        url: &Url,
        headers: &[(&str, String)],
    ) -> Result<String, String> {
        match self {
            Credentials::SharedKey { account, key } => {
                let text = string_to_sign(method, url, headers, account);
                Ok(format!("SharedKey {account}:{}", signature(key, &text)))
            }
            Credentials::Token { source, cached } => {
                let mut cached = cached.lock().await;
                if let Some((token, until)) = cached.as_ref()
                    && Instant::now() + MARGIN < *until
                {
                    return Ok(format!("Bearer {token}"));
                }
                let (token, lifetime) = source.fetch().await?;
                *cached = Some((token.clone(), Instant::now() + lifetime));
                Ok(format!("Bearer {token}"))
            }
        }
    }
}

impl Source {
    /// A new token and how long it lasts.
    async fn fetch(&self) -> Result<(String, Duration), String> {
        let client = storage::http_client()?;
        let request = match self {
            Source::Secret {
                url,
                client_id,
                secret,
            } => {
                let pairs = [
                    ("grant_type", "client_credentials"),
                    ("client_id", client_id),
                    ("client_secret", secret),
                    ("scope", SCOPE),
                ];
                (client.post(url))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(form(&pairs))
            }
            Source::Identity { url, client_id } => {
                let mut pairs = vec![("api-version", "2018-02-01"), ("resource", RESOURCE)];
                if let Some(id) = client_id {
                    pairs.push(("client_id", id));
                }
                let url = format!("{url}?{}", form(&pairs));
                client.get(url).header("metadata", "true")
            }
        };
        // The error of reqwest names the address, which holds no secret here.
        let answer = (request.timeout(Duration::from_secs(60)).send().await)
            .map_err(|error| format!("cannot get an Entra ID token: {error}"))?;
        let status = answer.status();
        let body: Value =
            serde_json::from_str(&answer.text().await.unwrap_or_default()).unwrap_or(Value::Null);
        let token = body["access_token"]
            .as_str()
            .filter(|_| status.is_success());
        let Some(token) = token else {
            let reason = (body["error_description"].as_str())
                .or(body["error"].as_str())
                .unwrap_or_default();
            return Err(format!(
                "Entra ID refused a token for Azure Batch ({status}): {reason}"
            ));
        };
        // The instance metadata service gives the lifetime as text.
        let seconds = (body["expires_in"].as_u64())
            .or_else(|| body["expires_in"].as_str()?.parse().ok())
            .unwrap_or(3600);
        Ok((token.to_owned(), Duration::from_secs(seconds)))
    }
}

/// The string to sign of a Batch request, from Microsoft's Shared Key rules for Batch: the verb,
/// the standard headers, the sorted `ocp-` headers, then the account, path and sorted query.
pub fn string_to_sign(
    method: &str,
    url: &Url,
    headers: &[(&str, String)],
    account: &str,
) -> String {
    let header = |name: &str| {
        (headers.iter())
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map_or("", |(_, value)| value.as_str())
    };
    let mut text = format!("{method}\n");
    for name in SIGNED {
        text.push_str(header(name));
        text.push('\n');
    }
    let mut ocp: Vec<(String, &str)> = (headers.iter())
        .filter(|(key, _)| key.to_ascii_lowercase().starts_with("ocp-"))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim()))
        .collect();
    ocp.sort();
    for (name, value) in ocp {
        text.push_str(&format!("{name}:{value}\n"));
    }
    text.push_str(&format!("/{account}{}", url.path()));
    let mut query: Vec<(String, String)> = (url.query_pairs())
        .map(|(name, value)| (name.to_lowercase(), value.into_owned()))
        .collect();
    query.sort();
    let mut joined: Vec<(String, Vec<String>)> = Vec::new();
    for (name, value) in query {
        match joined.last_mut() {
            Some((last, values)) if *last == name => values.push(value),
            _ => joined.push((name, vec![value])),
        }
    }
    for (name, values) in joined {
        text.push_str(&format!("\n{name}:{}", values.join(",")));
    }
    text
}

/// The Base64 HMAC-SHA256 of `text` with the account key.
pub fn signature(key: &[u8], text: &str) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key)
        .unwrap_or_else(|_| unreachable!("HMAC takes keys of every length"));
    mac.update(text.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

/// A form body, as `application/x-www-form-urlencoded` writes it.
fn form(pairs: &[(&str, &str)]) -> String {
    let mut url = Url::parse("form:").unwrap_or_else(|_| unreachable!("a valid address"));
    url.query_pairs_mut().extend_pairs(pairs);
    url.query().unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request as Microsoft's Batch Shared Key rules sign it, with a made-up key. The
    /// signature was computed separately with Python's `hmac` over the same text.
    #[test]
    fn signs_known_request() {
        let url = Url::parse(
            "https://lab.westeurope.batch.azure.com/jobs/job-1/tasks?api-version=2024-07-01.20.0&timeout=30&$select=id,state",
        )
        .expect("a URL");
        let headers = [
            (
                "Content-Type",
                "application/json; odata=minimalmetadata".to_owned(),
            ),
            ("Content-Length", "42".to_owned()),
            ("ocp-date", "Fri, 06 Nov 2026 10:00:00 GMT".to_owned()),
            ("OCP-Client-Request-Id", " 1234 ".to_owned()),
        ];
        let text = string_to_sign("POST", &url, &headers, "lab");
        let expected = "POST\n\n\n42\n\napplication/json; odata=minimalmetadata\n\n\n\n\n\n\n\
                        ocp-client-request-id:1234\nocp-date:Fri, 06 Nov 2026 10:00:00 GMT\n\
                        /lab/jobs/job-1/tasks\n$select:id,state\napi-version:2024-07-01.20.0\n\
                        timeout:30";
        assert_eq!(text, expected);
        let key = STANDARD
            .decode("dXdlLXRlc3Qta2V5LW5vdC1yZWFs")
            .expect("Base64");
        assert_eq!(
            signature(&key, &text),
            "FCrctebjWVzRIh2qekKHZGfDUQmKmxIbDJUav11TjtI="
        );
        let get = Url::parse("https://lab.westeurope.batch.azure.com/pools/p%201?api-version=1")
            .expect("a URL");
        let date = [("ocp-date", "Fri, 06 Nov 2026 10:00:00 GMT".to_owned())];
        assert_eq!(
            string_to_sign("GET", &get, &date, "lab"),
            "GET\n\n\n\n\n\n\n\n\n\n\n\nocp-date:Fri, 06 Nov 2026 10:00:00 GMT\n\
             /lab/pools/p%201\napi-version:1"
        );
    }

    #[test]
    fn finds_credentials() {
        let settings = Settings::default();
        let variables = |name: &str| match name {
            "AZURE_TENANT_ID" => Some("t".into()),
            "AZURE_CLIENT_ID" => Some("c".into()),
            "AZURE_CLIENT_SECRET" => Some("s".into()),
            "AZURE_BATCH_ACCOUNT_KEY" => Some("a2V5".into()),
            _ => None,
        };
        let found = Credentials::find(&settings, "lab", variables).expect("credentials");
        let Credentials::Token {
            source: Source::Secret { url, .. },
            ..
        } = &found
        else {
            panic!("a service principal: {found:?}");
        };
        assert_eq!(url, "https://login.microsoftonline.com/t/oauth2/v2.0/token");
        let key = |name: &str| (name == "AZURE_BATCH_ACCOUNT_KEY").then(|| "a2V5".into());
        let found = Credentials::find(&settings, "lab", key).expect("credentials");
        assert!(matches!(&found, Credentials::SharedKey { key, .. } if key == b"key"));
        let system = |name: &str| (name == "AZURE_MANAGED_IDENTITY_SYSTEM").then(|| "true".into());
        let found = Credentials::find(&settings, "lab", system).expect("credentials");
        assert_eq!(format!("{found:?}"), "managed identity");
        let error = Credentials::find(&settings, "lab", |_| None).expect_err("none");
        assert!(error.contains("account_key"), "{error}");
    }
}
