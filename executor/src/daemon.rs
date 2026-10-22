//! Runs daemon tasks on the local engine: on a network of the run, until they are ready, then on.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use bollard::models::{ContainerCreateBody, NetworkCreateRequest, PortBinding};
use bollard::query_parameters::{CreateContainerOptions, StopContainerOptions};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::job::{ExecutorError, Finished};
use crate::local::LocalContainer;

/// The variable of a task's environment that makes it a daemon: the JSON of a [`Daemon`].
/// The executor takes it out; the task never sees it.
pub const DAEMON_ENV: &str = "UWE_DAEMON";

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
    Http {
        port: u16,
        path: String,
        /// `HTTP` or `HTTPS`, as Kubernetes names them; `HTTP` without it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheme: Option<String>,
        /// The host to ask instead of the address of the task.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        headers: Vec<Header>,
    },
    Tcp {
        port: u16,
    },
}

/// A header that an HTTP probe sends, like `httpHeaders` of Kubernetes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub name: String,
    pub value: String,
}

/// Where the probes of a daemon connect: its address, or a port published on this machine.
struct Target {
    host: String,
    /// The published port that stands in for the port of the probe.
    port: Option<u16>,
    /// Whether the port is forwarded, so that a connection alone tells nothing.
    forwarded: bool,
}

/// Publishes a port of a container on the loopback address of this machine, at a free port.
fn publish(body: &mut ContainerCreateBody, port: u16) {
    let key = format!("{port}/tcp");
    body.exposed_ports.get_or_insert_default().push(key.clone());
    let binding = PortBinding {
        host_ip: Some("127.0.0.1".into()),
        host_port: Some(String::new()),
    };
    let config = body.host_config.get_or_insert_default();
    (config.port_bindings.get_or_insert_default()).insert(key, Some(vec![binding]));
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
            let name = format!("uwe-{}-{stamp}", std::process::id());
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
    /// `cancel` fires or its `time` is over. Its outcome is the address on the network of the run.
    pub(crate) async fn serve(
        &self,
        name: &str,
        mut body: ContainerCreateBody,
        daemon: Daemon,
        root: std::path::PathBuf,
        time: Option<Duration>,
        cancel: &CancellationToken,
        share: crate::Share<'static>,
    ) -> Result<Finished, ExecutorError> {
        let client = self.engine.client().clone();
        if let Some(port) = self.forwarded(&daemon) {
            publish(&mut body, port);
        }
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
        // The daemon stops when its run says so, or at once when it fails before it is ready.
        let stop = cancel.child_token();
        let watched = (
            self.engine.clone(),
            client.clone(),
            id.clone(),
            root.clone(),
            stop.clone(),
        );
        let watcher = tokio::spawn(async move {
            let (engine, client, id, root, stop) = watched;
            // Like a pod of Argo past its deadline, a daemon stops at the end of its time.
            let over = async {
                match time {
                    Some(time) => tokio::time::sleep(time).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = stop.cancelled() => {}
                () = over => {}
            }
            let options = StopContainerOptions {
                t: Some(10),
                ..Default::default()
            };
            let stopped = tokio::time::timeout(
                Duration::from_secs(15),
                client.stop_container(&id, Some(options)),
            )
            .await
            .is_ok_and(|result| result.is_ok());
            if !crate::engine::remove_owned(&engine, &id).await && !stopped {
                share.hold();
            }
            drop(share);
            let _ = tokio::fs::remove_dir_all(&root).await;
        });
        self.daemons.lock().expect("the daemons").push(watcher);
        let started = self.start_daemon(name, &id, &daemon, cancel);
        let address = match time {
            Some(time) => tokio::time::timeout(time, started)
                .await
                .unwrap_or_else(|_| {
                    Err(ExecutorError::Engine {
                        engine: self.engine.name.clone(),
                        message: format!(
                            "daemon {name} was not ready within its time of {} seconds",
                            time.as_secs()
                        ),
                    })
                }),
            None => started.await,
        };
        if address.is_err() {
            stop.cancel();
        }
        Ok(Finished {
            exit_code: Some(0),
            files: Vec::new(),
            root,
            usage: None,
            address: Some(address?),
        })
    }

    /// Starts the created container of a daemon and gives its address once it is ready.
    async fn start_daemon(
        &self,
        name: &str,
        id: &str,
        daemon: &Daemon,
        cancel: &CancellationToken,
    ) -> Result<String, ExecutorError> {
        let client = self.engine.client();
        let failed = |error| crate::image::failed(&self.engine, error);
        client.start_container(id, None).await.map_err(failed)?;
        let network = self.daemon_network().await?;
        let inspected = client.inspect_container(id, None).await.map_err(failed)?;
        let settings = inspected.network_settings.as_ref();
        let address = (settings.and_then(|settings| settings.networks.as_ref()))
            .and_then(|networks| networks.get(&network))
            .and_then(|endpoint| endpoint.ip_address.clone())
            .filter(|address| !address.is_empty())
            .ok_or_else(|| ExecutorError::Engine {
                engine: self.engine.name.clone(),
                message: format!("daemon {name} has no address on the network {network}"),
            })?;
        if let Some(probe) = &daemon.probe {
            // A port published on this machine stands in for the address, which it cannot reach.
            let published = self.forwarded(daemon).and_then(|port| {
                let ports = settings?.ports.as_ref()?;
                let bindings = ports.get(&format!("{port}/tcp"))?.as_ref()?;
                let port = bindings.first()?.host_port.as_deref()?.parse().ok()?;
                Some(Target {
                    host: "127.0.0.1".into(),
                    port: Some(port),
                    forwarded: true,
                })
            });
            let target = match (published, self.forwarded(daemon)) {
                (Some(target), _) => target,
                (None, Some(_)) => {
                    return Err(ExecutorError::Engine {
                        engine: self.engine.name.clone(),
                        message: format!("daemon {name} got no port on this machine for its probe"),
                    });
                }
                (None, None) => Target {
                    host: address.clone(),
                    port: None,
                    forwarded: false,
                },
            };
            self.wait_ready(id, &target, probe, cancel).await?;
        }
        Ok(address)
    }

    /// The port of the HTTP or TCP probe of a daemon that this machine reaches only through a
    /// published port: a rootless engine, or one in a VM, keeps container addresses to itself.
    fn forwarded(&self, daemon: &Daemon) -> Option<u16> {
        let (Check::Http {
            port, host: None, ..
        }
        | Check::Tcp { port }) = &daemon.probe.as_ref()?.check
        else {
            return None;
        };
        (self.probe_ports || self.engine.rootless || !cfg!(target_os = "linux")).then_some(*port)
    }

    /// Probes a daemon until it is ready; a daemon that ended is not.
    async fn wait_ready(
        &self,
        id: &str,
        target: &Target,
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
            let ready = tokio::time::timeout(limit, self.check(id, target, &probe.check))
                .await
                .unwrap_or(false);
            if ready {
                return Ok(());
            }
            pause(probe.period_seconds.max(1)).await?;
        }
    }

    /// Whether one check of a probe passes: a command in the container that ends with 0, a
    /// page with a status below 400, or a port that takes a connection. Like the kubelet, an
    /// HTTPS probe checks no certificate.
    async fn check(&self, id: &str, target: &Target, check: &Check) -> bool {
        match check {
            Check::Command(command) => self.run_inside(id, command).await,
            Check::Http {
                port,
                path,
                scheme,
                host,
                headers,
            } => {
                let scheme = match scheme.as_deref() {
                    Some(scheme) if scheme.eq_ignore_ascii_case("https") => "https",
                    _ => "http",
                };
                let host = host.as_deref().unwrap_or(&target.host);
                let port = target.port.unwrap_or(*port);
                let path = path.trim_start_matches('/');
                let url = format!("{scheme}://{host}:{port}/{path}");
                let built = reqwest::Client::builder()
                    .no_proxy()
                    .danger_accept_invalid_certs(true)
                    .build();
                let Ok(client) = built else {
                    return false;
                };
                let mut request = client.get(url);
                for header in headers {
                    request = request.header(&header.name, &header.value);
                }
                (request.send().await).is_ok_and(|answer| answer.status().as_u16() < 400)
            }
            Check::Tcp { port } => {
                let port = target.port.unwrap_or(*port);
                let Ok(mut stream) = tokio::net::TcpStream::connect((&*target.host, port)).await
                else {
                    return false;
                };
                // A forwarded port takes every connection and closes it at once when nothing
                // listens behind it; a listening port keeps it open or answers.
                if !target.forwarded {
                    return true;
                }
                let mut byte = [0; 1];
                let read = tokio::io::AsyncReadExt::read(&mut stream, &mut byte);
                match tokio::time::timeout(Duration::from_millis(300), read).await {
                    Err(_) => true,
                    Ok(read) => read.is_ok_and(|count| count > 0),
                }
            }
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

    #[tokio::test]
    async fn retains_daemon_share() {
        use crate::test_engine::{Reply, Server};
        use futures_util::FutureExt;
        let (deleting, reached) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let mut deleting = Some(deleting);
        let mut network = String::new();
        let server = Server::start(6, move |request| {
            if request.path.contains("/containers/create?") {
                Reply::Json(201, serde_json::json!({"Id": "daemon-id", "Warnings": []}))
            } else if request.path.ends_with("/start") || request.path.contains("/stop?") {
                Reply::Json(204, serde_json::Value::Null)
            } else if request.path.ends_with("/networks/create") {
                network = request.body["Name"].as_str().unwrap().into();
                Reply::Json(201, serde_json::json!({"Id": "network-id", "Warning": ""}))
            } else if request.method == "DELETE" {
                deleting.take().unwrap().send(()).unwrap();
                released.recv().unwrap();
                Reply::Json(204, serde_json::Value::Null)
            } else {
                assert!(request.path.ends_with("/containers/daemon-id/json"));
                Reply::Json(200, serde_json::json!({"NetworkSettings": {"Networks": {network.clone(): {"IPAddress": "10.1.0.2"}}}}))
            }
        }).await;
        let folder = tempfile::tempdir().unwrap();
        let backend = LocalContainer::new(
            server.engine.clone(),
            folder.path().into(),
            crate::Pool::new(1, 1 << 30),
        );
        let share = backend.pool.share(1, Some(1 << 30), 0).await.owned();
        let cancel = CancellationToken::new();
        backend
            .serve(
                "daemon",
                ContainerCreateBody::default(),
                Daemon::default(),
                folder.path().join("task"),
                None,
                &cancel,
                share,
            )
            .await
            .unwrap();
        let mut waiting = Box::pin(backend.pool.share(1, Some(1 << 30), 0));
        assert!((&mut waiting).now_or_never().is_none());
        cancel.cancel();
        reached.await.unwrap();
        assert!((&mut waiting).now_or_never().is_none());
        release.send(()).unwrap();
        let watchers = std::mem::take(&mut *backend.daemons.lock().unwrap());
        for watcher in watchers {
            watcher.await.unwrap();
        }
        let admitted = waiting.await;
        assert_eq!(admitted.cpus, 1);
        drop(admitted);
        server.finish();
    }

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
