//! Reads Synapse files of `syn://` addresses through the Synapse REST API with a personal
//! access token.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION};
use reqwest::{Method, StatusCode};
use serde_json::Value;

use crate::error::Problem;
use crate::web::{Web, network, origin_client};
use crate::{Downloaded, Metadata};

/// What a Synapse client signs in with. Unset values come from the environment.
#[derive(Clone, Default)]
pub struct Options {
    /// The address of the REST API, `https://repo-prod.prod.sagebase.org` by default.
    pub endpoint: Option<String>,
    /// A personal access token; else `SYNAPSE_AUTH_TOKEN` of the environment.
    pub token: Option<String>,
}

/// The options of the `synapse` scope of a Nextflow config, for `syn://` addresses that no
/// provider of the config file serves.
static SCOPE: Mutex<Option<Options>> = Mutex::new(None);

/// Uses these options for `syn://` addresses that no provider of the config file serves, from
/// the first request on.
pub fn use_scope(options: Options) {
    *SCOPE.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(options);
}

/// The options of the built-in provider.
pub(crate) fn scope() -> Options {
    let scope = SCOPE.lock().unwrap_or_else(|poison| poison.into_inner());
    scope.clone().unwrap_or_default()
}

const ENDPOINT: &str = "https://repo-prod.prod.sagebase.org";
const VARIABLE: &str = "SYNAPSE_AUTH_TOKEN";
const REQUEST: Duration = Duration::from_secs(60);
pub(crate) struct Synapse {
    endpoint: String,
    token: Option<String>,
    client: reqwest::Client,
}

/// What a `syn://` address names: an entity, maybe of one version, or a file to write below a
/// folder entity.
#[derive(Debug, PartialEq, Eq)]
enum Address<'a> {
    Entity {
        id: &'a str,
        version: Option<&'a str>,
    },
    Child {
        folder: &'a str,
        path: &'a str,
    },
}

impl Synapse {
    pub fn new(options: Options) -> Result<Synapse, Problem> {
        let token = (options.token)
            .or_else(|| std::env::var(VARIABLE).ok())
            .filter(|token| !token.is_empty());
        let endpoint = options.endpoint.unwrap_or_else(|| ENDPOINT.to_owned());
        Ok(Synapse {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            token,
            client: origin_client().cloned().map_err(Problem::fixed)?,
        })
    }

    /// The secrets that messages must not show.
    pub fn secrets(&self) -> Vec<String> {
        self.token.iter().cloned().collect()
    }

    /// An entity: a file, or a directory for folders, projects and other containers. A path
    /// below a folder is a place to write to, which does not exist yet.
    pub async fn stat(&self, target: &str) -> Result<Option<Metadata>, Problem> {
        match address(target)? {
            Address::Child { .. } => Ok(None),
            Address::Entity { id, version } => {
                Ok(self.entity(id, version).await?.as_ref().map(metadata))
            }
        }
    }

    /// Downloads a file entity through a presigned address.
    pub async fn download(&self, target: &str, file: &Path) -> Result<Downloaded, Problem> {
        let Address::Entity { id, version } = address(target)? else {
            return Err(Problem::missing());
        };
        let entity = self
            .entity(id, version)
            .await?
            .ok_or_else(Problem::missing)?;
        let kind = entity["concreteType"].as_str().unwrap_or_default();
        if !kind.ends_with(".FileEntity") {
            let kind = kind.rsplit('.').next().unwrap_or_default();
            return Err(Problem::fixed(format!(
                "Cannot read {id}: entity is a {kind}, not a file. Only FileEntity types can be \
                 downloaded."
            )));
        }
        let handle = text(&entity["dataFileHandleId"])
            .ok_or_else(|| Problem::fixed(format!("No file handle found for {id}")))?;
        let path = format!(
            "/file/v1/file/{handle}?redirect=false&fileAssociateType=FileEntity&\
             fileAssociateId={id}"
        );
        let url = self.presigned(&path).await?;
        Web::public()?.download(&url, file, 0).await?;
        Ok(Downloaded {
            version: metadata(&entity).version,
        })
    }

    /// The JSON answer of a REST request, `None` for 404.
    async fn rest(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>, Problem> {
        let url = format!("{}{path}", self.endpoint);
        let mut request = (self.client.request(method.clone(), &url))
            .bearer_auth(self.token()?)
            .header(ACCEPT, "application/json")
            .timeout(REQUEST);
        if method != Method::GET {
            let body = body.map(|body| body.to_string()).unwrap_or_default();
            request = request.header(CONTENT_TYPE, "application/json").body(body);
        }
        let response = request.send().await.map_err(network)?;
        let status = response.status();
        let body = response.text().await.map_err(network)?;
        match status.as_u16() {
            200 | 201 if body.trim().is_empty() => Ok(Some(Value::Null)),
            200 | 201 => serde_json::from_str(&body).map(Some).map_err(|error| {
                Problem::fixed(format!("the Synapse answer at {path} is not JSON: {error}"))
            }),
            404 => Ok(None),
            _ => Err(refused(status, &body)),
        }
    }

    async fn entity(&self, id: &str, version: Option<&str>) -> Result<Option<Value>, Problem> {
        let path = match version {
            Some(version) => format!("/repo/v1/entity/{id}/version/{version}"),
            None => format!("/repo/v1/entity/{id}"),
        };
        self.rest(Method::GET, &path, None).await
    }

    /// The presigned address of a file handle, from a request that follows no redirect: a
    /// redirect names the address, else the answer does, as text or as `preSignedURL`.
    async fn presigned(&self, path: &str) -> Result<String, Problem> {
        let request = (self.client.get(format!("{}{path}", self.endpoint)))
            .bearer_auth(self.token()?)
            .timeout(REQUEST);
        let response = request.send().await.map_err(network)?;
        let status = response.status();
        if status.is_redirection() {
            let location = response.headers().get(LOCATION);
            return (location.and_then(|value| value.to_str().ok()))
                .map(str::to_owned)
                .ok_or_else(|| Problem::fixed("Redirect response without Location header"));
        }
        let body = response.text().await.map_err(network)?;
        if status != StatusCode::OK {
            return Err(refused(status, &body));
        }
        if body.trim_start().starts_with("http") {
            return Ok(body.trim().to_owned());
        }
        let answer: Value = serde_json::from_str(&body).unwrap_or_default();
        (answer["preSignedURL"].as_str().map(str::to_owned))
            .ok_or_else(|| Problem::fixed("the Synapse answer names no presigned address"))
    }

    fn token(&self) -> Result<&str, Problem> {
        self.token.as_deref().ok_or_else(|| {
            Problem::fixed(
                "Synapse authentication token not configured. Set `token` of the storage \
                 provider, `synapse.authToken` of the Nextflow config, or the environment \
                 variable SYNAPSE_AUTH_TOKEN",
            )
        })
    }
}

/// Parses `syn://syn123`, `syn://syn123.5` (version 5), `syn://syn123.fastq.gz` and
/// `syn://syn123.5.fastq.gz` (a suffix for file name patterns), and `syn://syn123/a/b.txt`.
fn address(target: &str) -> Result<Address<'_>, Problem> {
    let invalid = || {
        Problem::fixed(format!(
            "Invalid Synapse URI: {target}. Expected format: syn://syn1234567, \
             syn://syn1234567.5, syn://syn1234567.fastq.gz, or syn://syn1234567/filename.txt"
        ))
    };
    let rest = target.strip_prefix("syn://").ok_or_else(invalid)?;
    let rest = rest.trim_end_matches('/');
    let digits = |text: &str| text.bytes().take_while(u8::is_ascii_digit).count();
    let id_end = (rest.strip_prefix("syn")).map_or(0, |after| 3 + digits(after));
    if id_end <= 3 {
        return Err(invalid());
    }
    let (id, tail) = rest.split_at(id_end);
    if let Some(path) = tail.strip_prefix('/') {
        return Ok(Address::Child { folder: id, path });
    }
    let (version, suffix) = match tail.strip_prefix('.') {
        Some(after) if digits(after) > 0 && after[digits(after)..].is_empty() => (Some(after), ""),
        Some(after) if digits(after) > 0 && after[digits(after)..].starts_with('.') => {
            (Some(&after[..digits(after)]), &after[digits(after)..])
        }
        _ => (None, tail),
    };
    let decorative = |suffix: &str| {
        (suffix.strip_prefix('.')).is_some_and(|name| {
            !name.is_empty() && !name.contains(|c: char| c == '/' || c.is_whitespace())
        })
    };
    match suffix.is_empty() || decorative(suffix) {
        true => Ok(Address::Entity { id, version }),
        false => Err(invalid()),
    }
}

/// A file entity is a file; folders, projects and other entities are directories. The file
/// handle is the version of a file whose size Synapse gives.
fn metadata(entity: &Value) -> Metadata {
    let file = entity["concreteType"]
        .as_str()
        .is_some_and(|kind| kind.ends_with(".FileEntity"));
    let size = entity["fileSize"].as_u64();
    Metadata {
        directory: !file,
        modified: (entity["modifiedOn"].as_str())
            .and_then(|time| time.parse::<jiff::Timestamp>().ok())
            .map(SystemTime::from),
        size: size.unwrap_or(0),
        version: text(&entity["dataFileHandleId"]).filter(|_| file && size.is_some()),
    }
}

/// An id, which Synapse writes as text or as a number.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// An answer that is not a success; errors of the server may go away.
fn refused(status: StatusCode, body: &str) -> Problem {
    let message = match status.as_u16() {
        401 => "Synapse authentication failed. Your token may be invalid or expired.".to_owned(),
        403 => "Access denied. You don't have permission to access this Synapse entity.".into(),
        404 => return Problem::missing(),
        code => format!("Synapse API error (HTTP {code}): {body}"),
    };
    match status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        true => Problem::passing(message),
        false => Problem::fixed(message),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_entity_addresses() {
        let entity = |id, version| Some(Address::Entity { id, version });
        let parsed = |text| address(text).ok();
        assert_eq!(parsed("syn://syn123"), entity("syn123", None));
        assert_eq!(parsed("syn://syn123/"), entity("syn123", None));
        assert_eq!(parsed("syn://syn123.5"), entity("syn123", Some("5")));
        assert_eq!(parsed("syn://syn123.fastq.gz"), entity("syn123", None));
        assert_eq!(
            parsed("syn://syn123.5.fastq.gz"),
            entity("syn123", Some("5"))
        );
        assert_eq!(parsed("syn://syn123.5x"), entity("syn123", None));
        let child = Address::Child {
            folder: "syn200",
            path: "sub/out.txt",
        };
        assert_eq!(parsed("syn://syn200/sub/out.txt"), Some(child));
        for wrong in [
            "syn://abc",
            "syn://syn",
            "syn://syn12 a",
            "syn://syn1.",
            "s3://syn1",
        ] {
            let message = address(wrong).err().map(|problem| problem.message);
            assert!(message.is_some_and(|message| message.starts_with("Invalid Synapse URI")));
        }
    }

    #[test]
    fn versions_need_sizes() {
        let file = json!({
            "concreteType": "org.sagebionetworks.repo.model.FileEntity", "dataFileHandleId": "9001", "fileSize": 19,
            "modifiedOn": "2026-01-03T03:04:05.000Z",
        });
        let found = metadata(&file);
        assert_eq!((found.directory, found.size), (false, 19));
        assert_eq!(found.version.as_deref(), Some("9001"));
        assert!(found.modified.is_some());
        let unsized_file = json!({ "concreteType": "org.sagebionetworks.repo.model.FileEntity", "dataFileHandleId": 9001 });
        assert_eq!(metadata(&unsized_file).version, None);
        assert!(
            metadata(&json!({ "concreteType": "org.sagebionetworks.repo.model.Folder" })).directory
        );
    }
}
