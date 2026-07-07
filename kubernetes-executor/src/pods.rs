//! What Kubernetes says about the pods of the Jobs of a backend. One `kubectl get pods` asks
//! about all of them at once, at most once per poll interval, so that many tasks do not load
//! the API server. Every call names its kubeconfig; `kubectl` never falls back to another.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::manifest::LABEL;

/// The reasons of a container that cannot start, which waiting does not cure.
const STUCK: [&str; 5] = [
    "ErrImagePull",
    "ImagePullBackOff",
    "InvalidImageName",
    "CreateContainerConfigError",
    "CreateContainerError",
];

/// How `kubectl` reaches the cluster: always with this kubeconfig, never the user's default.
#[derive(Clone, Debug)]
pub struct Kubectl {
    /// The command, `kubectl` or a full path.
    pub program: PathBuf,
    pub kubeconfig: PathBuf,
    /// The namespace of the Jobs; `None` for the one of the kubeconfig's context.
    pub namespace: Option<String>,
}

impl Kubectl {
    /// A `kubectl` command with the kubeconfig and the namespace.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .env("KUBECONFIG", &self.kubeconfig)
            .arg("--kubeconfig")
            .arg(&self.kubeconfig);
        if let Some(namespace) = &self.namespace {
            command.args(["--namespace", namespace]);
        }
        command
    }
}

/// What the cluster says about the pod of a Job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Look {
    /// The pod waits for a node, starts or runs.
    Active,
    /// The container ended with this exit code, for this reason, like `Completed`, `Error` or
    /// `OOMKilled`.
    Ended { exit_code: i32, reason: String },
    /// The container cannot start, like for an image that cannot be pulled; the text says why.
    Stuck(String),
    /// The pod was stopped without its container ending, for this reason, like
    /// `DeadlineExceeded` or `Evicted`.
    Stopped(String),
    /// The cluster lists no pod of the Job.
    Missing,
    /// Nothing is known yet: the Job is newer than the last answer, or `kubectl` failed.
    Unknown,
}

/// The Jobs of a backend and what the cluster last said about their pods.
#[derive(Debug, Default)]
pub struct Pods {
    inner: Mutex<Snapshot>,
}

#[derive(Debug, Default)]
struct Snapshot {
    jobs: BTreeSet<String>,
    taken: Option<Instant>,
    /// The Jobs that existed when the cluster was last asked.
    asked: BTreeSet<String>,
    /// What the cluster said about the pod of each Job; `None` when `kubectl` failed.
    looks: Option<HashMap<String, Look>>,
}

impl Pods {
    pub async fn add(&self, job: &str) {
        self.inner.lock().await.jobs.insert(job.to_owned());
    }

    pub async fn remove(&self, job: &str) {
        self.inner.lock().await.jobs.remove(job);
    }

    /// What the cluster says about the pod of a Job, asking it when its last answer is older
    /// than `poll`.
    pub async fn look(&self, job: &str, poll: Duration, kubectl: &Kubectl, label: &str) -> Look {
        let mut snapshot = self.inner.lock().await;
        if snapshot.taken.is_none_or(|taken| taken.elapsed() >= poll) {
            snapshot.asked = snapshot.jobs.clone();
            snapshot.looks = ask(kubectl, label).await;
            snapshot.taken = Some(Instant::now());
        }
        if !snapshot.asked.contains(job) {
            return Look::Unknown;
        }
        match &snapshot.looks {
            None => Look::Unknown,
            Some(looks) => looks.get(job).cloned().unwrap_or(Look::Missing),
        }
    }
}

/// The pods with the label of a backend, by the name of their Job.
async fn ask(kubectl: &Kubectl, label: &str) -> Option<HashMap<String, Look>> {
    let output = (kubectl.command())
        .args(["get", "pods", "--output", "json", "--selector"])
        .arg(format!("{LABEL}={label}"))
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())?;
    let list: Value = serde_json::from_slice(&output.stdout).ok()?;
    Some(looks(&list))
}

/// What a list of pods says about each Job. With more pods of one Job, the last one counts.
pub fn looks(list: &Value) -> HashMap<String, Look> {
    let items = list["items"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    (items.iter())
        .filter_map(|pod| {
            let job = pod["metadata"]["labels"]["job-name"].as_str()?;
            Some((job.to_owned(), look(pod)))
        })
        .collect()
}

fn look(pod: &Value) -> Look {
    let status = &pod["status"];
    let state = &status["containerStatuses"][0]["state"];
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    if let Some(ended) = state.get("terminated") {
        return Look::Ended {
            exit_code: ended["exitCode"].as_i64().unwrap_or(-1) as i32,
            reason: text(&ended["reason"]),
        };
    }
    if let Some(waiting) = state.get("waiting") {
        let reason = text(&waiting["reason"]);
        if STUCK.contains(&reason.as_str()) {
            return Look::Stuck(format!("{reason}: {}", text(&waiting["message"])));
        }
    }
    match status["phase"].as_str() {
        Some("Failed") => Look::Stopped(match text(&status["reason"]) {
            reason if reason.is_empty() => "Failed".to_owned(),
            reason => reason,
        }),
        _ => Look::Active,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn pods_tell_how_their_jobs_stand() {
        let pod = |job: &str, status: Value| json!({"metadata": {"labels": {"job-name": job}}, "status": status});
        let list = json!({"items": [
            pod("a", json!({"phase": "Pending"})),
            pod("b", json!({"phase": "Succeeded", "containerStatuses": [
                {"state": {"terminated": {"exitCode": 0, "reason": "Completed"}}}]})),
            pod("c", json!({"phase": "Failed", "containerStatuses": [
                {"state": {"terminated": {"exitCode": 137, "reason": "OOMKilled"}}}]})),
            pod("d", json!({"phase": "Pending", "containerStatuses": [
                {"state": {"waiting": {"reason": "ImagePullBackOff", "message": "no such image"}}}]})),
            pod("e", json!({"phase": "Failed", "reason": "DeadlineExceeded"})),
        ]});
        let looks = looks(&list);
        assert_eq!(looks["a"], Look::Active);
        let ended = |exit_code, reason: &str| Look::Ended {
            exit_code,
            reason: reason.to_owned(),
        };
        assert_eq!(looks["b"], ended(0, "Completed"));
        assert_eq!(looks["c"], ended(137, "OOMKilled"));
        assert_eq!(
            looks["d"],
            Look::Stuck("ImagePullBackOff: no such image".into())
        );
        assert_eq!(looks["e"], Look::Stopped("DeadlineExceeded".into()));
    }
}
