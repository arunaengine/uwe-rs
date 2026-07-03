//! Image digests from their registry, for backends without a local container engine, like TES
//! or a cluster: a `HEAD` request for the manifest of the tag gives the digest, as `docker
//! pull` finds it. Registries that ask for a token, like Docker Hub, quay.io and ghcr.io, give
//! an anonymous one for public images.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, WWW_AUTHENTICATE};

use crate::job::ExecutorError;

/// The manifest types a registry may answer with, as `docker pull` asks for them.
const MANIFESTS: &str = "application/vnd.oci.image.index.v1+json, \
     application/vnd.docker.distribution.manifest.list.v2+json, \
     application/vnd.oci.image.manifest.v1+json, \
     application/vnd.docker.distribution.manifest.v2+json";

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
        let (registry, repository) = match own {
            true => {
                let (host, path) = name.split_once('/').unwrap_or((name, ""));
                (host.to_owned(), path.to_owned())
            }
            false if name.contains('/') => ("registry-1.docker.io".to_owned(), name.to_owned()),
            false => ("registry-1.docker.io".to_owned(), format!("library/{name}")),
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
    let parsed = Reference::parse(reference);
    if parsed.digest.is_some() {
        return Ok(Some(reference.to_owned()));
    }
    let failed = |message: String| ExecutorError::Engine {
        engine: parsed.registry.clone(),
        message,
    };
    let client = reqwest::Client::builder()
        .user_agent(concat!("iunctus/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|error| failed(error.to_string()))?;
    let url = format!(
        "https://{}/v2/{}/manifests/{}",
        parsed.registry, parsed.repository, parsed.tag
    );
    let head = |token: Option<String>| {
        let mut request = client.head(&url).header(ACCEPT, MANIFESTS);
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        request.send()
    };
    let mut response = head(None)
        .await
        .map_err(|error| failed(error.to_string()))?;
    if response.status() == StatusCode::UNAUTHORIZED {
        let challenge = (response.headers().get(WWW_AUTHENTICATE))
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let token = token(&client, &challenge).await.map_err(failed)?;
        response = head(Some(token))
            .await
            .map_err(|error| failed(error.to_string()))?;
    }
    if !response.status().is_success() {
        return Err(failed(format!(
            "the registry has no image {reference}: {}",
            response.status()
        )));
    }
    let digest = (response.headers().get("docker-content-digest"))
        .and_then(|value| value.to_str().ok())
        .filter(|digest| digest.starts_with("sha256:"));
    Ok(digest.map(|digest| format!("{}@{digest}", parsed.name)))
}

/// An anonymous token for the challenge of a registry, like
/// `Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="..."`.
async fn token(client: &reqwest::Client, challenge: &str) -> Result<String, String> {
    let fields = challenge.strip_prefix("Bearer ").unwrap_or(challenge);
    let field = |name: &str| {
        fields.split(',').find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then(|| value.trim_matches('"').to_owned())
        })
    };
    let realm = field("realm").ok_or_else(|| format!("unexpected challenge: {challenge}"))?;
    let mut url = realm;
    for (index, name) in ["service", "scope"].into_iter().enumerate() {
        if let Some(value) = field(name) {
            let separator = if index == 0 && !url.contains('?') {
                '?'
            } else {
                '&'
            };
            url.push_str(&format!("{separator}{name}={}", encode(&value)));
        }
    }
    let text = (client.get(&url).send().await)
        .and_then(|response| response.error_for_status())
        .map_err(|error| error.to_string())?
        .text()
        .await
        .map_err(|error| error.to_string())?;
    let answer: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| error.to_string())?;
    let token = answer.get("token").or_else(|| answer.get("access_token"));
    token
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "the registry gave no token".to_owned())
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
mod tests {
    use super::*;

    #[test]
    fn references_name_registry_repository_and_tag() {
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
        let parsed = Reference::parse("nfcore/tool@sha256:ab");
        assert_eq!(parsed.digest.as_deref(), Some("sha256:ab"));
        assert_eq!(parsed.repository, "nfcore/tool");
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
