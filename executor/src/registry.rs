//! Reads image digests and configuration from registries without a local engine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue, WWW_AUTHENTICATE};
use reqwest::{Method, StatusCode};
use tokio::sync::OnceCell;

use crate::credentials::{Credential, Found};
use crate::job::ExecutorError;

/// The manifest types a registry may answer with, as `docker pull` asks for them.
const MANIFESTS: &str = "application/vnd.oci.image.index.v1+json, \
     application/vnd.docker.distribution.manifest.list.v2+json, \
     application/vnd.oci.image.manifest.v1+json, \
     application/vnd.docker.distribution.manifest.v2+json";

/// How long a request to a registry may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// An image reference split into its registry, repository and tag or digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    /// The host of the registry, like `quay.io`; `registry-1.docker.io` for Docker Hub.
    pub registry: String,
    /// The repository, like `biocontainers/fastqc`, or `library/debian` on Docker Hub.
    pub repository: String,
    /// The tag, `latest` when the reference names none.
    pub tag: String,
    /// The digest, when the reference is pinned already.
    pub digest: Option<String>,
    /// The name as the reference gives it, without tag and digest.
    pub name: String,
}

impl Reference {
    /// Reads a reference like `debian:stable-slim`, `quay.io/biocontainers/fastqc:0.12.1` or
    /// `ubuntu@sha256:...`, as Docker reads it.
    pub fn parse(reference: &str) -> Reference {
        let (rest, digest) = match reference.split_once('@') {
            Some((rest, digest)) => (rest, Some(digest.to_owned())),
            None => (reference, None),
        };
        // A tag follows the last `:` after the last `/`; a `:` before it is a port.
        let slash = rest.rfind('/').map_or(0, |index| index + 1);
        let (name, tag) = match rest[slash..].rfind(':') {
            Some(colon) => (&rest[..slash + colon], &rest[slash + colon + 1..]),
            None => (rest, "latest"),
        };
        let first = name.split('/').next().unwrap_or_default();
        let own = name.contains('/') && (first.contains(['.', ':']) || first == "localhost");
        // Docker Hub by its own names, like `docker.io/debian`, is Docker Hub as well.
        let hub = |path: &str| match path.contains('/') {
            true => ("registry-1.docker.io".to_owned(), path.to_owned()),
            false => ("registry-1.docker.io".to_owned(), format!("library/{path}")),
        };
        let (registry, repository) = match own {
            true => match name.split_once('/').unwrap_or((name, "")) {
                ("docker.io" | "index.docker.io", path) => hub(path),
                (host, path) => (host.to_owned(), path.to_owned()),
            },
            false => hub(name),
        };
        Reference {
            registry,
            repository,
            tag: tag.to_owned(),
            digest,
            name: name.to_owned(),
        }
    }
}

/// The image pinned to the digest that its registry gives for its tag, like
/// `debian@sha256:...`; `None` when the registry has no digest for it.
pub async fn pin(reference: &str) -> Result<Option<String>, ExecutorError> {
    // An address with its own scheme, like the `https://` of a SIF file, is no image of a
    // registry.
    if reference.contains("://") {
        return Ok(None);
    }
    let parsed = Reference::parse(reference);
    if parsed.digest.is_some() {
        return Ok(Some(reference.to_owned()));
    }
    let response = ask(&parsed, Method::HEAD, &manifest_url(&parsed), MANIFESTS).await?;
    let digest = (response.headers().get("docker-content-digest"))
        .and_then(|value| value.to_str().ok())
        .filter(|digest| digest.starts_with("sha256:"));
    Ok(digest.map(|digest| format!("{}@{digest}", parsed.name)))
}

/// Checks a published manifest by digest. Only HTTP 404 means it must be republished.
pub async fn exists(reference: &str) -> Result<bool, ExecutorError> {
    let parsed = Reference::parse(reference);
    let digest = parsed.digest.as_deref().ok_or_else(|| {
        ExecutorError::Unsupported("registry reuse requires a digest reference".into())
    })?;
    let (response, _) = request(
        &parsed,
        Method::HEAD,
        &manifest_url(&parsed),
        MANIFESTS,
        None,
    )
    .await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(false);
    }
    if !response.status().is_success()
        || response
            .headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok())
            != Some(digest)
    {
        return Err(ExecutorError::Engine {
            engine: parsed.registry,
            message: format!(
                "published manifest check failed: {} (details redacted)",
                response.status()
            ),
        });
    }
    Ok(true)
}

/// What an image runs by itself: its entrypoint and its default command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageConfig {
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    /// The user that the image runs as, like `1000:100` or `nobody`; empty for root.
    pub user: String,
}

impl ImageConfig {
    /// The arguments that a container of the image runs for a task, as `docker run` builds
    /// them: the entrypoint, then the task's arguments, or the image's command when the task
    /// has none.
    pub fn command(&self, arguments: &[String]) -> Vec<String> {
        let rest = if arguments.is_empty() {
            &self.cmd
        } else {
            arguments
        };
        self.entrypoint.iter().chain(rest).cloned().collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedImage {
    pub reference: String,
    pub config: ImageConfig,
}

type ResolvedImages = HashMap<(String, String), Arc<OnceCell<ResolvedImage>>>;

/// The configs of images, each asked of its registry once.
#[derive(Debug, Default)]
pub struct ImageConfigs {
    known: Mutex<ResolvedImages>,
}

impl ImageConfigs {
    /// Configs that are known already, as for images that no registry has.
    pub fn known(configs: impl IntoIterator<Item = (String, ImageConfig)>) -> ImageConfigs {
        let known = (configs.into_iter())
            .map(|(reference, config)| {
                (
                    (reference.clone(), native_platform()),
                    Arc::new(OnceCell::new_with(Some(ResolvedImage {
                        reference,
                        config,
                    }))),
                )
            })
            .collect();
        ImageConfigs {
            known: Mutex::new(known),
        }
    }

    /// The config of an image, from its registry the first time.
    pub async fn get(&self, reference: &str) -> Result<ImageConfig, ExecutorError> {
        self.get_for(reference, None).await
    }

    /// The config for the target platform, or the native Linux platform when omitted.
    pub async fn get_for(
        &self,
        reference: &str,
        platform: Option<&str>,
    ) -> Result<ImageConfig, ExecutorError> {
        Ok(self.resolved_for(reference, platform).await?.config)
    }

    /// The selected manifest reference and config for the target platform.
    pub async fn resolved_for(
        &self,
        reference: &str,
        platform: Option<&str>,
    ) -> Result<ResolvedImage, ExecutorError> {
        let platform = platform.map(str::to_owned).unwrap_or_else(native_platform);
        let cell = {
            let mut known = self
                .known
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            known
                .entry((reference.to_owned(), platform.clone()))
                .or_default()
                .clone()
        };
        let config = cell
            .get_or_try_init(|| resolve_for(reference, Some(&platform)))
            .await?;
        Ok(config.clone())
    }
}

/// The config of an image from its registry for the native Linux platform.
pub async fn config(reference: &str) -> Result<ImageConfig, ExecutorError> {
    config_for(reference, None).await
}

fn native_platform() -> String {
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        architecture => architecture,
    };
    format!("linux/{architecture}")
}

/// The manifest config for a target platform, including an optional architecture variant.
pub async fn config_for(
    reference: &str,
    platform: Option<&str>,
) -> Result<ImageConfig, ExecutorError> {
    Ok(resolve_for(reference, platform).await?.config)
}

/// Resolves an index to the manifest whose metadata and execution match the platform.
pub async fn resolve_for(
    reference: &str,
    platform: Option<&str>,
) -> Result<ResolvedImage, ExecutorError> {
    let wanted = platform.map(str::to_owned).unwrap_or_else(native_platform);
    let mut resolved = reference.to_owned();
    let parsed = Reference::parse(reference);
    let failed = |message: String| ExecutorError::Engine {
        engine: parsed.registry.clone(),
        message,
    };
    let read = |text: String| -> Result<serde_json::Value, ExecutorError> {
        serde_json::from_str(&text).map_err(|error| failed(error.to_string()))
    };
    let text = |response: reqwest::Response| async {
        let text = response.text().await;
        text.map_err(|error| failed(error.to_string()))
    };
    let response = ask(&parsed, Method::GET, &manifest_url(&parsed), MANIFESTS).await?;
    let mut manifest = read(text(response).await?)?;
    if let Some(manifests) = manifest.get("manifests").and_then(|list| list.as_array()) {
        let parts: Vec<&str> = wanted.split('/').collect();
        let selected = manifests.iter().find(|entry| {
            let platform = &entry["platform"];
            (2..=3).contains(&parts.len())
                && platform["os"] == parts[0]
                && platform["architecture"] == parts[1]
                && (parts.len() == 2 || platform["variant"] == parts[2])
        });
        let digest = selected
            .and_then(|entry| entry["digest"].as_str())
            .ok_or_else(|| {
                ExecutorError::Unsupported(format!(
                    "the index of {reference} has no image for {wanted}"
                ))
            })?;
        resolved = format!("{}@{digest}", parsed.name);
        let url = registry_url(&parsed, &format!("manifests/{digest}"));
        let response = ask(&parsed, Method::GET, &url, MANIFESTS).await?;
        manifest = read(text(response).await?)?;
    }
    let digest = manifest["config"]["digest"]
        .as_str()
        .ok_or_else(|| failed(format!("the manifest of {reference} names no config")))?;
    let url = registry_url(&parsed, &format!("blobs/{digest}"));
    let response = ask(&parsed, Method::GET, &url, "*/*").await?;
    let blob = read(text(response).await?)?;
    if platform.is_some() {
        let parts: Vec<&str> = wanted.split('/').collect();
        if !(2..=3).contains(&parts.len())
            || blob["os"] != parts[0]
            || blob["architecture"] != parts[1]
            || (parts.len() == 3 && blob["variant"] != parts[2])
        {
            return Err(ExecutorError::Unsupported(format!(
                "the image {resolved} does not match {wanted}"
            )));
        }
    }
    let words = |value: &serde_json::Value| -> Vec<String> {
        (value.as_array().into_iter().flatten())
            .filter_map(|word| word.as_str().map(str::to_owned))
            .collect()
    };
    Ok(ResolvedImage {
        reference: resolved,
        config: ImageConfig {
            entrypoint: words(&blob["config"]["Entrypoint"]),
            cmd: words(&blob["config"]["Cmd"]),
            user: (blob["config"]["User"].as_str().unwrap_or_default()).to_owned(),
        },
    })
}

/// The address of the manifest of a reference: by its digest when it has one, else its tag.
fn manifest_url(parsed: &Reference) -> String {
    let version = parsed.digest.as_deref().unwrap_or(&parsed.tag);
    registry_url(parsed, &format!("manifests/{version}"))
}

/// An address of the registry API of a repository.
fn registry_url(parsed: &Reference, path: &str) -> String {
    let host = parsed.registry.split(':').next().unwrap_or_default();
    let local = host == "localhost"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|address| address.is_loopback());
    let scheme = if local { "http" } else { "https" };
    format!(
        "{scheme}://{}/v2/{}/{path}",
        parsed.registry, parsed.repository
    )
}

/// A request to a registry, retrying its authentication challenge with local credentials.
async fn ask(
    parsed: &Reference,
    method: Method,
    url: &str,
    accept: &str,
) -> Result<reqwest::Response, ExecutorError> {
    ask_with(parsed, method, url, accept, None).await
}

async fn ask_with(
    parsed: &Reference,
    method: Method,
    url: &str,
    accept: &str,
    credentials: Option<Found>,
) -> Result<reqwest::Response, ExecutorError> {
    let (response, credentials) = request(parsed, method, url, accept, credentials).await?;
    check_response(parsed, response, &credentials)
}

async fn request(
    parsed: &Reference,
    method: Method,
    url: &str,
    accept: &str,
    credentials: Option<Found>,
) -> Result<(reqwest::Response, Found), ExecutorError> {
    let failed = |message: String| ExecutorError::Engine {
        engine: parsed.registry.clone(),
        message,
    };
    let client = crate::http::client().map_err(failed)?;
    let send = |auth: Option<HeaderValue>| {
        let mut request = (client.request(method.clone(), url))
            .header(ACCEPT, accept)
            .timeout(TIMEOUT);
        if let Some(auth) = auth {
            request = request.header(AUTHORIZATION, auth);
        }
        request.send()
    };
    let mut response = send(None)
        .await
        .map_err(|_| failed("registry request failed (details redacted)".into()))?;
    let mut credentials = credentials.unwrap_or_default();
    if response.status() == StatusCode::UNAUTHORIZED {
        let challenge = (response.headers().get(WWW_AUTHENTICATE))
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        if credentials.credential.is_none() && !credentials.helper {
            credentials = crate::credentials::lookup(&parsed.registry).map_err(failed)?;
        }
        let scheme = challenge.split_whitespace().next().unwrap_or_default();
        let auth = if scheme.eq_ignore_ascii_case("Bearer") {
            let token = token(client, &challenge, credentials.credential.as_ref())
                .await
                .map_err(|message| failed(format!("{message}{}", credentials.hint())))?;
            Some(bearer(&token).map_err(failed)?)
        } else if scheme.eq_ignore_ascii_case("Basic") {
            match &credentials.credential {
                Some(Credential::Basic(user, password)) => Some(basic(user, password)),
                _ => None,
            }
        } else {
            None
        };
        if auth.is_some() {
            response = send(auth).await.map_err(|_| {
                failed("authenticated registry request failed (details redacted)".into())
            })?;
        }
    }
    Ok((response, credentials))
}

fn check_response(
    parsed: &Reference,
    response: reqwest::Response,
    credentials: &Found,
) -> Result<reqwest::Response, ExecutorError> {
    let failed = |message| ExecutorError::Engine {
        engine: parsed.registry.clone(),
        message,
    };
    let version = match &parsed.digest {
        Some(digest) => format!("@{digest}"),
        None => format!(":{}", parsed.tag),
    };
    let status = response.status();
    let absent = [
        StatusCode::NOT_FOUND,
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
    ];
    if absent.contains(&status) {
        // Another attempt gives the same answer.
        return Err(ExecutorError::MissingImage {
            image: format!("{}{version}", parsed.name),
            reason: format!(
                "the registry {} answers {status}{}",
                parsed.registry,
                if status == StatusCode::NOT_FOUND {
                    ""
                } else {
                    credentials.hint()
                }
            ),
        });
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        // Docker Hub limits the manifests that one address may ask for per hour.
        return Err(failed(format!(
            "the registry limits the requests of this address ({status}) and answered none \
             for {}{version}; its limit resets within the hour",
            parsed.name
        )));
    }
    if !status.is_success() {
        return Err(failed(format!(
            "the registry has no image {}{version}: {status}",
            parsed.name
        )));
    }
    Ok(response)
}

/// Exchanges Basic credentials or an identity refresh token for a registry access token.
async fn token(
    client: &reqwest::Client,
    challenge: &str,
    credential: Option<&Credential>,
) -> Result<String, String> {
    if let Some(Credential::Bearer(token)) = credential {
        return Ok(token.clone());
    }
    let fields = challenge.split_once(' ').map_or("", |(_, fields)| fields);
    let field = |name: &str| {
        fields.split(',').find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then(|| value.trim_matches('"').to_owned())
        })
    };
    let realm = field("realm").ok_or_else(|| "registry challenge has no token realm".to_owned())?;
    let mut url =
        reqwest::Url::parse(&realm).map_err(|_| "invalid token realm (redacted)".to_owned())?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if (url.scheme() != "https" && !(url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("token realm must use HTTPS or loopback HTTP without URL credentials".into());
    }
    let mut fields = Vec::new();
    for name in ["service", "scope"] {
        if let Some(value) = field(name) {
            fields.push((name, value));
        }
    }
    let request = match credential {
        Some(Credential::Identity(token)) => {
            fields.extend([
                ("grant_type", "refresh_token".into()),
                ("refresh_token", token.clone()),
                ("client_id", "iunctus".into()),
            ]);
            let body = fields
                .iter()
                .map(|(name, value)| format!("{name}={}", encode(value)))
                .collect::<Vec<_>>()
                .join("&");
            client
                .post(url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body)
        }
        credential => {
            url.query_pairs_mut().extend_pairs(&fields);
            let request = client.get(url);
            match credential {
                Some(Credential::Basic(user, password)) => {
                    request.header(AUTHORIZATION, basic(user, password))
                }
                _ => request,
            }
        }
    };
    let response = request
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|_| "registry token request failed (details redacted)".to_owned())?;
    if !response.status().is_success() {
        return Err(format!(
            "registry token exchange returned {} (body redacted)",
            response.status()
        ));
    }
    let text = response
        .text()
        .await
        .map_err(|_| "cannot read registry token response".to_owned())?;
    let answer: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| "invalid registry token response (body redacted)".to_owned())?;
    let token = answer.get("token").or_else(|| answer.get("access_token"));
    token
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "the registry gave no token".to_owned())
}

fn basic(user: &str, password: &str) -> HeaderValue {
    use base64::Engine;
    let encoded = base64::prelude::BASE64_STANDARD.encode(format!("{user}:{password}"));
    let mut header = HeaderValue::from_str(&format!("Basic {encoded}")).expect("base64 header");
    header.set_sensitive(true);
    header
}

fn bearer(token: &str) -> Result<HeaderValue, String> {
    let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| "invalid registry token (redacted)".to_owned())?;
    header.set_sensitive(true);
    Ok(header)
}

/// A value for a query of a URL, with the characters that have a meaning there encoded.
fn encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b':' | b'/' => {
                encoded.push(char::from(byte))
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
#[path = "registry_auth_tests.rs"]
mod auth_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confines_registry_http() {
        for host in ["localhost:5000", "127.0.0.1:5000", "127.0.0.2:5000"] {
            let parsed = Reference::parse(&format!("{host}/tool:1"));
            assert!(manifest_url(&parsed).starts_with("http://"), "{host}");
        }
        for host in ["localhost.example.org", "127.example.org", "192.0.2.1:5000"] {
            let parsed = Reference::parse(&format!("{host}/tool:1"));
            assert!(manifest_url(&parsed).starts_with("https://"), "{host}");
        }
    }

    #[test]
    fn parses_image_references() {
        let parsed = Reference::parse("debian:stable-slim");
        assert_eq!(parsed.registry, "registry-1.docker.io");
        assert_eq!(parsed.repository, "library/debian");
        assert_eq!(parsed.tag, "stable-slim");
        assert_eq!(parsed.name, "debian");
        let parsed = Reference::parse("quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0");
        assert_eq!(parsed.registry, "quay.io");
        assert_eq!(parsed.repository, "biocontainers/fastqc");
        assert_eq!(parsed.tag, "0.12.1--hdfd78af_0");
        let parsed = Reference::parse("localhost:5000/tool");
        assert_eq!(
            (parsed.registry.as_str(), parsed.tag.as_str()),
            ("localhost:5000", "latest")
        );
        for name in [
            "docker.io/python:3-slim",
            "index.docker.io/library/python:3-slim",
        ] {
            let parsed = Reference::parse(name);
            assert_eq!(parsed.registry, "registry-1.docker.io", "{name}");
            assert_eq!(parsed.repository, "library/python", "{name}");
        }
        let parsed = Reference::parse("docker.io/nfcore/tool:1");
        assert_eq!(parsed.repository, "nfcore/tool");
        let parsed = Reference::parse("nfcore/tool@sha256:ab");
        assert_eq!(parsed.digest.as_deref(), Some("sha256:ab"));
        assert_eq!(parsed.repository, "nfcore/tool");
    }

    /// A registry on a local port with one image `tool:1` in an index of two platforms. It
    /// answers each request with the document of its path, and asks for `tool:3` are over its
    /// limit.
    fn registry() -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("an address").port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
                let mut line = String::new();
                reader.read_line(&mut line).expect("a request");
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                    line.clear();
                }
                let body = match path.as_str() {
                    "/v2/tool/manifests/1" => {
                        r#"{"manifests": [{"digest": "sha256:aa", "platform": {"os": "linux", "architecture": "arm64"}}, {"digest": "sha256:bb", "platform": {"os": "linux", "architecture": "amd64"}}]}"#
                    }
                    "/v2/tool/manifests/sha256:bb" => r#"{"config": {"digest": "sha256:cc"}}"#,
                    "/v2/tool/manifests/sha256:aa" => r#"{"config": {"digest": "sha256:dd"}}"#,
                    "/v2/tool/blobs/sha256:cc" => {
                        r#"{"os":"linux","architecture":"amd64","config": {"Entrypoint": ["/usr/local/env-execute"], "Cmd": ["bash"], "User": "1000"}}"#
                    }
                    "/v2/tool/blobs/sha256:dd" => {
                        r#"{"os":"linux","architecture":"arm64","config": {"Entrypoint": ["/arm-entry"], "Cmd": ["arm-command"], "User": "2000"}}"#
                    }
                    _ => "",
                };
                let status = match (path.as_str(), body.is_empty()) {
                    ("/v2/tool/manifests/3", _) => "429 Too Many Requests",
                    (_, true) => "404 Not Found",
                    (_, false) => "200 OK",
                };
                let mut stream = stream;
                let answer = format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        format!("localhost:{port}")
    }

    #[tokio::test]
    async fn rejects_image_schemes() {
        let address = "https://depot.galaxyproject.org/singularity/fastqc:0.12.1--hdfd78af_0";
        assert_eq!(pin(address).await.expect("no request"), None);
    }

    #[tokio::test]
    async fn fetches_registry_configs() {
        let registry = registry();
        let configs = ImageConfigs::default();
        let reference = format!("{registry}/tool:1");
        let config = configs
            .get_for(&reference, Some("linux/amd64"))
            .await
            .expect("a config");
        assert_eq!(config.entrypoint, ["/usr/local/env-execute"]);
        assert_eq!(config.cmd, ["bash"]);
        assert_eq!(config.user, "1000");
        let arm = configs
            .get_for(&reference, Some("linux/arm64"))
            .await
            .unwrap();
        assert_eq!(arm.entrypoint, ["/arm-entry"]);
        assert_eq!(arm.cmd, ["arm-command"]);
        assert_eq!(arm.user, "2000");
        let resolved = configs
            .resolved_for(&reference, Some("linux/arm64"))
            .await
            .unwrap();
        assert_eq!(resolved.reference, format!("{registry}/tool@sha256:aa"));
        assert_eq!(resolved.config, arm);
        let wrong = config_for(&format!("{registry}/tool@sha256:bb"), Some("linux/arm64"))
            .await
            .unwrap_err();
        assert!(matches!(wrong, ExecutorError::Unsupported(_)), "{wrong}");
        assert_eq!(
            configs
                .get_for(&reference, Some("linux/amd64"))
                .await
                .unwrap(),
            config
        );
        let error = configs
            .get_for(&reference, Some("linux/ppc64le"))
            .await
            .unwrap_err();
        assert!(matches!(error, ExecutorError::Unsupported(_)), "{error}");
        assert!(!error.retryable());
        // The entrypoint runs the task's arguments, or the image's command when it has none.
        let arguments = ["bash".to_owned(), "-c".to_owned(), "true".to_owned()];
        let command = config.command(&arguments);
        assert_eq!(command, ["/usr/local/env-execute", "bash", "-c", "true"]);
        assert_eq!(config.command(&[]), ["/usr/local/env-execute", "bash"]);
        let missing = config_error(&format!("{registry}/tool:2")).await;
        assert!(missing.contains("cannot be pulled"), "{missing}");
        // A registry over its limit says so; the image may well be there.
        let limited = config_error(&format!("{registry}/tool:3")).await;
        assert!(
            limited.contains("limits the requests of this address"),
            "{limited}"
        );
        // Known configs need no registry.
        let known = ImageConfigs::known([("x@sha256:dd".to_owned(), ImageConfig::default())]);
        assert_eq!(
            known.get("x@sha256:dd").await.expect("known"),
            ImageConfig::default()
        );
    }

    async fn config_error(reference: &str) -> String {
        config(reference).await.expect_err("an error").to_string()
    }

    #[tokio::test]
    #[ignore = "needs the network"]
    async fn registries_give_digests() {
        for image in [
            "debian:stable-slim",
            "quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0",
        ] {
            let pinned = pin(image).await.expect("pins").expect("a digest");
            assert!(pinned.contains("@sha256:"), "{pinned}");
        }
    }
}
