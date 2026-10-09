//! What the container engine knows about images: their digests and default commands.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use bollard::errors::Error as ApiError;
use bollard::models::ImageInspect;
use bollard::query_parameters::CreateImageOptions;
use futures_util::StreamExt;

use crate::engine::ContainerEngine;
use crate::job::ExecutorError;

/// The engine operation that creates a locally available image from an archive.
#[derive(Clone, Copy, Debug)]
pub enum ImageSource {
    Build,
    Load,
    Import,
}

impl ContainerEngine {
    /// Prepares an image under a content-addressed tag and returns its immutable engine ID.
    pub async fn prepare_image(
        &self,
        source: ImageSource,
        archive: &std::path::Path,
        tag: &str,
        expected: Option<&str>,
        cancel: &crate::CancellationToken,
    ) -> Result<String, ExecutorError> {
        tokio::select! {
            () = cancel.cancelled() => Err(ExecutorError::Cancelled),
            result = tokio::time::timeout(std::time::Duration::from_secs(600), async {
                match inspect(self, tag).await {
                    Ok(image) => return image.id.ok_or_else(|| self.image_error("cached image has no ID")),
                    Err(ExecutorError::Engine { .. }) => {},
                    Err(error) => return Err(error),
                }
                let file = tokio::fs::File::open(archive).await?;
                let body = bollard::body_try_stream(tokio_util::io::ReaderStream::new(file));
                let mut loaded = Vec::new();
                match source {
                    ImageSource::Build => {
                        let options = bollard::query_parameters::BuildImageOptions {
                            t: Some(tag.to_owned()), rm: true, forcerm: true,
                            pull: Some("false".into()),
                            ..Default::default()
                        };
                        let mut stream = self.client().build_image(options, None, Some(body));
                        while let Some(step) = stream.next().await {
                            let step = step.map_err(|error| failed(self, error))?;
                            if let Some(error) = step.error_detail.and_then(|detail| detail.message) {
                                return Err(self.image_error(&error));
                            }
                        }
                    }
                    ImageSource::Load => {
                        let mut stream = self.client().import_image(
                            bollard::query_parameters::ImportImageOptions::default(), body, None);
                        while let Some(step) = stream.next().await {
                            let step = step.map_err(|error| failed(self, error))?;
                            if let Some(error) = step.error_detail.and_then(|detail| detail.message) {
                                return Err(self.image_error(&error));
                            }
                            if let Some(text) = step.stream {
                                for line in text.lines() {
                                    if let Some(image) = line.strip_prefix("Loaded image: ")
                                        .or_else(|| line.strip_prefix("Loaded image ID: ")) {
                                        loaded.push(image.to_owned());
                                    }
                                }
                            }
                        }
                    }
                    ImageSource::Import => {
                        let (repo, tag) = tag.rsplit_once(':').ok_or_else(|| self.image_error("image tag is missing"))?;
                        let options = CreateImageOptions {
                            from_src: Some("-".into()), repo: Some(repo.into()), tag: Some(tag.into()),
                            ..Default::default()
                        };
                        let mut stream = self.client().create_image(Some(options), Some(body), None);
                        while let Some(step) = stream.next().await {
                            let step = step.map_err(|error| failed(self, error))?;
                            if let Some(error) = step.error_detail.and_then(|detail| detail.message) {
                                return Err(self.image_error(&error));
                            }
                        }
                    }
                }
                let id = if matches!(source, ImageSource::Load) {
                    let mut identities = std::collections::BTreeSet::new();
                    for reference in loaded {
                        let image = inspect(self, &reference).await?;
                        identities.insert(image.id.ok_or_else(|| self.image_error("loaded image has no ID"))?);
                    }
                    let id = match expected {
                        Some(expected) => inspect(self, expected).await?.id
                            .ok_or_else(|| self.image_error("requested image has no ID"))?,
                        None if identities.len() == 1 => identities.first().unwrap().clone(),
                        None => return Err(self.image_error("dockerLoad needs dockerImageId for an archive with multiple images")),
                    };
                    if !identities.contains(&id) {
                        return Err(self.image_error("dockerLoad did not contain dockerImageId"));
                    }
                    self.tag_image(&id, tag).await?;
                    id
                } else {
                    inspect(self, tag).await?.id.ok_or_else(|| self.image_error("prepared image has no ID"))?
                };
                self.tag_image(&id, &format!("iunctus-cwl:{}", id.trim_start_matches("sha256:"))).await?;
                Ok(id)
            }) => result.unwrap_or_else(|_| Err(self.image_error("image preparation exceeded 600 seconds"))),
        }
    }

    async fn tag_image(&self, id: &str, reference: &str) -> Result<(), ExecutorError> {
        let (repo, tag) = reference
            .rsplit_once(':')
            .ok_or_else(|| self.image_error("image tag is missing"))?;
        self.client()
            .tag_image(
                id,
                Some(bollard::query_parameters::TagImageOptions {
                    repo: Some(repo.into()),
                    tag: Some(tag.into()),
                }),
            )
            .await
            .map_err(|error| failed(self, error))
    }

    fn image_error(&self, message: &str) -> ExecutorError {
        ExecutorError::Engine {
            engine: self.name.clone(),
            message: message.to_owned(),
        }
    }
}

/// The image pinned to its digest, like `debian@sha256:...`, pulling it first when it is not
/// local. `None` for an image without a registry digest, such as one built locally.
pub(crate) async fn pin(
    engine: &ContainerEngine,
    reference: &str,
) -> Result<Option<String>, ExecutorError> {
    let image = available(engine, reference).await?;
    if reference.contains("@sha256:") {
        return Ok(Some(reference.to_owned()));
    }
    Ok(pinned(reference, &image.repo_digests.unwrap_or_default()))
}

pub(crate) async fn available(
    engine: &ContainerEngine,
    reference: &str,
) -> Result<ImageInspect, ExecutorError> {
    available_for(engine, reference, None).await
}

pub(crate) async fn available_for(
    engine: &ContainerEngine,
    reference: &str,
    platform: Option<&str>,
) -> Result<ImageInspect, ExecutorError> {
    if let Ok(image) = inspect(engine, reference).await
        && platform.is_none_or(|platform| matches_platform(&image, platform))
    {
        return Ok(image);
    }
    let reference = match platform {
        Some(platform) => {
            crate::registry::resolve_for(reference, Some(platform))
                .await?
                .reference
        }
        None => reference.to_owned(),
    };
    pull(engine, &reference, platform).await?;
    let image = inspect(engine, &reference).await?;
    if let Some(platform) = platform
        && !matches_platform(&image, platform)
    {
        return Err(ExecutorError::Unsupported(format!(
            "the engine's image {reference} does not match {platform}"
        )));
    }
    Ok(image)
}

fn matches_platform(image: &ImageInspect, platform: &str) -> bool {
    let parts: Vec<&str> = platform.split('/').collect();
    (2..=3).contains(&parts.len())
        && image.os.as_deref() == Some(parts[0])
        && image.architecture.as_deref() == Some(parts[1])
        && (parts.len() == 2 || image.variant.as_deref() == Some(parts[2]))
}

async fn inspect(engine: &ContainerEngine, reference: &str) -> Result<ImageInspect, ExecutorError> {
    let client = engine.client();
    (client.inspect_image(reference).await).map_err(|error| failed(engine, error))
}

/// Pulls an image, as `docker pull` does.
async fn pull(
    engine: &ContainerEngine,
    reference: &str,
    platform: Option<&str>,
) -> Result<(), ExecutorError> {
    let options = CreateImageOptions {
        from_image: Some(reference.to_owned()),
        platform: platform.unwrap_or_default().to_owned(),
        ..CreateImageOptions::default()
    };
    let mut progress = engine.client().create_image(Some(options), None, None);
    while let Some(step) = progress.next().await {
        let step = step.map_err(|error| failed(engine, error))?;
        if let Some(error) = step.error_detail.and_then(|detail| detail.message) {
            if missing(&error) {
                return Err(ExecutorError::MissingImage {
                    image: reference.to_owned(),
                    reason: error,
                });
            }
            return Err(ExecutorError::Engine {
                engine: engine.name.clone(),
                message: format!("cannot pull {reference}: {error}"),
            });
        }
    }
    Ok(())
}

/// What a container of an image runs by default: its entrypoint and its command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Commands {
    entrypoint: Vec<String>,
    command: Vec<String>,
    /// The user that the image runs as, like `1000:100` or `nobody`; empty for root.
    pub(crate) user: String,
}

impl Commands {
    /// Asks the engine about an image.
    pub(crate) async fn of(
        engine: &ContainerEngine,
        reference: &str,
        platform: Option<&str>,
    ) -> Result<Commands, ExecutorError> {
        let image = match platform {
            None => inspect(engine, reference).await?,
            Some(_) => available_for(engine, reference, platform).await?,
        };
        let config = image.config.unwrap_or_default();
        Ok(Commands {
            entrypoint: config.entrypoint.unwrap_or_default(),
            command: config.cmd.unwrap_or_default(),
            user: config.user.unwrap_or_default(),
        })
    }

    /// What a container of the image runs for a task: its arguments through the entrypoint,
    /// as `docker run` runs them, or, for a task without arguments, the default command of
    /// the image, its entrypoint and then its command.
    pub(crate) fn arguments(
        &self,
        reference: &str,
        arguments: Vec<String>,
    ) -> Result<Vec<String>, ExecutorError> {
        let arguments = match arguments.is_empty() {
            true => self.command.clone(),
            false => arguments,
        };
        let command: Vec<String> = (self.entrypoint.iter().cloned()).chain(arguments).collect();
        if command.is_empty() {
            return Err(ExecutorError::NoCommand(reference.to_owned()));
        }
        Ok(command)
    }
}

/// The reference with the digest of its repository in the image's `RepoDigests`.
fn pinned(reference: &str, digests: &[String]) -> Option<String> {
    let parsed = crate::registry::Reference::parse(reference);
    let (_, hash) = digests
        .iter()
        .filter_map(|digest| digest.split_once('@'))
        .find(|(name, hash)| {
            let candidate = crate::registry::Reference::parse(name);
            hash.starts_with("sha256:")
                && candidate.registry == parsed.registry
                && candidate.repository == parsed.repository
        })?;
    Some(format!("{}@{hash}", parsed.name))
}

/// Whether the answer of a pull says that the registry has no such image, or does not let
/// this machine have it, rather than that the network failed. Docker and Podman give such
/// answers as text, like `manifest unknown` or `pull access denied`.
fn missing(answer: &str) -> bool {
    let answer = answer.to_lowercase();
    [
        "manifest unknown",
        "not found",
        "does not exist",
        "access denied",
        "unauthorized",
    ]
    .iter()
    .any(|words| answer.contains(words))
}

/// A request to the engine failed; another attempt may work.
pub(crate) fn failed(engine: &ContainerEngine, error: ApiError) -> ExecutorError {
    ExecutorError::Engine {
        engine: engine.name.clone(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_requested_platform() {
        let image = ImageInspect {
            os: Some("linux".into()),
            architecture: Some("arm".into()),
            variant: Some("v7".into()),
            ..ImageInspect::default()
        };
        assert!(matches_platform(&image, "linux/arm/v7"));
        assert!(matches_platform(&image, "linux/arm"));
        assert!(!matches_platform(&image, "linux/arm/v6"));
        assert!(!matches_platform(&image, "linux/amd64"));
        assert!(!matches_platform(&image, "arm"));
    }

    #[tokio::test]
    async fn pulls_pinned_reference() {
        use std::io::{BufRead, BufReader, Write};
        let folder = tempfile::tempdir().unwrap();
        let socket = folder.path().join("engine.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            let mut pulled = false;
            for stream in listener.incoming().take(8) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap().to_owned();
                requests.push(path.clone());
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                let (status, body) = if path.contains("/images/create?") {
                    pulled = true;
                    ("200 OK", "{\"status\":\"pulled\"}\n")
                } else if path.contains("/images/") && !pulled {
                    ("404 Not Found", "{\"message\":\"missing\"}")
                } else if path.contains("/images/") {
                    (
                        "200 OK",
                        "{\"Id\":\"sha256:arm\",\"Os\":\"linux\",\"Architecture\":\"arm64\",\"RepoDigests\":[\"example/tool@sha256:ab\"],\"Config\":{\"Cmd\":[\"arm-command\"],\"User\":\"2000\"}}",
                    )
                } else {
                    ("200 OK", "{}")
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            requests
        });
        let engine = ContainerEngine::connect(socket.to_str().unwrap())
            .await
            .unwrap();
        let reference = "example/tool@sha256:ab";
        assert!(Commands::of(&engine, reference, None).await.is_err());
        assert_eq!(
            pin(&engine, reference).await.unwrap().as_deref(),
            Some(reference)
        );
        let commands = Commands::of(&engine, reference, Some("linux/arm64"))
            .await
            .unwrap();
        assert_eq!(commands.user, "2000");
        assert_eq!(commands.command, ["arm-command"]);
        pull(&engine, reference, Some("linux/arm64")).await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|path| path.contains("/images/create?"))
                .count(),
            2
        );
        assert!(
            requests[4].contains("example%2Ftool%40sha256%3Aab"),
            "{requests:?}"
        );
        assert!(
            requests[7].contains("platform=linux%2Farm64"),
            "{requests:?}"
        );
    }

    #[test]
    fn avoids_missing_retries() {
        assert!(missing(
            "manifest for debian:nosuchtag not found: manifest unknown: manifest unknown"
        ));
        assert!(missing(
            "pull access denied for nosuchimage, repository does not exist or may require \
             'docker login'"
        ));
        assert!(!missing(
            "Get \"https://registry-1.docker.io/v2/\": dial tcp: lookup registry-1.docker.io: \
             i/o timeout"
        ));
    }

    #[test]
    fn uses_image_command() {
        let words =
            |words: &[&str]| -> Vec<String> { words.iter().map(|w| (*w).to_owned()).collect() };
        let commands = Commands {
            entrypoint: words(&["/entry.sh"]),
            command: words(&["serve"]),
            user: String::new(),
        };
        let given = commands
            .arguments("tool", words(&["-c", "x"]))
            .expect("a command");
        assert_eq!(given, ["/entry.sh", "-c", "x"]);
        let default = commands.arguments("tool", Vec::new()).expect("a command");
        assert_eq!(default, ["/entry.sh", "serve"]);
        let none = Commands::default()
            .arguments("tool", Vec::new())
            .unwrap_err();
        assert!(matches!(none, ExecutorError::NoCommand(_)), "{none}");
    }

    #[test]
    fn preserves_digest_repository() {
        let digests = [
            "localhost:5000/tool@sha256:cd".to_owned(),
            "docker.io/library/debian@sha256:ab".to_owned(),
        ];
        assert_eq!(
            pinned("debian:stable-slim", &digests).as_deref(),
            Some("debian@sha256:ab")
        );
        assert_eq!(
            pinned("localhost:5000/tool", &digests).as_deref(),
            Some("localhost:5000/tool@sha256:cd")
        );
        assert_eq!(
            pinned("localhost:5000/tool", &["x@sha256:cd".to_owned()]),
            None
        );
        assert_eq!(
            pinned("index.docker.io/debian:latest", &digests).as_deref(),
            Some("index.docker.io/debian@sha256:ab")
        );
        assert_eq!(pinned("built", &[]), None);
    }
}
