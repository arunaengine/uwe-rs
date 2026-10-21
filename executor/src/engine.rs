//! Finds and connects to local Docker or Podman through the Docker Engine API.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use bollard::errors::Error as ApiError;
use bollard::query_parameters::RemoveContainerOptions;
use bollard::{API_DEFAULT_VERSION, Docker};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Docker,
    Podman,
}

/// How long a request to the engine may take before its answer starts. Long waits, like for
/// the end of a container, ask again instead.
const TIMEOUT_SECONDS: u64 = 600;

/// Removal must finish before the runner's one-minute cancellation drain expires.
const REMOVE_GRACE: Duration = Duration::from_secs(10);

pub(crate) async fn remove(client: &Docker, id: &str) {
    let result = tokio::time::timeout(REMOVE_GRACE, async {
        loop {
            let options = RemoveContainerOptions {
                force: true,
                ..Default::default()
            };
            match client.remove_container(id, Some(options)).await {
                Ok(())
                | Err(ApiError::DockerResponseServerError {
                    status_code: 404, ..
                }) => return Ok(()),
                // Automatic removal can hold the container's removal lock after it exits.
                Err(ApiError::DockerResponseServerError {
                    status_code: 409, ..
                }) => tokio::time::sleep(Duration::from_millis(100)).await,
                Err(error) => return Err(error.to_string()),
            }
        }
    })
    .await;
    let reason = match result {
        Ok(Ok(())) => return,
        Ok(Err(reason)) => reason,
        Err(_) => "removal was not confirmed within 10 seconds".into(),
    };
    eprintln!("iunctus: warning: could not remove container {id}: {reason}");
}

/// A created container that is removed when the guard goes, so that a run whose future is
/// dropped between creating and removing a container leaves none behind.
pub(crate) struct Created {
    client: Docker,
    id: String,
}

impl Created {
    pub(crate) fn new(client: &Docker, id: &str) -> Created {
        Created {
            client: client.clone(),
            id: id.to_owned(),
        }
    }

    /// Removes the container while the run goes on.
    pub(crate) async fn remove(mut self) {
        let id = std::mem::take(&mut self.id);
        remove(&self.client, &id).await;
    }
}

impl Drop for Created {
    // A dropped future cannot await, so a thread of its own removes the container before
    // the drop returns, also while the runtime of the run shuts down.
    fn drop(&mut self) {
        if self.id.is_empty() {
            return;
        }
        let (client, id) = (self.client.clone(), std::mem::take(&mut self.id));
        let removal = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(runtime) = runtime {
                runtime.block_on(remove(&client, &id));
            }
        });
        let _ = removal.join();
    }
}

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

/// No container engine answers: where iunctus looked, and how to turn one on.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct NoEngine(String);

impl ContainerEngine {
    /// The engine that `UWE_ENGINE` names, else Docker when its socket answers, else
    /// Podman.
    pub async fn find() -> Result<ContainerEngine, NoEngine> {
        let named = std::env::var("UWE_ENGINE").ok();
        Self::named(named.as_deref().filter(|name| !name.is_empty())).await
    }

    /// The engine of a name as `UWE_ENGINE` takes it: `docker`, `podman`, or a socket like
    /// `/run/user/1000/podman/podman.sock` or `unix:///var/run/docker.sock`. `None` looks for
    /// Docker, then for Podman.
    pub async fn named(name: Option<&str>) -> Result<ContainerEngine, NoEngine> {
        let sockets = match name {
            None => [docker_sockets(), podman_sockets()].concat(),
            Some("docker") => docker_sockets(),
            Some("podman") => podman_sockets(),
            Some(socket) => vec![socket.trim_start_matches("unix://").to_owned()],
        };
        let mut failed = Vec::new();
        for socket in &sockets {
            // A socket that is not there needs no reason; one that is there and does not
            // answer does, like one that the user may not use.
            if !std::path::Path::new(socket).exists() {
                continue;
            }
            match Self::connect(socket).await {
                Ok(engine) => return Ok(engine),
                Err(reason) => failed.push(format!("{socket}: {reason}")),
            }
        }
        Err(NoEngine(missing(name, &sockets, &failed)))
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

/// Why no engine answers at `sockets`, and what to do, in one sentence. `failed` holds the
/// sockets that are there but did not answer, with the reason.
fn missing(name: Option<&str>, sockets: &[String], failed: &[String]) -> String {
    let denied = failed
        .iter()
        .any(|reason| reason.to_lowercase().contains("permission denied"));
    let mut how = match name {
        Some("podman") => {
            "turn on the socket of Podman with `systemctl --user enable --now podman.socket`"
                .to_owned()
        }
        Some("docker") => "start Docker".to_owned(),
        Some(socket) => format!("start the engine of {socket}"),
        None => "start Docker, or turn on the socket of Podman with `systemctl --user enable \
                 --now podman.socket`, or name a socket with UWE_ENGINE"
            .to_owned(),
    };
    if denied {
        how = format!(
            "let your user use the socket, like with `sudo usermod -aG docker $USER` and a new \
             login, or {how}"
        );
    }
    let at = match sockets.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, others)) => format!("{} or {last}", others.join(", ")),
        None => String::new(),
    };
    let mut message = format!(
        "no container engine answers at {at}; iunctus runs every task in a container, so {how}"
    );
    if !failed.is_empty() {
        message.push_str(&format!(" ({})", failed.join("; ")));
    }
    message
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

    /// A future that runs a container and is dropped before it removed the container, like
    /// the task of a run that failed meanwhile, still removes it.
    #[test]
    fn removes_dropped_containers() {
        use std::io::{BufRead, BufReader, Read, Write};
        let folder = tempfile::tempdir().unwrap();
        let socket = folder.path().join("engine.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let (sender, requests) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut first, mut line, mut length) = (String::new(), String::new(), 0);
                reader.read_line(&mut first).unwrap();
                while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    line.clear();
                }
                reader.read_exact(&mut vec![0; length]).unwrap();
                let body = r#"{"ApiVersion":"1.44"}"#;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
                let _ = sender.send(first);
            }
        });
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let engine = ContainerEngine::connect(socket.to_str().unwrap())
                .await
                .unwrap();
            let created = Created::new(engine.client(), "left-id");
            let running = tokio::spawn(async move {
                let _created = created;
                std::future::pending::<()>().await;
            });
            running.abort();
            assert!(running.await.unwrap_err().is_cancelled());
        });
        let removal = std::iter::from_fn(|| requests.recv_timeout(Duration::from_secs(5)).ok())
            .find(|request| request.starts_with("DELETE "));
        assert!(removal.is_some_and(|request| request.contains("/containers/left-id")));
    }

    #[test]
    fn preserves_user_files() {
        let mut engine = EngineCommand::docker("docker");
        assert_eq!(engine.user_options(1000, 100), ["--user", "1000:100"]);
        engine.rootless = true;
        assert!(engine.user_options(1000, 100).is_empty());
        engine.kind = EngineKind::Podman;
        let options = engine.user_options(1000, 100);
        assert_eq!(options, ["--userns=keep-id", "--user", "1000:100"]);
    }

    #[test]
    fn explains_missing_engine() {
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

    #[test]
    fn explains_denied_socket() {
        let sockets = ["/var/run/docker.sock".to_owned()];
        let failed = ["/var/run/docker.sock: Permission denied".to_owned()];
        let text = missing(None, &sockets, &failed);
        assert!(text.starts_with("no container engine answers at /var/run/docker.sock; "));
        assert!(text.contains("sudo usermod -aG docker $USER"), "{text}");
        assert!(
            text.ends_with("(/var/run/docker.sock: Permission denied)"),
            "{text}"
        );
        let text = missing(Some("docker"), &sockets, &[]);
        assert_eq!(
            text,
            "no container engine answers at /var/run/docker.sock; iunctus runs every task in a \
             container, so start Docker"
        );
    }
}
