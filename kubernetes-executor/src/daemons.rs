//! Runs daemon tasks as Jobs whose pod counts as ended once it is ready, and stops them later.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::time::Duration;

use executor::daemon::{Check, Daemon};
use executor::{ExecutorError, Finished};
use kube::api::{DeleteParams, ListParams, Preconditions, PropagationPolicy};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::backend::KubernetesBackend;

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

/// The address of a pod that is ready, `Err` with the reason for a pod that ended first, and
/// `Ok(None)` for one that is not ready yet.
pub fn ready(pod: &Value) -> Result<Option<String>, String> {
    let status = &pod["status"];
    if matches!(status["phase"].as_str(), Some("Failed" | "Succeeded")) {
        return Err(format!("the pod of the daemon ended: {}", status["phase"]));
    }
    let conditions = (status["conditions"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let ready = (conditions.iter())
        .any(|condition| condition["type"] == "Ready" && condition["status"] == "True");
    let address = status["podIP"]
        .as_str()
        .filter(|address| !address.is_empty());
    Ok(address.filter(|_| ready).map(str::to_owned))
}

impl KubernetesBackend {
    /// Waits until the pod of a daemon's Job is ready, and stops the Job once `cancel` fires.
    /// The outcome names the IP of the pod, as Argo gives a daemon step.
    pub(crate) async fn serve(
        &self,
        name: &str,
        uid: &str,
        root: PathBuf,
        lock: impl Send + 'static,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let pods = self.cluster.pods();
        let jobs = self.cluster.jobs();
        let (name, uid, stop) = (name.to_owned(), uid.to_owned(), cancel.clone());
        let watched = (jobs.clone(), name.clone(), uid.clone(), root.clone());
        let watcher = tokio::spawn(async move {
            let (jobs, name, uid, root) = watched;
            stop.cancelled().await;
            let parameters = DeleteParams {
                propagation_policy: Some(PropagationPolicy::Foreground),
                preconditions: Some(Preconditions {
                    uid: Some(uid),
                    ..Preconditions::default()
                }),
                ..DeleteParams::default()
            };
            let _ = jobs.delete(&name, &parameters).await;
            let _ = tokio::fs::remove_dir_all(&root).await;
            // The attempt stays locked until its Job is gone, so no recovery takes it.
            drop(lock);
        });
        self.daemons.lock().expect("the daemons").push(watcher);
        let selector = ListParams::default().labels(&format!("job-name={name}"));
        loop {
            let listed = pods.list(&selector).await.ok();
            for pod in listed.into_iter().flat_map(|list| list.items) {
                let pod = serde_json::to_value(pod).unwrap_or_default();
                match ready(&pod) {
                    Ok(Some(address)) => {
                        return Ok(Finished {
                            exit_code: Some(0),
                            files: Vec::new(),
                            root,
                            usage: None,
                            address: Some(address),
                        });
                    }
                    Ok(None) => {}
                    Err(reason) => return Err(ExecutorError::Unsupported(reason)),
                }
            }
            tokio::select! {
                () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
                () = tokio::time::sleep(self.poll.min(Duration::from_secs(2))) => {}
            }
        }
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
    fn reads_ready_pods() {
        let pod = |phase: &str, ready: &str| {
            json!({"status": {"phase": phase, "podIP": "10.1.2.3",
                "conditions": [{"type": "Ready", "status": ready}]}})
        };
        assert_eq!(ready(&pod("Running", "True")), Ok(Some("10.1.2.3".into())));
        assert_eq!(ready(&pod("Running", "False")), Ok(None));
        assert!(ready(&pod("Failed", "False")).is_err());
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
