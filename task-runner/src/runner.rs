//! One runner per run. It hands tasks to an executor one at a time and keeps the cache.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use content_store::{Store, StoreError};
use executor::{Backend, ExecutorError, Job};
use serde::{Deserialize, Serialize};
use storage::{Storage, StorageError};
use task_model::{Digest, ReuseScope, SpecError, TaskRequest, Tree, TreeEntry};
use tokio::sync::OnceCell;

use tokio_util::sync::CancellationToken;

use crate::outputs::{self, Found};

/// An output of a task result, in the store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFile {
    /// The path as the task named it: relative to the work directory, or absolute.
    pub path: String,
    /// The digest of the file, or of the manifest of a directory.
    pub digest: Digest,
    /// The size of the file, or of all files in the directory.
    pub size: u64,
    /// Where the store keeps it under the run that made it; for a directory, the prefix of
    /// its files.
    pub location: String,
    pub directory: bool,
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
    /// A file of this machine could not be read or written.
    #[error("{}: {error}", path.display())]
    File {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("cannot read the cache entry {key}: {message}")]
    Entry { key: Digest, message: String },
    #[error("the input {0} is a remote directory, which iunctus cannot read yet")]
    Remote(String),
    /// A remote input could not be read through the storage providers.
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("the name of `{0}` is not UTF-8")]
    Name(PathBuf),
    #[error("the directory manifest of the input `{0}` names a path outside the directory")]
    Tree(String),
}

impl RunnerError {
    /// Whether the cause is a feature that the runner does not support yet.
    pub fn unsupported(&self) -> bool {
        match self {
            RunnerError::Remote(_) => true,
            RunnerError::Storage(error) => error.unsupported(),
            _ => false,
        }
    }

    /// Whether the machine failed rather than the task, so that another attempt may work.
    pub fn retryable(&self) -> bool {
        match self {
            RunnerError::Executor(error) => error.retryable(),
            RunnerError::Storage(error) => error.retryable(),
            _ => false,
        }
    }
}

/// How a runner treats the inputs of tasks.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Where remote inputs, and inputs of a remote store, are downloaded before a task.
    pub downloads: PathBuf,
    /// Whether local input files are copied into the store, as tasks on another machine
    /// need. A backend on this machine reads them where they are, so their digests are enough.
    pub import_inputs: bool,
    /// The storage providers that remote inputs come from.
    pub storage: Storage,
}

/// Where the content of an input comes from, for staging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// A local file or directory, which a backend on this machine shows as it is.
    Local(PathBuf),
    /// Literal content of the workflow, which the task gets a copy of.
    Content,
    /// Only the store has it.
    Stored,
}

/// A task request, with where the content of each input comes from.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub request: TaskRequest,
    /// One per input of the specification, in its order.
    pub(crate) sources: Vec<Source>,
}

/// An image pinned to its digest once per run, or `None` without one.
type Pinned = OnceCell<Option<String>>;

/// The runner of a run. Its clones share the backend, so that tasks run at the same time.
pub struct Runner<B> {
    pub(crate) store: Store,
    backend: Arc<B>,
    run: String,
    pub(crate) settings: Settings,
    pinned: Arc<Mutex<HashMap<String, Arc<Pinned>>>>,
}

impl<B> Clone for Runner<B> {
    fn clone(&self) -> Self {
        Runner {
            store: self.store.clone(),
            backend: self.backend.clone(),
            run: self.run.clone(),
            settings: self.settings.clone(),
            pinned: self.pinned.clone(),
        }
    }
}

impl<B: Backend> Runner<B> {
    /// A runner for the run `run`, whose outputs go to `runs/<run>/` in the store.
    pub fn new(store: Store, backend: B, run: &str, settings: Settings) -> Self {
        Runner {
            store,
            backend: Arc::new(backend),
            run: run.to_owned(),
            settings,
            pinned: Arc::default(),
        }
    }

    /// The image pinned to its digest where it has one, found once per run.
    pub async fn pin(&self, image: &str) -> Result<String, RunnerError> {
        let cell = {
            let mut pinned = self
                .pinned
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            pinned.entry(image.to_owned()).or_default().clone()
        };
        let digest = cell.get_or_try_init(|| self.backend.pin(image)).await?;
        Ok(digest.clone().unwrap_or_else(|| image.to_owned()))
    }

    /// Stores a local file by its content, for tasks that take it as input.
    pub async fn import(&self, file: &Path) -> Result<(Digest, u64), RunnerError> {
        Ok(self.store.put_file(file).await?)
    }

    /// Stores bytes by their content, like a task script.
    pub async fn import_bytes(&self, bytes: &[u8]) -> Result<Digest, RunnerError> {
        Ok(self
            .store
            .put_content(Bytes::copy_from_slice(bytes))
            .await?)
    }

    /// The manifest of a local directory, which is stored, and the size of its files. The
    /// files are stored by their content too when the runner imports inputs.
    pub async fn import_directory(&self, directory: &Path) -> Result<(Digest, u64), RunnerError> {
        let mut entries = Vec::new();
        for found in outputs::walk(directory).await? {
            entries.push(match found {
                Found::File {
                    relative,
                    path,
                    executable,
                } => {
                    let (digest, size) = match self.settings.import_inputs {
                        true => self.store.put_file(&path).await?,
                        false => content_store::hash_file(&path).await?,
                    };
                    TreeEntry::File {
                        path: relative,
                        digest,
                        size,
                        executable,
                    }
                }
                Found::EmptyDirectory { relative } => TreeEntry::EmptyDirectory { path: relative },
            });
        }
        let tree = Tree { entries };
        let size = tree.size();
        Ok((self.store.put_tree(&tree).await?, size))
    }

    /// Stores a directory manifest, like the one of an empty directory.
    pub async fn import_tree(&self, tree: &Tree) -> Result<Digest, RunnerError> {
        Ok(self.store.put_tree(tree).await?)
    }

    /// Reads an object of the store, like a stored output.
    pub async fn read(&self, location: &str) -> Result<Bytes, RunnerError> {
        Ok(self.store.get_object(location).await?)
    }

    /// Reads content by its digest, like a captured standard output.
    pub async fn read_content(&self, digest: &Digest) -> Result<Bytes, RunnerError> {
        Ok(self.store.get_content(digest).await?)
    }

    /// The backend that runs the tasks.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Where an object is on disk, for a local store.
    pub fn local_path(&self, location: &str) -> Option<PathBuf> {
        self.store.local_path(location)
    }

    /// Runs a request, unless the cache has a result for it that the reuse scope allows. The
    /// task stops when `cancel` fires.
    pub async fn execute(
        &self,
        prepared: &Prepared,
        cancel: &CancellationToken,
    ) -> Result<TaskResult, RunnerError> {
        let request = &prepared.request;
        let key = request.spec.cache_key()?;
        let namespace = request.spec.namespace.as_str();
        if request.scope == ReuseScope::Shared
            && let Some(bytes) = self.store.get_entry(namespace, &key).await?
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
        let staging = self.stage(prepared).await?;
        let name = format!("{}-{task}-{}", self.run, request.attempt);
        let job = Job {
            name: &name,
            spec: &request.spec,
            staging: &staging,
            placement: &request.placement,
        };
        let finished = self.backend.run(job, cancel).await?;
        let stored =
            outputs::store_outputs(&self.store, &self.run, &finished, &task, request.attempt).await;
        (tokio::fs::remove_dir_all(&finished.root).await).map_err(at(&finished.root))?;
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
            self.store.put_entry(namespace, &key, bytes.into()).await?;
        }
        Ok(result)
    }
}

/// An error of a file operation on this machine, with the path of the file.
pub(crate) fn at(path: &Path) -> impl FnOnce(std::io::Error) -> RunnerError + '_ {
    move |error| RunnerError::File {
        path: path.to_owned(),
        error,
    }
}

/// A task identity as one path segment of the store.
pub(crate) fn segment(identity: &str) -> String {
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
