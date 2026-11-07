//! Describes the remote tasks that one run left behind and finds the journals of their attempts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::ExecutorError;

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
