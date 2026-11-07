//! Reads and writes `lakefs://<repository>/<ref>/<path>` addresses through the lakeFS REST API,
//! moving the data through presigned addresses, and commits branches.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use base64::Engine;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::Problem;
use crate::web::{Web, network, origin_client, upload_client};
use crate::{Downloaded, Entry, Metadata, StorageError, Written};

/// What a lakeFS client signs in with.
#[derive(Clone, Default)]
pub struct Options {
    /// The base of the API, ending with `/api/v1`.
    pub endpoint: Option<String>,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    /// Whether writing to a missing branch makes it.
    pub create_branches: bool,
    /// The ref that new branches start from, `main` by default.
    pub branch_source: Option<String>,
    /// How long an API request may take, 60 seconds by default.
    pub timeout: Option<Duration>,
}

/// The options of the `lakefs` scope of a Nextflow config, for `lakefs://` addresses that no
/// provider of the config file serves.
static SCOPE: Mutex<Option<Options>> = Mutex::new(None);

/// Uses these options for `lakefs://` addresses that no provider of the config file serves,
/// and for commits, from the first request on.
pub fn use_scope(options: Options) {
    *SCOPE.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(options);
}

/// The options of the built-in provider.
pub(crate) fn scope() -> Options {
    let scope = SCOPE.lock().unwrap_or_else(|poison| poison.into_inner());
    scope.clone().unwrap_or_default()
}

/// Commits the branch of a `lakefs://` address with the options of [`use_scope`] and returns the
/// id of the commit.
pub fn commit(target: &str, message: &str) -> Result<String, StorageError> {
    let (target, message) = (target.to_owned(), message.to_owned());
    crate::blocking::run(async move {
        let failed = failed("commit", &target);
        let lakefs = Lakefs::new(scope()).map_err(&failed)?;
        let (repository, branch, _) = address(&target).map_err(&failed)?;
        (lakefs.commit(repository, branch, &message).await).map_err(&failed)
    })
}

/// Makes the branch of a `lakefs://` address if it is missing, as writes do with
/// `create_branches`.
pub fn create_branch(target: &str) -> Result<(), StorageError> {
    let target = target.to_owned();
    crate::blocking::run(async move {
        let failed = failed("make the branch of", &target);
        let lakefs = Lakefs::new(scope()).map_err(&failed)?;
        let (repository, branch, _) = address(&target).map_err(&failed)?;
        (lakefs.branch(repository, branch).await).map_err(&failed)
    })
}

const TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct Lakefs {
    endpoint: String,
    /// The value of the `Authorization` header.
    authorization: String,
    secret: String,
    create_branches: bool,
    branch_source: String,
    timeout: Duration,
    /// The branches known to exist.
    branches: Mutex<HashSet<(String, String)>>,
}

impl Lakefs {
    pub fn new(options: Options) -> Result<Lakefs, Problem> {
        let endpoint = (options.endpoint.filter(|endpoint| !endpoint.is_empty()))
            .ok_or_else(|| Problem::fixed("lakeFS needs `apiUrl`, the address of its API"))?;
        let (Some(access), Some(secret)) = (options.access_key, options.secret_key) else {
            return Err(Problem::fixed(
                "lakeFS needs `accessKey` and `secretKey`, the credentials of a user",
            ));
        };
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("{access}:{secret}"));
        tracing::debug!("Created lakeFS file system provider | apiUrl={endpoint}");
        Ok(Lakefs {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            authorization: format!("Basic {encoded}"),
            secret,
            create_branches: options.create_branches,
            branch_source: options.branch_source.unwrap_or_else(|| "main".to_owned()),
            timeout: options.timeout.unwrap_or(TIMEOUT),
            branches: Mutex::new(HashSet::new()),
        })
    }

    /// The secrets that messages must not show.
    pub fn secrets(&self) -> Vec<String> {
        let encoded = self.authorization.trim_start_matches("Basic ").to_owned();
        vec![self.secret.clone(), encoded]
    }

    /// An object, or a directory for a ref or a prefix of objects.
    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, Problem> {
        let (repository, reference, path) = address(target)?;
        if path.is_empty() {
            let path = format!("/repositories/{repository}/refs/{reference}/objects/ls?amount=1");
            return Ok(self
                .json(Method::GET, &path, None)
                .await?
                .map(|_| directory()));
        }
        let found = self.object(repository, reference, path, false).await?;
        if let Some(object) = found {
            return Ok(Some(metadata(&object)));
        }
        let page = self
            .page(repository, reference, &format!("{path}/"), None)
            .await?;
        let any = (page.as_ref()).is_some_and(|page| {
            page["results"]
                .as_array()
                .is_some_and(|results| !results.is_empty())
        });
        Ok(any.then(directory))
    }

    /// The objects and common prefixes one level below a prefix, through every page.
    pub async fn list(&self, target: &str) -> Result<Vec<Entry>, Problem> {
        let (repository, reference, path) = address(target)?;
        let prefix = match path.trim_end_matches('/') {
            "" => String::new(),
            path => format!("{path}/"),
        };
        let mut entries = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page = self
                .page(repository, reference, &prefix, after.as_deref())
                .await?;
            let Some(page) = page else {
                return Err(Problem::missing());
            };
            for result in page["results"].as_array().into_iter().flatten() {
                let Some(path) = result["path"].as_str() else {
                    continue;
                };
                tracing::debug!("key {path}");
                let name = path.strip_prefix(prefix.as_str()).unwrap_or(path);
                entries.push(Entry {
                    name: name.trim_end_matches('/').to_owned(),
                    directory: result["path_type"] == "common_prefix",
                });
            }
            let pagination = &page["pagination"];
            match (
                pagination["has_more"].as_bool(),
                pagination["next_offset"].as_str(),
            ) {
                (Some(true), Some(next)) if !next.is_empty() => after = Some(next.to_owned()),
                _ => return Ok(entries),
            }
        }
    }

    /// Downloads through the presigned address of the object, else through the API when the
    /// store of lakeFS gives none.
    pub async fn download(&self, target: &str, file: &Path) -> Result<Downloaded, Problem> {
        let (repository, reference, path) = address(target)?;
        let object =
            (self.object(repository, reference, path, true).await?).ok_or_else(Problem::missing)?;
        let version = metadata(&object).version;
        let presigned = object["physical_address"].as_str().unwrap_or_default();
        if presigned.starts_with("https://") || presigned.starts_with("http://") {
            Web::public()?.download(presigned, file, 0).await?;
            return Ok(Downloaded { version });
        }
        let path = format!(
            "/repositories/{repository}/refs/{reference}/objects?path={}",
            encode(path)
        );
        let client = origin_client().map_err(Problem::fixed)?;
        let request = (client.get(format!("{}{path}", self.endpoint)))
            .header("authorization", &self.authorization);
        let mut response = request.send().await.map_err(network)?;
        if !response.status().is_success() {
            return Err(refused(
                response.status(),
                &response.text().await.unwrap_or_default(),
            ));
        }
        let mut output = crate::web::open(file, false).await?;
        while let Some(chunk) = response.chunk().await.map_err(network)? {
            (output.write_all(&chunk).await).map_err(|error| Problem::local(file, error))?;
        }
        Ok(Downloaded { version })
    }

    /// Stages a file on its branch: lakeFS names a place in its store, the file goes there
    /// through the presigned address, and lakeFS links it to the path.
    pub async fn upload(&self, file: &Path, target: &str) -> Result<Written, Problem> {
        let (repository, branch, path) = address(target)?;
        if path.is_empty() || path.ends_with('/') {
            return Err(Problem::fixed("the address names no object"));
        }
        self.branch(repository, branch).await?;
        let size = (tokio::fs::metadata(file).await)
            .map_err(|error| Problem::local(file, error))?
            .len();
        let staging = format!(
            "/repositories/{repository}/branches/{branch}/staging/backing?path={}",
            encode(path)
        );
        let place = (self
            .json(Method::GET, &format!("{staging}&presign=true"), None)
            .await?)
            .ok_or_else(Problem::missing)?;
        let Some(url) = place["presigned_url"]
            .as_str()
            .filter(|url| !url.is_empty())
        else {
            return self
                .upload_direct(file, repository, branch, path, size)
                .await;
        };
        tracing::debug!(
            "******** staging {path} to {}",
            place["physical_address"].as_str().unwrap_or_default()
        );
        let client = upload_client().map_err(Problem::fixed)?;
        let response = (client.put(url))
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, size)
            .body(body(file).await?)
            .send()
            .await
            .map_err(network)?;
        let status = response.status();
        if !status.is_success() {
            let message = format!("the presigned upload of {path} answered {status}");
            return Err(Problem::passing(message));
        }
        let tag = (response.headers().get(ETAG))
            .and_then(|tag| tag.to_str().ok())
            .map(|tag| tag.trim_matches('"').to_owned());
        let mut link = json!({
            "staging": place, "size_bytes": size, "user_metadata": {}, "force": false,
        });
        if let Some(tag) = &tag {
            link["checksum"] = tag.clone().into();
        }
        let linked =
            (self.json(Method::PUT, &staging, Some(link)).await?).ok_or_else(Problem::missing)?;
        Ok(Written {
            size,
            version: metadata(&linked).version,
        })
    }

    /// Uploads a file through the API, for stores that give no presigned address.
    async fn upload_direct(
        &self,
        file: &Path,
        repository: &str,
        branch: &str,
        path: &str,
        size: u64,
    ) -> Result<Written, Problem> {
        let url = format!(
            "{}/repositories/{repository}/branches/{branch}/objects?path={}",
            self.endpoint,
            encode(path)
        );
        let client = upload_client().map_err(Problem::fixed)?;
        let response = (client.post(url))
            .header("authorization", &self.authorization)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, size)
            .body(body(file).await?)
            .send()
            .await
            .map_err(network)?;
        let status = response.status();
        let text = response.text().await.map_err(network)?;
        if !status.is_success() {
            return Err(refused(status, &text));
        }
        let object: Value = serde_json::from_str(&text).unwrap_or_default();
        Ok(Written {
            size,
            version: metadata(&object).version,
        })
    }

    /// Makes sure that a branch exists, and makes it from the source ref when that is allowed.
    async fn branch(&self, repository: &str, branch: &str) -> Result<(), Problem> {
        let key = (repository.to_owned(), branch.to_owned());
        if self.known(&key) {
            return Ok(());
        }
        let path = format!("/repositories/{repository}/branches/{branch}");
        if self.json(Method::GET, &path, None).await?.is_none() {
            if !self.create_branches {
                return Err(Problem::fixed(format!(
                    "the branch '{branch}' does not exist in the lakeFS repository \
                     '{repository}'; turn on autoCreateBranch to make it"
                )));
            }
            let source = &self.branch_source;
            tracing::info!(
                "Reference '{branch}' does not exist in repository '{repository}', \
                 auto-creating branch from '{source}'"
            );
            let body = json!({ "name": branch, "source": source, "force": false, "hidden": false });
            let made = format!("/repositories/{repository}/branches");
            self.json(Method::POST, &made, Some(body)).await?;
            tracing::info!(
                "Created branch '{branch}' from '{source}' in repository '{repository}'"
            );
        }
        let mut branches = self
            .branches
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        branches.insert(key);
        Ok(())
    }

    fn known(&self, key: &(String, String)) -> bool {
        let branches = self
            .branches
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        branches.contains(key)
    }

    async fn commit(
        &self,
        repository: &str,
        branch: &str,
        message: &str,
    ) -> Result<String, Problem> {
        let path = format!("/repositories/{repository}/branches/{branch}");
        (self.json(Method::GET, &path, None).await?).ok_or_else(|| {
            Problem::fixed(format!(
                "the branch '{branch}' does not exist in the lakeFS repository '{repository}'"
            ))
        })?;
        tracing::info!(
            "Committing lakeFS repository '{repository}' branch '{branch}' with message: {message}"
        );
        let body = json!({
            "message": message, "metadata": {}, "allow_empty": false, "force": false,
        });
        let made = (self
            .json(Method::POST, &format!("{path}/commits"), Some(body))
            .await?)
            .ok_or_else(Problem::missing)?;
        let id = (made["id"].as_str())
            .ok_or_else(|| Problem::fixed("the lakeFS answer names no commit"))?;
        tracing::info!("Created commit '{id}' on branch '{branch}' in repository '{repository}'");
        Ok(id.to_owned())
    }

    /// The stat of an object, with a presigned address if asked for; `None` when missing.
    async fn object(
        &self,
        repository: &str,
        reference: &str,
        path: &str,
        presign: bool,
    ) -> Result<Option<Value>, Problem> {
        let path = format!(
            "/repositories/{repository}/refs/{reference}/objects/stat?path={}&presign={presign}",
            encode(path)
        );
        self.json(Method::GET, &path, None).await
    }

    async fn page(
        &self,
        repository: &str,
        reference: &str,
        prefix: &str,
        after: Option<&str>,
    ) -> Result<Option<Value>, Problem> {
        let mut path = format!(
            "/repositories/{repository}/refs/{reference}/objects/ls?prefix={}&delimiter=%2F&\
             presign=false&user_metadata=false",
            encode(prefix)
        );
        if let Some(after) = after {
            path.push_str(&format!("&after={}", encode(after)));
        }
        self.json(Method::GET, &path, None).await
    }

    /// The JSON answer of an API request, `None` for 404.
    async fn json(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>, Problem> {
        let client = origin_client().map_err(Problem::fixed)?;
        let mut request = (client.request(method, format!("{}{path}", self.endpoint)))
            .header("authorization", &self.authorization)
            .timeout(self.timeout);
        if let Some(body) = body {
            request = request
                .header(CONTENT_TYPE, "application/json; charset=utf-8")
                .body(body.to_string());
        }
        let response = request.send().await.map_err(network)?;
        let status = response.status();
        let text = response.text().await.map_err(network)?;
        match status {
            StatusCode::NOT_FOUND => Ok(None),
            status if status.is_success() => {
                Ok(Some(serde_json::from_str(&text).unwrap_or(Value::Null)))
            }
            status => Err(refused(status, &text)),
        }
    }
}

/// The repository, the ref and the path of a `lakefs://` address.
fn address(target: &str) -> Result<(&str, &str, &str), Problem> {
    let invalid = || {
        Problem::fixed(format!(
            "{target} is not a lakeFS address like lakefs://<repository>/<ref>/<path>"
        ))
    };
    let rest = target.strip_prefix("lakefs://").ok_or_else(invalid)?;
    let (repository, rest) = rest.split_once('/').ok_or_else(invalid)?;
    let (reference, path) = rest.split_once('/').unwrap_or((rest, ""));
    match repository.is_empty() || reference.is_empty() {
        true => Err(invalid()),
        false => Ok((repository, reference, path)),
    }
}

/// The body of an upload, streamed from the file.
async fn body(file: &Path) -> Result<reqwest::Body, Problem> {
    let source =
        (tokio::fs::File::open(file).await).map_err(|error| Problem::local(file, error))?;
    let chunks = futures_util::stream::unfold(source, |mut source| async move {
        let mut buffer = vec![0; 1 << 20];
        match source.read(&mut buffer).await {
            Ok(0) => None,
            Ok(read) => {
                buffer.truncate(read);
                Some((Ok::<_, std::io::Error>(buffer), source))
            }
            Err(error) => Some((Err(error), source)),
        }
    });
    Ok(reqwest::Body::wrap_stream(chunks))
}

/// Percent-encodes all but the unreserved characters of RFC 3986 and `/`.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Size, time and checksum of an object of the API.
fn metadata(object: &Value) -> Metadata {
    Metadata {
        directory: false,
        modified: (object["mtime"].as_u64())
            .map(|seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)),
        size: object["size_bytes"].as_u64().unwrap_or(0),
        version: (object["checksum"].as_str())
            .filter(|checksum| !checksum.is_empty())
            .map(str::to_owned),
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

/// An answer that is not a success, with lakeFS's message; errors of the server may go away.
fn refused(status: StatusCode, body: &str) -> Problem {
    let detail: Value = serde_json::from_str(body).unwrap_or_default();
    let message = match detail["message"].as_str() {
        Some(message) => format!("lakeFS answered {status}: {message}"),
        None => format!("lakeFS answered {status}"),
    };
    match status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        true => Problem::passing(message),
        false => Problem::fixed(message),
    }
}

/// An error of a request that no provider of the config file makes.
fn failed<'a>(action: &'static str, target: &'a str) -> impl Fn(Problem) -> StorageError + 'a {
    move |problem| StorageError::Failed {
        action,
        address: target.to_owned(),
        provider: "lakeFS, with the lakefs scope".to_owned(),
        message: problem.message,
        retryable: problem.retryable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        assert_eq!(
            address("lakefs://repo1/main/data/in.txt").ok(),
            Some(("repo1", "main", "data/in.txt"))
        );
        assert_eq!(
            address("lakefs://repo1/main").ok(),
            Some(("repo1", "main", ""))
        );
        assert!(address("lakefs://repo1").is_err());
        assert!(address("lakefs:///main/a").is_err());
        assert_eq!(encode("data/a b.txt"), "data/a%20b.txt");
    }
}
