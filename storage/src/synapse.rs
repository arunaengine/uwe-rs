//! Reads Synapse files of `syn://` addresses and publishes files into Synapse folders, through
//! the Synapse REST API with a personal access token.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::SeekFrom;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use md5::{Digest, Md5};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderName, LOCATION};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::error::Problem;
use crate::web::{Web, network, origin_client, upload_client};
use crate::{Downloaded, Metadata, Written};

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
const FILE: &str = "org.sagebionetworks.repo.model.FileEntity";
const FOLDER: &str = "org.sagebionetworks.repo.model.Folder";
const REQUEST: Duration = Duration::from_secs(60);
const PART_UPLOAD: Duration = Duration::from_secs(600);
/// Parts start at 5 MiB and double until a file needs at most 10,000 of them; S3 takes parts
/// of at most 5 GiB.
const FIRST_PART: u64 = 5 << 20;
const MOST_PARTS: u64 = 10_000;
const LARGEST_PART: u64 = 5 << 30;

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

/// Writing: `syn://<folder>/<path>` makes the folders of the path that are missing, uploads the
/// file in parts and makes a file entity, or a new version of the one with that name.
impl Synapse {
    pub async fn upload(&self, file: &Path, target: &str) -> Result<Written, Problem> {
        let (base, path) = match address(target)? {
            Address::Child { folder, path } => (folder, path),
            Address::Entity { id, .. } => {
                return Err(Problem::fixed(format!(
                    "Cannot write to {id}: path must include a filename (e.g., \
                     syn://folder/filename.txt)"
                )));
            }
        };
        tracing::info!("Uploading {path} to Synapse folder {base}");
        let folder = self
            .entity(base, None)
            .await?
            .ok_or_else(Problem::missing)?;
        if folder["concreteType"] != FOLDER {
            return Err(Problem::fixed(format!(
                "Cannot write to {base}: not a Folder"
            )));
        }
        let (folders, name) = path.rsplit_once('/').unwrap_or(("", path));
        let mut parent = base.to_owned();
        for part in folders.split('/').filter(|part| !part.is_empty()) {
            parent = match self.child(&parent, part, FOLDER).await? {
                Some(id) => id,
                None => {
                    let body = json!({ "concreteType": FOLDER, "name": part, "parentId": parent });
                    let made = self.create(body).await?;
                    tracing::info!("Created folder '{part}' ({made}) under {base}");
                    made
                }
            };
        }
        tracing::info!("Uploading file '{name}' to Synapse folder {base}");
        let size = (tokio::fs::metadata(file).await)
            .map_err(|error| Problem::local(file, error))?
            .len();
        let handle = self.multipart(file, name, size).await?;
        let id = match self.child(&parent, name, FILE).await? {
            None => {
                let body = json!({
                    "concreteType": FILE, "name": name, "parentId": parent,
                    "dataFileHandleId": handle,
                });
                self.create(body).await?
            }
            Some(id) => {
                tracing::info!("File '{name}' already exists ({id}), uploading as new version");
                let current = self.entity(&id, None).await?.ok_or_else(Problem::missing)?;
                let body = json!({
                    "concreteType": FILE, "id": id, "name": name,
                    "parentId": current["parentId"], "etag": current["etag"],
                    "dataFileHandleId": handle,
                });
                let path = format!("/repo/v1/entity/{id}");
                self.rest(Method::PUT, &path, Some(body)).await?;
                id
            }
        };
        tracing::info!("Created FileEntity {id} in folder {parent}");
        tracing::info!("Upload complete: {path}");
        Ok(Written {
            size,
            version: Some(handle),
        })
    }

    /// Uploads a file in parts and returns its file handle; Synapse may already hold the bytes.
    async fn multipart(&self, file: &Path, name: &str, size: u64) -> Result<String, Problem> {
        let part = part_size(size)?;
        let request = json!({
            "concreteType": "org.sagebionetworks.repo.model.file.MultipartUploadRequest",
            "fileName": name,
            "fileSizeBytes": size,
            "contentMD5Hex": file_md5(file).await?,
            "contentType": content_type(name),
            "partSizeBytes": part,
        });
        let status = self
            .call(Method::POST, "/file/v1/file/multipart", request)
            .await?;
        if status["state"] == "COMPLETED" {
            tracing::info!("Upload already completed (file exists in Synapse)");
            return handle(&status);
        }
        let upload = text(&status["uploadId"])
            .ok_or_else(|| Problem::fixed("the Synapse answer names no upload"))?;
        let mut source =
            (tokio::fs::File::open(file).await).map_err(|error| Problem::local(file, error))?;
        for number in 1..=size.div_ceil(part).max(1) {
            let start = (number - 1) * part;
            let mut bytes = vec![0; (size - start).min(part) as usize];
            source
                .seek(SeekFrom::Start(start))
                .await
                .map_err(|error| Problem::local(file, error))?;
            (source.read_exact(&mut bytes).await).map_err(|error| Problem::local(file, error))?;
            self.part(&upload, number, bytes).await?;
        }
        let path = format!("/file/v1/file/multipart/{upload}/complete");
        handle(&self.call(Method::PUT, &path, Value::Null).await?)
    }

    /// Sends one part to its presigned address, with exactly the headers Synapse signed, and
    /// adds it to the upload.
    async fn part(&self, upload: &str, number: u64, bytes: Vec<u8>) -> Result<(), Problem> {
        let path = format!("/file/v1/file/multipart/{upload}/presigned/url/batch");
        let answer = self.call(Method::POST, &path, json!({ "partNumbers": [number] }));
        let answer = answer.await?;
        let presigned = &answer["partPresignedUrls"][0];
        let url = (presigned["uploadPresignedUrl"].as_str()).ok_or_else(|| {
            Problem::fixed(format!("Failed to get presigned URL for part {number}"))
        })?;
        let digest = hex(&Md5::digest(&bytes));
        let client = upload_client().map_err(Problem::fixed)?;
        let mut request = client.put(url).timeout(PART_UPLOAD).body(bytes);
        let headers = presigned["signedHeaders"].as_object().into_iter().flatten();
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Problem::fixed("Synapse signed a header that is not valid"))?;
            request = request.header(name, text(value).unwrap_or_default());
        }
        let response = request.send().await.map_err(network)?;
        let status = response.status();
        if status != StatusCode::OK {
            let body = response.text().await.unwrap_or_default();
            let message = format!(
                "Failed to upload part to S3: HTTP {} - {body}",
                status.as_u16()
            );
            return Err(Problem::passing(message));
        }
        let path = format!("/file/v1/file/multipart/{upload}/add/{number}?partMD5Hex={digest}");
        let added = self.call(Method::PUT, &path, Value::Null).await?;
        match added["addPartState"].as_str() {
            Some("ADD_FAILED") => Err(Problem::passing(format!(
                "Synapse did not add part {number}: {}",
                added["errorMessage"].as_str().unwrap_or_default()
            ))),
            _ => Ok(()),
        }
    }

    /// The child of a folder with this name and type, through every page of children.
    async fn child(&self, parent: &str, name: &str, kind: &str) -> Result<Option<String>, Problem> {
        let mut page_token = Value::Null;
        loop {
            let mut request = json!({ "parentId": parent, "includeTypes": ["folder", "file"] });
            if !page_token.is_null() {
                request["nextPageToken"] = page_token;
            }
            let answer = self.call(Method::POST, "/repo/v1/entity/children", request);
            let answer = answer.await?;
            let page = answer["page"].as_array().into_iter().flatten();
            let found = page
                .filter(|child| child["name"] == name && child["type"] == kind)
                .find_map(|child| text(&child["id"]));
            if found.is_some() || answer["nextPageToken"].is_null() {
                return Ok(found);
            }
            page_token = answer["nextPageToken"].clone();
        }
    }

    /// Makes an entity and returns its id.
    async fn create(&self, entity: Value) -> Result<String, Problem> {
        let made = self.call(Method::POST, "/repo/v1/entity", entity).await?;
        text(&made["id"]).ok_or_else(|| Problem::fixed("the Synapse answer names no entity"))
    }

    /// A REST request whose 404 is an error.
    async fn call(&self, method: Method, path: &str, body: Value) -> Result<Value, Problem> {
        let body = (!body.is_null()).then_some(body);
        self.rest(method, path, body).await?.ok_or_else(|| {
            Problem::fixed(format!(
                "no such Synapse request {path}: Synapse entity not found. The entity may not \
                 exist or you may not have access."
            ))
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

fn handle(status: &Value) -> Result<String, Problem> {
    text(&status["resultFileHandleId"])
        .ok_or_else(|| Problem::fixed("the Synapse upload ended without a file handle"))
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

/// The size of the parts of a file: 5 MiB, doubled until the file needs at most 10,000.
fn part_size(size: u64) -> Result<u64, Problem> {
    let mut part = FIRST_PART;
    while size.div_ceil(part) > MOST_PARTS {
        part *= 2;
    }
    match part <= LARGEST_PART {
        true => Ok(part),
        false => Err(Problem::fixed(format!(
            "File size {size} is too large. Maximum supported file size is {} bytes.",
            LARGEST_PART * MOST_PARTS
        ))),
    }
}

/// The content type by the last extension of a file name.
fn content_type(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map(|(_, extension)| extension);
    match extension.map(str::to_lowercase).as_deref() {
        Some("txt" | "vcf" | "fastq" | "fq" | "fasta" | "fa" | "bed") => "text/plain",
        Some("csv") => "text/csv",
        Some("tsv") => "text/tab-separated-values",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("html" | "htm") => "text/html",
        Some("pdf") => "application/pdf",
        Some("gz" | "gzip") => "application/gzip",
        Some("zip") => "application/zip",
        Some("tar") => "application/x-tar",
        _ => "application/octet-stream",
    }
}

async fn file_md5(file: &Path) -> Result<String, Problem> {
    let mut source =
        (tokio::fs::File::open(file).await).map_err(|error| Problem::local(file, error))?;
    let mut digest = Md5::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = (source.read(&mut buffer).await).map_err(|error| Problem::local(file, error))?;
        if read == 0 {
            return Ok(hex(&digest.finalize()));
        }
        digest.update(&buffer[..read]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
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
    fn sizes_parts() {
        assert_eq!(part_size(0).ok(), Some(FIRST_PART));
        assert_eq!(part_size(FIRST_PART * MOST_PARTS).ok(), Some(FIRST_PART));
        assert_eq!(
            part_size(FIRST_PART * MOST_PARTS + 1).ok(),
            Some(2 * FIRST_PART)
        );
        assert!(part_size(LARGEST_PART * MOST_PARTS + 1).is_err());
    }

    #[test]
    fn types_by_extension() {
        assert_eq!(content_type("reads.FQ"), "text/plain");
        assert_eq!(content_type("table.tsv"), "text/tab-separated-values");
        assert_eq!(content_type("reads.fastq.gz"), "application/gzip");
        assert_eq!(content_type("aligned.bam"), "application/octet-stream");
        assert_eq!(content_type("README"), "application/octet-stream");
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
