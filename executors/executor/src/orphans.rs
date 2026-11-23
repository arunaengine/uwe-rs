//! Describes the remote tasks that one run left behind, finds their journals and stops them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::ExecutorError;
use crate::attempts::Journals;

/// The remote task of one attempt of a run, as its journal and its server tell.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Orphan {
    /// The attempt, like `<run>-<task>-<attempt>`: the name of its scratch folder.
    pub attempt: String,
    /// The task on its server, once the journal knows it: the id of a TES task or a SLURM job,
    /// or the name of a Kubernetes Job.
    pub id: Option<String>,
    /// What the server last said of the task, in its own words, like `RUNNING` of TES,
    /// `PENDING` of SLURM or `active` of Kubernetes: `None` when it was not asked or did not answer.
    pub state: Option<String>,
    pub outcome: Outcome,
}

/// What became of the task of an attempt.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "outcome", content = "reason", rename_all = "snake_case")]
pub enum Outcome {
    /// The task has not ended. Only a report gives this.
    Running,
    /// The task had ended, or the server never took it.
    Ended,
    /// A stop canceled the task, and the server confirmed that it ended.
    Canceled,
    /// The state of the task is not known, for this reason.
    Unclear(String),
    /// A stop left the task as it was, for this reason. A later call tries again.
    Kept(String),
    /// Stopping the task failed with this error. Its journal stays for a later call.
    Failed(String),
}

/// The scratch folders under `scratch` with a journal of the run `run`, by name: journals that
/// name it, and older journals that name no run but whose folder starts with `<run>-`.
pub fn journals(scratch: &Path, run: &str) -> Result<Vec<PathBuf>, ExecutorError> {
    let prefix = format!("{run}-");
    let entries = match std::fs::read_dir(scratch) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut found = Vec::new();
    for entry in entries {
        let root = entry?.path();
        let named = root.file_name().and_then(|name| name.to_str());
        if !named.is_some_and(|name| name.starts_with(&prefix)) {
            continue;
        }
        if read(&root).is_some_and(|journal| journal["run"].is_null() || journal["run"] == run) {
            found.push(root);
        }
    }
    found.sort();
    Ok(found)
}

/// The journal in `root`, when it is there and can be read.
pub fn read(root: &Path) -> Option<Value> {
    let bytes = std::fs::read(root.join("remote.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether the uwe process of the attempt in `root` still holds the lock of its journal.
pub fn held(root: &Path) -> bool {
    let lock = File::options().write(true).open(root.join("remote.lock"));
    lock.is_ok_and(|lock| matches!(lock.try_lock(), Err(TryLockError::WouldBlock)))
}

/// The orphan of the journal in `root`, with its task `id`, before any server is asked.
pub fn orphan(root: &Path, id: Option<&str>, outcome: Outcome) -> Orphan {
    Orphan {
        attempt: (root.file_name().unwrap_or_default().to_string_lossy()).into_owned(),
        id: id.filter(|id| !id.is_empty()).map(str::to_owned),
        state: None,
        outcome,
    }
}

/// What the server of a job says of it, in its own words, and whether the job ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobState {
    pub state: String,
    pub ended: bool,
}

/// The calls of a remote executor that [`report`] and [`stop`] need: the state of a job, and
/// its cancellation. `Ok(None)` is a job that its server does not know any more.
pub trait RemoteJobs: Send + Sync {
    fn state<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<JobState>, String>>;
    fn cancel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), String>>;

    /// The scheduler that the ids of jobs belong to, like its endpoint, region and queue. A
    /// journal of another scheduler names a job that this one must not ask about or stop.
    fn scope(&self) -> String;
}

/// How often and how long a stop asks whether a cancelled job ended.
const CONFIRM_PAUSE: Duration = Duration::from_millis(500);
const CONFIRM_TRIES: u32 = 120;

/// The jobs that the run `run` left in the journals of an executor, with what their servers
/// say of them. Changes nothing.
pub async fn report(
    jobs: &dyn RemoteJobs,
    journals: Option<&Journals>,
    run: &str,
) -> Result<Vec<Orphan>, ExecutorError> {
    let mut found = Vec::new();
    let Some(journals) = journals else {
        return Ok(found);
    };
    for attempt in journals.store.list(&journals.executor, run)? {
        let Some(journal) = journals.store.load(&journals.executor, &attempt)? else {
            continue;
        };
        let id = journal["id"].as_str().filter(|id| !id.is_empty());
        let mut orphan = named(&attempt, id, Outcome::Ended);
        if let Some(reason) = foreign(&journal, journals, jobs, run) {
            orphan.outcome = Outcome::Unclear(reason.into());
        } else if let Some(id) = id {
            (orphan.state, orphan.outcome) = match jobs.state(id).await {
                Ok(Some(state)) if state.ended => (Some(state.state), Outcome::Ended),
                Ok(Some(state)) => (Some(state.state), Outcome::Running),
                Ok(None) => (None, Outcome::Ended),
                Err(error) => (None, Outcome::Unclear(error)),
            };
        } else {
            orphan.outcome = Outcome::Unclear("the server has not given the id of the job".into());
        }
        found.push(orphan);
    }
    Ok(found)
}

/// Cancels the jobs that the run `run` left and waits until their servers confirm that they
/// ended. It keeps the job of an attempt whose uwe process still holds its journal.
pub async fn stop(
    jobs: &dyn RemoteJobs,
    journals: Option<&Journals>,
    run: &str,
) -> Result<Vec<Orphan>, ExecutorError> {
    let mut found = Vec::new();
    let Some(journals) = journals else {
        return Ok(found);
    };
    let executor = &journals.executor;
    for attempt in journals.store.list(executor, run)? {
        let Some(journal) = journals.store.load(executor, &attempt)? else {
            continue;
        };
        let id = journal["id"].as_str().filter(|id| !id.is_empty());
        let kept = |reason: &str| named(&attempt, id, Outcome::Kept(reason.into()));
        if let Some(reason) = foreign(&journal, journals, jobs, run) {
            found.push(kept(reason));
            continue;
        }
        let _held = match journals.store.acquire(executor, &attempt) {
            Ok(Some(held)) => held,
            Ok(None) => {
                found.push(kept("the uwe process of the attempt still runs"));
                continue;
            }
            Err(error) => {
                found.push(kept(&format!("the journal cannot be held: {error}")));
                continue;
            }
        };
        // The process of the attempt may have ended its job before it let go.
        let Some(journal) = journals.store.load(executor, &attempt)? else {
            continue;
        };
        let Some(id) = journal["id"].as_str().filter(|id| !id.is_empty()) else {
            found.push(kept("the server has not given the id of the job"));
            continue;
        };
        let mut orphan = named(&attempt, Some(id), Outcome::Ended);
        orphan.outcome = match jobs.state(id).await {
            Err(error) => Outcome::Kept(error),
            Ok(state) if state.as_ref().is_none_or(|state| state.ended) => {
                orphan.state = state.map(|state| state.state);
                journals.store.remove(executor, &attempt)?;
                Outcome::Ended
            }
            Ok(state) => {
                orphan.state = state.map(|state| state.state);
                match cancelled(jobs, id).await {
                    Ok(last) => {
                        orphan.state = last.or(orphan.state);
                        journals.store.remove(executor, &attempt)?;
                        Outcome::Canceled
                    }
                    Err(error) => Outcome::Failed(error),
                }
            }
        };
        found.push(orphan);
    }
    Ok(found)
}

/// How many times the end of a cancelled attempt asks whether its job ended, before its journal
/// stays for a later stop.
const SETTLE_TRIES: u32 = 10;

/// Whether the server says, within a short wait, that a job that was asked to stop ended or is
/// gone.
pub async fn confirmed(jobs: &dyn RemoteJobs, id: &str) -> bool {
    for _ in 0..SETTLE_TRIES {
        match jobs.state(id).await {
            Ok(None) => return true,
            Ok(Some(state)) if state.ended => return true,
            Ok(Some(_)) => tokio::time::sleep(CONFIRM_PAUSE).await,
            Err(_) => return false,
        }
    }
    false
}

/// Cancels a job and waits until its server says that it ended; gives its last state.
async fn cancelled(jobs: &dyn RemoteJobs, id: &str) -> Result<Option<String>, String> {
    jobs.cancel(id).await?;
    for _ in 0..CONFIRM_TRIES {
        match jobs.state(id).await? {
            None => return Ok(None),
            Some(state) if state.ended => return Ok(Some(state.state)),
            Some(_) => tokio::time::sleep(CONFIRM_PAUSE).await,
        }
    }
    Err("the server did not confirm that the job ended".into())
}

/// Why a journal is not one that `journals` and the scheduler of `jobs` may act on for the run
/// `run`, if it is not.
fn foreign(
    journal: &Value,
    journals: &Journals,
    jobs: &dyn RemoteJobs,
    run: &str,
) -> Option<&'static str> {
    if journal["run"].as_str() != Some(run) {
        return Some("the journal names no run, so it may belong to another");
    }
    if journal["backend"].as_str() != Some(journals.executor.as_str()) {
        return Some("another executor owns the job");
    }
    // The same id may name another job of another endpoint, region, queue or cluster.
    (journal["scope"].as_str() != Some(jobs.scope().as_str()))
        .then_some("the job belongs to another scheduler than the one of this executor")
}

fn named(attempt: &str, id: Option<&str>, outcome: Outcome) -> Orphan {
    Orphan {
        attempt: attempt.to_owned(),
        id: id.map(str::to_owned),
        state: None,
        outcome,
    }
}
