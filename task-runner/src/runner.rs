//! One runner per run. It hands tasks to an executor one at a time and keeps the cache.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use content_store::{Holder, Lease, Store, StoreError, layout};
use executor::{Backend, ExecutorError, Job};
use serde::{Deserialize, Serialize};
use storage::{Storage, StorageError};
use task_model::{Digest, ReuseScope, SpecError, TaskRequest, Tree};
use tokio::sync::OnceCell;

use tokio_util::sync::CancellationToken;

use crate::entry::{self, CacheEntry, Stale};
use crate::inputs::{Inputs, Mode};
use crate::outputs;

/// How long a task first waits for another run that runs the same task, before it looks for
/// the result again; each wait is twice as long as the one before, up to [`LONGEST_PAUSE`].
const FIRST_PAUSE: Duration = Duration::from_millis(100);
const LONGEST_PAUSE: Duration = Duration::from_secs(2);

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
    /// What the attempt used, when the backend measured it.
    pub usage: Option<executor::Usage>,
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
    /// A lookup of the cache cannot know a remote input that no run downloaded yet.
    #[error("the input {0} is not downloaded yet")]
    NotDownloaded(String),
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
    /// The lease of the run in the store, which it holds from its first task on, so that
    /// clean-up knows that the run goes on.
    lease: Arc<OnceCell<Option<Lease>>>,
}

/// Whose turn it is to run a task whose key another run may run at the same moment.
enum Turn {
    /// The other run stored the result, which answers this request too.
    Reused(TaskResult),
    /// This run runs the task, and holds the lease of its key until it has stored the result.
    Ours(Lease),
}

impl<B> Clone for Runner<B> {
    fn clone(&self) -> Self {
        Runner {
            store: self.store.clone(),
            backend: self.backend.clone(),
            run: self.run.clone(),
            settings: self.settings.clone(),
            pinned: self.pinned.clone(),
            lease: self.lease.clone(),
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
            lease: Arc::default(),
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
        self.inputs().bytes(bytes).await
    }

    /// The manifest of a local directory, which is stored, and the size of its files. The
    /// files are stored by their content too when the runner imports inputs.
    pub async fn import_directory(&self, directory: &Path) -> Result<(Digest, u64), RunnerError> {
        self.inputs().directory(directory).await
    }

    /// Stores a directory manifest, like the one of an empty directory.
    pub async fn import_tree(&self, tree: &Tree) -> Result<Digest, RunnerError> {
        self.inputs().tree(tree).await
    }

    /// How a run makes the digests of inputs.
    fn inputs(&self) -> Inputs<'_> {
        Inputs {
            store: &self.store,
            settings: &self.settings,
            mode: Mode::Run,
        }
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

    /// Runs a request, unless the cache has a result for it that the reuse scope allows. When
    /// another run, or another task of this run, runs the same task at the same moment, the
    /// request waits for its result instead of running it too. The task stops when `cancel`
    /// fires.
    pub async fn execute(
        &self,
        prepared: &Prepared,
        cancel: &CancellationToken,
    ) -> Result<TaskResult, RunnerError> {
        self.hold_run_lease().await;
        let request = &prepared.request;
        let key = request.spec.cache_key()?;
        let namespace = request.spec.namespace.as_str();
        let _turn = match request.scope {
            ReuseScope::Shared => {
                if let Some(result) = self.reuse(namespace, &key).await? {
                    return Ok(result);
                }
                match self
                    .turn(namespace, &key, &request.identity, cancel)
                    .await?
                {
                    Turn::Reused(result) => return Ok(result),
                    Turn::Ours(lease) => Some(lease),
                }
            }
            ReuseScope::Lineage | ReuseScope::None => None,
        };
        let task = segment(&request.identity);
        let staging = self.stage(prepared).await?;
        let name = format!("{}-{task}-{}", self.run, request.attempt);
        let job = Job {
            name: &name,
            spec: &request.spec,
            staging: &staging,
            placement: &request.placement,
        };
        let begun = Instant::now();
        let finished = self.backend.run(job, cancel).await?;
        let milliseconds = begun.elapsed().as_millis() as u64;
        let stored =
            outputs::store_outputs(&self.store, &self.run, &finished, &task, request.attempt).await;
        (tokio::fs::remove_dir_all(&finished.root).await).map_err(at(&finished.root))?;
        let result = stored?;
        if result.exit_code == Some(0) && request.scope != ReuseScope::None {
            let entry = CacheEntry {
                run: self.run.clone(),
                task,
                files: result.files.clone(),
                stdout: result.stdout.clone(),
                stderr: result.stderr.clone(),
                milliseconds: Some(milliseconds),
            };
            let bytes = entry.to_bytes(&key)?;
            self.store.put_entry(namespace, &key, bytes.into()).await?;
        }
        Ok(result)
    }

    /// The stored result of a key, when every file of it is still there. An entry whose files
    /// are gone is removed, so that the task runs again. A result that answers a request is
    /// marked as used now, for clean-up by the time of the last use.
    async fn reuse(
        &self,
        namespace: &str,
        key: &Digest,
    ) -> Result<Option<TaskResult>, RunnerError> {
        let Some(found) = entry::stored(&self.store, namespace, key, Stale::Remove).await? else {
            return Ok(None);
        };
        // A mark that cannot be written only makes the entry look older to clean-up.
        let _ = self.store.mark_used(namespace, key, &self.run).await;
        Ok(Some(found.result()))
    }

    /// Waits until this run may run the task of `key`: while another run, or another task of
    /// this run, runs the same task, it holds the lease of the key. When it has stored the
    /// result, that result answers this request too; when it failed or stopped, this run takes
    /// the lease and runs the task itself. The lease of a run that crashed ends with its process
    /// (see [`content_store::Lease`]).
    async fn turn(
        &self,
        namespace: &str,
        key: &Digest,
        identity: &str,
        cancel: &CancellationToken,
    ) -> Result<Turn, RunnerError> {
        let name = format!("tasks/{}/{}", layout::namespace_name(namespace), key.hex());
        let holder = Holder::new(&self.run, identity);
        let mut pause = FIRST_PAUSE;
        loop {
            if let Some(lease) = self.store.try_lease(&name, &holder).await? {
                // The other run may have stored the result just before it gave the lease back.
                if let Some(result) = self.reuse(namespace, key).await? {
                    return Ok(Turn::Reused(result));
                }
                return Ok(Turn::Ours(lease));
            }
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                () = cancel.cancelled() => {
                    return Err(RunnerError::Executor(ExecutorError::Cancelled));
                }
            }
            pause = (pause * 2).min(LONGEST_PAUSE);
            if let Some(result) = self.reuse(namespace, key).await? {
                return Ok(Turn::Reused(result));
            }
        }
    }

    /// Takes the lease of the run in the store once, before its first task. A store that gives
    /// no lease costs only the protection from a clean-up that runs at the same time.
    async fn hold_run_lease(&self) {
        let name = format!("runs/{}", self.run);
        let holder = Holder::new(&self.run, "run");
        let take = async || self.store.try_lease(&name, &holder).await.ok().flatten();
        self.lease.get_or_init(take).await;
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
