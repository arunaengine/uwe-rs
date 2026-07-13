//! The container engine of this machine, Docker or Podman, found without any config and reached
//! through the Docker Engine API over its socket, so that no program like `docker` is needed;
//! and the command of an engine on another machine, for job scripts.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::path::PathBuf;

use bollard::{API_DEFAULT_VERSION, Docker};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Docker,
    Podman,
}

/// How long a request to the engine may take before its answer starts. Long waits, like for
/// the end of a container, ask again instead.
const TIMEOUT_SECONDS: u64 = 600;

/// The engine of this machine, with a client of its API.
#[derive(Clone)]
pub struct ContainerEngine {
    /// The engine and its socket, for messages, like `Docker at /var/run/docker.sock`.
    pub name: String,
    pub kind: EngineKind,
    /// Whether the engine runs without root, as Podman usually does.
    pub rootless: bool,
    client: Docker,
}

impl fmt::Debug for ContainerEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContainerEngine")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("rootless", &self.rootless)
            .finish()
    }
}

/// No container engine answers.
#[derive(Debug, thiserror::Error)]
#[error("iunctus runs every task in a container, but {0}")]
pub struct NoEngine(String);

impl ContainerEngine {
    /// The engine that `IUNCTUS_ENGINE` names, else Docker when its socket answers, else
    /// Podman.
    pub async fn find() -> Result<ContainerEngine, NoEngine> {
        let named = std::env::var("IUNCTUS_ENGINE").ok();
        Self::named(named.as_deref().filter(|name| !name.is_empty())).await
    }

    /// The engine of a name as `IUNCTUS_ENGINE` takes it: `docker`, `podman`, or a socket like
    /// `/run/user/1000/podman/podman.sock` or `unix:///var/run/docker.sock`. `None` looks for
    /// Docker, then for Podman.
    pub async fn named(name: Option<&str>) -> Result<ContainerEngine, NoEngine> {
        let sockets = match name {
            None => [docker_sockets(), podman_sockets()].concat(),
            Some("docker") => docker_sockets(),
            Some("podman") => podman_sockets(),
            Some(socket) => vec![socket.trim_start_matches("unix://").to_owned()],
        };
        let mut reasons = Vec::new();
        for socket in sockets {
            match Self::connect(&socket).await {
                Ok(engine) => return Ok(engine),
                Err(reason) => reasons.push(format!("{socket}: {reason}")),
            }
        }
        let how = match name {
            Some("podman") => "turn on the socket of Podman with `systemctl --user enable --now \
                               podman.socket`"
                .to_owned(),
            Some("docker") => "start Docker".to_owned(),
            Some(socket) => format!("start the engine of {socket}"),
            None => "start Docker, or turn on the socket of Podman with `systemctl --user enable \
                     --now podman.socket`, or name a socket with IUNCTUS_ENGINE"
                .to_owned(),
        };
        Err(NoEngine(format!(
            "no container engine answers ({}). To run tasks, {how}.",
            reasons.join("; ")
        )))
    }

    /// The engine at a socket, asked about itself.
    pub async fn connect(socket: &str) -> Result<ContainerEngine, String> {
        let client = Docker::connect_with_socket(socket, TIMEOUT_SECONDS, API_DEFAULT_VERSION)
            .map_err(|error| error.to_string())?;
        let version = client.version().await.map_err(|error| error.to_string())?;
        let podman = (version.components.iter().flatten())
            .any(|component| component.name.to_lowercase().contains("podman"));
        let info = client.info().await.map_err(|error| error.to_string())?;
        let rootless =
            (info.security_options.iter().flatten()).any(|option| option.contains("rootless"));
        let kind = if podman {
            EngineKind::Podman
        } else {
            EngineKind::Docker
        };
        let name = match kind {
            EngineKind::Docker => "Docker",
            EngineKind::Podman => "Podman",
        };
        Ok(ContainerEngine {
            name: format!("{name} at {socket}"),
            kind,
            rootless,
            client,
        })
    }

    /// The client of the engine's API.
    pub fn client(&self) -> &Docker {
        &self.client
    }

    /// The user that containers run as, so that the files they write belong to the user with
    /// these ids. A rootless engine maps the container's root to that user, so they run as
    /// root there.
    pub fn user(&self, user: u32, group: u32) -> Option<String> {
        (!self.rootless).then(|| format!("{user}:{group}"))
    }

    /// The engine as a command, for scripts that run it.
    pub fn command(&self) -> EngineCommand {
        EngineCommand {
            program: match self.kind {
                EngineKind::Docker => "docker".to_owned(),
                EngineKind::Podman => "podman".to_owned(),
            },
            kind: self.kind,
            rootless: self.rootless,
        }
    }
}

/// The sockets where Docker answers: the one of `DOCKER_HOST`, the system one, and the one of
/// a Docker without root.
fn docker_sockets() -> Vec<String> {
    let mut sockets = Vec::new();
    if let Ok(host) = std::env::var("DOCKER_HOST")
        && let Some(path) = host.strip_prefix("unix://")
    {
        sockets.push(path.to_owned());
    }
    sockets.push("/var/run/docker.sock".to_owned());
    if let Some(runtime) = runtime_directory() {
        sockets.push(runtime.join("docker.sock").to_string_lossy().into_owned());
    }
    sockets
}

/// The sockets where Podman answers with its Docker-compatible API: the one of the user, and
/// the system one.
fn podman_sockets() -> Vec<String> {
    let mut sockets = Vec::new();
    if let Some(runtime) = runtime_directory() {
        let socket = runtime.join("podman/podman.sock");
        sockets.push(socket.to_string_lossy().into_owned());
    }
    sockets.push("/run/podman/podman.sock".to_owned());
    sockets
}

fn runtime_directory() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
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
    fn a_missing_engine_says_how_to_turn_it_on() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let missing = ContainerEngine::named(Some("/nowhere/iunctus.sock"));
        let error = runtime.block_on(missing).expect_err("no engine");
        let text = error.to_string();
        assert!(text.contains("/nowhere/iunctus.sock"), "{text}");
        let podman = runtime.block_on(ContainerEngine::named(Some("podman")));
        if let Err(error) = podman {
            let text = error.to_string();
            assert!(
                text.contains("systemctl --user enable --now podman.socket"),
                "{text}"
            );
        }
    }
}
