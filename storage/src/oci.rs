//! Reads and writes Oracle Cloud Object Storage objects of `oci://` addresses through its REST
//! API, each request signed with the API key of an OCI user.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG, IF_MATCH, LAST_MODIFIED, RANGE};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::Problem;
use crate::oci_key::Signer;
use crate::web::{network, open, origin_client, upload_client};
use crate::{Downloaded, Entry, Metadata, Written};

/// What an OCI client signs in with. Unset values come from the environment and the OCI
/// config file, `~/.oci/config`.
#[derive(Clone, Default)]
pub struct Options {
    /// The profile of the config file; else `OCI_PROFILE`, `OCI_DEFAULT_PROFILE`, `DEFAULT`.
    pub profile: Option<String>,
    pub region: Option<String>,
    /// With `user`, `fingerprint` and `private_key`, the config file is not read.
    pub tenancy: Option<String>,
    pub user: Option<String>,
    pub fingerprint: Option<String>,
    /// The API key as PEM text.
    pub private_key: Option<String>,
    /// The address of the service; `https://objectstorage.<region>.oraclecloud.com` without one.
    pub endpoint: Option<String>,
}

/// The options of the `oci` scope of a Nextflow config, for `oci://` addresses that no
/// provider of the config file serves.
static SCOPE: Mutex<Option<Options>> = Mutex::new(None);

/// Uses these options for `oci://` addresses that no provider of the config file serves, from
/// the first request on.
pub fn use_scope(options: Options) {
    *SCOPE.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(options);
}

/// The options of the built-in provider.
pub(crate) fn scope() -> Options {
    let scope = SCOPE.lock().unwrap_or_else(|poison| poison.into_inner());
    scope.clone().unwrap_or_default()
}

const REQUEST: Duration = Duration::from_secs(60);
/// A single `PUT` takes objects of at most 50 GiB.
const LARGEST_PUT: u64 = 50 << 30;
const CHUNK: usize = 1 << 20;

pub(crate) struct Oci {
    endpoint: String,
    signer: Signer,
    namespace: tokio::sync::OnceCell<String>,
}

impl Oci {
    pub fn new(options: Options) -> Result<Oci, Problem> {
        let signer = Signer::find(&options).map_err(Problem::fixed)?;
        let endpoint = (options.endpoint.clone()).unwrap_or_else(|| {
            format!("https://objectstorage.{}.oraclecloud.com", signer.region())
        });
        Ok(Oci {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            signer,
            namespace: tokio::sync::OnceCell::new(),
        })
    }

    /// The secrets that messages must not show.
    pub fn secrets(&self) -> Vec<String> {
        vec![self.signer.secret().to_owned()]
    }

    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, Problem> {
        let (bucket, key) = location(target)?;
        let namespace = self.namespace().await?;
        if key.is_empty() {
            let path = format!("/n/{namespace}/b/{}", encode(bucket));
            let response = self.send(Method::GET, &path, Request::Empty).await?;
            return Ok(found(response).await?.map(|_| directory()));
        }
        let path = object_path(namespace, bucket, key);
        let response = self.send(Method::HEAD, &path, Request::Empty).await?;
        if let Some(response) = found(response).await? {
            return Ok(Some(metadata(&response)));
        }
        let page = self.page(bucket, &format!("{key}/"), None).await?;
        let any = page["objects"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
            || page["prefixes"]
                .as_array()
                .is_some_and(|items| !items.is_empty());
        Ok(any.then(directory))
    }

    pub async fn list(&self, target: &str) -> Result<Vec<Entry>, Problem> {
        let (bucket, key) = location(target)?;
        let prefix = match key.trim_end_matches('/') {
            "" => String::new(),
            key => format!("{key}/"),
        };
        let mut entries = Vec::new();
        let mut start: Option<String> = None;
        loop {
            let page = self.page(bucket, &prefix, start.as_deref()).await?;
            let objects = page["objects"].as_array().into_iter().flatten();
            for name in objects.filter_map(|object| object["name"].as_str()) {
                let name = name.strip_prefix(prefix.as_str()).unwrap_or(name);
                if !name.is_empty() {
                    entries.push(Entry {
                        name: name.to_owned(),
                        directory: false,
                    });
                }
            }
            let folders = page["prefixes"].as_array().into_iter().flatten();
            for name in folders.filter_map(Value::as_str) {
                let name = name.strip_prefix(prefix.as_str()).unwrap_or(name);
                entries.push(Entry {
                    name: name.trim_end_matches('/').to_owned(),
                    directory: true,
                });
            }
            match page["nextStartWith"].as_str() {
                Some(next) if !next.is_empty() => start = Some(next.to_owned()),
                _ => return Ok(entries),
            }
        }
    }

    /// Downloads an object into `file`, from `offset` on, bound to its ETag.
    pub async fn download(
        &self,
        target: &str,
        file: &Path,
        offset: u64,
    ) -> Result<Downloaded, Problem> {
        let (bucket, key) = location(target)?;
        let path = object_path(self.namespace().await?, bucket, key);
        let mut headers = Vec::new();
        if offset > 0 {
            let head = self.send(Method::HEAD, &path, Request::Empty).await?;
            let head = found(head).await?.ok_or_else(Problem::missing)?;
            let tag = header(&head, ETAG).ok_or_else(|| {
                Problem::fixed("the object has no ETag to continue a download with")
            })?;
            headers.push((RANGE.as_str(), format!("bytes={offset}-")));
            headers.push((IF_MATCH.as_str(), tag));
        }
        let response = self
            .send(Method::GET, &path, Request::Headers(headers))
            .await?;
        let mut response = found(response).await?.ok_or_else(Problem::missing)?;
        let version = header(&response, ETAG);
        let mut output = open(file, offset > 0).await?;
        while let Some(chunk) = response.chunk().await.map_err(network)? {
            (output.write_all(&chunk).await).map_err(|error| Problem::local(file, error))?;
        }
        output
            .flush()
            .await
            .map_err(|error| Problem::local(file, error))?;
        Ok(Downloaded { version })
    }

    /// Uploads a file with one `PUT`, streamed from the disk.
    pub async fn upload(&self, file: &Path, target: &str) -> Result<Written, Problem> {
        let (bucket, key) = location(target)?;
        if key.is_empty() || key.ends_with('/') {
            return Err(Problem::fixed("the address names no object"));
        }
        let size = (tokio::fs::metadata(file).await)
            .map_err(|error| Problem::local(file, error))?
            .len();
        if size > LARGEST_PUT {
            return Err(Problem::fixed(format!(
                "the file has {size} bytes; uwe writes OCI objects of at most 50 GiB"
            )));
        }
        let source =
            (tokio::fs::File::open(file).await).map_err(|error| Problem::local(file, error))?;
        let path = object_path(self.namespace().await?, bucket, key);
        let response = self
            .send(Method::PUT, &path, Request::File { source, size })
            .await?;
        let response = found(response).await?.ok_or_else(Problem::missing)?;
        Ok(Written {
            size,
            version: header(&response, ETAG),
        })
    }

    /// The namespace of the tenancy, asked once.
    async fn namespace(&self) -> Result<&str, Problem> {
        let namespace = self.namespace.get_or_try_init(|| async {
            let response = self.send(Method::GET, "/n/", Request::Empty).await?;
            let response = found(response).await?.ok_or_else(Problem::missing)?;
            let body = response.text().await.map_err(network)?;
            match serde_json::from_str::<Value>(&body) {
                Ok(Value::String(namespace)) => Ok(namespace),
                _ => Err(Problem::fixed("the answer of OCI names no namespace")),
            }
        });
        namespace.await.map(String::as_str)
    }

    /// One page of the objects below `prefix`, with the folders one level down.
    async fn page(
        &self,
        bucket: &str,
        prefix: &str,
        start: Option<&str>,
    ) -> Result<Value, Problem> {
        let namespace = self.namespace().await?;
        let mut path = format!(
            "/n/{namespace}/b/{}/o?prefix={}&delimiter=%2F",
            encode(bucket),
            encode(prefix)
        );
        if let Some(start) = start {
            path.push_str(&format!("&start={}", encode(start)));
        }
        let response = self.send(Method::GET, &path, Request::Empty).await?;
        let Some(response) = found(response).await? else {
            return Ok(Value::Null);
        };
        let body = response.text().await.map_err(network)?;
        serde_json::from_str(&body)
            .map_err(|error| Problem::fixed(format!("the OCI listing is not JSON: {error}")))
    }

    /// Sends a signed request to `path`, which holds its query.
    async fn send(
        &self,
        method: Method,
        path: &str,
        request: Request,
    ) -> Result<reqwest::Response, Problem> {
        let url = format!("{}{path}", self.endpoint);
        let host = reqwest::Url::parse(&url)
            .ok()
            .and_then(|url| {
                let host = url.host_str()?.to_owned();
                Some(match url.port() {
                    Some(port) => format!("{host}:{port}"),
                    None => host,
                })
            })
            .ok_or_else(|| Problem::fixed(format!("{url} is not a web address")))?;
        let date = jiff::Timestamp::now()
            .strftime("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        let signed = vec![
            ("date", date.clone()),
            (
                "(request-target)",
                format!("{} {path}", method.as_str().to_lowercase()),
            ),
            ("host", host),
        ];
        let client = match &request {
            Request::File { .. } => upload_client().map_err(Problem::fixed)?,
            _ => origin_client().map_err(Problem::fixed)?,
        };
        let mut builder = client.request(method, &url).header("date", date);
        match request {
            Request::Empty => builder = builder.timeout(REQUEST),
            Request::Headers(headers) => {
                for (name, value) in headers {
                    builder = builder.header(name, value);
                }
            }
            // Object Storage leaves the body of an object out of the signature.
            Request::File { source, size } => {
                let chunks = futures_util::stream::unfold(source, |mut source| async move {
                    let mut buffer = vec![0; CHUNK];
                    match source.read(&mut buffer).await {
                        Ok(0) => None,
                        Ok(read) => {
                            buffer.truncate(read);
                            Some((Ok::<_, std::io::Error>(buffer), source))
                        }
                        Err(error) => Some((Err(error), source)),
                    }
                });
                builder = builder
                    .header(CONTENT_LENGTH, size)
                    .header(CONTENT_TYPE, "application/octet-stream")
                    .body(reqwest::Body::wrap_stream(chunks));
            }
        }
        let authorization = self.signer.authorization(&signed).map_err(Problem::fixed)?;
        let response = builder
            .header("authorization", authorization)
            .send()
            .await
            .map_err(network)?;
        Ok(response)
    }
}

/// The body of a request.
enum Request {
    Empty,
    Headers(Vec<(&'static str, String)>),
    File { source: tokio::fs::File, size: u64 },
}

/// The bucket and object name of `oci:///bucket/name` or `oci://bucket/name`.
fn location(target: &str) -> Result<(&str, &str), Problem> {
    let rest = target
        .strip_prefix("oci://")
        .ok_or_else(|| Problem::fixed("not an oci:// address"))?;
    let rest = rest.trim_start_matches('/');
    let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
    match bucket.is_empty() {
        true => Err(Problem::fixed("the address names no bucket")),
        false => Ok((bucket, key)),
    }
}

fn object_path(namespace: &str, bucket: &str, key: &str) -> String {
    format!("/n/{namespace}/b/{}/o/{}", encode(bucket), encode(key))
}

/// Percent-encodes all but the unreserved characters of RFC 3986, `/` too.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The answer of a request: `None` for 404, an error for other failures.
async fn found(response: reqwest::Response) -> Result<Option<reqwest::Response>, Problem> {
    let status = response.status();
    if status.is_success() {
        return Ok(Some(response));
    }
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body = response.text().await.unwrap_or_default();
    let detail: Value = serde_json::from_str(&body).unwrap_or_default();
    let message = match detail["message"].as_str() {
        Some(message) => format!("OCI answered {status}: {message}"),
        None => format!("OCI answered {status}"),
    };
    match status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        true => Err(Problem::passing(message)),
        false => Err(Problem::fixed(message)),
    }
}

fn header(response: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    let value = response.headers().get(name)?.to_str().ok()?;
    Some(value.to_owned())
}

fn metadata(response: &reqwest::Response) -> Metadata {
    let size = (header(response, CONTENT_LENGTH)).and_then(|length| length.parse().ok());
    Metadata {
        directory: false,
        modified: header(response, LAST_MODIFIED)
            .and_then(|value| opendal::raw::Timestamp::parse_rfc2822(&value).ok())
            .map(SystemTime::from),
        size: size.unwrap_or(0),
        version: header(response, ETAG),
    }
}

fn directory() -> Metadata {
    Metadata {
        directory: true,
        modified: None,
        size: 0,
        version: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        assert_eq!(
            location("oci:///data/a/b.txt").ok(),
            Some(("data", "a/b.txt"))
        );
        assert_eq!(
            location("oci://data/a/b.txt").ok(),
            Some(("data", "a/b.txt"))
        );
        assert_eq!(location("oci:///data").ok(), Some(("data", "")));
        assert!(location("oci:///").is_err());
        assert_eq!(
            object_path("ns", "data", "a/b c.txt"),
            "/n/ns/b/data/o/a%2Fb%20c.txt"
        );
    }
}
