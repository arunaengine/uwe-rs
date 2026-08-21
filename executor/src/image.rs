//! What the container engine knows about images: their digests and default commands.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use bollard::errors::Error as ApiError;
use bollard::models::ImageInspect;
use bollard::query_parameters::CreateImageOptions;
use futures_util::StreamExt;

use crate::engine::ContainerEngine;
use crate::job::ExecutorError;

/// The image pinned to its digest, like `debian@sha256:...`, pulling it first when it is not
/// local. `None` for an image without a registry digest, such as one built locally.
pub(crate) async fn pin(
    engine: &ContainerEngine,
    reference: &str,
) -> Result<Option<String>, ExecutorError> {
    if reference.contains("@sha256:") {
        return Ok(Some(reference.to_owned()));
    }
    let image = match inspect(engine, reference).await {
        Ok(image) => image,
        Err(_) => {
            pull(engine, reference).await?;
            inspect(engine, reference).await?
        }
    };
    Ok(pinned(reference, &image.repo_digests.unwrap_or_default()))
}

async fn inspect(engine: &ContainerEngine, reference: &str) -> Result<ImageInspect, ExecutorError> {
    let client = engine.client();
    (client.inspect_image(reference).await).map_err(|error| failed(engine, error))
}

/// Pulls an image, as `docker pull` does.
async fn pull(engine: &ContainerEngine, reference: &str) -> Result<(), ExecutorError> {
    let options = CreateImageOptions {
        from_image: Some(reference.to_owned()),
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
    ) -> Result<Commands, ExecutorError> {
        let config = inspect(engine, reference).await?.config.unwrap_or_default();
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

/// The reference with the first digest of the image's `RepoDigests`, which look like
/// `debian@sha256:...`; the repository of the reference is kept.
fn pinned(reference: &str, digests: &[String]) -> Option<String> {
    let first = digests
        .first()
        .filter(|digest| digest.contains("@sha256:"))?;
    let hash = first
        .split_once('@')
        .map_or(first.as_str(), |(_, hash)| hash);
    let name = reference
        .rsplit_once(':')
        .filter(|(_, tag)| !tag.contains('/'))
        .map_or(reference, |(name, _)| name);
    Some(format!("{name}@{hash}"))
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
    fn answers_of_registries_without_the_image_are_not_retried() {
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
    fn tasks_run_through_the_entrypoint_or_run_the_default_command() {
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
    fn digests_keep_the_repository() {
        let digests = ["docker.io/library/debian@sha256:ab".to_owned()];
        assert_eq!(
            pinned("debian:stable-slim", &digests).as_deref(),
            Some("debian@sha256:ab")
        );
        assert_eq!(
            pinned("localhost:5000/tool", &["x@sha256:cd".to_owned()]).as_deref(),
            Some("localhost:5000/tool@sha256:cd")
        );
        assert_eq!(pinned("built", &[]), None);
    }
}
