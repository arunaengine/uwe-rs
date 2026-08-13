//! What SLURM says about the jobs of a run. One `squeue` asks about all jobs at once, at most
//! once per poll interval, so that many tasks do not load the scheduler. The tools write JSON
//! where they can (`--json`, since SLURM 21.08); older ones write text, which is read then.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
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
    json: Json,
}

/// Whether the SLURM tools of the cluster write JSON: not known yet, yes, or no.
#[derive(Debug, Default)]
struct Json(AtomicU8);

impl Json {
    const WORKS: u8 = 1;
    const FAILS: u8 = 2;

    /// Whether JSON is worth a try.
    fn maybe(&self) -> bool {
        self.0.load(Ordering::Relaxed) != Self::FAILS
    }

    fn found(&self, works: bool) {
        let value = if works { Self::WORKS } else { Self::FAILS };
        self.0.store(value, Ordering::Relaxed);
    }
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
            snapshot.states = ask(squeue, &jobs, &self.json).await;
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
async fn ask(
    squeue: &Path,
    jobs: &BTreeSet<String>,
    json: &Json,
) -> Option<HashMap<String, String>> {
    if jobs.is_empty() {
        return Some(HashMap::new());
    }
    let list: Vec<&str> = jobs.iter().map(String::as_str).collect();
    if json.maybe() {
        let output = Command::new(squeue)
            .args(["--json", "--states=all"])
            .arg(format!("--jobs={}", list.join(",")))
            .output()
            .await
            .ok()?;
        let error = String::from_utf8_lossy(&output.stderr);
        if error.contains("Invalid job id") {
            return Some(HashMap::new());
        }
        let read = serde_json::from_slice(&output.stdout).ok();
        if let Some(states) = read
            .as_ref()
            .filter(|_| output.status.success())
            .and_then(json_states)
        {
            json.found(true);
            return Some(states);
        }
    }
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
    // The text works where JSON did not, so this version writes none.
    json.found(false);
    Some(states(&String::from_utf8_lossy(&output.stdout)))
}

/// The state of each job in the JSON of `squeue --json`, where the state is a word or, since
/// SLURM 23.02, a list of words.
fn json_states(answer: &Value) -> Option<HashMap<String, String>> {
    let jobs = answer.get("jobs")?.as_array()?;
    let states = (jobs.iter())
        .filter_map(|job| {
            let id = match &job["job_id"] {
                Value::Number(number) => number.to_string(),
                Value::String(text) => text.clone(),
                _ => return None,
            };
            Some((id, first_word(&job["job_state"])?))
        })
        .collect();
    Some(states)
}

/// A state that is a word, or the first word of a list.
fn first_word(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => items.first()?.as_str().map(str::to_owned),
        _ => None,
    }
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

impl Queue {
    /// The state in which a job ended, from the accounting of SLURM, like `OUT_OF_MEMORY`;
    /// `None` when the cluster keeps no accounting or does not know the job.
    pub async fn accounted(&self, sacct: &Path, id: &str) -> Option<String> {
        if self.json.maybe() {
            let output = Command::new(sacct)
                .arg("--json")
                .arg(format!("--jobs={id}"))
                .output()
                .await
                .ok()?;
            let read: Option<Value> = serde_json::from_slice(&output.stdout).ok();
            if output.status.success()
                && let Some(answer) = read
            {
                return first_word(&answer["jobs"][0]["state"]["current"]);
            }
        }
        accounted(sacct, id).await
    }
}

impl Queue {
    /// The node of a job, from `squeue`, else from the accounting of SLURM; `None` when
    /// neither knows it.
    pub async fn node(&self, squeue: &Path, sacct: &Path, id: &str) -> Option<String> {
        let asked = [
            (
                squeue,
                ["--noheader", "--states=all", "--format=%N"].as_slice(),
            ),
            (
                sacct,
                [
                    "--allocations",
                    "--noheader",
                    "--parsable2",
                    "--format=NodeList",
                ]
                .as_slice(),
            ),
        ];
        for (program, arguments) in asked {
            let output = Command::new(program)
                .args(arguments)
                .arg(format!("--jobs={id}"))
                .output()
                .await;
            let Ok(output) = output else { continue };
            let text = String::from_utf8_lossy(&output.stdout);
            let node = text.lines().next().map(str::trim).unwrap_or_default();
            if output.status.success() && node_name(node) {
                return Some(node.to_owned());
            }
        }
        None
    }
}

/// Whether SLURM names a node, rather than none like `None assigned` or `(null)`.
fn node_name(text: &str) -> bool {
    !text.is_empty() && !text.contains([' ', '(', ')'])
}

/// The state in which a job ended, from the text of the accounting of SLURM.
async fn accounted(sacct: &Path, id: &str) -> Option<String> {
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

    #[test]
    fn json_gives_the_states_of_jobs_of_old_and_new_versions() {
        // SLURM 22.05 writes a word, 23.02 and later a list of words.
        let answer = serde_json::json!({"jobs": [
            {"job_id": 12, "job_state": "RUNNING"},
            {"job_id": 13, "job_state": ["OUT_OF_MEMORY"]},
        ]});
        let states = json_states(&answer).expect("states");
        assert_eq!(states.get("12").map(String::as_str), Some("RUNNING"));
        assert_eq!(states.get("13").map(String::as_str), Some("OUT_OF_MEMORY"));
        assert_eq!(json_states(&serde_json::json!({"errors": []})), None);
    }
}
