//! What SLURM says about the jobs of a run. One `squeue` asks about all jobs at once, at most
//! once per poll interval, so that many tasks do not load the scheduler.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::sync::Mutex;

/// The states in which a job has ended.
const ENDED: [&str; 10] = [
    "BOOT_FAIL",
    "CANCELLED",
    "COMPLETED",
    "DEADLINE",
    "FAILED",
    "NODE_FAIL",
    "OUT_OF_MEMORY",
    "PREEMPTED",
    "REVOKED",
    "TIMEOUT",
];

/// What the queue says about a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Look {
    /// The job waits or runs.
    Active,
    /// The job ended in this state, like `COMPLETED` or `TIMEOUT`.
    Ended(String),
    /// The queue no longer lists the job: it ended a while ago.
    Gone,
    /// Nothing is known yet: the job is new, or `squeue` failed.
    Unknown,
}

/// The jobs of a backend and what `squeue` last said about them.
#[derive(Debug, Default)]
pub struct Queue {
    inner: Mutex<Snapshot>,
}

#[derive(Debug, Default)]
struct Snapshot {
    jobs: BTreeSet<String>,
    /// When `squeue` was last asked, and about which jobs.
    taken: Option<Instant>,
    asked: BTreeSet<String>,
    /// The state of each job that `squeue` listed; `None` when it failed.
    states: Option<HashMap<String, String>>,
}

impl Queue {
    pub async fn add(&self, id: &str) {
        self.inner.lock().await.jobs.insert(id.to_owned());
    }

    pub async fn remove(&self, id: &str) {
        self.inner.lock().await.jobs.remove(id);
    }

    /// What the queue says about a job, asking `squeue` when its last answer is older than
    /// `poll`.
    pub async fn look(&self, id: &str, poll: Duration, squeue: &Path) -> Look {
        let mut snapshot = self.inner.lock().await;
        if snapshot.taken.is_none_or(|taken| taken.elapsed() >= poll) {
            let jobs = snapshot.jobs.clone();
            snapshot.states = ask(squeue, &jobs).await;
            snapshot.taken = Some(Instant::now());
            snapshot.asked = jobs;
        }
        if !snapshot.asked.contains(id) {
            return Look::Unknown;
        }
        match &snapshot.states {
            None => Look::Unknown,
            Some(states) => match states.get(id) {
                None => Look::Gone,
                Some(state) if ENDED.contains(&state.as_str()) => Look::Ended(state.clone()),
                Some(_) => Look::Active,
            },
        }
    }
}

/// The states of jobs, also of those that ended lately, by `squeue`. A job that SLURM has
/// forgotten makes `squeue` fail when it is the only one asked for; it is then left out.
async fn ask(squeue: &Path, jobs: &BTreeSet<String>) -> Option<HashMap<String, String>> {
    if jobs.is_empty() {
        return Some(HashMap::new());
    }
    let list: Vec<&str> = jobs.iter().map(String::as_str).collect();
    let output = Command::new(squeue)
        .args(["--noheader", "--states=all", "--format=%i %T"])
        .arg(format!("--jobs={}", list.join(",")))
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return error.contains("Invalid job id").then(HashMap::new);
    }
    Some(states(&String::from_utf8_lossy(&output.stdout)))
}

/// The state of each job in the output of `squeue --format='%i %T'`.
fn states(text: &str) -> HashMap<String, String> {
    (text.lines())
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            Some((words.next()?.to_owned(), words.next()?.to_owned()))
        })
        .collect()
}

/// The state in which a job ended, from the accounting of SLURM, like `OUT_OF_MEMORY`; `None`
/// when the cluster keeps no accounting or does not know the job.
pub async fn accounted(sacct: &Path, id: &str) -> Option<String> {
    let output = Command::new(sacct)
        .args([
            "--allocations",
            "--noheader",
            "--parsable2",
            "--format=State",
        ])
        .arg(format!("--jobs={id}"))
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout);
    // A cancelled job reads like `CANCELLED by 1000`.
    let state = text.lines().next()?.split_whitespace().next()?;
    Some(state.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squeue_lines_give_the_states_of_jobs() {
        let states = states("12 RUNNING\n13 OUT_OF_MEMORY\n\n");
        assert_eq!(states.get("12").map(String::as_str), Some("RUNNING"));
        assert_eq!(states.get("13").map(String::as_str), Some("OUT_OF_MEMORY"));
        assert_eq!(states.len(), 2);
    }
}
