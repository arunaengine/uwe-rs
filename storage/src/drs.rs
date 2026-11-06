//! Reads GA4GH DRS objects of `drs://<host>/<object id>` addresses, with Gen3 or bearer tokens.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use crate::error::Problem;
use crate::web::{Web, network, shared_client};
use crate::{Downloaded, Metadata};

/// What a DRS client signs in with. Unset values come from the environment.
#[derive(Clone, Default)]
pub struct Options {
    /// `gen3`, `bearer` or `none`; by default `gen3` with a credential, else `none`.
    pub authentication: Option<String>,
    /// The Gen3 API key or the bearer token; else the value of the variable `variable`.
    pub credential: Option<String>,
    /// The environment variable that holds the credential, `GEN3_API_KEY` by default.
    pub variable: Option<String>,
    /// For `gen3`: the path of the token exchange on the DRS server.
    pub token_path: Option<String>,
    /// The `type` of access method to prefer, like `s3` or `https`.
    pub protocol: Option<String>,
    /// Servers, as `host:port`, that serve plain HTTP instead of HTTPS, like a local test.
    pub plain_http: Vec<String>,
}

/// The options of the `drs` scope of a Nextflow config, for `drs://` addresses that no
/// provider of the config file serves.
static SCOPE: Mutex<Option<Options>> = Mutex::new(None);

/// Uses these options for `drs://` addresses that no provider of the config file serves, from
/// the first request on.
pub fn use_scope(options: Options) {
    *SCOPE.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(options);
}

/// The options of the built-in provider.
pub(crate) fn scope() -> Options {
    let scope = SCOPE.lock().unwrap_or_else(|poison| poison.into_inner());
    scope.clone().unwrap_or_default()
}

const TOKEN_PATH: &str = "/user/credentials/cdis/access_token";
const VARIABLE: &str = "GEN3_API_KEY";
const OBJECTS: &str = "/ga4gh/drs/v1/objects/";
/// How long a REST request may take, and how long a Gen3 token without a readable expiry lives.
const REQUEST: Duration = Duration::from_secs(60);
const UNKNOWN_EXPIRY: Duration = Duration::from_secs(20 * 60);

pub(crate) struct Drs {
    login: Login,
    credential: Option<String>,
    variable: String,
    token_path: String,
    protocol: Option<String>,
    plain_http: Vec<String>,
    client: reqwest::Client,
    /// The Gen3 access token of each host, and when to renew it. One exchange at a time.
    tokens: tokio::sync::Mutex<HashMap<String, Token>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Login {
    Gen3,
    Bearer,
    Anonymous,
}

struct Token {
    value: String,
    renew: SystemTime,
}

impl Drs {
    pub fn new(options: Options) -> Result<Drs, Problem> {
        let variable = options.variable.unwrap_or_else(|| VARIABLE.to_owned());
        let credential = (options.credential)
            .or_else(|| std::env::var(&variable).ok())
            .filter(|credential| !credential.is_empty());
        let login = match options.authentication.as_deref().map(str::to_lowercase) {
            None if credential.is_some() => Login::Gen3,
            None => Login::Anonymous,
            Some(name) => match name.as_str() {
                "gen3" => Login::Gen3,
                "bearer" => Login::Bearer,
                "none" => Login::Anonymous,
                _ => {
                    return Err(Problem::fixed(format!(
                        "Unknown DRS authentication provider '{name}'. Expected one of: gen3, bearer, \
                         none."
                    )));
                }
            },
        };
        let what = match login {
            Login::Gen3 => "a Gen3 API key (the \"api_key\" value of the Gen3 credentials.json)",
            Login::Bearer => "a bearer token (a valid OAuth2 access token)",
            Login::Anonymous => "",
        };
        if login != Login::Anonymous && credential.is_none() {
            return Err(Problem::fixed(format!(
                "DRS needs {what}: set `credential` of the storage provider, or the \
                 environment variable {variable}"
            )));
        }
        Ok(Drs {
            login,
            credential,
            variable,
            token_path: options.token_path.unwrap_or_else(|| TOKEN_PATH.to_owned()),
            protocol: options.protocol,
            plain_http: options.plain_http,
            client: shared_client()?,
            tokens: tokio::sync::Mutex::default(),
        })
    }

    /// The secrets that messages must not show.
    pub fn secrets(&self) -> Vec<String> {
        self.credential.iter().cloned().collect()
    }

    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, Problem> {
        let (host, id) = parts(target)?;
        let object = self.request(host, &format!("{OBJECTS}{id}")).await?;
        Ok(object.as_ref().map(metadata))
    }

    /// Downloads an object through the access method that the options prefer.
    pub async fn download(&self, target: &str, file: &Path) -> Result<Downloaded, Problem> {
        let (host, id) = parts(target)?;
        let object =
            (self.request(host, &format!("{OBJECTS}{id}")).await?).ok_or_else(Problem::missing)?;
        let method = choose(&object, self.protocol.as_deref())
            .ok_or_else(|| Problem::fixed(format!("DRS object {id} exposes no access methods")))?;
        let inline = method["access_url"]["url"].as_str();
        let (url, headers) = match (inline, method["access_id"].as_str()) {
            (Some(url), _) if url.starts_with("https://") || url.starts_with("http://") => {
                (url.to_owned(), method["access_url"]["headers"].clone())
            }
            (_, Some(access)) => {
                let path = format!("{OBJECTS}{id}/access/{access}");
                let answer = (self.request(host, &path).await?).ok_or_else(Problem::missing)?;
                let url = answer["url"].as_str().ok_or_else(|| {
                    Problem::fixed(format!("DRS access response missing 'url': {answer}"))
                })?;
                (url.to_owned(), answer["headers"].clone())
            }
            (Some(url), None) => (url.to_owned(), Value::Null),
            (None, None) => {
                return Err(Problem::fixed(format!(
                    "DRS access method for {id} has neither access_url nor access_id"
                )));
            }
        };
        if !url.starts_with("https://") && !url.starts_with("http://") {
            let scheme = url.split_once("://").map_or("", |(scheme, _)| scheme);
            return Err(Problem::fixed(format!(
                "the access method of DRS object {id} gives a {scheme} address, and uwe \
                 downloads DRS objects over HTTP only; set the preferred access protocol"
            )));
        }
        // The DRS specification asks clients to send the headers of an access URL with it.
        let web = match header_map(&headers)? {
            headers if headers.is_empty() => Web::public()?,
            headers => Web::with_headers(headers)?,
        };
        web.download(&url, file, 0).await?;
        Ok(Downloaded {
            version: metadata(&object).version,
        })
    }

    /// The JSON answer of a DRS request, `None` for 404. A 401 to a request with a token is
    /// sent once more, for Gen3 with a new token.
    async fn request(&self, host: &str, path: &str) -> Result<Option<Value>, Problem> {
        let url = format!("{}{path}", self.base(host));
        let mut fresh = false;
        loop {
            let token = self.token(host, fresh).await?;
            let mut request = (self.client.get(&url))
                .header(ACCEPT, "application/json")
                .timeout(REQUEST);
            if let Some(token) = &token {
                request = request.bearer_auth(token);
            }
            tracing::debug!("GET {url}");
            let response = request.send().await.map_err(network)?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED && token.is_some() && !fresh {
                tracing::debug!("DRS answered 401 at {url}; trying once more");
                fresh = true;
                continue;
            }
            let body = response.text().await.map_err(network)?;
            return match status.as_u16() {
                200 | 201 => serde_json::from_str(&body).map(Some).map_err(|error| {
                    Problem::fixed(format!("the DRS answer at {url} is not JSON: {error}"))
                }),
                404 => Ok(None),
                401 => Err(Problem::fixed(format!(
                    "DRS authentication failed (HTTP 401) at {url}. Your credential may be \
                     invalid or expired."
                ))),
                403 => Err(Problem::fixed(
                    "Access denied. You don't have permission to access this DRS object.",
                )),
                code => Err(passing_on(
                    status,
                    format!("DRS API error (HTTP {code}): {body}"),
                )),
            };
        }
    }

    /// The token for the requests to a host: none, the bearer token, or a Gen3 access token,
    /// exchanged anew when `fresh` or when it is about to end.
    async fn token(&self, host: &str, fresh: bool) -> Result<Option<String>, Problem> {
        let credential = self.credential.clone().unwrap_or_default();
        match self.login {
            Login::Anonymous => return Ok(None),
            Login::Bearer => return Ok(Some(credential)),
            Login::Gen3 => {}
        }
        let mut tokens = self.tokens.lock().await;
        let now = SystemTime::now();
        if let Some(token) = tokens.get(host).filter(|token| !fresh && now < token.renew) {
            return Ok(Some(token.value.clone()));
        }
        let url = format!("{}{}", self.base(host), self.token_path);
        tracing::debug!("POST (Gen3 token exchange) {url}");
        let response = (self.client.post(&url))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::json!({ "api_key": credential }).to_string())
            .timeout(REQUEST)
            .send()
            .await
            .map_err(network)?;
        let status = response.status();
        let body = response.text().await.map_err(network)?;
        match status.as_u16() {
            200 => {}
            code @ (401 | 403) => {
                return Err(Problem::fixed(format!(
                    "Gen3 token exchange failed (HTTP {code}) at {url}. Your API key may be \
                     invalid or expired (the environment variable {}).",
                    self.variable
                )));
            }
            code => {
                let message = format!("Gen3 token exchange error (HTTP {code}): {body}");
                return Err(passing_on(status, message));
            }
        }
        let answer: Value = serde_json::from_str(&body).unwrap_or_default();
        let Some(value) = answer["access_token"].as_str() else {
            return Err(Problem::fixed(format!(
                "Gen3 token exchange response missing access_token: {body}"
            )));
        };
        let renew = expiry(value, now)
            .checked_sub(Duration::from_secs(60))
            .unwrap_or(now);
        let value = value.to_owned();
        let token = Token {
            value: value.clone(),
            renew,
        };
        tokens.insert(host.to_owned(), token);
        Ok(Some(value))
    }

    fn base(&self, host: &str) -> String {
        match self.plain_http.iter().any(|plain| plain == host) {
            true => format!("http://{host}"),
            false => format!("https://{host}"),
        }
    }
}

/// The host and the object id of `drs://<host>/<object id>`; the id may hold slashes.
fn parts(target: &str) -> Result<(&str, &str), Problem> {
    target
        .strip_prefix("drs://")
        .and_then(|rest| rest.split_once('/'))
        .filter(|(host, id)| !host.is_empty() && !id.is_empty())
        .ok_or_else(|| {
            Problem::fixed(format!(
                "Invalid DRS URI: {target}. Expected format: drs://<host>/<object_id>"
            ))
        })
}

/// A DRS object is always a file. Its version is its first checksum, which binds the bytes
/// only together with a size.
fn metadata(object: &Value) -> Metadata {
    let size = object["size"].as_u64();
    let checksum = &object["checksums"][0];
    let version = match (checksum["type"].as_str(), checksum["checksum"].as_str()) {
        (Some(kind), Some(sum)) if size.is_some() => Some(format!("{kind}:{sum}")),
        _ => None,
    };
    Metadata {
        directory: false,
        modified: (object["updated_time"].as_str())
            .and_then(|time| time.parse::<jiff::Timestamp>().ok())
            .map(SystemTime::from),
        size: size.unwrap_or(0),
        version,
    }
}

/// The access method of the preferred type, else the first of type `s3`, then `gs`, else the
/// first one.
fn choose<'a>(object: &'a Value, protocol: Option<&str>) -> Option<&'a Value> {
    let methods = object["access_methods"].as_array()?;
    let of_type = |kind: &str| methods.iter().find(|method| method["type"] == kind);
    (protocol.and_then(of_type))
        .or_else(|| of_type("s3"))
        .or_else(|| of_type("gs"))
        .or_else(|| methods.first())
}

/// When a Gen3 access token ends: the `exp` claim of the JWT, without checking its signature,
/// else 20 minutes from `now`.
fn expiry(token: &str, now: SystemTime) -> SystemTime {
    let claims = (token.split('.').nth(1))
        .and_then(|payload| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload.trim_end_matches('='))
                .ok()
        })
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    match claims.and_then(|claims| claims["exp"].as_u64()) {
        Some(seconds) => UNIX_EPOCH + Duration::from_secs(seconds),
        None => now + UNKNOWN_EXPIRY,
    }
}

/// The headers of an access URL, `"Name: value"` texts, marked as sensitive.
fn header_map(headers: &Value) -> Result<HeaderMap, Problem> {
    let mut map = HeaderMap::new();
    let lines = headers.as_array().map(Vec::as_slice).unwrap_or_default();
    for (name, value) in lines
        .iter()
        .filter_map(|line| line.as_str()?.split_once(':'))
    {
        let invalid = || Problem::fixed("the DRS access URL has a header that is not valid");
        let name = HeaderName::from_bytes(name.trim().as_bytes()).map_err(|_| invalid())?;
        let mut value = HeaderValue::from_str(value.trim()).map_err(|_| invalid())?;
        value.set_sensitive(true);
        map.insert(name, value);
    }
    Ok(map)
}

/// A refused request: errors of the server may go away.
fn passing_on(status: StatusCode, message: String) -> Problem {
    match status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        true => Problem::passing(message),
        false => Problem::fixed(message),
    }
}
