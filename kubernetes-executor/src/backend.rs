//! Runs task attempts as Kubernetes Jobs with staged inputs and bounded recovery.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fs::{File, TryLockError};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use executor::manifest::parse;
use executor::poll::{CANCEL_GRACE, Poll, REQUEST_GRACE};
use executor::process::Mount;
use executor::registry::{self, ImageConfigs};
use executor::wrapper::{Folders, MANIFEST, STDERR};
use executor::{
    Access, Backend, CONTROL, ContainerRun, ExecutorError, Finished, Job, containers, place,
};
use k8s_openapi::api::core::v1::Event;
use kube::api::{
    DeleteParams, ListParams, LogParams, PostParams, Preconditions, PropagationPolicy,
};
use task_model::TaskSpec;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::cluster::Cluster;
use crate::manifest::{Limits, Volumes, job_name};
use crate::pods::{Look, Pods};
use crate::staging::{PodStaging, Prepared};

/// How often the cluster is asked about the pods, by default.
const POLL: Duration = Duration::from_secs(5);

/// How long the cluster may take to take a Job, as a busy API server answers slowly. A Job
/// whose answer is lost is found again by its name.
const SUBMIT_GRACE: Duration = Duration::from_secs(60);

/// How much longer than its task a pod may run, by default.
const STAGING_TIME: Duration = Duration::from_secs(5 * 60);

/// How long a Job may have no pod, or a pod that cannot start, before it counts as failed.
const GRACE: Duration = Duration::from_secs(120);

/// How long a pod may find no node, without a scale-up of the cluster, by default.
const UNSCHEDULABLE_TIME: Duration = Duration::from_secs(3600);

/// The Kubernetes executor's backend.
#[derive(Debug)]
pub struct KubernetesBackend {
    pub cluster: Cluster,
    /// Where each attempt gets its scratch directory, which the nodes must see at the same
    /// path.
    pub scratch: PathBuf,
    /// How often the cluster is asked about the pods.
    pub poll: Duration,
    /// The entrypoints and default commands of images, from their registries.
    pub configs: ImageConfigs,
    /// Where the containers see the folders of an attempt.
    pub volumes: Volumes,
    /// How the pods get their files when the nodes share no folders with this machine; `None`
    /// when they see the scratch folder.
    pub remote: Option<PodStaging>,
    /// How much longer than its task a pod may run: for image pulls, staging and the start of
    /// its containers. The task itself keeps its time.
    pub staging_time: Duration,
    /// How long a Job may have no pod, or a pod that cannot start, before it fails; a pod that
    /// waits for quota of the namespace waits on.
    pub start_grace: Duration,
    /// How long a pod may find no node before it fails with the reason of the scheduler. Each
    /// new scale-up of the cluster for it starts the time again.
    pub unschedulable_time: Duration,
    /// The label value of the Jobs of this backend.
    label: String,
    pub(crate) pods: Pods,
    /// Whether the warning that events cannot be listed was logged.
    warned: AtomicBool,
    /// The tasks that stop the daemons of the run when it asks them to.
    pub(crate) daemons: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// How the container of a Job ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    /// It exited with this code, for this reason.
    Exited(i32, String),
    /// The pod was stopped for this reason, like `DeadlineExceeded`.
    Stopped(String),
    /// The pod of a daemon is ready at this IP.
    Ready(String),
}

impl KubernetesBackend {
    /// A backend with its attempts in `scratch`, which runs them in `cluster`.
    pub fn new(cluster: Cluster, scratch: PathBuf) -> KubernetesBackend {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        KubernetesBackend {
            cluster,
            scratch,
            poll: POLL,
            configs: ImageConfigs::default(),
            volumes: Volumes::Host,
            remote: None,
            staging_time: STAGING_TIME,
            start_grace: GRACE,
            unschedulable_time: UNSCHEDULABLE_TIME,
            label: format!("{}-{millis}", std::process::id()),
            pods: Pods::default(),
            warned: AtomicBool::new(false),
            daemons: std::sync::Mutex::default(),
        }
    }
}

impl Backend for KubernetesBackend {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        registry::pin(image).await
    }

    async fn release(&self) {
        self.release_daemons().await;
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        if cancel.is_cancelled() {
            return Err(ExecutorError::Cancelled);
        }
        if job.placement.offline {
            return Err(ExecutorError::Unsupported(
                "the Kubernetes executor cannot run a task without network".into(),
            ));
        }
        tokio::select! {
            result = self.reconcile() => result?,
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
        }
        let root = self.scratch.join(job.name);
        let placed = {
            // Without shared folders the inputs go to the pod through the staging root.
            let staging = match self.remote {
                Some(_) => executor::Staging::default(),
                None => job.staging.clone(),
            };
            let root = root.clone();
            tokio::task::spawn_blocking(move || place(&root, &staging))
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?
        };
        let mut staged = None;
        let result = match placed {
            // Another attempt has this name; its directory is not ours to remove.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(error.into());
            }
            Err(error) => Err(error.into()),
            Ok(mounts) => {
                let ownership = ownership(&root)?;
                self.attempt(job, &root, &mounts, &mut staged, ownership, cancel)
                    .await
            }
        };
        // Without a journal no Job of the attempt can run any more. A Job that may still run
        // keeps its journal, and the recovery of the attempt cleans the staging root.
        let ended = !root.join("remote.json").try_exists().unwrap_or(true);
        if let (Some(remote), Some(prepared), true) = (&self.remote, &staged, ended) {
            remote.stage.clean(&prepared.prefix, &prepared.plan).await;
        }
        if result.is_err() && ended {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        result
    }
}

impl KubernetesBackend {
    /// Writes the wrapper of a staged attempt, creates its Job and waits for its end. What it
    /// staged goes to `prepared` as soon as it is in the staging root.
    async fn attempt(
        &self,
        job: Job<'_>,
        root: &Path,
        mounts: &[Mount],
        prepared: &mut Option<Prepared>,
        ownership: Owned,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        // The command runs through the entrypoint of the image, as `docker run` and the local
        // executor run it; a task without one runs the image's default command.
        let mut spec = job.spec.clone();
        let daemon = match spec.environment.remove(executor::daemon::DAEMON_ENV) {
            Some(task_model::EnvironmentValue::Literal(text)) => Some(
                serde_json::from_str::<executor::daemon::Daemon>(&text)
                    .map_err(|_| ExecutorError::Unsupported("invalid daemon settings".into()))?,
            ),
            _ => None,
        };
        let platforms: Vec<&str> = spec
            .image
            .iter()
            .chain(
                spec.containers
                    .iter()
                    .filter_map(|container| container.image.as_ref()),
            )
            .filter_map(|image| image.platform.as_deref())
            .collect();
        if let Some(platform) = platforms.first() {
            if platforms.iter().any(|other| other != platform) {
                return Err(ExecutorError::Unsupported(
                    "Kubernetes image groups require the same platform".into(),
                ));
            }
            let platform = platform.to_string();
            for image in spec.image.iter_mut().chain(
                spec.containers
                    .iter_mut()
                    .filter_map(|container| container.image.as_mut()),
            ) {
                image.platform = Some(platform.clone());
            }
        }
        let image = spec.image.as_mut().ok_or(ExecutorError::NoImage)?;
        let config = if image.platform.is_some() {
            let resolved = self
                .configs
                .resolved_for(&image.reference, image.platform.as_deref())
                .await?;
            image.reference = resolved.reference;
            resolved.config
        } else {
            self.configs.get(&image.reference).await?
        };
        if !spec.replaces_entrypoint || spec.arguments.is_empty() {
            spec.arguments = config.command(&job.spec.arguments);
        }
        if spec.arguments.is_empty() {
            return Err(ExecutorError::NoCommand(image.reference.clone()));
        }
        for container in &mut spec.containers {
            let image = container.image.as_mut().ok_or(ExecutorError::NoImage)?;
            let selected = if image.platform.is_some() {
                let resolved = self
                    .configs
                    .resolved_for(&image.reference, image.platform.as_deref())
                    .await?;
                image.reference = resolved.reference;
                Some(resolved.config)
            } else {
                None
            };
            if container.replaces_entrypoint && !container.arguments.is_empty() {
                continue;
            }
            let config = match selected {
                Some(config) => config,
                None => self.configs.get(&image.reference).await?,
            };
            container.arguments = config.command(&container.arguments);
            if container.arguments.is_empty() {
                return Err(ExecutorError::NoCommand(image.reference.clone()));
            }
        }
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let owner = tokio::fs::metadata(root).await?;
        // The nodes of a cluster run containers with root, as a rootful engine does.
        let access = Access::of((owner.uid(), owner.gid()), false, spec.run_as, &config.user);
        if access.hand_back.is_some() {
            executor::access::open(root)?;
        }
        let name = job_name(job.name);
        let run = ContainerRun {
            name: &name,
            image,
            root,
            workdir: job.spec.workdir.as_deref(),
            mounts,
            owner: (owner.uid(), owner.gid()),
            cpus: job.placement.cpus.max(1),
            memory: job.placement.memory_bytes,
            // A pod has no switch of its own for the network; a NetworkPolicy would be one.
            offline: false,
            wrapper_path: None,
            access: &access,
        };
        match &self.remote {
            Some(_) if !spec.containers.is_empty() => {
                return Err(ExecutorError::Unsupported(
                    "Kubernetes object-store and kubedal staging run only tasks of one container"
                        .into(),
                ));
            }
            Some(remote) => *prepared = Some(remote.prepare(job, &run, &spec).await?),
            None => {}
        }
        let prepared = &*prepared;
        let run = match prepared {
            Some(prepared) => ContainerRun {
                mounts: &prepared.mounts,
                ..run
            },
            None => run,
        };
        // A task of several containers is one pod with a container for each image, which runs
        // the wrappers of the containers of that image, as the local executor runs them.
        let mut groups: Vec<(task_model::Image, PathBuf)> = Vec::new();
        match spec.containers.is_empty() {
            // With a staging root the pod gets its wrapper from there.
            true if prepared.is_some() => {}
            true => tokio::fs::write(run.script(), run.wrapper(&spec)).await?,
            false => {
                let (control, home) = (root.join(CONTROL), text(&root.join(executor::HOME)));
                let owner = access.owner();
                let folders = Folders {
                    work: &run.inside(),
                    control: &text(&control),
                    home: &home,
                    outbox: "",
                    owner: &owner,
                };
                for (group, path) in containers::write_wrappers(&spec, &control, &folders)? {
                    groups.push((group.image, path.into()));
                }
                // The folders of the wrappers are new, so they are opened too.
                if access.hand_back.is_some() {
                    executor::access::open(root)?;
                }
            }
        }
        let runs: Vec<ContainerRun<'_>> = match groups.is_empty() {
            true => vec![run],
            false => (groups.iter())
                .map(|(image, path)| ContainerRun {
                    image,
                    wrapper_path: Some(path),
                    ..run
                })
                .collect(),
        };
        let limits = Limits {
            gpus: job.placement.accelerators.iter().map(|gpu| gpu.count).sum(),
            seconds: job.placement.time_seconds,
            margin: self.staging_time.as_secs(),
        };
        if limits.gpus > 0 && runs.len() > 1 {
            return Err(ExecutorError::Unsupported(
                "Kubernetes GPU sharing across several image groups is not supported".into(),
            ));
        }
        let volumes = (prepared.as_ref()).map_or(&self.volumes, |prepared| &prepared.volumes);
        let mut manifest = crate::manifest::job(&name, &self.label, &runs, &limits, volumes)
            .map_err(ExecutorError::Unsupported)?;
        if let (Some(remote), Some(prepared)) = (&self.remote, prepared) {
            remote.complete(&mut manifest, prepared, root, limits.seconds);
        }
        if let Some(probe) = daemon.as_ref().and_then(crate::daemons::readiness) {
            manifest["spec"]["template"]["spec"]["containers"][0]["readinessProbe"] = probe;
        }
        let platforms: Vec<&str> = runs
            .iter()
            .filter_map(|run| run.image.platform.as_deref())
            .collect();
        if let Some(platform) = platforms.first() {
            let mut parts = platform.split('/');
            manifest["spec"]["template"]["spec"]["nodeSelector"] = serde_json::json!({
                "kubernetes.io/os": parts.next().unwrap_or_default(),
                "kubernetes.io/arch": parts.next().unwrap_or_default(),
            });
        }
        if cancel.is_cancelled() {
            return Err(ExecutorError::Cancelled);
        }
        let host = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        let started = owner_start(std::process::id()).ok_or_else(|| {
            std::io::Error::other("cannot identify the Kubernetes coordinator process")
        })?;
        let mut journal = serde_json::json!({"backend": "Kubernetes", "server": self.cluster.server(),
            "namespace": self.cluster.namespace(), "name": name, "label": self.label, "uid": null,
            "owner": std::process::id(), "host": host, "started": started, "locked": true});
        // The recovery of the attempt removes its objects from the staging root.
        if let Some(prepared) = prepared {
            journal["staging"] =
                serde_json::json!({"prefix": prepared.prefix, "plan": prepared.plan});
        }
        journal_at(root, &journal)?;
        let uid = tokio::select! {
            result = self.create(manifest) => result.map_err(|error| unresolved(root, error))?,
            () = cancel.cancelled() => return Err(unresolved(root, "cancelled during submission; the response was not recorded")),
        };
        journal["uid"] = serde_json::json!(uid);
        journal_at(root, &journal)?;
        if daemon.is_some() {
            return self
                .serve(&name, journal, root.to_owned(), ownership, cancel)
                .await;
        }
        self.pods.add(&name).await;
        let ended = self.wait(&name, false, cancel).await;
        // The log of a container that failed tells why, as when it could not run the wrapper.
        let log = match &ended {
            Ok(Ended::Exited(status, _)) if *status != 0 => {
                tokio::time::timeout(REQUEST_GRACE, self.log(&name, None))
                    .await
                    .ok()
                    .flatten()
            }
            _ => None,
        };
        // Kubernetes removes the pod of a kubedal volume once kubedal uploaded its outputs.
        let grace = (self.remote.as_ref()).and_then(PodStaging::grace);
        let grace = grace.unwrap_or(CANCEL_GRACE);
        let stopped = Self::stop(&self.cluster, self.poll, root, &journal, grace).await;
        self.pods.remove(&name).await;
        stopped?;
        if let (Some(remote), Some(prepared), Ok(_)) = (&self.remote, prepared, &ended) {
            remote.collect(prepared, root).await?;
        }
        let finished = finish(ended?, root.to_owned(), limits.seconds, &spec).await;
        match (finished, log) {
            (Err(ExecutorError::Engine { engine, message }), Some(log)) => {
                Err(ExecutorError::Engine {
                    engine,
                    message: format!("{message}: {log}"),
                })
            }
            (finished, _) => finished,
        }
    }

    /// Creates a Job from its manifest.
    async fn create(&self, manifest: serde_json::Value) -> Result<String, ExecutorError> {
        let job = serde_json::from_value(manifest)
            .map_err(|error| machine(format!("the Job is not valid: {error}")))?;
        let jobs = self.cluster.jobs();
        let accepted =
            tokio::time::timeout(SUBMIT_GRACE, jobs.create(&PostParams::default(), &job))
                .await
                .map_err(|_| machine("the Job submission response timed out".into()))?
                .map_err(|error| machine(format!("the cluster did not take the Job: {error}")))?;
        accepted
            .metadata
            .uid
            .filter(|uid| !uid.is_empty())
            .ok_or_else(|| machine("the Job submission response has no UID".into()))
    }

    /// Resolves accepted work whose original coordinator is no longer running.
    pub async fn reconcile(&self) -> Result<(), ExecutorError> {
        tokio::time::timeout(CANCEL_GRACE, async {
            std::fs::create_dir_all(&self.scratch)?;
            let gate = File::options()
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.scratch.join("remote-recovery.lock"))?;
            loop {
                match gate.try_lock() {
                    Ok(()) => break,
                    Err(TryLockError::WouldBlock) => {
                        tokio::time::sleep(Duration::from_millis(10)).await
                    }
                    Err(TryLockError::Error(error)) => return Err(error.into()),
                }
            }
            self.reconcile_records().await
        })
        .await
        .map_err(|_| unresolved(&self.scratch, "admission recovery timed out"))?
    }

    async fn reconcile_records(&self) -> Result<(), ExecutorError> {
        let entries = match std::fs::read_dir(&self.scratch) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let host = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        for entry in entries {
            tokio::task::yield_now().await;
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let root = entry.path();
            let path = root.join("remote.json");
            if !path.try_exists()? {
                continue;
            }
            let mut journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|error| unresolved(&root, error))?;
            if journal["backend"] != "Kubernetes"
                || journal["host"].as_str() != Some(host.as_str())
                || journal["server"].as_str() != Some(self.cluster.server())
                || journal["namespace"].as_str() != Some(self.cluster.namespace())
            {
                return Err(unresolved(
                    &root,
                    "the original coordinator host or cluster settings changed",
                ));
            }
            let live_owner = journal["owner"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
                .and_then(owner_start)
                .as_deref()
                == journal["started"].as_str()
                && journal["started"].is_string();
            let _ownership = if journal["locked"] == true {
                let lock = match File::options().write(true).open(root.join("remote.lock")) {
                    Ok(lock) => lock,
                    Err(_) if !path.try_exists()? => continue,
                    Err(error) => return Err(unresolved(&root, error)),
                };
                let mut locked = lock.try_lock();
                // A process that this one starts holds a copy of the lock until it runs its
                // program, so a lock without a live owner gets a moment to go away.
                for _ in 0..100 {
                    if live_owner || !matches!(locked, Err(TryLockError::WouldBlock)) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    locked = lock.try_lock();
                }
                match locked {
                    Err(TryLockError::WouldBlock) if live_owner => continue,
                    Err(TryLockError::WouldBlock) => {
                        return Err(unresolved(
                            &root,
                            "the journal lock has no matching live coordinator",
                        ));
                    }
                    Err(TryLockError::Error(error)) => return Err(unresolved(&root, error)),
                    Ok(()) => Owned(lock),
                }
            } else {
                if live_owner {
                    continue;
                }
                Owned(File::open(&path)?)
            };
            if !path.try_exists()? {
                continue;
            }
            journal = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|error| unresolved(&root, error))?;
            if journal["backend"] != "Kubernetes"
                || journal["host"].as_str() != Some(host.as_str())
                || journal["server"].as_str() != Some(self.cluster.server())
                || journal["namespace"].as_str() != Some(self.cluster.namespace())
            {
                return Err(unresolved(
                    &root,
                    "the original coordinator host or cluster settings changed",
                ));
            }
            // A submission without an answer must not block every later run.
            if journal["uid"].as_str().is_none_or(str::is_empty) {
                match self.identify(&journal).await {
                    Submitted::Found(uid) => {
                        journal["uid"] = serde_json::json!(uid);
                        journal_at(&root, &journal)?;
                    }
                    Submitted::Absent => {
                        tracing::warn!(attempt = %root.display(),
                            "the Job of an unclear submission is not in the cluster; the attempt is lost");
                        std::fs::remove_file(&path)?;
                        Self::clean_staging(self.remote.as_ref(), &journal).await;
                        continue;
                    }
                    Submitted::Unclear(reason) => {
                        tracing::warn!(attempt = %root.display(), reason,
                            "cannot resolve an unclear submission yet");
                        continue;
                    }
                }
            }
            Self::stop(&self.cluster, self.poll, &root, &journal, CANCEL_GRACE).await?;
            Self::clean_staging(self.remote.as_ref(), &journal).await;
        }
        Ok(())
    }

    /// Removes the objects in the staging root that the journal of an attempt names.
    pub(crate) async fn clean_staging(remote: Option<&PodStaging>, journal: &serde_json::Value) {
        let staging = &journal["staging"];
        let (prefix, plan) = (staging["prefix"].as_str(), staging["plan"].as_str());
        if let (Some(remote), Some(prefix), Some(plan)) = (remote, prefix, plan) {
            remote.stage.clean(prefix, plan).await;
        }
    }

    /// Finds the Job of a journal whose submission left no UID, by its name, which only this
    /// attempt has, and the label of the backend that made it.
    async fn identify(&self, journal: &serde_json::Value) -> Submitted {
        let Some(name) = journal["name"].as_str().filter(|name| !name.is_empty()) else {
            return Submitted::Unclear("the journal names no Job");
        };
        let found = tokio::time::timeout(REQUEST_GRACE, self.cluster.jobs().get_opt(name)).await;
        match found {
            Ok(Ok(Some(job))) => {
                let labels = job.metadata.labels.as_ref();
                let label = labels.and_then(|labels| labels.get(crate::manifest::LABEL));
                match job.metadata.uid.filter(|uid| !uid.is_empty()) {
                    Some(uid) if label.map(String::as_str) == journal["label"].as_str() => {
                        Submitted::Found(uid)
                    }
                    _ => Submitted::Unclear("a Job of another backend has the name"),
                }
            }
            Ok(Ok(None)) => {
                let selector = ListParams::default().labels(&format!("job-name={name}"));
                let pods = self.cluster.pods();
                match tokio::time::timeout(REQUEST_GRACE, pods.list(&selector)).await {
                    Ok(Ok(list)) if list.items.is_empty() => Submitted::Absent,
                    Ok(Ok(_)) => Submitted::Unclear("pods of the Job are still there"),
                    _ => Submitted::Unclear("the cluster cannot list the pods"),
                }
            }
            _ => Submitted::Unclear("the cluster cannot tell"),
        }
    }

    /// The last lines of the log of the pod of a Job, of its `container` or else its first one,
    /// when there is one.
    async fn log(&self, name: &str, container: Option<&str>) -> Option<String> {
        let pods = self.cluster.pods();
        let selector = ListParams::default().labels(&format!("job-name={name}"));
        let list = pods.list(&selector).await.ok()?;
        let pod = list.items.last()?.metadata.name.clone()?;
        let parameters = LogParams {
            tail_lines: Some(5),
            container: container.map(str::to_owned),
            ..LogParams::default()
        };
        let log = pods.logs(&pod, &parameters).await.ok()?;
        let lines: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
        (!lines.is_empty()).then(|| lines.join("; "))
    }

    /// Waits for a container outcome or cancellation before confirming termination, or for a
    /// `daemon` until its pod is ready.
    pub(crate) async fn wait(
        &self,
        name: &str,
        daemon: bool,
        cancel: &CancellationToken,
    ) -> Result<Ended, ExecutorError> {
        let mut poll = Poll::new(self.poll);
        // Since when the pod finds no node, and how many scale-ups it caused so far.
        let mut unscheduled: Option<(Instant, u64)> = None;
        // Since when the Job has no pod.
        let mut missing: Option<Instant> = None;
        loop {
            tracing::debug!(job = name, "polling Kubernetes job");
            let look = poll
                .read(cancel, async {
                    tokio::time::timeout(
                        REQUEST_GRACE,
                        self.pods.look(name, self.poll, &self.cluster, &self.label),
                    )
                    .await
                    .unwrap_or_else(|_| Look::Unavailable("pod query timed out".into()))
                })
                .await
                .map_err(|_| ExecutorError::Cancelled)?;
            if !matches!(look, Look::Unschedulable(_)) {
                unscheduled = None;
            }
            if !matches!(look, Look::Missing) {
                missing = None;
            }
            let problem = match look {
                Look::Ready(address) if daemon => return Ok(Ended::Ready(address)),
                Look::Active | Look::Unknown | Look::Ready(_) => None,
                Look::Unschedulable(reason) => {
                    let (since, scaled) = *unscheduled.get_or_insert((Instant::now(), 0));
                    if since.elapsed() >= self.unschedulable_time {
                        // A cluster that grows for the pod gets the time again.
                        let now = self.scale_ups(name).await;
                        if now <= scaled {
                            let message = format!("the pod cannot be scheduled: {reason}");
                            return Err(machine(message));
                        }
                        unscheduled = Some((Instant::now(), now));
                    }
                    None
                }
                Look::Ended { exit_code, reason } => return Ok(Ended::Exited(exit_code, reason)),
                Look::Stopped(reason) => return Ok(Ended::Stopped(reason)),
                Look::InitFailed {
                    container,
                    exit_code,
                    reason,
                } => {
                    let log = self.log(name, Some(&container));
                    let log = tokio::time::timeout(REQUEST_GRACE, log)
                        .await
                        .ok()
                        .flatten();
                    let message = format!(
                        "the init container {container} failed (exit code {exit_code}, {reason})"
                    );
                    return Err(machine(match log {
                        Some(log) => format!("{message}: {log}"),
                        None => message,
                    }));
                }
                Look::Stuck(reason) => Some(format!("the pod cannot start: {reason}")),
                Look::Missing => {
                    missing.get_or_insert_with(Instant::now);
                    Some(MISSING.to_owned())
                }
                Look::Unavailable(error) => Some(format!("cannot list the pods: {error}")),
            };
            // A pod may appear late, and an image may be pulled at the next try.
            if poll.troubled(problem.is_some(), self.start_grace)
                && let Some(problem) = problem
            {
                if problem != MISSING {
                    return Err(machine(problem));
                }
                match self.creation(name).await {
                    // Pods wait for quota of the namespace as jobs wait in a queue.
                    Creation::Quota => {
                        tracing::debug!(job = name, "the Job waits for quota");
                        poll.troubled(false, self.start_grace);
                    }
                    Creation::Failed(message) => {
                        let message = format!("the cluster cannot make the pod: {message}");
                        return Err(machine(message));
                    }
                    Creation::Unknown => return Err(machine(problem)),
                    // Without events a lack of quota looks like any other cause, so the Job
                    // waits as long as a pod may wait for a node.
                    Creation::Unreadable(error) => {
                        self.warn_events(&error);
                        if missing.is_some_and(|since| since.elapsed() >= self.unschedulable_time) {
                            let message = format!(
                                "{problem}; the events that tell why cannot be listed: {error}"
                            );
                            return Err(machine(message));
                        }
                        poll.troubled(false, self.start_grace);
                    }
                }
            }
        }
    }

    /// Why the Job controller made no pod, from the last `FailedCreate` event of the Job.
    async fn creation(&self, name: &str) -> Creation {
        let fields = format!("involvedObject.kind=Job,involvedObject.name={name}");
        let events = match self.events(&fields).await {
            Ok(events) => events,
            Err(error) => return Creation::Unreadable(error),
        };
        let failed = events
            .iter()
            .rev()
            .find(|event| event.reason.as_deref() == Some("FailedCreate"));
        match failed.and_then(|event| event.message.clone()) {
            Some(message) if message.contains("exceeded quota") => Creation::Quota,
            Some(message) => Creation::Failed(message),
            None => Creation::Unknown,
        }
    }

    /// How many scale-ups the cluster autoscaler made for the pod of a Job so far.
    async fn scale_ups(&self, name: &str) -> u64 {
        let events = self
            .events("involvedObject.kind=Pod,reason=TriggeredScaleUp")
            .await
            .unwrap_or_default();
        (events.iter())
            .filter(|event| {
                let pod = event.involved_object.name.as_deref().unwrap_or_default();
                pod.strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with('-'))
            })
            .map(|event| event.count.map_or(1, |count| count.max(1) as u64))
            .sum()
    }

    /// The events of the namespace that match `fields`, oldest first, or why the cluster
    /// cannot list them.
    async fn events(&self, fields: &str) -> Result<Vec<Event>, String> {
        let parameters = ListParams::default().fields(fields);
        let api = self.cluster.events();
        let mut events = match tokio::time::timeout(REQUEST_GRACE, api.list(&parameters)).await {
            Ok(Ok(list)) => list.items,
            Ok(Err(error)) => return Err(error.to_string()),
            Err(_) => return Err("the request timed out".into()),
        };
        events.sort_by_key(|event| event.last_timestamp.clone());
        Ok(events)
    }

    /// Warns once that events cannot be listed, which hides why a Job has no pod.
    fn warn_events(&self, error: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                namespace = self.cluster.namespace(),
                error,
                "cannot list the events of the namespace: the account of the kubeconfig needs \
                 the right to `list` `events`; a Job without a pod now waits up to \
                 unschedulable_minutes, as for quota, before it fails"
            );
        }
    }

    /// Releases ownership only after the accepted Job and all its pods disappear.
    pub(crate) async fn stop(
        cluster: &Cluster,
        poll: Duration,
        root: &Path,
        journal: &serde_json::Value,
        grace: Duration,
    ) -> Result<(), ExecutorError> {
        let name = journal["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| unresolved(root, "the Job name was not recorded"))?;
        let uid = journal["uid"]
            .as_str()
            .filter(|uid| !uid.is_empty())
            .ok_or_else(|| {
                unresolved(
                    root,
                    "the accepted UID was not recorded; resolve the submission on the cluster",
                )
            })?;
        let jobs = cluster.jobs();
        let pods = cluster.pods();
        let selector = ListParams::default().labels(&format!("job-name={name}"));
        executor::poll::until_stopped(grace, poll.min(Duration::from_secs(1)), || async {
            match tokio::time::timeout(REQUEST_GRACE, jobs.get_opt(name)).await {
                Ok(Ok(Some(job))) => {
                    if job.metadata.uid.as_deref() != Some(uid)
                        || job
                            .metadata
                            .labels
                            .as_ref()
                            .and_then(|labels| labels.get(crate::manifest::LABEL))
                            .map(String::as_str)
                            != journal["label"].as_str()
                    {
                        return Some(Err(unresolved(root, "the Job identity changed")));
                    }
                    let parameters = DeleteParams {
                        propagation_policy: Some(PropagationPolicy::Foreground),
                        preconditions: Some(Preconditions {
                            uid: Some(uid.to_owned()),
                            ..Preconditions::default()
                        }),
                        ..DeleteParams::default()
                    };
                    let _ =
                        tokio::time::timeout(REQUEST_GRACE, jobs.delete(name, &parameters)).await;
                }
                Ok(Ok(None)) => {
                    if let Ok(Ok(list)) =
                        tokio::time::timeout(REQUEST_GRACE, pods.list(&selector)).await
                        && list.items.is_empty()
                    {
                        return Some(Ok(()));
                    }
                }
                _ => {}
            }
            None
        })
        .await
        .map_err(|_| unresolved(root, format!("Job {name} termination was not confirmed")))??;
        std::fs::remove_file(root.join("remote.json"))?;
        Ok(())
    }
}

/// The problem of a Job without a pod.
const MISSING: &str = "the Job has no pod";

/// Why the Job controller made no pod.
enum Creation {
    /// The namespace has no quota left for it now.
    Quota,
    /// It failed for this reason, like a policy that refuses the pod.
    Failed(String),
    /// No event tells.
    Unknown,
    /// The events cannot be listed, for this reason.
    Unreadable(String),
}

/// What became of a submission whose answer was lost.
enum Submitted {
    /// The Job with this UID.
    Found(String),
    /// The cluster has no such Job: it never took it, or it is gone.
    Absent,
    /// The cluster cannot tell yet, for this reason.
    Unclear(&'static str),
}

/// The lock of an attempt. It lets go of the lock before it closes the file, as a process that
/// this one starts holds a copy of the file, and so the lock, until it runs its program.
struct Owned(File);

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn ownership(root: &Path) -> Result<Owned, ExecutorError> {
    let lock = File::options()
        .write(true)
        .create_new(true)
        .open(root.join("remote.lock"))?;
    lock.try_lock()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(Owned(lock))
}

fn owner_start(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    if matches!(fields.split_whitespace().next()?, "Z" | "X") {
        return None;
    }
    fields.split_whitespace().nth(19).map(str::to_owned)
}

fn unresolved(root: &Path, error: impl std::fmt::Display) -> ExecutorError {
    std::io::Error::other(format!(
        "Kubernetes ownership is unresolved: {error}; retained {}",
        root.join("remote.json").display()
    ))
    .into()
}

fn journal_at(root: &Path, journal: &serde_json::Value) -> Result<(), ExecutorError> {
    use std::io::Write;
    let path = root.join("remote.json.next");
    let mut file = std::fs::File::create(&path)?;
    file.write_all(journal.to_string().as_bytes())?;
    file.sync_all()?;
    std::fs::rename(path, root.join("remote.json"))?;
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}

/// What an attempt left behind, by how its container ended.
async fn finish(
    ended: Ended,
    root: PathBuf,
    time: Option<u64>,
    spec: &TaskSpec,
) -> Result<Finished, ExecutorError> {
    let control = root.join(CONTROL);
    let recorded = containers::recorded(spec, &control);
    let (exit_code, note) = outcome(recorded, &ended, time).map_err(machine)?;
    if let Some(note) = note {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(control.join(STDERR))
            .await?;
        file.write_all(note.as_bytes()).await?;
        // A file of Tokio writes in the background until it is flushed.
        file.flush().await?;
    }
    let files = match exit_code {
        0 => {
            let manifest = tokio::fs::read_to_string(control.join(MANIFEST)).await;
            parse(&manifest.unwrap_or_default())?
        }
        _ => Vec::new(),
    };
    Ok(Finished {
        address: None,
        exit_code: Some(exit_code),
        files,
        usage: executor::Usage::of(&root),
        root,
    })
}

/// Combines wrapper and container or job status into a task exit code; machine failures return
/// `Err`.
fn outcome(
    recorded: Option<i32>,
    ended: &Ended,
    time: Option<u64>,
) -> Result<(i32, Option<String>), String> {
    match ended {
        Ended::Exited(0, _) => recorded
            .map(|code| (code, None))
            .ok_or_else(|| "the container ended without the exit code of the task".to_owned()),
        Ended::Exited(executor::wrapper::TIMED_OUT, _) if time.is_some() => {
            Ok((143, Some(too_long(time))))
        }
        Ended::Exited(status, reason) if reason == "OOMKilled" => Ok((
            *status,
            Some("iunctus: Kubernetes stopped the task for lack of memory\n".to_owned()),
        )),
        Ended::Exited(status @ 129..=192, _) => Ok((
            *status,
            Some(format!(
                "iunctus: the container ended with exit code {status}, maybe for lack of memory\n"
            )),
        )),
        // A captured wrapper ends with the code of its task, which it recorded first.
        Ended::Exited(status, _) if recorded == Some(*status) => Ok((*status, None)),
        Ended::Exited(status, reason) => Err(format!(
            "the container could not run the task (exit code {status}, {reason})"
        )),
        Ended::Stopped(reason) if reason == "DeadlineExceeded" => Ok((143, Some(too_long(time)))),
        Ended::Stopped(reason) => Err(format!("the pod was stopped: {reason}")),
        Ended::Ready(_) => Err("the pod is ready, but the task never ended".to_owned()),
    }
}

/// The note of a task that ran longer than its time, or than Kubernetes allows.
fn too_long(time: Option<u64>) -> String {
    match time {
        Some(time) => format!("iunctus: the task ran longer than its time of {time} seconds\n"),
        None => "iunctus: the task ran longer than Kubernetes allows\n".to_owned(),
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(crate) fn machine(message: String) -> ExecutorError {
    ExecutorError::Engine {
        engine: "Kubernetes".into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn termination_preserves_journal() {
        let folder = tempfile::tempdir().unwrap();
        let config = folder.path().join("kubeconfig");
        std::fs::write(&config, "apiVersion: v1\nkind: Config\ncurrent-context: test\nclusters:\n- name: test\n  cluster:\n    server: http://127.0.0.1:9\ncontexts:\n- name: test\n  context:\n    cluster: test\n    user: test\nusers:\n- name: test\n  user: {}\n").unwrap();
        let cluster = Cluster::connect(&config, None).await.unwrap();
        let backend = KubernetesBackend::new(cluster, folder.path().to_owned());
        let journal = serde_json::json!({"name": "accepted", "uid": "original", "label": "test"});
        journal_at(folder.path(), &journal).unwrap();
        let grace = Duration::from_millis(10);
        let stopped = KubernetesBackend::stop(
            &backend.cluster,
            backend.poll,
            folder.path(),
            &journal,
            grace,
        );
        let error = stopped.await.unwrap_err();
        assert!(!error.retryable());
        assert!(error.to_string().contains("termination was not confirmed"));
        assert!(folder.path().join("remote.json").is_file());
    }

    #[test]
    fn classifies_container_failures() {
        let exited = |status, reason: &str| Ended::Exited(status, reason.to_owned());
        assert_eq!(
            outcome(Some(3), &exited(0, "Completed"), None),
            Ok((3, None))
        );
        assert!(outcome(None, &exited(0, "Completed"), None).is_err());
        let memory = outcome(None, &exited(137, "OOMKilled"), None).expect("an end");
        assert_eq!(memory.0, 137);
        assert!(memory.1.expect("a note").contains("lack of memory"));
        assert!(outcome(None, &exited(127, "Error"), None).is_err());
        assert_eq!(outcome(Some(1), &exited(1, "Error"), None), Ok((1, None)));
        assert!(outcome(Some(0), &exited(1, "Error"), None).is_err());
        let late = Ended::Stopped("DeadlineExceeded".into());
        assert_eq!(outcome(None, &late, Some(9)).map(|(code, _)| code), Ok(143));
        // The wrapper of a task that ran out of its time ends with the code of `timeout`.
        let timed = outcome(Some(143), &exited(124, "Error"), Some(9)).expect("an end");
        assert_eq!(timed.0, 143);
        assert!(timed.1.expect("a note").contains("time of 9 seconds"));
        assert!(outcome(None, &exited(124, "Error"), None).is_err());
        assert!(outcome(None, &Ended::Stopped("Evicted".into()), None).is_err());
    }
}
