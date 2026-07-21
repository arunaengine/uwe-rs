//! The cache entry of a task: the outputs of a successful execution, the run and task that made
//! them, and how long the task ran. A stored entry answers a request only while every file it
//! names is still in the store.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use content_store::Store;
use serde::{Deserialize, Serialize};
use task_model::Digest;

use crate::{RunnerError, StoredFile, TaskResult};

/// A cache entry, as `ac/v1/<namespace>/<key>.json` holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    /// The run that made the result.
    pub run: String,
    /// The task in that run, as the store names its folder, like `FASTQC-12`.
    pub task: String,
    pub files: Vec<StoredFile>,
    pub stdout: Option<Digest>,
    pub stderr: Option<Digest>,
    /// How long the task ran, in milliseconds; entries of earlier versions do not tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milliseconds: Option<u64>,
}

impl CacheEntry {
    pub fn parse(key: &Digest, bytes: &[u8]) -> Result<CacheEntry, RunnerError> {
        serde_json::from_slice(bytes).map_err(|error| RunnerError::Entry {
            key: key.clone(),
            message: error.to_string(),
        })
    }

    pub fn to_bytes(&self, key: &Digest) -> Result<Vec<u8>, RunnerError> {
        serde_json::to_vec(self).map_err(|error| RunnerError::Entry {
            key: key.clone(),
            message: error.to_string(),
        })
    }

    /// The result that the entry gives a request.
    pub fn result(self) -> TaskResult {
        TaskResult {
            exit_code: Some(0),
            files: self.files,
            stdout: self.stdout,
            stderr: self.stderr,
            reused: true,
        }
    }

    /// Whether the store still has every file of the entry: each output, the manifest of each
    /// directory, and the captured streams.
    pub async fn complete(&self, store: &Store) -> Result<bool, RunnerError> {
        for file in &self.files {
            // In an object store, the location of a directory is only the prefix of its files,
            // no object itself; its manifest stands for it there.
            let checkable = !file.directory || store.local_path(&file.location).is_some();
            let present = (!file.directory || store.has_tree(&file.digest).await?)
                && (!checkable || store.exists(&file.location).await?);
            if !present {
                return Ok(false);
            }
        }
        for digest in self.stdout.iter().chain(&self.stderr) {
            if !store.has_content(digest).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// What to do with an entry whose files are gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code, reason = "a lookup of the cache leaves stale entries")]
pub(crate) enum Stale {
    /// Remove it, so that the task runs again and stores a new entry.
    Remove,
    /// Leave it, as a lookup without side effects does.
    Keep,
}

/// The stored entry of a key, when it is complete.
pub(crate) async fn stored(
    store: &Store,
    namespace: &str,
    key: &Digest,
    stale: Stale,
) -> Result<Option<CacheEntry>, RunnerError> {
    let Some(bytes) = store.get_entry(namespace, key).await? else {
        return Ok(None);
    };
    let entry = CacheEntry::parse(key, &bytes)?;
    if entry.complete(store).await? {
        return Ok(Some(entry));
    }
    if stale == Stale::Remove {
        store.remove_entry(namespace, key).await?;
    }
    Ok(None)
}
