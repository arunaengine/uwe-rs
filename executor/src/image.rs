//! What the container engine knows about images: their digests and default commands.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::process::Output;

use tokio::process::Command;

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
    let inspect = || {
        Command::new(&engine.program)
            .args(["image", "inspect", "--format", "{{json .RepoDigests}}"])
            .arg(reference)
            .output()
    };
    let mut output = inspect().await.map_err(|error| failed(engine, error))?;
    if !output.status.success() {
        let pull = Command::new(&engine.program)
            .args(["pull", "--quiet", reference])
            .output()
            .await
            .map_err(|error| failed(engine, error))?;
        if !pull.status.success() {
            return Err(engine_error(engine, &pull));
        }
        output = inspect().await.map_err(|error| failed(engine, error))?;
    }
    Ok(pinned(reference, &String::from_utf8_lossy(&output.stdout)))
}

/// The default command of an image: its entrypoint, then its command.
pub(crate) async fn default_command(
    engine: &ContainerEngine,
    reference: &str,
) -> Result<Vec<String>, ExecutorError> {
    let format = "{{json .Config.Entrypoint}}\n{{json .Config.Cmd}}";
    let output = Command::new(&engine.program)
        .args(["image", "inspect", "--format", format, reference])
        .output()
        .await
        .map_err(|error| failed(engine, error))?;
    if !output.status.success() {
        return Err(engine_error(engine, &output));
    }
    let mut command = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let part: Option<Vec<String>> =
            serde_json::from_str(line).map_err(|error| ExecutorError::Engine {
                engine: engine.program.clone(),
                message: error.to_string(),
            })?;
        command.extend(part.unwrap_or_default());
    }
    if command.is_empty() {
        return Err(ExecutorError::NoCommand(reference.to_owned()));
    }
    Ok(command)
}

/// The reference with the first digest of `docker image inspect`'s `RepoDigests`, which look
/// like `["debian@sha256:..."]`; the repository of the reference is kept.
fn pinned(reference: &str, digests: &str) -> Option<String> {
    let first = digests
        .split('"')
        .nth(1)
        .filter(|digest| digest.contains("@sha256:"))?;
    let hash = first.split_once('@').map_or(first, |(_, hash)| hash);
    let name = reference
        .rsplit_once(':')
        .filter(|(_, tag)| !tag.contains('/'))
        .map_or(reference, |(name, _)| name);
    Some(format!("{name}@{hash}"))
}

/// The engine could not be started.
pub(crate) fn failed(engine: &ContainerEngine, error: std::io::Error) -> ExecutorError {
    ExecutorError::Engine {
        engine: engine.program.clone(),
        message: error.to_string(),
    }
}

/// The engine failed, with what it said, or its exit status.
pub(crate) fn engine_error(engine: &ContainerEngine, output: &Output) -> ExecutorError {
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    ExecutorError::Engine {
        engine: engine.program.clone(),
        message: match message.is_empty() {
            true => format!("it ended with {}", output.status),
            false => message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_keep_the_repository() {
        let digests = r#"["docker.io/library/debian@sha256:ab"]"#;
        assert_eq!(
            pinned("debian:stable-slim", digests).as_deref(),
            Some("debian@sha256:ab")
        );
        assert_eq!(
            pinned("localhost:5000/tool", r#"["x@sha256:cd"]"#).as_deref(),
            Some("localhost:5000/tool@sha256:cd")
        );
        assert_eq!(pinned("built", "[]"), None);
    }
}
