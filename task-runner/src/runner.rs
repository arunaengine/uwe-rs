//! One runner per run. It hands tasks to an executor one at a time and keeps the cache.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use bytes::Bytes;
use content_store::{Store, StoreError, layout};
use executor::{Executor, ExecutorError, Finished, Staged, Staging};
use serde::{Deserialize, Serialize};
use task_model::{Digest, FileKind, ReuseScope, SpecError, TaskRequest, TaskSpec};

/// A file of a task result, in the store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFile {
    /// The path relative to the work directory.
    pub path: String,
    pub digest: Digest,
    pub size: u64,
    /// Where the store keeps it under the run that made it.
    pub location: String,
}

/// What a task produced, now or in an earlier run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskResult {
    /// The tool's exit code; `None` when the task was killed before it recorded one.
    pub exit_code: Option<i32>,
    pub files: Vec<StoredFile>,
    /// The digests of the captured standard output and error, when the task got that far.
    pub stdout: Option<Digest>,
    pub stderr: Option<Digest>,
    /// Whether a stored result answered the request.
    pub reused: bool,
}

/// A cache entry: the outputs of a successful execution, and the run and task that made them.
#[derive(Serialize, Deserialize)]
struct Entry {
    run: String,
    task: String,
    files: Vec<StoredFile>,
    stdout: Option<Digest>,
    stderr: Option<Digest>,
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Executor(#[from] ExecutorError),
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("cannot read the cache entry {key}: {message}")]
    Entry { key: Digest, message: String },
    #[error("directory inputs are not supported yet: {0}")]
    Directory(String),
}

pub struct Runner<E> {
    store: Store,
    executor: E,
    runtime: tokio::runtime::Runtime,
    run: String,
    /// Where inputs of a remote store are downloaded before a task.
    downloads: PathBuf,
}

impl<E: Executor> Runner<E> {
    /// A runner for the run `run`, whose outputs go to `runs/<run>/` in the store.
    pub fn new(
        store: Store,
        executor: E,
        run: &str,
        downloads: &Path,
    ) -> Result<Self, RunnerError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Self {
            store,
            executor,
            runtime,
            run: run.to_owned(),
            downloads: downloads.to_owned(),
        })
    }

    /// Stores a local file by its content, for tasks that take it as input.
    pub fn import(&self, file: &Path) -> Result<(Digest, u64), RunnerError> {
        Ok(self.runtime.block_on(self.store.put_file(file))?)
    }

    /// Stores bytes by their content, like a task script.
    pub fn import_bytes(&self, bytes: &[u8]) -> Result<Digest, RunnerError> {
        Ok(self
            .runtime
            .block_on(self.store.put_content(Bytes::copy_from_slice(bytes)))?)
    }

    /// Reads an object of the store, like a stored output.
    pub fn read(&self, location: &str) -> Result<Bytes, RunnerError> {
        Ok(self.runtime.block_on(self.store.get_object(location))?)
    }

    /// Reads content by its digest, like a captured standard output.
    pub fn read_content(&self, digest: &Digest) -> Result<Bytes, RunnerError> {
        Ok(self.runtime.block_on(self.store.get_content(digest))?)
    }

    /// The executor that runs the tasks.
    pub fn executor(&self) -> &E {
        &self.executor
    }

    /// Where an object is on disk, for a local store.
    pub fn local_path(&self, location: &str) -> Option<PathBuf> {
        self.store.local_path(location)
    }

    /// Runs a request, unless the cache has a result for it that the reuse scope allows.
    pub fn execute(&self, request: &TaskRequest) -> Result<TaskResult, RunnerError> {
        let key = request.spec.cache_key()?;
        let namespace = request.spec.namespace.as_str();
        if request.scope == ReuseScope::Shared
            && let Some(bytes) = self
                .runtime
                .block_on(self.store.get_entry(namespace, &key))?
        {
            let entry: Entry =
                serde_json::from_slice(&bytes).map_err(|error| RunnerError::Entry {
                    key: key.clone(),
                    message: error.to_string(),
                })?;
            return Ok(TaskResult {
                exit_code: Some(0),
                files: entry.files,
                stdout: entry.stdout,
                stderr: entry.stderr,
                reused: true,
            });
        }
        let task = segment(&request.identity);
        let staging = self.stage(&request.spec)?;
        let name = format!("{}-{task}-{}", self.run, request.attempt);
        let finished = self.executor.run(&name, &request.spec, &staging)?;
        let stored = self.store_outputs(&finished, &task, request.attempt);
        std::fs::remove_dir_all(&finished.root)?;
        let result = stored?;
        if result.exit_code == Some(0) && request.scope != ReuseScope::None {
            let entry = Entry {
                run: self.run.clone(),
                task,
                files: result.files.clone(),
                stdout: result.stdout.clone(),
                stderr: result.stderr.clone(),
            };
            let bytes = serde_json::to_vec(&entry).map_err(|error| RunnerError::Entry {
                key: key.clone(),
                message: error.to_string(),
            })?;
            self.runtime
                .block_on(self.store.put_entry(namespace, &key, bytes.into()))?;
        }
        Ok(result)
    }

    /// What the executor places for a specification: each input file, and the standard input.
    fn stage(&self, spec: &TaskSpec) -> Result<Staging, RunnerError> {
        let mut staging = Staging::default();
        for input in &spec.inputs {
            if input.kind == FileKind::Directory {
                return Err(RunnerError::Directory(input.path.clone()));
            }
            staging.files.push(Staged {
                path: input.path.clone(),
                source: self.local_content(&input.digest)?,
                executable: input.executable,
            });
        }
        if let Some(digest) = &spec.stdin {
            staging.stdin = Some(self.local_content(digest)?);
        }
        Ok(staging)
    }

    /// A local file with the content of a digest: the store's own file for a local store, else
    /// a download.
    fn local_content(&self, digest: &Digest) -> Result<PathBuf, RunnerError> {
        if let Some(path) = self.store.local_path(&layout::content(digest)) {
            return Ok(path);
        }
        let path = self.downloads.join(digest.hex());
        if !path.exists() {
            std::fs::create_dir_all(&self.downloads)?;
            let bytes = self.runtime.block_on(self.store.get_content(digest))?;
            // A name of this process first, then a rename, so that another run that downloads
            // the same content never reads half a file.
            let part = self
                .downloads
                .join(format!("{}.{}", digest.hex(), std::process::id()));
            std::fs::write(&part, bytes)?;
            std::fs::rename(&part, &path)?;
        }
        Ok(path)
    }

    /// Stores the outputs of a finished task under its attempt, reading each file in parts,
    /// and the captured streams by their content.
    fn store_outputs(
        &self,
        finished: &Finished,
        task: &str,
        attempt: u32,
    ) -> Result<TaskResult, RunnerError> {
        let mut files = Vec::new();
        for collected in &finished.files {
            let local = finished.local_path(&collected.path);
            let location = self.runtime.block_on(self.store.put_output_file(
                &self.run,
                task,
                attempt,
                &collected.path,
                &local,
            ))?;
            let (digest, size) = self.runtime.block_on(self.store.put_file(&local))?;
            files.push(StoredFile {
                path: collected.path.clone(),
                digest,
                size,
                location,
            });
        }
        let log = |file: PathBuf| -> Result<Option<Digest>, RunnerError> {
            if !file.is_file() {
                return Ok(None);
            }
            Ok(Some(self.runtime.block_on(self.store.put_file(&file))?.0))
        };
        Ok(TaskResult {
            exit_code: finished.exit_code,
            files,
            stdout: log(finished.stdout())?,
            stderr: log(finished.stderr())?,
            reused: false,
        })
    }
}

/// A task identity as one path segment of the store.
fn segment(identity: &str) -> String {
    identity
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}
