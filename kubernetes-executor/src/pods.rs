//! Polls backend pods together and interprets their task and job states.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use kube::api::ListParams;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::cluster::Cluster;
use crate::manifest::LABEL;

/// The reasons of a container that cannot start, which waiting does not cure.
const STUCK: [&str; 5] = [
    "ErrImagePull",
    "ImagePullBackOff",
    "InvalidImageName",
    "CreateContainerConfigError",
    "CreateContainerError",
];

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
    /// The scheduler finds no node for the pod yet; the text is its reason.
    Unschedulable(String),
    /// An init container, like stage-in, ended badly with this exit code and reason, so the
    /// task never starts.
    InitFailed {
        container: String,
        exit_code: i32,
        reason: String,
    },
    /// The pod was stopped without its container ending, for this reason, like
    /// `DeadlineExceeded` or `Evicted`.
    Stopped(String),
    /// The pod runs and is ready at this IP, as a daemon waits for.
    Ready(String),
    /// The cluster lists no pod of the Job.
    Missing,
    /// Nothing is known yet: the Job is newer than the last answer, or `kubectl` failed.
    Unknown,
    /// The API could not list the pods; repeated errors must not look like active work.
    Unavailable(String),
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
    /// What the cluster said about the pod of each Job.
    looks: Option<Result<HashMap<String, Look>, String>>,
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
    pub async fn look(&self, job: &str, poll: Duration, cluster: &Cluster, label: &str) -> Look {
        let mut snapshot = self.inner.lock().await;
        if snapshot.taken.is_none_or(|taken| taken.elapsed() >= poll) {
            snapshot.asked = snapshot.jobs.clone();
            snapshot.looks = Some(ask(cluster, label).await);
            snapshot.taken = Some(Instant::now());
        }
        if !snapshot.asked.contains(job) {
            return Look::Unknown;
        }
        match &snapshot.looks {
            None => Look::Unknown,
            Some(Ok(looks)) => looks.get(job).cloned().unwrap_or(Look::Missing),
            Some(Err(error)) => Look::Unavailable(error.clone()),
        }
    }
}

/// The pods with the label of a backend, by the name of their Job.
async fn ask(cluster: &Cluster, label: &str) -> Result<HashMap<String, Look>, String> {
    let selector = ListParams::default().labels(&format!("{LABEL}={label}"));
    let list = cluster
        .pods()
        .list(&selector)
        .await
        .map_err(|error| error.to_string())?;
    let list = serde_json::to_value(&list).map_err(|error| error.to_string())?;
    Ok(looks(&list))
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

/// How the pod of a Job stands. Its containers end when every one of them ended; one that
/// ended badly ends them all, since the others may wait for it.
fn look(pod: &Value) -> Look {
    let status = &pod["status"];
    let statuses = (status["containerStatuses"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let ended: Vec<(i32, String)> = (statuses.iter())
        .filter_map(|container| container["state"].get("terminated"))
        .map(|ended| {
            let exit_code = ended["exitCode"].as_i64().unwrap_or(-1) as i32;
            (exit_code, text(&ended["reason"]))
        })
        .collect();
    let badly = ended.iter().find(|(exit_code, _)| *exit_code != 0);
    let all = !statuses.is_empty() && ended.len() == statuses.len();
    if let Some((exit_code, reason)) = badly.or(ended.first().filter(|_| all)) {
        return Look::Ended {
            exit_code: *exit_code,
            reason: reason.clone(),
        };
    }
    let init = (status["initContainerStatuses"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    for container in init {
        let ended = &container["state"]["terminated"];
        let exit_code = ended["exitCode"].as_i64().unwrap_or(0) as i32;
        if ended.is_object() && exit_code != 0 {
            return Look::InitFailed {
                container: text(&container["name"]),
                exit_code,
                reason: text(&ended["reason"]),
            };
        }
    }
    // An init container that cannot be pulled is stuck like the task container.
    for container in init.iter().chain(statuses) {
        if let Some(waiting) = container["state"].get("waiting") {
            let reason = text(&waiting["reason"]);
            if STUCK.contains(&reason.as_str()) {
                return Look::Stuck(format!("{reason}: {}", text(&waiting["message"])));
            }
        }
    }
    let conditions = (status["conditions"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let unscheduled = conditions.iter().find(|condition| {
        condition["type"] == "PodScheduled"
            && condition["status"] == "False"
            && condition["reason"] == "Unschedulable"
    });
    if let Some(condition) = unscheduled {
        return Look::Unschedulable(text(&condition["message"]));
    }
    let ready = (conditions.iter())
        .any(|condition| condition["type"] == "Ready" && condition["status"] == "True");
    let address = (status["podIP"].as_str()).filter(|address| ready && !address.is_empty());
    match (status["phase"].as_str(), address) {
        (Some("Failed"), _) => Look::Stopped(match text(&status["reason"]) {
            reason if reason.is_empty() => "Failed".to_owned(),
            reason => reason,
        }),
        (Some("Running"), Some(address)) => Look::Ready(address.to_owned()),
        _ => Look::Active,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reports_job_states() {
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
            pod("f", json!({"phase": "Running", "containerStatuses": [
                {"state": {"terminated": {"exitCode": 0, "reason": "Completed"}}},
                {"state": {"running": {}}}]})),
            pod("g", json!({"phase": "Running", "containerStatuses": [
                {"state": {"running": {}}},
                {"state": {"terminated": {"exitCode": 137, "reason": "OOMKilled"}}}]})),
            pod("i", json!({"phase": "Running", "podIP": "10.1.2.3",
                "conditions": [{"type": "Ready", "status": "True"}]})),
            pod("j", json!({"phase": "Running", "podIP": "10.1.2.3",
                "conditions": [{"type": "Ready", "status": "False"}]})),
            pod("h", json!({"phase": "Pending", "conditions": [{"type": "PodScheduled",
                "status": "False", "reason": "Unschedulable",
                "message": "0/3 nodes are available: 3 Insufficient memory."}]})),
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
        // A pod of several containers ends when all ended, or when one ended badly.
        assert_eq!(looks["f"], Look::Active);
        assert_eq!(looks["g"], ended(137, "OOMKilled"));
        let message = "0/3 nodes are available: 3 Insufficient memory.";
        assert_eq!(looks["h"], Look::Unschedulable(message.into()));
        assert_eq!(looks["i"], Look::Ready("10.1.2.3".into()));
        assert_eq!(looks["j"], Look::Active);
    }

    #[test]
    fn reports_init_containers() {
        let pod = |job: &str, init: Value| {
            let waiting = json!([{"state": {"waiting": {"reason": "PodInitializing"}}}]);
            let status = json!({"phase": "Pending", "initContainerStatuses": init,
                "containerStatuses": waiting});
            json!({"metadata": {"labels": {"job-name": job}}, "status": status})
        };
        let list = json!({"items": [
            pod("a", json!([{"name": "helper", "state": {"waiting": {"reason": "ErrImagePull",
                "message": "rate limit"}}}])),
            pod("b", json!([{"name": "helper", "state": {"terminated": {"exitCode": 0}}},
                {"name": "stage-in", "state": {"terminated": {"exitCode": 1,
                "reason": "Error"}}}])),
            pod("c", json!([{"name": "stage-in", "state": {"running": {}}}])),
        ]});
        let looks = looks(&list);
        assert_eq!(looks["a"], Look::Stuck("ErrImagePull: rate limit".into()));
        let failed = Look::InitFailed {
            container: "stage-in".into(),
            exit_code: 1,
            reason: "Error".into(),
        };
        assert_eq!(looks["b"], failed);
        assert_eq!(looks["c"], Look::Active);
    }
}
