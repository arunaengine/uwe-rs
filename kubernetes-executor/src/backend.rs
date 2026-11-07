//! Runs task attempts as Kubernetes Jobs with staged inputs and bounded recovery.
// Copyright (c) 2026 The uwe Contributors
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
    DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams, Preconditions,
    PropagationPolicy,
};
use task_model::TaskSpec;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::cluster::Cluster;
use crate::manifest::{Limits, SharedVolume, Volumes, job_name};
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
    /// Host-approved reference volumes, mounted read-only independently of task scratch.
    pub references: Vec<SharedVolume>,
    /// How the pods get their files when the nodes share no folders with this machine; `None`
    /// when they see the scratch folder.
    pub remote: Option<PodStaging>,
    /// Trusted CLI workloads may receive reusable staging credentials; hosted workloads cannot.
    pub trusted: bool,
    /// How much longer than its task a pod may run: for image pulls, staging and the start of
    /// its containers. The task itself keeps its time.
    pub staging_time: Duration,
    /// How long a Job may have no pod, or a pod that cannot start, before it fails; a pod that
    /// waits for quota of the namespace waits on.
    pub start_grace: Duration,
    /// How long a pod may find no node before it fails with the reason of the scheduler. Each
    /// new scale-up of the cluster for it starts the time again.
    pub unschedulable_time: Duration,
    /// The pod options of every Job, before those of its task.
    pub pod: crate::PodOptions,
    /// The Common Workflow Scheduler that learns of each task before its Job, if any.
    pub cws: Option<crate::cws::Client>,
    /// The label value of the Jobs of this backend.
    label: String,
    pub(crate) pods: Pods,
    /// Whether the warning that events cannot be listed was logged.
    warned: AtomicBool,
    indexed: AtomicBool,
    pending: tokio::sync::Mutex<Option<std::fs::ReadDir>>,
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
            references: Vec::new(),
            remote: None,
            trusted: true,
            staging_time: STAGING_TIME,
            start_grace: GRACE,
            unschedulable_time: UNSCHEDULABLE_TIME,
            pod: crate::PodOptions::default(),
            cws: None,
            label: format!("{}-{millis}", std::process::id()),
            pods: Pods::default(),
            warned: AtomicBool::new(false),
            indexed: AtomicBool::new(false),
            pending: tokio::sync::Mutex::new(None),
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
                "the Kubernetes executor cannot run a task without network, like a CWL tool \
                 without NetworkAccess; let its step use the network with `allowed_steps` in \
                 `[network]` of the config file"
                    .into(),
            ));
        }
        if !self.trusted && self.remote.as_ref().is_some_and(PodStaging::credentials) {
            return Err(ExecutorError::Unsupported(
                "untrusted Kubernetes tasks require presigned or CSI staging".into(),
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
        let shm = match spec.environment.remove("UWE_CONTAINER_FLAGS") {
            None => None,
            Some(task_model::EnvironmentValue::Literal(text)) => {
                let flags: std::collections::BTreeMap<String, i64> = serde_json::from_str(&text)
                    .map_err(|_| {
                        ExecutorError::Unsupported("invalid Kubernetes container flags".into())
                    })?;
                if flags.get("shm").is_some_and(|size| *size < 0) {
                    return Err(ExecutorError::Unsupported(
                        "Kubernetes shared-memory size must be nonnegative".into(),
                    ));
                }
                flags
                    .get("shm")
                    .copied()
                    .filter(|size| *size > 0)
                    .map(|size| size as u64)
            }
            Some(_) => {
                return Err(ExecutorError::Unsupported(
                    "Kubernetes container flags must be literal".into(),
                ));
            }
        };
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
            if let Some(remote) = &self.remote
                && (spec.output_manifest.is_some() || matches!(&remote.via, crate::Via::Helper(_)))
            {
                remote.platform(platform)?;
            }
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
        let mut mounts = mounts.to_vec();
        if self.remote.is_none() {
            mounts.retain(|mount| {
                !mount.read_only
                    || !self
                        .references
                        .iter()
                        .any(|reference| mount.source.starts_with(&reference.path))
            });
            for input in &job.staging.inputs {
                if !input.copy
                    && !input.in_place
                    && self
                        .references
                        .iter()
                        .any(|reference| input.source.starts_with(&reference.path))
                {
                    mounts.push(Mount {
                        source: input.source.clone(),
                        target: input.source.to_string_lossy().into_owned(),
                        read_only: true,
                    });
                    if input.path.starts_with('/') && Path::new(&input.path) != input.source {
                        mounts.push(Mount {
                            source: input.source.clone(),
                            target: input.path.clone(),
                            read_only: true,
                        });
                    }
                }
            }
        }
        let run = ContainerRun {
            name: &name,
            image,
            root,
            workdir: job.spec.workdir.as_deref(),
            mounts: &mounts,
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
            Some(remote) => {
                let mut staging = job.staging.clone();
                let mut references = Vec::new();
                staging.inputs.retain(|input| {
                    if !input.copy
                        && !input.in_place
                        && self
                            .references
                            .iter()
                            .any(|reference| input.source.starts_with(&reference.path))
                    {
                        references.push(Mount {
                            source: input.source.clone(),
                            target: input.path.clone(),
                            read_only: true,
                        });
                        references.push(Mount {
                            source: input.source.clone(),
                            target: input.source.to_string_lossy().into_owned(),
                            read_only: true,
                        });
                        false
                    } else {
                        true
                    }
                });
                for reference in &references {
                    if reference.target.contains('\0')
                        || Path::new(&reference.target)
                            .components()
                            .any(|part| matches!(part, std::path::Component::ParentDir))
                    {
                        return Err(ExecutorError::Unsupported(
                            "reference input path leaves its folder".into(),
                        ));
                    }
                }
                staging.directories.retain(|directory| {
                    !self
                        .references
                        .iter()
                        .any(|reference| Path::new(directory).starts_with(&reference.path))
                });
                let mut staged = remote
                    .prepare(
                        Job {
                            staging: &staging,
                            ..job
                        },
                        &run,
                        &spec,
                        job.staging,
                    )
                    .await?;
                staged.mounts.extend(references);
                *prepared = Some(staged);
            }
            None => {}
        }
        let staged = &*prepared;
        let run = match staged {
            Some(staged) => ContainerRun {
                mounts: &staged.mounts,
                ..run
            },
            None => run,
        };
        // A task of several containers is one pod with a container for each image, which runs
        // the wrappers of the containers of that image, as the local executor runs them.
        let mut groups: Vec<(task_model::Image, PathBuf)> = Vec::new();
        match spec.containers.is_empty() {
            // With a staging root the pod gets its wrapper from there.
            true if staged.is_some() => {}
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
        let volumes = (staged.as_ref()).map_or(&self.volumes, |staged| &staged.volumes);
        let mut manifest = crate::manifest::job_with(
            &name,
            &self.label,
            &runs,
            &limits,
            volumes,
            &self.references,
        )
        .map_err(ExecutorError::Unsupported)?;
        if let (Some(remote), Some(staged)) = (&self.remote, staged) {
            remote.complete(&mut manifest, staged, root, limits.seconds);
        }
        if let Some(probe) = daemon.as_ref().and_then(crate::daemons::readiness) {
            manifest["spec"]["template"]["spec"]["containers"][0]["readinessProbe"] = probe;
        }
        if let Some(size) = shm {
            crate::manifest::shared_memory(&mut manifest, size)
                .map_err(ExecutorError::Unsupported)?;
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
        let mut options = self.pod.clone();
        if let Some(list) = job.placement.options.get("pod") {
            options.add(list).map_err(ExecutorError::Unsupported)?;
        }
        (options.apply(&mut manifest, job.placement)).map_err(ExecutorError::Unsupported)?;
        let resources = manifest["spec"]["template"]["spec"]["resources"].clone();
        if !resources.is_null() {
            manifest["spec"]["suspend"] = serde_json::json!(true);
        }
        self.validate(&manifest).await?;
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
        if let Some(staged) = staged {
            let remote = self.remote.as_ref().expect("staged backend");
            journal["staging"] = serde_json::json!({"prefix": staged.prefix, "plan": staged.plan,
                "root": remote.stage.objects.url(""), "cleanup": remote.stage.cleanup});
            journal["collect"] = serde_json::json!(daemon.is_none());
        }
        journal_at(root, &journal)?;
        track(root)?;
        let (uid, retained) = tokio::select! {
            result = self.create(manifest) => result.map_err(|error| unresolved(root, error))?,
            () = cancel.cancelled() => return Err(unresolved(root, "cancelled during submission; the response was not recorded")),
        };
        journal["uid"] = serde_json::json!(uid);
        journal_at(root, &journal)?;
        let retained = if retained && !resources.is_null() {
            let patch = Patch::Merge(serde_json::json!({"metadata": {"uid": journal["uid"]},
                "spec": {"suspend": false}}));
            let activated = tokio::time::timeout(
                SUBMIT_GRACE,
                self.cluster
                    .jobs()
                    .patch(&name, &PatchParams::default(), &patch),
            )
            .await
            .map_err(|error| unresolved(root, error))?
            .map_err(|error| unresolved(root, error))?;
            let activated =
                serde_json::to_value(activated).map_err(|error| unresolved(root, error))?;
            resources_match(
                &resources,
                &activated["spec"]["template"]["spec"]["resources"],
            )
        } else {
            retained
        };
        if !retained {
            Self::stop(&self.cluster, self.poll, root, &journal, CANCEL_GRACE).await?;
            Self::settle(self.remote.as_ref(), root, &journal).await?;
            *prepared = None;
            return Err(ExecutorError::Unsupported(
                "Kubernetes admission changed aggregate task caps".into(),
            ));
        }
        if let Some(cws) = &self.cws {
            let (namespace, work) = (self.cluster.namespace(), root.join(executor::WORK));
            let copied = self.remote.is_some();
            // CWS knows a task by its pod, which the Job controller names.
            let registered = match self.pod_name(&name, cancel).await {
                Ok(pod) => (cws.task(job, &pod, namespace, &work, copied).await).map_err(machine),
                Err(error) => Err(error),
            };
            if let Err(error) = registered {
                Self::stop(&self.cluster, self.poll, root, &journal, CANCEL_GRACE).await?;
                Self::settle(self.remote.as_ref(), root, &journal).await?;
                *prepared = None;
                return Err(error);
            }
        }
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
        if let (Some(remote), Some(kept), Ok(_)) = (&self.remote, prepared.as_ref(), &ended)
            && let Err(error) = executor::remote::patiently(|| remote.collect(kept, root)).await
        {
            // The objects stay, as they are the only copy of the results.
            tracing::warn!(prefix = kept.prefix, %error, "cannot collect the results of an attempt; they stay in the staging root");
            *prepared = None;
            return Err(error);
        }
        if journal["collect"] == true && ended.is_ok() {
            if Finished::from_control(root.to_owned())?.exit_code.is_none() {
                return Err(unresolved(
                    root,
                    "the output capture has no valid exit record",
                ));
            }
            journal["collect"] = serde_json::json!(false);
            journal["stopped"] = serde_json::json!(true);
            journal_at(root, &journal)?;
        }
        let finished = match ended {
            Ok(ended) => finish(ended, root.to_owned(), limits.seconds, &spec).await,
            Err(error) => Err(error),
        };
        let finished = finished.and_then(|mut finished| {
            if self.remote.is_none() {
                finished.collect_outputs(&spec, job.staging)?;
            }
            Ok(finished)
        });
        if let Err(error) = Self::settle(self.remote.as_ref(), root, &journal).await {
            return match finished {
                Err(original) => Err(original),
                Ok(_) => Err(error),
            };
        }
        *prepared = None;
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
    async fn create(&self, manifest: serde_json::Value) -> Result<(String, bool), ExecutorError> {
        let resources = manifest["spec"]["template"]["spec"]["resources"].clone();
        let job = serde_json::from_value(manifest)
            .map_err(|error| machine(format!("the Job is not valid: {error}")))?;
        let jobs = self.cluster.jobs();
        let accepted =
            tokio::time::timeout(SUBMIT_GRACE, jobs.create(&PostParams::default(), &job))
                .await
                .map_err(|_| machine("the Job submission response timed out".into()))?
                .map_err(|error| machine(format!("the cluster did not take the Job: {error}")))?;
        let value = serde_json::to_value(&accepted).map_err(|error| machine(error.to_string()))?;
        let retained = resources.is_null()
            || resources_match(&resources, &value["spec"]["template"]["spec"]["resources"]);
        accepted
            .metadata
            .uid
            .filter(|uid| !uid.is_empty())
            .map(|uid| (uid, retained))
            .ok_or_else(|| machine("the Job submission response has no UID".into()))
    }

    /// A dry run must retain aggregate limits before multi-container work can start.
    async fn validate(&self, manifest: &serde_json::Value) -> Result<(), ExecutorError> {
        let resources = &manifest["spec"]["template"]["spec"]["resources"];
        if resources.is_null() {
            return Ok(());
        }
        let job = serde_json::from_value(manifest.clone())
            .map_err(|error| machine(format!("the Job is not valid: {error}")))?;
        let parameters = PostParams {
            dry_run: true,
            ..PostParams::default()
        };
        let accepted =
            tokio::time::timeout(SUBMIT_GRACE, self.cluster.jobs().create(&parameters, &job))
                .await
                .map_err(|_| machine("pod resource admission timed out".into()))?
                .map_err(|error| {
                    ExecutorError::Unsupported(format!(
                        "multi-container task caps require Kubernetes PodLevelResources: {error}",
                    ))
                })?;
        let accepted =
            serde_json::to_value(accepted).map_err(|error| machine(error.to_string()))?;
        if !resources_match(
            resources,
            &accepted["spec"]["template"]["spec"]["resources"],
        ) {
            return Err(ExecutorError::Unsupported(
                "the Kubernetes server did not retain aggregate task resource caps (PodLevelResources required)".into(),
            ));
        }
        Ok(())
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
        let pending = self.scratch.join("remote-pending");
        std::fs::create_dir_all(&pending)?;
        if !self.indexed.load(Ordering::Acquire) {
            // Import old journals once; later admissions visit only the bounded pending index.
            for entry in std::fs::read_dir(&self.scratch)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() && entry.path().join("remote.json").try_exists()? {
                    track(&entry.path())?;
                }
            }
            self.indexed.store(true, Ordering::Release);
        }
        let mut entries = Vec::new();
        {
            let mut cursor = self.pending.lock().await;
            if cursor.is_none() {
                *cursor = Some(std::fs::read_dir(&pending)?);
            }
            for _ in 0..64 {
                let Some(entry) = cursor.as_mut().expect("pending index").next() else {
                    *cursor = None;
                    break;
                };
                entries.push(entry?);
            }
        }
        let host = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        for entry in entries {
            tokio::task::yield_now().await;
            let root = self.scratch.join(entry.file_name());
            let path = root.join("remote.json");
            if !path.try_exists()? {
                match std::fs::remove_file(entry.path()) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                continue;
            }
            let mut journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|error| unresolved(&root, error))?;
            if journal["backend"] != "Kubernetes"
                || journal["server"].as_str() != Some(self.cluster.server())
                || journal["namespace"].as_str() != Some(self.cluster.namespace())
            {
                return Err(unresolved(&root, "the original cluster settings changed"));
            }
            let live_owner = journal["owner"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
                .and_then(owner_start)
                .as_deref()
                == journal["started"].as_str()
                && journal["host"].as_str() == Some(host.as_str())
                && journal["started"].is_string();
            let _ownership = if journal["locked"] == true {
                let lock = match File::options().write(true).open(root.join("remote.lock")) {
                    Ok(lock) => lock,
                    Err(_) if !path.try_exists()? => continue,
                    Err(error) => return Err(unresolved(&root, error)),
                };
                // The shared lock fences foreign processes too; PID namespaces cannot prove death.
                match lock.try_lock() {
                    Err(TryLockError::WouldBlock) => continue,
                    Err(TryLockError::Error(error)) => return Err(unresolved(&root, error)),
                    Ok(()) => Owned(lock),
                }
            } else {
                if journal["host"].as_str() != Some(host.as_str()) {
                    return Err(unresolved(
                        &root,
                        "a legacy journal has no transferable lock",
                    ));
                }
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
                || journal["server"].as_str() != Some(self.cluster.server())
                || journal["namespace"].as_str() != Some(self.cluster.namespace())
            {
                return Err(unresolved(&root, "the original cluster settings changed"));
            }
            if journal["stopped"] != true && journal["uid"].as_str().is_none_or(str::is_empty) {
                match self.identify(&root, &journal).await {
                    Submitted::Found(uid) => {
                        journal["uid"] = serde_json::json!(uid);
                        journal_at(&root, &journal)?;
                    }
                    Submitted::Absent => {
                        tracing::warn!(attempt = %root.display(),
                            "the Job of an unclear submission is not in the cluster; the attempt is lost");
                        journal["stopped"] = serde_json::json!(true);
                        journal_at(&root, &journal)?;
                    }
                    Submitted::Unclear(reason) => {
                        tracing::warn!(attempt = %root.display(), reason,
                            "cannot resolve an unclear submission yet");
                        continue;
                    }
                }
            }
            if journal["stopped"] != true {
                Self::stop(&self.cluster, self.poll, &root, &journal, CANCEL_GRACE).await?;
            }
            if let Err(error) = Self::settle(self.remote.as_ref(), &root, &journal).await {
                if journal["collect"] != true {
                    return Err(error);
                }
                tracing::warn!(attempt = %root.display(), %error,
                    "stopped attempt retains its pending output capture");
            }
        }
        Ok(())
    }

    /// Removes the objects in the staging root that the journal of an attempt names.
    pub(crate) async fn settle(
        remote: Option<&PodStaging>,
        root: &Path,
        journal: &serde_json::Value,
    ) -> Result<(), ExecutorError> {
        let staging = &journal["staging"];
        let (prefix, plan) = (staging["prefix"].as_str(), staging["plan"].as_str());
        if let (Some(prefix), Some(plan)) = (prefix, plan) {
            let remote =
                remote.ok_or_else(|| unresolved(root, "the staging configuration is missing"))?;
            if staging["root"]
                .as_str()
                .is_some_and(|value| value != remote.stage.objects.url(""))
            {
                return Err(unresolved(root, "the staging root changed"));
            }
            let mut stage = remote.stage.clone();
            stage.cleanup = staging["cleanup"].as_bool().unwrap_or(stage.cleanup);
            if journal["collect"] == true {
                if !stage.fetch(prefix, "exit", root).await? {
                    return Err(unresolved(root, "the output capture is not available"));
                }
                stage
                    .collect(prefix, root, executor::capture::RECORDS)
                    .await?;
                if Finished::from_control(root.to_owned())?.exit_code.is_none() {
                    return Err(unresolved(
                        root,
                        "the output capture has no valid exit record",
                    ));
                }
                let mut collected = journal.clone();
                collected["collect"] = serde_json::json!(false);
                journal_at(root, &collected)?;
            }
            stage.try_clean(prefix, plan).await?;
        }
        forget(root)
    }

    /// Finds the Job of a journal whose submission left no UID, by its name, which only this
    /// attempt has, and the label of the backend that made it.
    async fn identify(&self, root: &Path, journal: &serde_json::Value) -> Submitted {
        let age = self.poll * 4 + SUBMIT_GRACE + REQUEST_GRACE;
        if !executor::poll::old_enough(&root.join("remote.json"), age) {
            return Submitted::Unclear("the submission is too recent to tell");
        }
        // A Job counts as absent only when two answers in a row lack it.
        match self.look_up(journal).await {
            Submitted::Absent => {
                tokio::time::sleep(self.poll).await;
                self.look_up(journal).await
            }
            found => found,
        }
    }

    /// One answer of the cluster about the Job of a journal.
    async fn look_up(&self, journal: &serde_json::Value) -> Submitted {
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

    /// The name of the pod of the Job `name` once it exists, waiting at most as long as a pod
    /// may wait for a node.
    async fn pod_name(
        &self,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<String, ExecutorError> {
        let (pods, started) = (self.cluster.pods(), Instant::now());
        let selector = ListParams::default().labels(&format!("job-name={name}"));
        loop {
            let listed = tokio::time::timeout(REQUEST_GRACE, pods.list(&selector)).await;
            let first =
                (listed.ok().and_then(Result::ok)).and_then(|list| list.items.into_iter().next());
            if let Some(pod) = first.and_then(|pod| pod.metadata.name) {
                return Ok(pod);
            }
            if started.elapsed() >= self.unschedulable_time {
                return Err(machine(format!(
                    "the Job {name} made no pod for CWS to place"
                )));
            }
            tokio::select! {
                () = tokio::time::sleep(self.poll) => {}
                () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
            }
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
        if journal["stopped"] == true {
            return Ok(());
        }
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
        let mut journal = journal.clone();
        journal["stopped"] = serde_json::json!(true);
        journal_at(root, &journal)?;
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

fn forget(root: &Path) -> Result<(), ExecutorError> {
    std::fs::remove_file(root.join("remote.json"))?;
    File::open(root)?.sync_all()?;
    let pending = marker(root)?;
    match std::fs::remove_file(&pending) {
        Ok(()) => File::open(pending.parent().expect("index folder"))?.sync_all()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn marker(root: &Path) -> Result<PathBuf, ExecutorError> {
    let parent = root
        .parent()
        .ok_or_else(|| unresolved(root, "attempt has no parent"))?;
    let name = root
        .file_name()
        .ok_or_else(|| unresolved(root, "attempt has no name"))?;
    Ok(parent.join("remote-pending").join(name))
}

fn resources_match(expected: &serde_json::Value, actual: &serde_json::Value) -> bool {
    ["requests", "limits"].into_iter().all(|kind| {
        ["cpu", "memory"].into_iter().all(|name| {
            let Some(expected) = expected[kind][name].as_str() else {
                return true;
            };
            let actual = actual[kind][name].as_str().and_then(quantity);
            actual.is_some() && actual == quantity(expected)
        })
    })
}

fn quantity(text: &str) -> Option<u128> {
    let index = text
        .find(|letter: char| !letter.is_ascii_digit())
        .unwrap_or(text.len());
    let number = text[..index].parse::<u128>().ok()?;
    let suffix = &text[index..];
    let scale = match suffix {
        "m" => 1,
        "" => 1000,
        "k" => 1_000_000,
        "M" => 1_000_000_000,
        "G" => 1_000_000_000_000,
        "T" => 1_000_000_000_000_000,
        "P" => 1_000_000_000_000_000_000,
        "E" => 1_000_000_000_000_000_000_000,
        "Ki" => 1024 * 1000,
        "Mi" => 1024u128.pow(2) * 1000,
        "Gi" => 1024u128.pow(3) * 1000,
        "Ti" => 1024u128.pow(4) * 1000,
        "Pi" => 1024u128.pow(5) * 1000,
        "Ei" => 1024u128.pow(6) * 1000,
        _ => return None,
    };
    number.checked_mul(scale)
}

fn track(root: &Path) -> Result<(), ExecutorError> {
    let pending = marker(root)?;
    let parent = pending.parent().expect("index folder");
    std::fs::create_dir_all(parent)?;
    match File::options().write(true).create_new(true).open(&pending) {
        Ok(file) => file.sync_all()?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    File::open(parent)?.sync_all()?;
    File::open(root.parent().expect("attempt parent"))?.sync_all()?;
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
        // A task that ended with 124 itself recorded it; the timer ends the wrapper first.
        Ended::Exited(executor::wrapper::TIMED_OUT, _)
            if time.is_some() && recorded != Some(executor::wrapper::TIMED_OUT) =>
        {
            Ok((143, Some(too_long(time))))
        }
        Ended::Exited(status, reason) if reason == "OOMKilled" => Ok((
            *status,
            Some("uwe: Kubernetes stopped the task for lack of memory\n".to_owned()),
        )),
        Ended::Exited(status @ 129..=192, _) => Ok((
            *status,
            Some(format!(
                "uwe: the container ended with exit code {status}, maybe for lack of memory\n"
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
        Some(time) => format!("uwe: the task ran longer than its time of {time} seconds\n"),
        None => "uwe: the task ran longer than Kubernetes allows\n".to_owned(),
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

    #[test]
    fn accepts_canonical_quantities() {
        let expected = serde_json::json!({"limits": {"cpu": "2", "memory": "1073741824"},
            "requests": {"cpu": "2", "memory": "1073741824"}});
        let mut actual = serde_json::json!({"limits": {"cpu": "2000m", "memory": "1Gi"},
            "requests": {"cpu": "2", "memory": "1024Mi"}});
        assert!(resources_match(&expected, &actual));
        actual["limits"]["memory"] = serde_json::json!("2Gi");
        assert!(!resources_match(&expected, &actual));
        assert!(!resources_match(&expected, &serde_json::Value::Null));
    }

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
        let own = outcome(Some(124), &exited(124, "Error"), Some(9));
        assert_eq!(own, Ok((124, None)), "a task that ends with 124 itself");
        assert!(outcome(None, &Ended::Stopped("Evicted".into()), None).is_err());
    }
}
