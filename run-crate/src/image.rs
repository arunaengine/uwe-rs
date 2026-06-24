//! A container image reference split into the parts that a `ContainerImage` entity names.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// The parts of an image reference like `quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0` or
/// `debian:stable-slim@sha256:<hex>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageReference {
    /// The service that hosts the image, `docker.io` when the reference names none.
    pub registry: String,
    /// The name inside the registry, with `library/` for the official images of Docker Hub.
    pub name: String,
    pub tag: Option<String>,
    /// The hexadecimal SHA-256 digest of the image, when the reference is pinned.
    pub sha256: Option<String>,
}

impl ImageReference {
    /// Splits a reference the way Docker reads it: the first part is a registry when it has a
    /// dot or a port, or is `localhost`.
    pub(crate) fn parse(reference: &str) -> Self {
        let (rest, sha256) = match reference.split_once("@sha256:") {
            Some((rest, hex)) => (rest, Some(hex.to_owned())),
            None => (reference, None),
        };
        let (rest, tag) = match rest.rsplit_once(':') {
            Some((name, tag)) if !tag.contains('/') => (name, Some(tag.to_owned())),
            _ => (rest, None),
        };
        let (registry, name) = match rest.split_once('/') {
            Some((first, name)) if first.contains(['.', ':']) || first == "localhost" => {
                (first.to_owned(), name.to_owned())
            }
            Some(_) => ("docker.io".to_owned(), rest.to_owned()),
            None => ("docker.io".to_owned(), format!("library/{rest}")),
        };
        ImageReference {
            registry,
            name,
            tag,
            sha256,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_are_split_like_docker_does() {
        let image = ImageReference::parse("quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0");
        assert_eq!(image.registry, "quay.io");
        assert_eq!(image.name, "biocontainers/fastqc");
        assert_eq!(image.tag.as_deref(), Some("0.12.1--hdfd78af_0"));
        assert_eq!(image.sha256, None);
        let pinned = ImageReference::parse("debian:stable-slim@sha256:abc123");
        assert_eq!(
            pinned,
            ImageReference {
                registry: "docker.io".into(),
                name: "library/debian".into(),
                tag: Some("stable-slim".into()),
                sha256: Some("abc123".into()),
            }
        );
        let local = ImageReference::parse("localhost:5000/tools/bwa");
        assert_eq!(
            (local.registry.as_str(), local.name.as_str()),
            ("localhost:5000", "tools/bwa")
        );
        assert_eq!(local.tag, None);
        let user = ImageReference::parse("biocontainers/samtools:v1.9");
        assert_eq!(
            (user.registry.as_str(), user.name.as_str()),
            ("docker.io", "biocontainers/samtools")
        );
    }
}
