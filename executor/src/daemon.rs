//! Runs daemon tasks on the local engine: on a network of the run, until they are ready, then on.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use bollard::models::{ContainerCreateBody, NetworkCreateRequest};
use bollard::query_parameters::{CreateContainerOptions, StopContainerOptions};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::job::{ExecutorError, Finished};
use crate::local::LocalContainer;

/// The variable of a task's environment that makes it a daemon: the JSON of a [`Daemon`].
/// The executor takes it out; the task never sees it.
pub const DAEMON_ENV: &str = "IUNCTUS_DAEMON";

/// A task that serves others until its run stops it, like an Argo daemon: it counts as
/// ended once it is ready, and the run reaches it at the address of its outcome.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Daemon {
    /// How the task tells that it is ready, as a readiness probe of Kubernetes does; without
    /// one it is ready once it runs.
    #[serde(default)]
    pub probe: Option<Probe>,
}

/// A readiness probe, with the defaults of Kubernetes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub check: Check,
    #[serde(default)]
    pub delay_seconds: u64,
    #[serde(default = "period")]
    pub period_seconds: u64,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}

/// What a probe does: run a command in the container, ask for a page, or connect to a port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Check {
    Command(Vec<String>),
    Http { port: u16, path: String },
    Tcp { port: u16 },
}

fn period() -> u64 {
    10
}

fn timeout() -> u64 {
    1
}

impl LocalContainer {
    /// The network of the run that daemons and the tasks after them share, made at the
    /// first daemon.
    pub(crate) async fn daemon_network(&self) -> Result<String, ExecutorError> {
        let name = self.network.get_or_try_init(|| async {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let name = format!("iunctus-{}-{stamp}", std::process::id());
            let request = NetworkCreateRequest {
                name: name.clone(),
                ..Default::default()
            };
            (self.engine.client().create_network(request).await)
                .map_err(|error| crate::image::failed(&self.engine, error))?;
            Ok::<_, ExecutorError>(name)
        });
        name.await.cloned()
    }

    /// Starts the container of a daemon, waits until it is ready and leaves it running until
    /// `cancel` fires. Its outcome is the address on the network of the run.
    pub(crate) async fn serve(
        &self,
        name: &str,
        body: ContainerCreateBody,
        daemon: Daemon,
        root: std::path::PathBuf,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let client = self.engine.client().clone();
        let failed = |error| crate::image::failed(&self.engine, error);
        let options = CreateContainerOptions {
            name: Some(name.to_owned()),
            ..Default::default()
        };
        let id = crate::process::create_id(&self.engine, options, body, cancel)
            .await
            .map_err(|error| ExecutorError::Engine {
                engine: self.engine.name.clone(),
                message: error.to_string(),
            })?;
        let watched = (client.clone(), id.clone(), root.clone(), cancel.clone());
        // The daemon stops when its run says so, whether it got ready or not.
        let watcher = tokio::spawn(async move {
            let (client, id, root, cancel) = watched;
            cancel.cancelled().await;
            let options = StopContainerOptions {
                t: Some(10),
                ..Default::default()
            };
            let _ = client.stop_container(&id, Some(options)).await;
            crate::engine::remove(&client, &id).await;
            let _ = tokio::fs::remove_dir_all(&root).await;
        });
        self.daemons.lock().expect("the daemons").push(watcher);
        client.start_container(&id, None).await.map_err(failed)?;
        let network = self.daemon_network().await?;
        let inspected = client.inspect_container(&id, None).await.map_err(failed)?;
        let address = (inspected
            .network_settings
            .and_then(|settings| settings.networks))
        .and_then(|networks| networks.get(&network).cloned())
        .and_then(|endpoint| endpoint.ip_address)
        .filter(|address| !address.is_empty())
        .ok_or_else(|| ExecutorError::Engine {
            engine: self.engine.name.clone(),
            message: format!("daemon {name} has no address on the network {network}"),
        })?;
        if let Some(probe) = &daemon.probe {
            self.wait_ready(&id, &address, probe, cancel).await?;
        }
        Ok(Finished {
            exit_code: Some(0),
            files: Vec::new(),
            root,
            usage: None,
            address: Some(address),
        })
    }

    /// Probes a daemon until it is ready; a daemon that ended is not.
    async fn wait_ready(
        &self,
        id: &str,
        address: &str,
        probe: &Probe,
        cancel: &CancellationToken,
    ) -> Result<(), ExecutorError> {
        let client = self.engine.client();
        let pause = |seconds: u64| async move {
            tokio::select! {
                () = cancel.cancelled() => Err(ExecutorError::Cancelled),
                () = tokio::time::sleep(Duration::from_secs(seconds)) => Ok(()),
            }
        };
        pause(probe.delay_seconds).await?;
        let limit = Duration::from_secs(probe.timeout_seconds.max(1));
        loop {
            let state = client
                .inspect_container(id, None)
                .await
                .ok()
                .and_then(|found| found.state);
            if state.and_then(|state| state.running) != Some(true) {
                return Err(ExecutorError::Engine {
                    engine: self.engine.name.clone(),
                    message: "the daemon ended before it was ready".into(),
                });
            }
            let ready = tokio::time::timeout(limit, self.check(id, address, &probe.check))
                .await
                .unwrap_or(false);
            if ready {
                return Ok(());
            }
            pause(probe.period_seconds.max(1)).await?;
        }
    }

    /// Whether one check of a probe passes: a command in the container that ends with 0, a
    /// page with a status below 400, or a port that takes a connection.
    async fn check(&self, id: &str, address: &str, check: &Check) -> bool {
        match check {
            Check::Command(command) => self.run_inside(id, command).await,
            Check::Http { port, path } => {
                let url = format!("http://{address}:{port}/{}", path.trim_start_matches('/'));
                let Ok(client) = reqwest::Client::builder().no_proxy().build() else {
                    return false;
                };
                (client.get(url).send().await).is_ok_and(|answer| answer.status().as_u16() < 400)
            }
            Check::Tcp { port } => tokio::net::TcpStream::connect((address, *port))
                .await
                .is_ok(),
        }
    }

    async fn run_inside(&self, id: &str, command: &[String]) -> bool {
        let client = self.engine.client();
        let options = bollard::models::ExecConfig {
            cmd: Some(command.to_vec()),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };
        let Ok(exec) = client.create_exec(id, options).await else {
            return false;
        };
        if let Ok(bollard::exec::StartExecResults::Attached { mut output, .. }) =
            client.start_exec(&exec.id, None).await
        {
            while output.next().await.is_some() {}
        }
        let inspected = client.inspect_exec(&exec.id).await;
        inspected.ok().and_then(|exec| exec.exit_code) == Some(0)
    }

    /// Waits until every daemon stopped, once their run asked them to, and removes the
    /// network of the run.
    pub(crate) async fn release_daemons(&self) {
        let watchers: Vec<_> = std::mem::take(&mut *self.daemons.lock().expect("the daemons"));
        for watcher in watchers {
            let _ = watcher.await;
        }
        if let Some(network) = self.network.get() {
            let _ = self.engine.client().remove_network(network).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_daemon_settings() {
        let text = r#"{"probe":{"check":{"command":["cat","/tmp/ready"]},"timeout_seconds":3}}"#;
        let daemon: Daemon = serde_json::from_str(text).unwrap();
        let probe = daemon.probe.unwrap();
        assert_eq!(
            probe.check,
            Check::Command(vec!["cat".into(), "/tmp/ready".into()])
        );
        assert_eq!(
            (
                probe.delay_seconds,
                probe.period_seconds,
                probe.timeout_seconds
            ),
            (0, 10, 3)
        );
        assert_eq!(
            serde_json::from_str::<Daemon>("{}").unwrap(),
            Daemon::default()
        );
    }
}
