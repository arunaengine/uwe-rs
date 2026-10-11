//! Polls scheduler job states in batches through bounded SLURM tool calls.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::process::Command;
use tokio::sync::Mutex;

pub(crate) use executor::poll::REQUEST_GRACE;

pub(crate) async fn output(
    command: &mut Command,
    grace: Duration,
) -> std::io::Result<std::process::Output> {
    tokio::time::timeout(grace, command.kill_on_drop(true).output())
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "SLURM tool did not finish before its deadline",
            )
        })?
}

pub(crate) async fn look_one(squeue: &Path, id: &str, cluster: Option<&str>) -> Look {
    let mut command = Command::new(squeue);
    command
        .args(["--noheader", "--states=all", "--format=%i %T"])
        .arg(format!("--jobs={id}"));
    if let Some(cluster) = cluster {
        command.arg(format!("--clusters={cluster}"));
    }
    let Ok(output) = output(&mut command, REQUEST_GRACE).await else {
        return Look::Unknown;
    };
    if !output.status.success() {
        return if String::from_utf8_lossy(&output.stderr).contains("Invalid job id") {
            Look::Gone
        } else {
            Look::Unknown
        };
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut ended = None;
    let mut active = false;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut words = line.split_whitespace();
        let (Some(job), Some(state)) = (words.next(), words.next()) else {
            return Look::Unknown;
        };
        if words.next().is_some()
            || !(job == id
                || job
                    .strip_prefix(id)
                    .is_some_and(|suffix| suffix.starts_with('_') || suffix.starts_with('+')))
        {
            return Look::Unknown;
        }
        if ENDED.contains(&state) {
            ended = Some(state.to_owned());
        } else {
            active = true;
        }
    }
    if active {
        return Look::Active;
    }
    match ended {
        Some(state) => Look::Ended(state),
        None => Look::Gone,
    }
}

pub(crate) async fn identified(
    squeue: &Path,
    id: &str,
    cluster: Option<&str>,
) -> (Look, Option<Value>) {
    let mut command = Command::new(squeue);
    command
        .args(["--noheader", "--states=all", "--format=%i|%T|%j|%V|%o"])
        .arg(format!("--jobs={id}"))
        .env("TZ", "UTC")
        .env("SLURM_TIME_FORMAT", "%Y-%m-%dT%H:%M:%S");
    if let Some(cluster) = cluster {
        command.arg(format!("--clusters={cluster}"));
    }
    let Ok(output) = output(&mut command, REQUEST_GRACE).await else {
        return (Look::Unknown, None);
    };
    if !output.status.success() {
        return (
            if String::from_utf8_lossy(&output.stderr).contains("Invalid job id") {
                Look::Gone
            } else {
                Look::Unknown
            },
            None,
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (mut identity, mut ended, mut active) = (None, None, false);
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let columns: Vec<&str> = line.splitn(5, '|').collect();
        if columns.len() != 5
            || columns.iter().any(|column| column.is_empty())
            || !(columns[0] == id
                || columns[0]
                    .strip_prefix(id)
                    .is_some_and(|suffix| suffix.starts_with('_') || suffix.starts_with('+')))
        {
            return (Look::Unknown, None);
        }
        let selected =
            serde_json::json!({"name":columns[2], "submitted":columns[3], "script":columns[4]});
        if columns[3].len() != 19
            || !columns[3]
                .bytes()
                .enumerate()
                .all(|(position, byte)| match position {
                    4 | 7 => byte == b'-',
                    10 => byte == b'T',
                    13 | 16 => byte == b':',
                    _ => byte.is_ascii_digit(),
                })
        {
            return (Look::Unknown, None);
        }
        if identity
            .as_ref()
            .is_some_and(|previous| previous != &selected)
        {
            return (Look::Unknown, None);
        }
        identity = Some(selected);
        if ENDED.contains(&columns[1]) {
            ended = Some(columns[1].to_owned());
        } else {
            active = true;
        }
    }
    (
        if active {
            Look::Active
        } else {
            ended.map(Look::Ended).unwrap_or(Look::Gone)
        },
        identity,
    )
}

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
    refresh: Mutex<()>,
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
        let Ok(_refresh) = tokio::time::timeout(REQUEST_GRACE, self.refresh.lock()).await else {
            return Look::Unknown;
        };
        let mut snapshot = self.inner.lock().await;
        if snapshot.taken.is_none_or(|taken| taken.elapsed() >= poll) {
            let jobs = snapshot.jobs.clone();
            drop(snapshot);
            let states = ask(squeue, &jobs, &self.json).await;
            snapshot = self.inner.lock().await;
            snapshot.states = states;
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
        let output = output(
            Command::new(squeue)
                .args(["--json", "--states=all"])
                .arg(format!("--jobs={}", list.join(","))),
            REQUEST_GRACE,
        )
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
    let output = output(
        Command::new(squeue)
            .args(["--noheader", "--states=all", "--format=%i %T"])
            .arg(format!("--jobs={}", list.join(","))),
        REQUEST_GRACE,
    )
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
            let output = output(
                Command::new(sacct)
                    .arg("--json")
                    .arg(format!("--jobs={id}")),
                REQUEST_GRACE,
            )
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
            let output = output(
                Command::new(program)
                    .args(arguments)
                    .arg(format!("--jobs={id}")),
                REQUEST_GRACE,
            )
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
    let output = output(
        Command::new(sacct)
            .args([
                "--allocations",
                "--noheader",
                "--parsable2",
                "--format=State",
            ])
            .arg(format!("--jobs={id}")),
        REQUEST_GRACE,
    )
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

    /// Writes a tool from a child process. A process that another test starts meanwhile gets a
    /// copy of what this one has open, and a tool open for writing cannot run.
    fn write_tool(path: &Path, text: &str) {
        let script = "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"";
        let status = std::process::Command::new("sh")
            .args(["-c", script, "sh", text])
            .arg(path)
            .status();
        assert!(status.unwrap().success());
    }

    #[tokio::test]
    async fn preserves_array_members() {
        let folder = tempfile::tempdir().unwrap();
        let tool = folder.path().join("squeue");
        for (answer, expected) in [
            ("101 CANCELLED\n101_1 COMPLETING", Look::Active),
            ("101 CANCELLED", Look::Ended("CANCELLED".into())),
            ("101 CANCELLED\nmalformed", Look::Unknown),
            ("101 CANCELLED\n101 FAILED extra", Look::Unknown),
            ("101 CANCELLED\n102 COMPLETED", Look::Unknown),
            ("101 CANCELLED\n101+1 COMPLETING", Look::Active),
            ("unexpected", Look::Unknown),
            ("", Look::Gone),
        ] {
            write_tool(&tool, &format!("#!/bin/bash\nprintf '%b' '{answer}'\n"));
            assert_eq!(look_one(&tool, "101", None).await, expected);
        }
    }

    #[tokio::test]
    async fn deadlines_kill_tools() {
        let folder = tempfile::tempdir().unwrap();
        for grace in [Duration::from_millis(100), Duration::from_secs(5)] {
            let pid_file = folder.path().join(format!("{}.pid", grace.as_millis()));
            let mut command = Command::new("bash");
            command
                .args(["-c", "echo $$ > \"$1\"; exec sleep 30", "tool"])
                .arg(&pid_file);
            let result =
                tokio::time::timeout(Duration::from_millis(300), output(&mut command, grace)).await;
            match result {
                Ok(result) => assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut),
                Err(_) => assert_eq!(grace, Duration::from_secs(5)),
            }
            let pid = std::fs::read_to_string(pid_file).unwrap();
            let process = Path::new("/proc").join(pid.trim());
            for _ in 0..100 {
                if !process.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(
                !process.exists(),
                "cancelled scheduler process still exists"
            );
        }
    }

    #[tokio::test]
    async fn poll_releases_lock() {
        let folder = tempfile::tempdir().unwrap();
        let tool = folder.path().join("squeue");
        write_tool(&tool, "#!/bin/bash\necho $$ > \"$0.pid\"\nexec sleep 30\n");
        let queue = Queue::default();
        queue.add("1").await;
        let poll = queue.look("1", Duration::ZERO, &tool);
        // A busy machine may take a while to start the tool, so the waits are long; the query
        // sleeps longer still.
        let admission = async {
            for _ in 0..1000 {
                if tool.with_extension("pid").exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(tool.with_extension("pid").is_file());
            tokio::time::timeout(Duration::from_secs(5), async {
                queue.add("2").await;
                queue.remove("2").await;
            })
            .await
            .expect("a scheduler query must not block the snapshot lock");
        };
        tokio::select! {
            look = poll => panic!("the query ended: {look:?}"),
            () = admission => {}
        }
        assert!(queue.refresh.try_lock().is_ok());
    }

    #[test]
    fn parses_squeue_states() {
        let states = states("12 RUNNING\n13 OUT_OF_MEMORY\n\n");
        assert_eq!(states.get("12").map(String::as_str), Some("RUNNING"));
        assert_eq!(states.get("13").map(String::as_str), Some("OUT_OF_MEMORY"));
        assert_eq!(states.len(), 2);
    }

    #[test]
    fn parses_versioned_json() {
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
