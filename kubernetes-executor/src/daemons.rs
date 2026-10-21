//! Runs daemon tasks as Jobs whose pod counts as ended once it is ready, and stops them later.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use executor::daemon::{Check, Daemon};
use executor::poll::CANCEL_GRACE;
use executor::{ExecutorError, Finished};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::backend::{Ended, KubernetesBackend, machine};

/// The readiness probe of Kubernetes for a probe of a daemon.
pub fn readiness(daemon: &Daemon) -> Option<Value> {
    let probe = daemon.probe.as_ref()?;
    let mut value = match &probe.check {
        Check::Command(command) => json!({"exec": {"command": command}}),
        Check::Http {
            port,
            path,
            scheme,
            host,
            headers,
        } => {
            let mut get = json!({"port": port, "path": path});
            if let Some(scheme) = scheme {
                get["scheme"] = json!(scheme);
            }
            if let Some(host) = host {
                get["host"] = json!(host);
            }
            if !headers.is_empty() {
                get["httpHeaders"] = json!(headers);
            }
            json!({ "httpGet": get })
        }
        Check::Tcp { port } => json!({"tcpSocket": {"port": port}}),
    };
    value["initialDelaySeconds"] = json!(probe.delay_seconds);
    value["periodSeconds"] = json!(probe.period_seconds.max(1));
    value["timeoutSeconds"] = json!(probe.timeout_seconds.max(1));
    Some(value)
}

impl KubernetesBackend {
    /// Waits until the pod of a daemon's Job is ready, and stops the Job once `cancel` fires.
    /// The outcome names the IP of the pod, as Argo gives a daemon step.
    pub(crate) async fn serve(
        &self,
        name: &str,
        journal: Value,
        root: PathBuf,
        lock: impl Send + 'static,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let (cluster, poll, remote) = (self.cluster.clone(), self.poll, self.remote.clone());
        // The daemon stops when its run says so, or at once when it fails before it is ready.
        let stop = cancel.child_token();
        let watched = (root.clone(), stop.clone());
        let watcher = tokio::spawn(async move {
            let (root, stop) = watched;
            stop.cancelled().await;
            let stopped = Self::stop(&cluster, poll, &root, &journal, CANCEL_GRACE).await;
            // Without a confirmed end the journal stays, and the recovery of a run finishes it.
            match stopped {
                Ok(()) => {
                    Self::clean_staging(remote.as_ref(), &journal).await;
                    let _ = tokio::fs::remove_dir_all(&root).await;
                }
                Err(error) => tracing::warn!(%error, "cannot confirm that a daemon stopped"),
            }
            // The attempt stays locked until its Job is gone, so no recovery takes it.
            drop(lock);
        });
        self.daemons.lock().expect("the daemons").push(watcher);
        // A daemon waits with the checks of every pod, like those for a node or an image.
        self.pods.add(name).await;
        let ended = self.wait(name, true, cancel).await;
        self.pods.remove(name).await;
        if ended.is_err() {
            stop.cancel();
        }
        let reason = match ended? {
            Ended::Ready(address) => {
                return Ok(Finished {
                    exit_code: Some(0),
                    files: Vec::new(),
                    root,
                    usage: None,
                    address: Some(address),
                });
            }
            Ended::Exited(code, reason) => format!("exit code {code}, {reason}"),
            Ended::Stopped(reason) => reason,
        };
        stop.cancel();
        Err(machine(format!(
            "the pod of the daemon ended before it was ready ({reason})"
        )))
    }

    /// Waits until every daemon stopped, once their run asked them to.
    pub(crate) async fn release_daemons(&self) {
        let watchers: Vec<_> = std::mem::take(&mut *self.daemons.lock().expect("the daemons"));
        for watcher in watchers {
            let _ = watcher.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_daemon_probes() {
        let daemon: Daemon = serde_json::from_str(
            r#"{"probe": {"check": {"http": {"port": 8080, "path": "/health"}}, "delay_seconds": 2}}"#,
        )
        .unwrap();
        let probe = readiness(&daemon).unwrap();
        assert_eq!(probe["httpGet"]["port"], 8080);
        assert!(probe["httpGet"].get("scheme").is_none());
        let daemon: Daemon = serde_json::from_str(
            r#"{"probe": {"check": {"http": {"port": 8443, "path": "/", "scheme": "HTTPS", "host": "local", "headers": [{"name": "X", "value": "y"}]}}}}"#,
        )
        .unwrap();
        let get = &readiness(&daemon).unwrap()["httpGet"];
        assert_eq!(get["scheme"], "HTTPS");
        assert_eq!(get["host"], "local");
        assert_eq!(get["httpHeaders"], json!([{"name": "X", "value": "y"}]));
        assert_eq!(probe["initialDelaySeconds"], 2);
        assert_eq!(probe["periodSeconds"], 10);
    }
}
