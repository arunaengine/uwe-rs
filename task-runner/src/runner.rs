//! One runner per run. It hands tasks to an executor one at a time and keeps the cache.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use content_store::{Holder, Lease, Store, StoreError, layout};
use executor::{Backend, ExecutorError, Job};
use serde::{Deserialize, Serialize};
use storage::{Storage, StorageError};
use task_model::{Digest, ReuseScope, SpecError, TaskRequest, Tree};
use tokio::sync::OnceCell;

use tokio_util::sync::CancellationToken;
use workflow_graph::{ReproducibilityCheck, Verdict};

use crate::check;
use crate::digests::DigestCache;
use crate::entry::{self, CacheEntry, Stale};
use crate::inputs::{Inputs, Mode, Snapshots};
use crate::outputs;
use crate::settlement::{Candidate, Source as CandidateSource, timeout};

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
    /// When the tool of the attempt started and ended, as its wrapper wrote it; `None` for a
    /// stored result.
    pub started: Option<SystemTime>,
    pub ended: Option<SystemTime>,
    /// When the result came from the cache and the task ran again to check that its tool
    /// gives the same bytes: what it gave.
    pub check: Option<ReproducibilityCheck>,
    /// How the containers of a task that runs several ended, in the order of the task, the
    /// main one first.
    pub containers: Vec<ContainerResult>,
    /// Where other tasks reach a daemon task, which runs on after its result.
    pub address: Option<String>,
}

/// How a container of a task that runs several ended.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContainerResult {
    pub name: String,
    /// Its exit code; `None` when it recorded none.
    pub exit_code: Option<i32>,
    /// The digests of its captured standard output and error, when it got that far.
    pub stdout: Option<Digest>,
    pub stderr: Option<Digest>,
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
    /// A local input of the task is not there.
    #[error("the input {} does not exist", .0.display())]
    Missing(PathBuf),
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
    /// Only a file or directory of this machine can be changed in place.
    #[error("the input {0} is not on this machine, so a task cannot change it where it is")]
    NotInPlace(String),
}

impl RunnerError {
    /// Whether the cause is a feature that the runner does not support yet.
    pub fn unsupported(&self) -> bool {
        match self {
            RunnerError::Remote(_) | RunnerError::NotInPlace(_) => true,
            RunnerError::Executor(ExecutorError::Unsupported(_)) => true,
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
    /// need. A backend on this machine reads persistent input snapshots.
    pub import_inputs: bool,
    /// The storage providers that remote inputs come from.
    pub storage: Storage,
    /// The digests of local files that earlier runs read, so that an input that did not change
    /// is not read again; `None` reads every input.
    pub digests: Option<DigestCache>,
}

/// Where the content of an input comes from, for staging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// A persistent snapshot of a local file or directory.
    Local(PathBuf),
    /// Literal content of the workflow, which the task gets a copy of.
    Content,
    /// Only the store has it.
    Stored,
    /// A local file or directory that the task changes where it is.
    InPlace(PathBuf),
}

/// A task request, with where the content of each input comes from.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub request: TaskRequest,
    /// One per input of the specification, in its order.
    pub(crate) sources: Vec<Source>,
    /// The step of the task, which names its tool together with its image.
    pub(crate) step: String,
    /// How many tasks like this one may run one after another in one container: more than 1
    /// for a short task (see [`crate::together`]).
    pub(crate) per_container: usize,
    /// Whether a stored result may answer the request. When not, the task runs, and its
    /// result is stored all the same.
    pub(crate) reuse: bool,
    /// The private copies of its inputs, which go with the last clone of the request.
    pub(crate) snapshots: Snapshots,
}

/// An image pinned to its digest once per run, or `None` without one.
type Pinned = OnceCell<Option<String>>;

/// The runner of a run. Its clones share the backend, so that tasks run at the same time.
pub struct Runner<B> {
    pub(crate) store: Store,
    pub(crate) backend: Arc<B>,
    run: String,
    pub(crate) settings: Settings,
    pinned: Arc<Mutex<HashMap<String, Arc<Pinned>>>>,
    /// The lease of the run in the store, which it holds from its first task on, so that
    /// clean-up knows that the run goes on.
    lease: Arc<OnceCell<Lease>>,
    lost: CancellationToken,
    /// The share of results from the cache that run again for the reproducibility check, from
    /// 0 (none) to 1 (all).
    check: f64,
    /// Short tasks that wait for their group, and how many CPUs the groups may use at once.
    pub(crate) gathering: crate::together::Gathering,
    pub(crate) slots: usize,
    /// Whether a line says when a group of short tasks starts.
    pub(crate) verbose_groups: bool,
    /// The runs of the lineage that a resumed run continues, whose results answer tasks of
    /// images without a digest too.
    lineage: Arc<BTreeSet<String>>,
}

/// Whose turn it is to run a task whose key another run may run at the same moment.
enum Turn {
    /// The other run stored the result, which answers this request too.
    Reused(entry::Observed),
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
            lost: self.lost.clone(),
            check: self.check,
            gathering: self.gathering.clone(),
            slots: self.slots,
            verbose_groups: self.verbose_groups,
            lineage: self.lineage.clone(),
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
            lost: CancellationToken::new(),
            check: 0.0,
            gathering: Arc::default(),
            slots: std::thread::available_parallelism().map_or(1, usize::from),
            verbose_groups: false,
            lineage: Arc::default(),
        }
    }

    /// The id of the run.
    pub fn run_id(&self) -> &str {
        &self.run
    }

    /// Lets the results of these runs answer tasks whose image has no digest, as a resumed run
    /// takes the results of the runs that it continues.
    pub fn resume_from(&mut self, runs: BTreeSet<String>) {
        self.lineage = Arc::new(runs);
    }

    /// Runs a share of the tasks whose results come from the cache again, from 0 (none) to 1
    /// (all), and compares their outputs with the stored ones (the reproducibility check).
    pub fn check_reproducibility(&mut self, share: f64) {
        self.check = share;
    }

    /// Pins images to the digests given, instead of asking the engine or the registry, as a
    /// repeat of a run pins them to the digests that the run ran at.
    pub fn pin_images(&self, images: &HashMap<String, String>) {
        let mut pinned = self
            .pinned
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for (image, digest) in images {
            let known = Pinned::new_with(Some(Some(digest.clone())));
            pinned.insert(image.clone(), Arc::new(known));
        }
    }

    /// The image pinned to its digest where it has one, found once per run.
    pub async fn pin(&self, image: &str) -> Result<String, RunnerError> {
        if task_model::conda_image(image) {
            let pinned = self.backend.pin(image).await?;
            return Ok(pinned.unwrap_or_else(|| image.to_owned()));
        }
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
        let admission = self.admit().await?;
        self.phase(None, Some(&admission), None, self.store.put_file(file))
            .await
    }

    /// Stores bytes by their content, like a task script.
    pub async fn import_bytes(&self, bytes: &[u8]) -> Result<Digest, RunnerError> {
        let admission = self.admit().await?;
        self.phase(None, Some(&admission), None, self.inputs().bytes(bytes))
            .await
    }

    /// The manifest of a local directory, which is stored, and the size of its files. The
    /// files are stored by their content too when the runner imports inputs.
    pub async fn import_directory(&self, directory: &Path) -> Result<(Digest, u64), RunnerError> {
        let admission = self.admit().await?;
        self.phase(
            None,
            Some(&admission),
            None,
            self.inputs().directory(directory),
        )
        .await
    }

    /// Stores a directory manifest, like the one of an empty directory.
    pub async fn import_tree(&self, tree: &Tree) -> Result<Digest, RunnerError> {
        let admission = self.admit().await?;
        self.phase(None, Some(&admission), None, self.inputs().tree(tree))
            .await
    }

    /// How a run makes the digests of inputs.
    pub(crate) fn inputs(&self) -> Inputs<'_> {
        Inputs {
            store: &self.store,
            settings: &self.settings,
            mode: Mode::Run,
            snapshots: Snapshots::new(&self.settings, &self.run),
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

    /// Runs a request, unless the cache has a result for it that the reuse scope allows.
    pub async fn execute(
        &self,
        prepared: &Prepared,
        cancel: &CancellationToken,
    ) -> Result<TaskResult, RunnerError> {
        Ok(self.execute_inner(prepared, cancel, false).await?.0)
    }

    pub(crate) async fn execute_inner(
        &self,
        prepared: &Prepared,
        cancel: &CancellationToken,
        deferred: bool,
    ) -> Result<(TaskResult, Option<Candidate>), RunnerError> {
        let mut admission = Some(tokio::select! {
            result = self.admit() => result?,
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled.into()),
        });
        let request = &prepared.request;
        let key = request.spec.cache_key()?;
        let namespace = request.spec.namespace.as_str();
        // A tool that gave other bytes from the same inputs shares no results.
        let tool = match (&request.spec.image, request.scope) {
            (Some(image), ReuseScope::Shared) => {
                Some(check::tool(&image.reference, &prepared.step))
            }
            _ => None,
        };
        let shared = match &tool {
            Some(tool) => self
                .phase(
                    None,
                    admission.as_ref(),
                    Some(cancel),
                    self.store.unreproducible(tool),
                )
                .await?
                .is_none(),
            None => request.scope == ReuseScope::Shared,
        };
        if request.scope == ReuseScope::Lineage && prepared.reuse && !self.lineage.is_empty() {
            let lookup = self.reuse_lineage(namespace, &key);
            let found = self
                .phase(None, admission.as_ref(), Some(cancel), lookup)
                .await?;
            if let Some(found) = found {
                let candidate = deferred.then(|| Candidate {
                    namespace: namespace.to_owned(),
                    key,
                    source: CandidateSource::Hit(found.snapshot),
                });
                return Ok((found.entry.result(), candidate));
            }
        }
        let (earlier, _turn) = match shared && prepared.reuse {
            true => {
                let found = match self
                    .phase(
                        None,
                        admission.as_ref(),
                        Some(cancel),
                        self.reuse_guarded(namespace, &key),
                    )
                    .await?
                {
                    Some(entry) => Turn::Reused(entry),
                    None => {
                        drop(admission.take());
                        tokio::select! {
                            result = self.phase(None, None, Some(cancel), self.turn(namespace, &key, &request.identity, cancel)) => result?,
                            () = cancel.cancelled() => return Err(ExecutorError::Cancelled.into()),
                        }
                    }
                };
                match found {
                    Turn::Reused(entry) if check::chosen(self.check, &self.run, &key) => {
                        (Some(entry), None)
                    }
                    Turn::Reused(found) => {
                        let candidate = deferred.then(|| Candidate {
                            namespace: namespace.to_owned(),
                            key,
                            source: CandidateSource::Hit(found.snapshot),
                        });
                        return Ok((found.entry.result(), candidate));
                    }
                    Turn::Ours(lease) => (None, Some(lease)),
                }
            }
            false => (None, None),
        };
        drop(admission);
        // Task-lease waiting releases the admission gate; staging acquires it again.
        let admission = tokio::select! {
            result = self.admit() => result?,
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled.into()),
        };
        let task = segment(&request.identity);
        let staging = self
            .phase(
                _turn.as_ref(),
                Some(&admission),
                Some(cancel),
                self.stage(prepared),
            )
            .await?;
        drop(admission);
        let name = format!("{}-{task}-{}", self.run, request.attempt);
        let job = Job {
            name: &name,
            spec: &request.spec,
            staging: &staging,
            placement: &request.placement,
        };
        let begun = Instant::now();
        let stop = cancel.child_token();
        let work = async {
            match prepared.per_container > 1 && crate::together::joinable(&job) {
                true => self.together(job, prepared.per_container, &stop).await,
                false => self.backend.run(job, &stop).await,
            }
        };
        let finished = self.work(_turn.as_ref(), cancel, &stop, work).await?;
        let milliseconds = begun.elapsed().as_millis() as u64;
        let digests = self.settings.digests.as_ref();
        let stored = self
            .phase(
                _turn.as_ref(),
                None,
                Some(cancel),
                outputs::store_outputs(
                    &self.store,
                    &self.run,
                    &finished,
                    &task,
                    request.attempt,
                    digests,
                    &staging,
                ),
            )
            .await;
        let stored = match stored {
            Ok(mut result) => {
                let (spec, streams) = (&request.spec, (&result.stdout, &result.stderr));
                let containers = outputs::store_containers(&self.store, spec, &finished, streams);
                self.phase(_turn.as_ref(), None, Some(cancel), containers)
                    .await
                    .map(|containers| {
                        result.containers = containers;
                        result
                    })
            }
            Err(error) => Err(error),
        };
        let result = stored?;
        // A daemon still runs in its scratch directory; its backend removes it when it stops.
        if finished.address.is_none() {
            self.phase(_turn.as_ref(), None, Some(cancel), async {
                tokio::fs::remove_dir_all(&finished.root)
                    .await
                    .map_err(at(&finished.root))
            })
            .await?;
        }
        if let Some(earlier) = earlier {
            let result = self
                .phase(
                    _turn.as_ref(),
                    None,
                    Some(cancel),
                    self.checked(earlier.entry, &result, tool.as_ref(), prepared, &task),
                )
                .await?;
            let candidate = deferred.then(|| Candidate {
                namespace: namespace.to_owned(),
                key,
                source: CandidateSource::Hit(earlier.snapshot),
            });
            return Ok((result, candidate));
        }
        let mut candidate = None;
        if let Some(exit_code) = result.exit_code
            && (deferred || exit_code == 0)
            && request.scope != ReuseScope::None
        {
            let entry = CacheEntry {
                run: self.run.clone(),
                task,
                exit_code,
                files: result.files.clone(),
                stdout: result.stdout.clone(),
                stderr: result.stderr.clone(),
                milliseconds: Some(milliseconds),
                containers: result.containers.clone(),
            };
            if deferred {
                candidate = Some(Candidate {
                    namespace: namespace.to_owned(),
                    key,
                    source: CandidateSource::Publish(entry),
                });
            } else {
                let bytes = entry.to_bytes(&key)?;
                let put = self.store.put_entry(namespace, &key, bytes.into());
                let limited = async {
                    let limit = Duration::from_secs(10);
                    match tokio::time::timeout(limit, put).await {
                        Ok(written) => written.map_err(RunnerError::from),
                        Err(_) => Err(timeout()),
                    }
                };
                // As settlement does: missing a cache write only lets a later run run it again.
                match self
                    .phase(_turn.as_ref(), None, Some(cancel), limited)
                    .await
                {
                    Ok(_) => {}
                    Err(error) if !cancel.is_cancelled() => eprintln!(
                        "iunctus: warning: the result of task {} is not cached: {error}",
                        entry.task
                    ),
                    Err(error) => return Err(error),
                }
            }
        }
        self.authority(_turn.as_ref(), None).await?;
        if cancel.is_cancelled() {
            return Err(ExecutorError::Cancelled.into());
        }
        // Ordered outcomes must not retain a lease that another pending attempt needs.
        drop(_turn);
        Ok((result, candidate))
    }

    /// The result in the cache, with what the task gave when it ran again for the
    /// reproducibility check. A tool that gave other bytes is marked in the store, so that no
    /// run shares its results any more.
    async fn checked(
        &self,
        earlier: CacheEntry,
        now: &TaskResult,
        tool: Option<&Digest>,
        prepared: &Prepared,
        task: &str,
    ) -> Result<TaskResult, RunnerError> {
        let verdict = check::verdict(&earlier, now);
        if let (Verdict::Differs { paths }, Some(tool), Some(image)) =
            (&verdict, tool, &prepared.request.spec.image)
        {
            let note = check::Note {
                image: &image.reference,
                step: &prepared.step,
                run: &self.run,
                task,
                earlier_run: &earlier.run,
                differing: paths,
            };
            let bytes = serde_json::to_vec_pretty(&note).map_err(std::io::Error::other)?;
            self.store.mark_unreproducible(tool, bytes.into()).await?;
        }
        let earlier_run = earlier.run.clone();
        Ok(TaskResult {
            check: Some(ReproducibilityCheck {
                earlier_run,
                verdict,
            }),
            ..earlier.result()
        })
    }

    /// The stored result of a key, when every file of it is still there. An entry whose files
    /// are gone is removed, so that the task runs again. A result that answers a request is
    /// marked as used now, for clean-up by the time of the last use.
    async fn reuse(
        &self,
        namespace: &str,
        key: &Digest,
    ) -> Result<Option<entry::Observed>, RunnerError> {
        let admission = self.admit().await?;
        self.phase(
            None,
            Some(&admission),
            None,
            self.reuse_guarded(namespace, key),
        )
        .await
    }

    async fn reuse_guarded(
        &self,
        namespace: &str,
        key: &Digest,
    ) -> Result<Option<entry::Observed>, RunnerError> {
        let Some(found) = entry::observed(&self.store, namespace, key, Stale::Remove).await? else {
            return Ok(None);
        };
        self.authority(None, None).await?;
        self.store.mark_used(namespace, key, &self.run).await?;
        Ok(Some(found))
    }

    /// The stored result of a key when a run of the lineage made it, marked as used.
    async fn reuse_lineage(
        &self,
        namespace: &str,
        key: &Digest,
    ) -> Result<Option<entry::Observed>, RunnerError> {
        let found = entry::observed(&self.store, namespace, key, Stale::Remove).await?;
        let Some(found) = found.filter(|found| self.lineage.contains(&found.entry.run)) else {
            return Ok(None);
        };
        self.authority(None, None).await?;
        self.store.mark_used(namespace, key, &self.run).await?;
        Ok(Some(found))
    }

    /// Waits until this run may run the task of `key`: while another run, or another task of
    /// this run, runs the same task, it holds the lease of the key.
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
                if let Some(entry) = self
                    .phase(Some(&lease), None, Some(cancel), self.reuse(namespace, key))
                    .await?
                {
                    return Ok(Turn::Reused(entry));
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
            if let Some(entry) = self.reuse(namespace, key).await? {
                return Ok(Turn::Reused(entry));
            }
        }
    }

    /// Takes run protection under the maintenance gate before reading or storing inputs.
    pub(crate) async fn admit(&self) -> Result<crate::clean::Maintenance, RunnerError> {
        let guard = self
            .phase(None, None, None, crate::clean::admission(&self.store))
            .await?;
        let name = format!("runs/{}", self.run);
        let holder = Holder::new(&self.run, "run");
        let take = async || {
            self.store.try_lease(&name, &holder).await?.ok_or_else(|| {
                RunnerError::Io(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "the run lease is already held",
                ))
            })
        };
        self.phase(None, Some(&guard), None, self.lease.get_or_try_init(take))
            .await?;
        Ok(guard)
    }

    async fn authority(
        &self,
        task: Option<&Lease>,
        guard: Option<&crate::clean::Maintenance>,
    ) -> Result<(), RunnerError> {
        if self.lost.is_cancelled() {
            return Err(StoreError::Location("run lease authority was lost".into()).into());
        }
        let checked = tokio::time::timeout(Duration::from_secs(5), async {
            if let Some(lease) = self.lease.get() {
                lease.check().await?;
            }
            if let Some(lease) = task {
                lease.check().await?;
            }
            if let Some(guard) = guard {
                guard.check().await?;
            }
            Ok::<_, RunnerError>(())
        })
        .await
        .unwrap_or_else(|_| {
            Err(StoreError::Location("lease authority check timed out".into()).into())
        });
        if checked.is_err() {
            self.lost.cancel();
        }
        checked
    }

    async fn watch(
        &self,
        task: Option<&Lease>,
        guard: Option<&crate::clean::Maintenance>,
    ) -> RunnerError {
        loop {
            if let Err(error) = self.authority(task, guard).await {
                return error;
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
                () = self.lost.cancelled() => {}
            }
        }
    }

    pub(crate) async fn phase<T, E: Into<RunnerError>>(
        &self,
        task: Option<&Lease>,
        guard: Option<&crate::clean::Maintenance>,
        cancel: Option<&CancellationToken>,
        work: impl Future<Output = Result<T, E>>,
    ) -> Result<T, RunnerError> {
        let cancel = cancel.unwrap_or(&self.lost);
        self.authority(task, guard).await?;
        if cancel.is_cancelled() {
            return Err(ExecutorError::Cancelled.into());
        }
        let result = tokio::select! {
            biased;
            error = self.watch(task, guard) => return Err(error),
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled.into()),
            result = work => result.map_err(Into::into),
        };
        self.authority(task, guard).await?;
        if cancel.is_cancelled() {
            return Err(ExecutorError::Cancelled.into());
        }
        result
    }

    /// Checks are diagnostic, not an atomic fence for remote work or writes already submitted.
    async fn work<T, E: Into<RunnerError>>(
        &self,
        task: Option<&Lease>,
        cancel: &CancellationToken,
        stop: &CancellationToken,
        work: impl Future<Output = Result<T, E>>,
    ) -> Result<T, RunnerError> {
        self.authority(task, None).await?;
        tokio::pin!(work);
        let error = tokio::select! {
            biased;
            error = self.watch(task, None) => error,
            () = cancel.cancelled() => ExecutorError::Cancelled.into(),
            result = &mut work => {
                self.authority(task, None).await?;
                if cancel.is_cancelled() { return Err(ExecutorError::Cancelled.into()); }
                return result.map_err(Into::into);
            }
        };
        stop.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(60), &mut work).await;
        Err(error)
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
