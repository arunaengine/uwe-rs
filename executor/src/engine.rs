//! The container engine of this machine: Docker or Podman, found without any config.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::process::Command;

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Docker,
    Podman,
}

/// A container engine that answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerEngine {
    /// The command, like `docker`, `podman` or a full path.
    pub program: String,
    pub kind: EngineKind,
    /// Whether the engine runs without root, as Podman usually does.
    pub rootless: bool,
}

/// No container engine answers.
#[derive(Debug, thiserror::Error)]
#[error("iunctus runs every task in a container, but {0}")]
pub struct NoEngine(String);

impl ContainerEngine {
    /// The engine that `IUNCTUS_ENGINE` names, else Docker when its service answers, else
    /// Podman.
    pub fn find() -> Result<ContainerEngine, NoEngine> {
        if let Some(program) = std::env::var_os("IUNCTUS_ENGINE").filter(|name| !name.is_empty()) {
            let program = program.to_string_lossy().into_owned();
            return Self::probe(&program).map_err(|reason| {
                NoEngine(format!(
                    "the engine `{program}` that IUNCTUS_ENGINE names does not answer: {reason}"
                ))
            });
        }
        let mut reasons = Vec::new();
        for program in ["docker", "podman"] {
            match Self::probe(program) {
                Ok(engine) => return Ok(engine),
                Err(reason) => reasons.push(format!("{program}: {reason}")),
            }
        }
        Err(NoEngine(format!(
            "neither Docker nor Podman answers ({}). Install one of them or start its service, \
             or name the engine with IUNCTUS_ENGINE",
            reasons.join("; ")
        )))
    }

    /// An engine by its command, as Docker running with root, without asking it. For tests
    /// and callers that know their engine.
    pub fn docker(program: &str) -> ContainerEngine {
        ContainerEngine {
            program: program.to_owned(),
            kind: EngineKind::Docker,
            rootless: false,
        }
    }

    /// Asks an engine about itself. `info` needs the service, not just the command.
    fn probe(program: &str) -> Result<ContainerEngine, String> {
        let output = Command::new(program)
            .args(["info", "--format", "{{json .}}"])
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr);
            let first = message.lines().find(|line| !line.trim().is_empty());
            return Err(first.unwrap_or("it failed").trim().to_owned());
        }
        let info: Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("unreadable `info`: {error}"))?;
        Ok(match info.get("host") {
            // Podman reports its host in lowercase, with the rootless mode.
            Some(host) => ContainerEngine {
                program: program.to_owned(),
                kind: EngineKind::Podman,
                rootless: host["security"]["rootless"].as_bool().unwrap_or(false),
            },
            None => ContainerEngine {
                program: program.to_owned(),
                kind: EngineKind::Docker,
                rootless: (info["SecurityOptions"].as_array().into_iter().flatten())
                    .any(|option| option.as_str() == Some("name=rootless")),
            },
        })
    }

    /// The engine as a command, for the options of `run`.
    pub fn command(&self) -> EngineCommand {
        EngineCommand {
            program: self.program.clone(),
            kind: self.kind,
            rootless: self.rootless,
        }
    }
}

/// How a script runs a container engine by its command, like a job script on a node of a
/// cluster, which runs the `podman` or `docker` of that node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineCommand {
    /// The command, like `docker`, `podman` or a full path.
    pub program: String,
    pub kind: EngineKind,
    /// Whether the engine runs without root, as Podman usually does.
    pub rootless: bool,
}

impl EngineCommand {
    /// Docker running with root, by its command.
    pub fn docker(program: &str) -> EngineCommand {
        EngineCommand {
            program: program.to_owned(),
            kind: EngineKind::Docker,
            rootless: false,
        }
    }

    /// The options that run a container as the user with these ids, so that the files it
    /// writes belong to that user. Rootless Podman keeps the user's id inside the container;
    /// rootless Docker maps the container's root to the user, so it runs as root.
    pub fn user_options(&self, user: u32, group: u32) -> Vec<String> {
        let user = format!("{user}:{group}");
        match (self.kind, self.rootless) {
            (EngineKind::Docker, true) => Vec::new(),
            (EngineKind::Podman, true) => vec!["--userns=keep-id".into(), "--user".into(), user],
            _ => vec!["--user".into(), user],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn users_keep_their_files() {
        let mut engine = EngineCommand::docker("docker");
        assert_eq!(engine.user_options(1000, 100), ["--user", "1000:100"]);
        engine.rootless = true;
        assert!(engine.user_options(1000, 100).is_empty());
        engine.kind = EngineKind::Podman;
        let options = engine.user_options(1000, 100);
        assert_eq!(options, ["--userns=keep-id", "--user", "1000:100"]);
    }

    #[test]
    fn a_missing_engine_is_named() {
        let reason = ContainerEngine::probe("iunctus-no-such-engine").unwrap_err();
        assert!(!reason.is_empty());
    }
}
