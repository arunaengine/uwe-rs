//! Signs plain HTTPS requests to AWS services with Signature Version 4 and sends them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use bytes::Bytes;
use hmac::{Hmac, KeyInit, Mac};
use reqsign_aws_core::DefaultCredentialProvider;
use reqsign_core::{Context, HttpSend, ProvideCredential, StaticEnv};
use reqsign_file_read_tokio::TokioFileRead;
use sha2::{Digest, Sha256};

use crate::StorageError;
use crate::blocking::{Answer, exchange, run};

/// The environment variables that AWS's default credential chain reads.
pub const VARIABLES: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "AWS_CONFIG_FILE",
    "AWS_SHARED_CREDENTIALS_FILE",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "AWS_ROLE_ARN",
    "AWS_ROLE_SESSION_NAME",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
    "AWS_EC2_METADATA_DISABLED",
    "AWS_STS_REGIONAL_ENDPOINTS",
];

/// Access keys, as a config or a connection URL gives them.
#[derive(Clone, PartialEq, Eq)]
pub struct Keys {
    pub id: String,
    pub secret: String,
    pub token: Option<String>,
}

impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keys")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Where the credentials of a request come from.
#[derive(Clone, Debug)]
pub enum Credentials {
    Keys(Keys),
    /// AWS's default chain over this environment and home folder: keys of the environment,
    /// profile files, web identity, container and instance roles.
    Chain {
        environment: HashMap<String, String>,
        home: Option<PathBuf>,
    },
}

/// One request to an AWS service.
#[derive(Clone, Debug)]
pub struct Request {
    /// The signing name of the service, like `athena` or `ses`.
    pub service: String,
    pub region: String,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Signs the request with the hash of its body and sends it with the HTTP client of the
/// process; there are no retries.
pub fn send(request: Request, credentials: &Credentials) -> Result<Answer, StorageError> {
    let (method, url, body) = (
        request.method.clone(),
        request.url.clone(),
        request.body.clone(),
    );
    let headers = run(signed(request, credentials.clone(), None))?;
    exchange(&method, &url, &headers, Some(body))
}

/// The endpoint of a service: `AWS_ENDPOINT_URL_<SERVICE>`, then `AWS_ENDPOINT_URL`, as AWS's
/// tools read them, else the default; `service` is the upper-case id, like `ATHENA`.
pub fn endpoint(
    service: &str,
    default: String,
    variable: impl Fn(&str) -> Option<String>,
) -> String {
    let set = |name: &str| variable(name).filter(|value| !value.is_empty());
    let own = format!("AWS_ENDPOINT_URL_{service}");
    set(&own)
        .or_else(|| set("AWS_ENDPOINT_URL"))
        .unwrap_or(default)
        .trim_end_matches('/')
        .to_owned()
}

/// The headers of the request after signing at `time`, else now, with its credentials.
async fn signed(
    request: Request,
    credentials: Credentials,
    time: Option<jiff::Timestamp>,
) -> Result<Vec<(String, String)>, StorageError> {
    let failed = |message: String| StorageError::Failed {
        action: "sign a request to",
        address: request.url.clone(),
        provider: "aws".to_owned(),
        message,
        retryable: false,
    };
    let keys = keys_of(credentials).await.map_err(failed)?;
    let time = time.unwrap_or_else(jiff::Timestamp::now);
    sign(&request, &keys, time).map_err(failed)
}

/// The keys of the credentials, from AWS's default chain when they name none.
pub fn resolve(credentials: &Credentials) -> Result<Keys, StorageError> {
    let credentials = credentials.clone();
    run(async move {
        keys_of(credentials)
            .await
            .map_err(|message| StorageError::Failed {
                action: "find",
                address: "AWS credentials".to_owned(),
                provider: "aws".to_owned(),
                message,
                retryable: false,
            })
    })
}

async fn keys_of(credentials: Credentials) -> Result<Keys, String> {
    let (environment, home) = match credentials {
        Credentials::Keys(keys) => return Ok(keys),
        Credentials::Chain { environment, home } => (environment, home),
    };
    let context = Context::new()
        .with_file_read(TokioFileRead)
        .with_http_send(Web)
        .with_env(StaticEnv {
            home_dir: home,
            envs: environment,
        });
    let found = (DefaultCredentialProvider::new()
        .provide_credential(&context)
        .await)
        .map_err(|error| error.to_string())?;
    let found = found.ok_or("no AWS credentials were found")?;
    Ok(Keys {
        id: found.access_key_id,
        secret: found.secret_access_key,
        token: found.session_token,
    })
}

/// The user and password of Git over HTTPS to AWS CodeCommit at `time`: the key ID (with
/// `%` and the session token) and the SigV4 signature of a `GIT` request for the URL path.
pub fn git_account(
    url: &str,
    keys: &Keys,
    time: jiff::Timestamp,
) -> Result<(String, String), String> {
    let uri: http::Uri = (url.parse()).map_err(|error: http::uri::InvalidUri| error.to_string())?;
    let host = uri.host().unwrap_or_default();
    let parts: Vec<&str> = host.split('.').collect();
    let region = match parts.as_slice() {
        [_, region, _, _, ..] => *region,
        _ => return Err(format!("Cannot detect AWS region from URI {url}")),
    };
    let utc = time.to_zoned(jiff::tz::TimeZone::UTC);
    let stamp = utc.strftime("%Y%m%dT%H%M%S").to_string();
    let day = &stamp[..8];
    let canonical = format!("GIT\n{}\n\nhost:{host}\n\nhost\n", uri.path());
    let text = format!(
        "AWS4-HMAC-SHA256\n{stamp}\n{day}/{region}/codecommit/aws4_request\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let mut key = format!("AWS4{}", keys.secret).into_bytes();
    for part in [day, region, "codecommit", "aws4_request"] {
        key = hmac(&key, part.as_bytes())?;
    }
    let signature = hex(&hmac(&key, text.as_bytes())?);
    let user = match &keys.token {
        Some(token) => format!("{}%{token}", keys.id),
        None => keys.id.clone(),
    };
    Ok((user, format!("{stamp}Z{signature}")))
}

/// Signature Version 4 as AWS documents it: the canonical request, the string to sign and the
/// key derived for the day, region and service; the body hash is a signed header.
fn sign(
    request: &Request,
    keys: &Keys,
    time: jiff::Timestamp,
) -> Result<Vec<(String, String)>, String> {
    let uri: http::Uri = request
        .url
        .parse()
        .map_err(|error: http::uri::InvalidUri| error.to_string())?;
    let host = uri
        .authority()
        .ok_or("the address has no host")?
        .as_str()
        .to_owned();
    let utc = time.to_zoned(jiff::tz::TimeZone::UTC);
    let (stamp, day) = (
        utc.strftime("%Y%m%dT%H%M%SZ").to_string(),
        utc.strftime("%Y%m%d").to_string(),
    );
    let mut headers: Vec<(String, String)> = (request.headers.iter())
        .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
        .filter(|(name, _)| !matches!(name.as_str(), "host" | "authorization"))
        .collect();
    headers.push(("host".into(), host));
    headers.push(("x-amz-date".into(), stamp.clone()));
    headers.push((
        "x-amz-content-sha256".into(),
        hex(&Sha256::digest(&request.body)),
    ));
    if let Some(token) = &keys.token {
        headers.push(("x-amz-security-token".into(), token.clone()));
    }
    headers.sort();
    let canonical_headers: String = (headers.iter())
        .map(|(name, value)| {
            format!(
                "{name}:{}\n",
                value.split_whitespace().collect::<Vec<_>>().join(" ")
            )
        })
        .collect();
    let names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
    let names = names.join(";");
    let times = if request.service == "s3" { 1 } else { 2 };
    let mut path = decoded(uri.path());
    for _ in 0..times {
        path = encoded(&path, false);
    }
    let path = if path.is_empty() {
        "/".to_owned()
    } else {
        path
    };
    let mut query: Vec<(String, String)> = (uri.query().unwrap_or_default().split('&'))
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (encoded(&decoded(key), true), encoded(&decoded(value), true))
        })
        .collect();
    query.sort();
    let query: Vec<String> = query
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let payload = hex(&Sha256::digest(&request.body));
    let canonical = format!(
        "{}\n{path}\n{}\n{canonical_headers}\n{names}\n{payload}",
        request.method.to_ascii_uppercase(),
        query.join("&")
    );
    let scope = format!("{day}/{}/{}/aws4_request", request.region, request.service);
    let text = format!(
        "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let mut key = format!("AWS4{}", keys.secret).into_bytes();
    for part in [
        day.as_str(),
        &request.region,
        &request.service,
        "aws4_request",
    ] {
        key = hmac(&key, part.as_bytes())?;
    }
    let signature = hex(&hmac(&key, text.as_bytes())?);
    headers.push((
        "authorization".into(),
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={signature}",
            keys.id
        ),
    ));
    Ok(headers)
}

fn hmac(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).map_err(|e| e.to_string())?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// Percent-encodes all but the unreserved characters of RFC 3986, and `/` unless `slash`.
fn encoded(text: &str, slash: bool) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b'/' if !slash => out.push('/'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The text of a percent-encoded part of an address; `+` stays itself.
fn decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let pair = bytes
            .get(index + 1..index + 3)
            .and_then(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok());
        match (bytes[index], pair) {
            (b'%', Some(byte)) => {
                out.push(byte);
                index += 3;
            }
            (byte, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Sends the requests of the credential chain, like those to the instance metadata service.
#[derive(Debug)]
struct Web;

impl HttpSend for Web {
    async fn http_send(
        &self,
        request: http::Request<Bytes>,
    ) -> reqsign_core::Result<http::Response<Bytes>> {
        let failed = |message: String| reqsign_core::Error::unexpected(message);
        let client = crate::http_client().map_err(failed)?;
        let (parts, body) = request.into_parts();
        let response = (client.request(parts.method, parts.uri.to_string()))
            .headers(parts.headers)
            .body(body)
            .send()
            .await
            .map_err(|error| failed(error.to_string()))?;
        let mut answer = http::Response::builder().status(response.status());
        for (name, value) in response.headers() {
            answer = answer.header(name, value);
        }
        let body = (response.bytes().await).map_err(|error| failed(error.to_string()))?;
        answer.body(body).map_err(|error| failed(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example keys of AWS's documentation.
    fn keys() -> Credentials {
        Credentials::Keys(Keys {
            id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            token: None,
        })
    }

    fn authorization(method: &str, url: &str, headers: &[(&str, &str)], body: &[u8]) -> String {
        let request = Request {
            service: "s3".into(),
            region: "us-east-1".into(),
            method: method.into(),
            url: url.into(),
            headers: (headers.iter())
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            body: body.to_vec(),
        };
        let time = jiff::Timestamp::from_second(1_369_353_600).expect("a time");
        let headers = run(signed(request, keys(), Some(time))).expect("signs");
        let found = headers.iter().find(|(name, _)| name == "authorization");
        found.expect("an authorization").1.clone()
    }

    /// The examples of "Signature calculations for the Authorization header: transferring
    /// payload in a single chunk" in the Amazon S3 API reference.
    #[test]
    fn matches_aws_examples() {
        let scope = "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/\
                     aws4_request";
        let get = authorization(
            "GET",
            "https://examplebucket.s3.amazonaws.com/test.txt",
            &[("range", "bytes=0-9")],
            b"",
        );
        assert_eq!(
            get,
            format!(
                "{scope}, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
                 Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
            )
        );
        let put = authorization(
            "PUT",
            "https://examplebucket.s3.amazonaws.com/test$file.text",
            &[
                ("date", "Fri, 24 May 2013 00:00:00 GMT"),
                ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
            ],
            b"Welcome to Amazon S3.",
        );
        assert_eq!(
            put,
            format!(
                "{scope}, SignedHeaders=date;host;x-amz-content-sha256;x-amz-date;\
                 x-amz-storage-class, \
                 Signature=98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
            )
        );
        let list = authorization(
            "GET",
            "https://examplebucket.s3.amazonaws.com/?max-keys=2&prefix=J",
            &[],
            b"",
        );
        assert!(
            list.ends_with(
                "Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
            ),
            "{list}"
        );
    }

    #[test]
    fn chain_reads_environment() {
        let environment = HashMap::from([
            ("AWS_ACCESS_KEY_ID".to_owned(), "AKIDCHAIN".to_owned()),
            ("AWS_SECRET_ACCESS_KEY".to_owned(), "secret".to_owned()),
            ("AWS_SESSION_TOKEN".to_owned(), "token".to_owned()),
            ("AWS_EC2_METADATA_DISABLED".to_owned(), "true".to_owned()),
        ]);
        let credentials = Credentials::Chain {
            environment,
            home: None,
        };
        let request = Request {
            service: "athena".into(),
            region: "eu-west-1".into(),
            method: "POST".into(),
            url: "https://athena.eu-west-1.amazonaws.com/".into(),
            headers: Vec::new(),
            body: b"{}".to_vec(),
        };
        let headers = run(signed(request, credentials, None)).expect("signs");
        let header = |name: &str| headers.iter().find(|(key, _)| key == name).map(|(_, v)| v);
        let authorization = header("authorization").expect("an authorization");
        assert!(
            authorization.contains("Credential=AKIDCHAIN/"),
            "{authorization}"
        );
        assert!(
            authorization.contains("x-amz-security-token"),
            "{authorization}"
        );
        assert_eq!(
            header("x-amz-security-token").map(String::as_str),
            Some("token")
        );
    }

    /// The password computed separately from the rule that AWS's Git credential helper uses.
    #[test]
    fn derives_git_account() {
        let mut keys = Keys {
            id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            token: None,
        };
        let url = "https://git-codecommit.us-east-2.amazonaws.com/v1/repos/MyDemoRepo";
        let time: jiff::Timestamp = "2026-11-06T11:22:33Z".parse().expect("a time");
        let (user, password) = git_account(url, &keys, time).expect("an account");
        assert_eq!(user, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(
            password,
            "20261106T112233Z058a08801f361d45784a3678fc492bcf211059e494a161875ad1077a4986fc10"
        );
        keys.token = Some("session".into());
        let (user, _) = git_account(url, &keys, time).expect("an account");
        assert_eq!(user, "AKIAIOSFODNN7EXAMPLE%session");
        let short = git_account("https://codecommit.aws/v1/repos/x", &keys, time);
        assert_eq!(
            short.unwrap_err(),
            "Cannot detect AWS region from URI https://codecommit.aws/v1/repos/x"
        );
    }

    #[test]
    fn endpoint_prefers_service() {
        let variables = HashMap::from([
            ("AWS_ENDPOINT_URL".to_owned(), "http://all".to_owned()),
            (
                "AWS_ENDPOINT_URL_SESV2".to_owned(),
                "http://ses/".to_owned(),
            ),
        ]);
        let variable = |name: &str| variables.get(name).cloned();
        let default = || "https://default".to_owned();
        assert_eq!(endpoint("SESV2", default(), variable), "http://ses");
        assert_eq!(endpoint("ATHENA", default(), variable), "http://all");
        assert_eq!(endpoint("ATHENA", default(), |_| None), "https://default");
    }
}
