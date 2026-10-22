//! Polls scheduler job states in batches through bounded SLURM tool calls.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::process::Command;
use tokio::sync::Mutex;

/// How long one call of `squeue`, `sacct` or `scancel` may take by default. A busy controller
/// often answers after several seconds, and SLURM's own message timeout is 10 s.
pub const QUERY_GRACE: Duration = Duration::from_secs(60);

/// The longest pause between two asks of a queue whose answers failed.
const MOST_BACKOFF: Duration = Duration::from_secs(600);

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

pub(crate) async fn look_one(
    squeue: &Path,
    id: &str,
    cluster: Option<&str>,
    grace: Duration,
) -> Look {
    let mut command = Command::new(squeue);
    command
        .args(["--noheader", "--states=all", "--format=%i %T"])
        .arg(format!("--jobs={id}"));
    if let Some(cluster) = cluster {
        command.arg(format!("--clusters={cluster}"));
    }
    let Ok(output) = output(&mut command, grace).await else {
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
    grace: Duration,
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
    let Ok(output) = output(&mut command, grace).await else {
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

/// The ids of the jobs of `user` that have the name `name` and the script `script`, by
/// `squeue`; `None` when it fails.
pub(crate) async fn named(
    squeue: &Path,
    name: &str,
    script: &str,
    user: u32,
    cluster: Option<&str>,
    grace: Duration,
) -> Option<Vec<String>> {
    let mut command = Command::new(squeue);
    command
        .args(["--noheader", "--states=all", "--format=%i|%o"])
        .arg(format!("--name={name}"))
        .arg(format!("--user={user}"));
    if let Some(cluster) = cluster {
        command.arg(format!("--clusters={cluster}"));
    }
    let output = output(&mut command, grace).await.ok()?;
    if !output.status.success() {
        return None;
    }
    let mut ids = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((id, command)) = line.trim().split_once('|') else {
            continue;
        };
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        if command == script && !ids.iter().any(|known| known == id) {
            ids.push(id.to_owned());
        }
    }
    Some(ids)
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

/// The states of a held job that SLURM will not run unless someone releases it.
const HELD: [&str; 3] = ["REQUEUE_HOLD", "RESV_DEL_HOLD", "SPECIAL_EXIT"];

/// The reason of a pending job whose time is longer than its partition allows.
const OVER_TIME: &str = "PartitionTimeLimit";

/// What `squeue` says about one job: its state, and why it waits, when it does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Seen {
    state: String,
    reason: String,
}

impl Seen {
    /// Why the job is held for good: by its state, or as a pending job whose launch failed,
    /// which SLURM requeues held.
    fn held(&self) -> Option<String> {
        let failed = self.state == "PENDING" && self.reason.contains("requeued held");
        match (
            HELD.contains(&self.state.as_str()) || failed,
            self.reason.is_empty(),
        ) {
            (false, _) => None,
            (true, true) => Some(self.state.clone()),
            (true, false) => Some(format!("{}, {}", self.state, self.reason)),
        }
    }
}

/// What the queue says about a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Look {
    /// The job waits or runs.
    Active,
    /// The job ended in this state, like `COMPLETED` or `TIMEOUT`.
    Ended(String),
    /// The queue no longer lists the job: it ended a while ago.
    Gone,
    /// SLURM holds the job and will not run it on its own, for this reason.
    Held(String),
    /// The job asks for more time than its partition allows, so it waits for ever.
    OverTime,
    /// Nothing is known yet: the job is new, or `squeue` failed.
    Unknown,
}

/// The jobs of a backend and what `squeue` last said about them.
#[derive(Debug, Default)]
pub struct Queue {
    inner: Mutex<HashMap<Option<String>, Snapshot>>,
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
    states: Option<HashMap<String, Seen>>,
    /// How many asks in a row failed, which makes the next ask wait longer.
    failures: u32,
}

impl Queue {
    pub async fn add(&self, id: &str, cluster: Option<&str>) {
        self.inner
            .lock()
            .await
            .entry(cluster.map(str::to_owned))
            .or_default()
            .jobs
            .insert(id.to_owned());
    }

    pub async fn remove(&self, id: &str, cluster: Option<&str>) {
        self.inner
            .lock()
            .await
            .entry(cluster.map(str::to_owned))
            .or_default()
            .jobs
            .remove(id);
    }

    /// What the queue says about a job, asking `squeue` when its last answer is older than
    /// `poll`, or longer after failed asks, so that a slow controller gets fewer of them.
    pub async fn look(
        &self,
        id: &str,
        cluster: Option<&str>,
        poll: Duration,
        squeue: &Path,
        grace: Duration,
    ) -> Look {
        let Ok(_refresh) = tokio::time::timeout(grace, self.refresh.lock()).await else {
            return Look::Unknown;
        };
        let key = cluster.map(str::to_owned);
        let mut snapshots = self.inner.lock().await;
        let snapshot = snapshots.entry(key.clone()).or_default();
        let pause = (poll * 2u32.pow(snapshot.failures.min(10))).min(MOST_BACKOFF.max(poll));
        if snapshot.taken.is_none_or(|taken| taken.elapsed() >= pause) {
            let jobs = snapshot.jobs.clone();
            drop(snapshots);
            let states = ask(squeue, &jobs, cluster, &self.json, grace).await;
            snapshots = self.inner.lock().await;
            let snapshot = snapshots.entry(key.clone()).or_default();
            snapshot.failures = match states {
                Some(_) => 0,
                None => snapshot.failures + 1,
            };
            snapshot.states = states;
            snapshot.taken = Some(Instant::now());
            snapshot.asked = jobs;
        }
        let snapshot = snapshots.entry(key).or_default();
        if !snapshot.asked.contains(id) {
            return Look::Unknown;
        }
        match &snapshot.states {
            None => Look::Unknown,
            Some(states) => match states.get(id) {
                None => Look::Gone,
                Some(seen) if ENDED.contains(&seen.state.as_str()) => {
                    Look::Ended(seen.state.clone())
                }
                Some(seen) if seen.state == "PENDING" && seen.reason.contains(OVER_TIME) => {
                    Look::OverTime
                }
                Some(seen) => seen.held().map_or(Look::Active, Look::Held),
            },
        }
    }
}

/// The states of jobs, also of those that ended lately, by `squeue`. A job that SLURM has
/// forgotten makes `squeue` fail when it is the only one asked for; it is then left out.
async fn ask(
    squeue: &Path,
    jobs: &BTreeSet<String>,
    cluster: Option<&str>,
    json: &Json,
    grace: Duration,
) -> Option<HashMap<String, Seen>> {
    if jobs.is_empty() {
        return Some(HashMap::new());
    }
    let list: Vec<&str> = jobs.iter().map(String::as_str).collect();
    let clusters: Vec<_> = cluster
        .map(|cluster| format!("--clusters={cluster}"))
        .into_iter()
        .collect();
    if json.maybe() {
        let output = output(
            Command::new(squeue)
                .args(&clusters)
                .args(["--json", "--states=all"])
                .arg(format!("--jobs={}", list.join(","))),
            grace,
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
            .args(&clusters)
            .args(["--noheader", "--states=all", "--format=%i %T %r"])
            .arg(format!("--jobs={}", list.join(","))),
        grace,
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
/// SLURM 23.02, a list of words. The description of a state says more than its reason.
fn json_states(answer: &Value) -> Option<HashMap<String, Seen>> {
    let jobs = answer.get("jobs")?.as_array()?;
    let states = (jobs.iter())
        .filter_map(|job| {
            let id = match &job["job_id"] {
                Value::Number(number) => number.to_string(),
                Value::String(text) => text.clone(),
                _ => return None,
            };
            // The code of a job over the time of its partition decides; a description may hide it.
            let codes = match job["state_reason"].as_str() == Some(OVER_TIME) {
                true => [&job["state_reason"], &job["state_description"]],
                false => [&job["state_description"], &job["state_reason"]],
            };
            let reason = (codes.into_iter().filter_map(Value::as_str))
                .find(|reason| !reason.is_empty() && *reason != "None");
            let state = first_word(&job["job_state"])?;
            let reason = reason.unwrap_or_default().to_owned();
            Some((id, Seen { state, reason }))
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

/// The state of each job in the output of `squeue --format='%i %T %r'`.
fn states(text: &str) -> HashMap<String, Seen> {
    (text.lines())
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let (id, state) = (words.next()?.to_owned(), words.next()?.to_owned());
            let reason = words.collect::<Vec<_>>().join(" ");
            let reason = if reason == "None" {
                String::new()
            } else {
                reason
            };
            Some((id, Seen { state, reason }))
        })
        .collect()
}

impl Queue {
    /// The state in which a job ended, from the accounting of SLURM, like `OUT_OF_MEMORY`;
    /// `None` when the cluster keeps no accounting or does not know the job.
    pub async fn accounted(
        &self,
        sacct: &Path,
        id: &str,
        cluster: Option<&str>,
        grace: Duration,
    ) -> Option<String> {
        let clusters = cluster.map(|cluster| format!("--clusters={cluster}"));
        if self.json.maybe() {
            let output = output(
                Command::new(sacct)
                    .args(clusters)
                    .arg("--json")
                    .arg(format!("--jobs={id}")),
                grace,
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
        accounted(sacct, id, cluster, grace).await
    }
}

impl Queue {
    /// The node of a job, from `squeue`, else from the accounting of SLURM; `None` when
    /// neither knows it.
    pub async fn node(
        &self,
        squeue: &Path,
        sacct: &Path,
        id: &str,
        cluster: Option<&str>,
        grace: Duration,
    ) -> Option<String> {
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
                    .args(cluster.map(|cluster| format!("--clusters={cluster}")))
                    .args(arguments)
                    .arg(format!("--jobs={id}")),
                grace,
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
async fn accounted(
    sacct: &Path,
    id: &str,
    cluster: Option<&str>,
    grace: Duration,
) -> Option<String> {
    let output = output(
        Command::new(sacct)
            .args(cluster.map(|cluster| format!("--clusters={cluster}")))
            .args([
                "--allocations",
                "--noheader",
                "--parsable2",
                "--format=State",
            ])
            .arg(format!("--jobs={id}")),
        grace,
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
            assert_eq!(look_one(&tool, "101", None, QUERY_GRACE).await, expected);
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
    async fn queries_selected_clusters() {
        let folder = tempfile::tempdir().unwrap();
        let tool = folder.path().join("scheduler");
        write_tool(
            &tool,
            r#"#!/bin/bash
cluster=
for word in "$@"; do
    case "$word" in --clusters=*) cluster=${word#--clusters=} ;; --json) exit 1 ;; esac
done
case "$cluster" in alpha) state=RUNNING ;; beta) state=COMPLETED ;; *) exit 1 ;; esac
case "$*" in
    *'--format=%i|%o'*) echo '7|/script' ;;
    *'--format=State'*) echo "$state" ;;
    *'--format=%N'*) echo "node-$cluster" ;;
    *) echo "7 $state None" ;;
esac
"#,
        );
        let queue = Queue::default();
        queue.add("7", Some("alpha")).await;
        queue.add("7", Some("beta")).await;
        assert_eq!(
            queue
                .look("7", Some("alpha"), Duration::ZERO, &tool, QUERY_GRACE)
                .await,
            Look::Active
        );
        assert_eq!(
            queue
                .look("7", Some("beta"), Duration::ZERO, &tool, QUERY_GRACE)
                .await,
            Look::Ended("COMPLETED".into())
        );
        assert_eq!(
            queue
                .accounted(&tool, "7", Some("beta"), QUERY_GRACE)
                .await
                .as_deref(),
            Some("COMPLETED")
        );
        assert_eq!(
            named(
                &tool,
                "attempt",
                "/script",
                1000,
                Some("alpha"),
                QUERY_GRACE
            )
            .await
            .unwrap(),
            ["7"]
        );
        assert_eq!(
            queue
                .node(&tool, &tool, "7", Some("beta"), QUERY_GRACE)
                .await
                .as_deref(),
            Some("node-beta")
        );
    }

    #[tokio::test]
    async fn poll_releases_lock() {
        let folder = tempfile::tempdir().unwrap();
        let tool = folder.path().join("squeue");
        write_tool(&tool, "#!/bin/bash\necho $$ > \"$0.pid\"\nexec sleep 30\n");
        let queue = Queue::default();
        queue.add("1", None).await;
        let poll = queue.look("1", None, Duration::ZERO, &tool, QUERY_GRACE);
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
                queue.add("2", None).await;
                queue.remove("2", None).await;
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

    #[tokio::test]
    async fn backs_off_failures() {
        let folder = tempfile::tempdir().unwrap();
        let tool = folder.path().join("squeue");
        let calls = folder.path().join("calls");
        let text = format!("#!/bin/bash\necho >> '{}'\nexit 1\n", calls.display());
        write_tool(&tool, &text);
        let queue = Queue::default();
        queue.add("1", None).await;
        // Without a pause the queue would be asked about ten times in this second, with growing
        // pauses about three times.
        let poll = Duration::from_millis(100);
        let begun = Instant::now();
        while begun.elapsed() < Duration::from_secs(1) {
            assert_eq!(
                queue.look("1", None, poll, &tool, QUERY_GRACE).await,
                Look::Unknown
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Each ask tries JSON and then text.
        let asked = std::fs::read_to_string(&calls).unwrap().lines().count();
        assert!((2..=8).contains(&asked), "{asked}");
    }

    #[test]
    fn parses_squeue_states() {
        let states = states("12 RUNNING None\n13 OUT_OF_MEMORY\n\n");
        let state = |id: &str| states.get(id).map(|seen| seen.state.as_str());
        assert_eq!(state("12"), Some("RUNNING"));
        assert_eq!(state("13"), Some("OUT_OF_MEMORY"));
        assert_eq!(states.len(), 2);
        assert!(states.values().all(|seen| seen.reason.is_empty()));
    }

    #[test]
    fn detects_held_jobs() {
        let states = states(
            "12 PENDING launch failed requeued held\n13 SPECIAL_EXIT None\n\
             14 PENDING Priority\n15 PENDING JobHeldAdmin\n",
        );
        let held = |id: &str| states.get(id).and_then(Seen::held);
        assert_eq!(
            held("12").as_deref(),
            Some("PENDING, launch failed requeued held")
        );
        assert_eq!(held("13").as_deref(), Some("SPECIAL_EXIT"));
        // A job that waits for its turn, or that an administrator holds, may still run.
        assert_eq!(held("14"), None);
        assert_eq!(held("15"), None);
        let answer = serde_json::json!({"jobs": [{"job_id": 16, "job_state": ["REQUEUE_HOLD"],
            "state_reason": "JobHeldUser", "state_description": ""}]});
        let states = json_states(&answer).expect("states");
        assert_eq!(
            states.get("16").and_then(Seen::held).as_deref(),
            Some("REQUEUE_HOLD, JobHeldUser")
        );
    }

    #[test]
    fn parses_versioned_json() {
        // SLURM 22.05 writes a word, 23.02 and later a list of words.
        let answer = serde_json::json!({"jobs": [
            {"job_id": 12, "job_state": "RUNNING"},
            {"job_id": 13, "job_state": ["OUT_OF_MEMORY"]},
        ]});
        let states = json_states(&answer).expect("states");
        let state = |id: &str| states.get(id).map(|seen| seen.state.as_str());
        assert_eq!(state("12"), Some("RUNNING"));
        assert_eq!(state("13"), Some("OUT_OF_MEMORY"));
        assert_eq!(json_states(&serde_json::json!({"errors": []})), None);
    }
}
