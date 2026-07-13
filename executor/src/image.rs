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
            return Err(ExecutorError::Engine {
                engine: engine.name.clone(),
                message: format!("cannot pull {reference}: {error}"),
            });
        }
    }
    Ok(())
}

/// The default command of an image: its entrypoint, then its command.
pub(crate) async fn default_command(
    engine: &ContainerEngine,
    reference: &str,
) -> Result<Vec<String>, ExecutorError> {
    let config = inspect(engine, reference).await?.config.unwrap_or_default();
    let mut command = config.entrypoint.unwrap_or_default();
    command.extend(config.cmd.unwrap_or_default());
    if command.is_empty() {
        return Err(ExecutorError::NoCommand(reference.to_owned()));
    }
    Ok(command)
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
