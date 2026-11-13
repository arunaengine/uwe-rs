//! Runs a task in a container with the local Docker or Podman, in a scratch directory.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::errors::Error as ApiError;
use bollard::models::{ContainerCreateBody, HostConfig, MountType};
use bollard::query_parameters::{
    AttachContainerOptions, CreateContainerOptions, StopContainerOptions, WaitContainerOptions,
};
use futures_util::StreamExt;
use task_model::{Image, TaskSpec};
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::access::{self, Access, NO_NEW_PRIVILEGES};
use crate::engine::{ContainerEngine, Created, EngineCommand};
use crate::image::{self, Commands};
use crate::job::{CONTROL, ExecutorError, Finished, HOME, Job, Usage, WORK};
use crate::manifest::parse;
use crate::pool::{Pool, Share};
use crate::process::{Mount, ProcessError, create_id};
use crate::stage::place_cancellable;
use crate::wrapper::{ENDED, Folders, MANIFEST, STDERR, wrapper};

/// How long a task may take to end after it was asked to stop, before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// The least memory that Docker and Podman give a container (6 MiB).
const LEAST_MEMORY: u64 = 6 << 20;

/// The memory of the container that hands the files of a task back to the user (64 MiB).
const HAND_BACK_MEMORY: i64 = 64 << 20;

type ImageCommands = HashMap<(String, Option<String>), Arc<OnceCell<Commands>>>;

/// The local container executor.
#[derive(Debug)]
pub struct LocalContainer {
    pub engine: ContainerEngine,
    /// Where each task gets its scratch directory.
    pub scratch: PathBuf,
    /// The CPUs and memory that tasks share.
    pub pool: Pool,
    /// What containers of each image run by default, asked once per run, as the runner pins
    /// each image once per run.
    commands: Mutex<ImageCommands>,
    /// The network of the run that daemons and the tasks after them share, once a daemon ran.
    pub(crate) network: OnceCell<String>,
    /// The tasks that stop the daemons of the run when it asks them to.
    pub(crate) daemons: Mutex<Vec<JoinHandle<()>>>,
    /// Whether HTTP and TCP probes of daemons go through ports published on this machine, also
    /// when the engine is neither rootless nor in a VM, which always do so.
    pub probe_ports: bool,
}

/// How a container ended.
struct Ending {
    /// The exit code of the container; 0 when the wrapper said that it ended.
    status: i64,
    /// Whether the kernel killed the container's first process for lack of memory, as the
    /// engine tells it for a container whose wrapper did not end.
    out_of_memory: bool,
    /// The time of the task, when the container ran longer and was stopped.
    timed_out: Option<Duration>,
    /// The last lines that the container wrote, for errors.
    log: String,
}

impl crate::Backend for LocalContainer {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        image::pin(&self.engine, image).await
    }

    fn hold(&self) {
        self.pool.hold();
    }

    async fn release(&self) {
        self.release_daemons().await;
    }

    /// Runs a task in `<scratch>/<name>`, a name that must not be in use: stages its inputs,
    /// runs the wrapper in the task's image as the current user, and reads the control files.
    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let job = admit(job)?;
        let placement = job.placement;
        let groups = crate::containers::groups(job.spec).ok_or(ExecutorError::NoImage)?;
        let memory = placement.memory_bytes.map(|bytes| {
            if groups.len() <= 1 {
                bytes.max(LEAST_MEMORY)
            } else {
                bytes
            }
        });
        // A task that waits for its share when the run is held never starts.
        let share = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
            () = self.pool.held() => return Err(ExecutorError::Cancelled),
            share = self.pool.share(placement.cpus, memory, placement.priority) => share,
        };
        group_resources(share.cpus, share.memory, groups.len().max(1))?;
        let root = self.scratch.join(job.name);
        let placed = place_cancellable(&root, job.staging, cancel).await;
        let result = match placed {
            // Another attempt has this name; its directory is not ours to remove.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(error.into());
            }
            Err(_) if cancel.is_cancelled() => Err(ExecutorError::Cancelled),
            Err(error) => Err(error.into()),
            Ok(mounts) => self
                .contain(job, &root, &mounts, share, cancel)
                .await
                .and_then(|mut finished| {
                    finished.collect_outputs(job.spec, job.staging)?;
                    Ok(finished)
                }),
        };
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        result
    }
}

fn admit<'a>(job: Job<'a>) -> Result<Job<'a>, ExecutorError> {
    let flags = Flags::of(job.spec)?;
    if job.spec.environment.contains_key(FLAGS_ENV) && !job.spec.containers.is_empty() {
        return Err(ExecutorError::Unsupported(
            "container flags for multiple containers".into(),
        ));
    }
    if job
        .placement
        .accelerators
        .iter()
        .any(|accelerator| accelerator.count != 0)
        && flags.gpus.is_none()
    {
        return Err(ExecutorError::Unsupported(
            "the local executor does not support accelerators".into(),
        ));
    }
    Ok(job)
}

const FLAGS_ENV: &str = "UWE_CONTAINER_FLAGS";

#[derive(Default)]
struct Flags {
    cpus: Option<i64>,
    shm: Option<i64>,
    swap: Option<i64>,
    gpus: Option<i64>,
    /// The container whose network this one shares, as the containers of a pod share it.
    network: Option<String>,
}

impl Flags {
    fn of(spec: &TaskSpec) -> Result<Self, ExecutorError> {
        let Some(task_model::EnvironmentValue::Literal(text)) = spec.environment.get(FLAGS_ENV)
        else {
            return Ok(Self::default());
        };
        let failed = || ExecutorError::Unsupported("invalid container launch flags".into());
        let values: std::collections::BTreeMap<String, i64> =
            serde_json::from_str(text).map_err(|_| failed())?;
        if values
            .keys()
            .any(|name| !matches!(name.as_str(), "cpus" | "shm" | "swap" | "gpus"))
        {
            return Err(failed());
        }
        let flags = Self {
            cpus: values.get("cpus").copied(),
            shm: values.get("shm").copied(),
            swap: values.get("swap").copied(),
            gpus: values.get("gpus").copied(),
            network: None,
        };
        if flags.cpus.is_some_and(|value| value <= 0)
            || flags.shm.is_some_and(|value| value < 0)
            || flags.swap.is_some_and(|value| value < -1)
            || flags
                .gpus
                .is_some_and(|value| value < -1 || value > i32::MAX as i64)
        {
            return Err(failed());
        }
        Ok(flags)
    }

    fn apply(&self, config: &mut HostConfig) {
        if let Some(network) = &self.network {
            config.network_mode = Some(network.clone());
        }
        if let Some(cpus) = self.cpus {
            config.nano_cpus = Some(config.nano_cpus.map_or(cpus, |granted| granted.min(cpus)));
        }
        if let Some(shm) = self.shm {
            config.shm_size = Some(shm);
        }
        if let Some(swap) = self.swap {
            config.memory_swap = Some(swap);
        }
        if let Some(gpus) = self.gpus {
            config.device_requests = Some(vec![bollard::models::DeviceRequest {
                count: Some(gpus),
                capabilities: Some(vec![vec!["gpu".into()]]),
                ..Default::default()
            }]);
        }
    }
}

/// How a container runs the wrapper of a task staged in its scratch directory.
#[derive(Clone, Copy, Debug)]
pub struct ContainerRun<'a> {
    /// The name of the container.
    pub name: &'a str,
    pub image: &'a Image,
    /// The scratch directory, which the container sees at the same path.
    pub root: &'a Path,
    /// Where the container sees the work directory, when the task names a place for it.
    pub workdir: Option<&'a str>,
    /// What the container sees besides the scratch directory.
    pub mounts: &'a [Mount],
    /// The user and group that the task runs as, so that its files belong to them.
    pub owner: (u32, u32),
    /// The CPUs and bytes of memory that the container may use.
    pub cpus: u32,
    pub memory: Option<u64>,
    /// Whether the container runs without network.
    pub offline: bool,
    /// The wrapper that the container runs, when it is not the wrapper of the task in the
    /// control folder, as for the containers of a task that runs several.
    pub wrapper_path: Option<&'a Path>,
    /// Who the container runs as, and with which capabilities.
    pub access: &'a Access,
}

impl ContainerRun<'_> {
    /// Where the container sees the work directory: at the task's `workdir`, unless the
    /// container runs as Kubernetes runs it and only starts there.
    pub fn inside(&self) -> String {
        (self.work_place().map(str::to_owned)).unwrap_or_else(|| text(&self.root.join(WORK)))
    }

    /// The place of the work directory that the task names, if any.
    fn work_place(&self) -> Option<&str> {
        self.workdir.filter(|_| self.access.run_as.is_none())
    }

    /// Where the container starts: in the work directory, or, when it runs as Kubernetes runs
    /// it, in the task's `workdir` or else (`None`) in the working directory of its image.
    pub fn start(&self) -> Option<String> {
        match self.access.run_as {
            Some(_) => self.workdir.map(str::to_owned),
            None => Some(self.inside()),
        }
    }

    /// Maps scratch, workdir and input mounts into the container, including workdir
    /// replacements.
    pub fn binds(&self) -> Vec<Mount> {
        let inside = self.inside();
        let mounts: Vec<Mount> = (self.mounts.iter())
            .map(|mount| Mount {
                source: mount.source.clone(),
                target: match mount.target.as_str() {
                    "." => inside.clone(),
                    target if target.starts_with('/') => target.to_owned(),
                    target => format!("{}/{target}", inside.trim_end_matches('/')),
                },
                read_only: mount.read_only,
            })
            .collect();
        let mut binds = vec![Mount {
            source: self.root.to_owned(),
            target: text(self.root),
            read_only: false,
        }];
        if let Some(workdir) = self.work_place()
            && !mounts.iter().any(|mount| mount.target == workdir)
        {
            binds.push(Mount {
                source: self.root.join(WORK),
                target: workdir.to_owned(),
                read_only: false,
            });
        }
        binds.extend(mounts);
        binds
    }

    /// The wrapper in the control folder, which the container runs.
    pub fn script(&self) -> String {
        match self.wrapper_path {
            Some(path) => text(path),
            None => text(&self.root.join(CONTROL).join("wrapper.sh")),
        }
    }

    /// The wrapper of a task in this container: its work directory where the container sees
    /// it, and its control files and home folder in the scratch directory.
    pub fn wrapper(&self, spec: &TaskSpec) -> String {
        let control = text(&self.root.join(CONTROL));
        let home = text(&self.root.join(HOME));
        let folders = Folders {
            work: &self.inside(),
            control: &control,
            home: &home,
            outbox: "",
            owner: &self.access.owner(),
        };
        wrapper(spec, &folders)
    }

    /// The container that runs the wrapper with the shell of the image, for the engine's API.
    /// The folders are mounts, not `source:target` binds, so that a path may hold a `:`.
    pub fn create(&self) -> ContainerCreateBody {
        self.create_with_cpus(i64::from(self.cpus) * 1_000_000_000)
    }

    fn policy(&self) -> HostConfig {
        HostConfig {
            cap_drop: Some(vec!["ALL".to_owned()]),
            cap_add: Some(
                self.access
                    .capabilities
                    .iter()
                    .map(|&name| name.to_owned())
                    .collect(),
            ),
            security_opt: Some(vec![NO_NEW_PRIVILEGES.to_owned()]),
            auto_remove: Some(true),
            nano_cpus: Some(i64::from(self.cpus) * 1_000_000_000),
            memory: self.memory.map(|bytes| bytes.max(LEAST_MEMORY) as i64),
            network_mode: self.offline.then(|| "none".to_owned()),
            ..HostConfig::default()
        }
    }

    fn create_with_cpus(&self, nano_cpus: i64) -> ContainerCreateBody {
        let mounts = (self.binds().into_iter())
            .map(|bind| bollard::models::Mount {
                target: Some(bind.target),
                source: Some(text(&bind.source)),
                typ: Some(MountType::BIND),
                read_only: Some(bind.read_only),
                ..bollard::models::Mount::default()
            })
            .collect();
        ContainerCreateBody {
            image: Some(self.image.reference.clone()),
            // The shell of the image, wherever its `PATH` has it.
            entrypoint: Some(vec!["sh".to_owned()]),
            cmd: Some(vec![self.script()]),
            user: self.access.user.clone(),
            working_dir: self.start(),
            host_config: Some(HostConfig {
                mounts: Some(mounts),
                nano_cpus: Some(nano_cpus),
                ..self.policy()
            }),
            ..ContainerCreateBody::default()
        }
    }

    /// The arguments of the engine's command that run the wrapper with Bash in the image.
    pub fn arguments(&self, engine: &EngineCommand) -> Vec<String> {
        let policy = self.policy();
        let mut arguments = vec!["run".to_owned()];
        if policy.auto_remove == Some(true) {
            arguments.push("--rm".into());
        }
        arguments.extend(
            ["--name", self.name]
                .into_iter()
                .chain(["--entrypoint", "sh"])
                .map(str::to_owned),
        );
        match (self.access.run_as, &self.access.user) {
            (None, _) => arguments.extend(engine.user_options(self.owner.0, self.owner.1)),
            (Some(_), Some(user)) => arguments.extend(["--user".into(), user.clone()]),
            (Some(_), None) => {}
        }
        for capability in policy.cap_drop.into_iter().flatten() {
            arguments.extend(["--cap-drop".into(), capability]);
        }
        for capability in policy.cap_add.into_iter().flatten() {
            arguments.extend(["--cap-add".into(), capability]);
        }
        for option in policy.security_opt.into_iter().flatten() {
            arguments.extend(["--security-opt".into(), option]);
        }
        for bind in self.binds() {
            arguments.extend(["--volume".into(), volume(&bind)]);
        }
        if let Some(start) = self.start() {
            arguments.extend(["--workdir".into(), start]);
        }
        if let Some(nano_cpus) = policy.nano_cpus {
            arguments.extend(["--cpus".into(), (nano_cpus / 1_000_000_000).to_string()]);
        }
        if let Some(memory) = policy.memory {
            arguments.extend(["--memory".into(), memory.to_string()]);
        }
        if let Some(network) = policy.network_mode {
            arguments.extend(["--network".into(), network]);
        }
        if let Some(platform) = &self.image.platform {
            arguments.extend(["--platform".into(), platform.clone()]);
        }
        arguments.extend([self.image.reference.clone(), self.script()]);
        arguments
    }
}

impl LocalContainer {
    /// The executor of a run with the engine of this machine, which runs each task in its
    /// own scratch directory in `scratch`, within its share of `pool`.
    pub fn new(engine: ContainerEngine, scratch: PathBuf, pool: Pool) -> LocalContainer {
        let cpus = engine
            .cpus
            .map_or(pool.cpus(), |cpus| cpus.min(pool.cpus()));
        let memory = engine
            .memory
            .map_or(pool.memory(), |memory| memory.min(pool.memory()));
        let pool = Pool::new(cpus, memory);
        LocalContainer {
            engine,
            scratch,
            pool,
            commands: Mutex::default(),
            network: OnceCell::new(),
            daemons: Mutex::default(),
            probe_ports: false,
        }
    }

    /// Runs the container of a staged task to its end, within its share of the pool.
    async fn contain(
        &self,
        job: Job<'_>,
        root: &Path,
        mounts: &[Mount],
        share: Share<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        if !job.spec.containers.is_empty() {
            return self
                .contain_several(job, root, mounts, &share, cancel)
                .await;
        }
        let image = job.spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let mut spec = job.spec.clone();
        let mut flags = Flags::of(&spec)?;
        spec.environment.remove(FLAGS_ENV);
        let daemon = match spec.environment.remove(crate::daemon::DAEMON_ENV) {
            Some(task_model::EnvironmentValue::Literal(text)) => Some(
                serde_json::from_str::<crate::daemon::Daemon>(&text)
                    .map_err(|_| ExecutorError::Unsupported("invalid daemon settings".into()))?,
            ),
            _ => None,
        };
        // Tasks after a daemon reach it on the network of the run, as pods reach each other.
        if !job.placement.offline {
            flags.network = match &daemon {
                Some(_) => Some(self.daemon_network().await?),
                None => self.network.get().cloned(),
            };
        }
        if let Some(task_model::EnvironmentValue::Literal(names)) =
            spec.environment.remove("UWE_WORK_VARIABLES")
        {
            let names: Vec<String> = serde_json::from_str(&names).map_err(|_| {
                ExecutorError::Unsupported("invalid work directory environment binding".into())
            })?;
            for name in names {
                spec.environment
                    .insert(name, task_model::EnvironmentValue::WorkDirectory);
            }
            spec.validate()
                .map_err(|error| ExecutorError::Unsupported(error.to_string()))?;
        }
        // The command runs through the image's entrypoint, as `docker run` runs it, which
        // some images need to set up their tools, unless it replaces the entrypoint.
        let entrypoint = !spec.replaces_entrypoint || spec.arguments.is_empty();
        let commands = match entrypoint || spec.run_as.is_some() {
            true => self.commands(image).await?,
            false => Commands::default(),
        };
        if entrypoint {
            spec.arguments = commands.arguments(&image.reference, spec.arguments)?;
        }
        let owner = tokio::fs::metadata(root).await?;
        let access = self
            .access(root, (owner.uid(), owner.gid()), &spec, &commands)
            .await?;
        let container = format!("uwe-{}", job.name);
        let run = ContainerRun {
            name: &container,
            image,
            root,
            workdir: spec.workdir.as_deref(),
            mounts,
            owner: (owner.uid(), owner.gid()),
            cpus: share.cpus,
            memory: share.memory,
            offline: job.placement.offline,
            wrapper_path: None,
            access: &access,
        };
        tokio::fs::write(run.script(), run.wrapper(&spec)).await?;
        if let Some(daemon) = daemon {
            let time = job.placement.time_seconds.map(Duration::from_secs);
            let mut body = run.create_with_cpus(i64::from(share.cpus) * 1_000_000_000);
            if let Some(config) = &mut body.host_config {
                flags.apply(config);
                config.auto_remove = Some(false);
            }
            return self
                .serve(
                    &container,
                    body,
                    daemon,
                    root.to_owned(),
                    time,
                    cancel,
                    share.owned(),
                )
                .await;
        }
        let time = job.placement.time_seconds.map(Duration::from_secs);
        let ending = self
            .run_to_end(
                &run,
                i64::from(share.cpus) * 1_000_000_000,
                time,
                cancel,
                &flags,
            )
            .await;
        self.hand_back(&access, image, root, owner.uid(), cancel)
            .await;
        self.finish(ending?, root.to_owned(), &spec).await
    }

    /// Runs a task of several containers: one container of the engine for each image, all at
    /// once, on the same scratch directory and mounts, within the share of the task.
    async fn contain_several(
        &self,
        job: Job<'_>,
        root: &Path,
        mounts: &[Mount],
        share: &Share<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let mut spec = job.spec.clone();
        for container in &mut spec.containers {
            let image = container.image.as_ref().ok_or(ExecutorError::NoImage)?;
            if container.replaces_entrypoint && !container.arguments.is_empty() {
                continue;
            }
            let commands = self.commands(image).await?;
            let arguments = std::mem::take(&mut container.arguments);
            container.arguments = commands.arguments(&image.reference, arguments)?;
        }
        let owner = tokio::fs::metadata(root).await?;
        let groups = crate::containers::groups(&spec).ok_or(ExecutorError::NoImage)?;
        let resources = group_resources(share.cpus, share.memory, groups.len())?;
        let mut accesses = Vec::new();
        for group in &groups {
            let commands = match spec.run_as {
                Some(_) => self.commands(&group.image).await?,
                None => Commands::default(),
            };
            accesses.push(Access::of(
                (owner.uid(), owner.gid()),
                self.engine.rootless,
                spec.run_as,
                &commands.user,
            ));
        }
        let (control, home) = (root.join(CONTROL), text(&root.join(HOME)));
        let work = match spec.run_as {
            Some(_) => text(&root.join(WORK)),
            None => (spec.workdir.clone()).unwrap_or_else(|| text(&root.join(WORK))),
        };
        let folders = Folders {
            work: &work,
            control: &text(&control),
            home: &home,
            outbox: "",
            owner: "",
        };
        let groups = crate::containers::write_wrappers(&spec, &control, &folders)?;
        for (index, (group, path)) in groups.iter().enumerate() {
            let members: Vec<_> = group
                .members
                .iter()
                .map(|&position| {
                    (
                        text(&control.join(crate::containers::wrapper_path(&spec, position))),
                        text(&control.join(crate::containers::exit_file(&spec, position))),
                    )
                })
                .collect();
            tokio::fs::write(
                path,
                crate::containers::group_wrapper(&members, &accesses[index].owner(), &text(root)),
            )
            .await?;
        }
        // The folders of the wrappers are new, so they are opened after them.
        if let Some(access) = accesses.iter().find(|access| access.hand_back.is_some()) {
            open(access, root).await?;
        }
        let names: Vec<String> = (0..groups.len())
            .map(|index| format!("uwe-{}-{index}", job.name))
            .collect();
        let paths: Vec<PathBuf> = groups.iter().map(|(_, path)| path.into()).collect();
        let runs: Vec<ContainerRun<'_>> = (groups.iter().enumerate())
            .map(|(index, (group, _))| ContainerRun {
                name: &names[index],
                image: &group.image,
                root,
                workdir: spec.workdir.as_deref(),
                mounts,
                owner: (owner.uid(), owner.gid()),
                cpus: share.cpus,
                memory: resources[index].1,
                offline: job.placement.offline,
                wrapper_path: Some(&paths[index]),
                access: &accesses[index],
            })
            .collect();
        let time = job.placement.time_seconds.map(Duration::from_secs);
        let stop = cancel.child_token();
        // With services, every container shares the network of the first group that holds
        // one, which lives until the other containers ended, so that they reach each other
        // at `localhost` as in a pod.
        let sharing = (groups.iter())
            .position(|(group, _)| group.members.iter().any(|&at| spec.containers[at].service));
        let shared = (self.network.get().cloned()).filter(|_| !job.placement.offline);
        let flags: Vec<Flags> = (0..runs.len())
            .map(|index| Flags {
                network: match sharing.filter(|&at| at != index) {
                    Some(at) => Some(format!("container:{}", names[at])),
                    None => shared.clone(),
                },
                ..Flags::default()
            })
            .collect();
        let mut running: futures_util::stream::FuturesUnordered<_> = (runs.iter().enumerate())
            .map(|(index, run)| {
                let (flags, stop, cpus) = (&flags[index], &stop, resources[index].0);
                let owner = sharing.filter(|&at| at != index).map(|at| &names[at]);
                async move {
                    if let Some(owner) = owner {
                        self.wait_running(owner, stop).await?;
                    }
                    self.run_to_end(run, cpus, time, stop, flags).await
                }
            })
            .collect();
        let mut first: Option<Result<Ending, ExecutorError>> = None;
        let mut normal = None;
        while let Some(result) = running.next().await {
            let ended_well = matches!(&result, Ok(ending) if ending.status == 0
                && ending.timed_out.is_none() && !ending.out_of_memory);
            match (ended_well, &first) {
                (true, _) => normal = Some(result),
                (false, None) => {
                    stop.cancel();
                    first = Some(result);
                }
                (false, Some(_)) => {}
            }
        }
        drop(running);
        for (index, access) in accesses.iter().enumerate() {
            self.hand_back(access, &groups[index].0.image, root, owner.uid(), cancel)
                .await;
        }
        let ending = match (first, normal) {
            (Some(result), _) | (None, Some(result)) => result?,
            (None, None) => return Err(ExecutorError::NoImage),
        };
        self.finish(ending, root.to_owned(), &spec).await
    }

    /// Waits until the container of this name runs, for a container that joins its network.
    async fn wait_running(
        &self,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<(), ExecutorError> {
        let client = self.engine.client();
        for _ in 0..1200 {
            let running = client
                .inspect_container(name, None)
                .await
                .ok()
                .and_then(|found| found.state)
                .and_then(|state| state.running);
            if running == Some(true) {
                return Ok(());
            }
            tokio::select! {
                () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
        Err(ExecutorError::Engine {
            engine: self.engine.name.clone(),
            message: format!("container {name}, whose network the others share, did not start"),
        })
    }

    /// How the containers of a task run, and its scratch directory `root` open to every user
    /// when they may run as another user than `owner`, the user who runs uwe.
    async fn access(
        &self,
        root: &Path,
        owner: (u32, u32),
        spec: &TaskSpec,
        commands: &Commands,
    ) -> Result<Access, ExecutorError> {
        let access = Access::of(owner, self.engine.rootless, spec.run_as, &commands.user);
        open(&access, root).await?;
        Ok(access)
    }

    /// Returns scratch outputs to `user` with a restricted root helper when the wrapper could
    /// not.
    async fn hand_back(
        &self,
        access: &Access,
        image: &Image,
        root: &Path,
        user: u32,
        cancel: &CancellationToken,
    ) {
        let Some(owner) = access.hand_back else {
            return;
        };
        let walked = root.to_owned();
        let foreign = tokio::task::spawn_blocking(move || access::foreign(&walked, user));
        if !foreign.await.unwrap_or(true) {
            return;
        }
        let client = self.engine.client();
        let name = format!(
            "{}-hand-back",
            root.file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
        );
        let name = format!("uwe-{name}");
        let body = ContainerCreateBody {
            image: Some(image.reference.clone()),
            entrypoint: Some(access::hand_back_command(root, owner)),
            cmd: Some(Vec::new()),
            user: Some("0:0".to_owned()),
            host_config: Some(HostConfig {
                auto_remove: Some(true),
                mounts: Some(vec![bollard::models::Mount {
                    target: Some(text(root)),
                    source: Some(text(root)),
                    typ: Some(MountType::BIND),
                    ..bollard::models::Mount::default()
                }]),
                cap_drop: Some(vec!["ALL".to_owned()]),
                cap_add: Some(access::HAND_BACK_CAPABILITIES.map(str::to_owned).to_vec()),
                security_opt: Some(vec![NO_NEW_PRIVILEGES.to_owned()]),
                network_mode: Some("none".to_owned()),
                memory: Some(HAND_BACK_MEMORY),
                ..HostConfig::default()
            }),
            ..ContainerCreateBody::default()
        };
        let options = CreateContainerOptions {
            name: Some(name.clone()),
            platform: image.platform.clone().unwrap_or_default(),
        };
        let id = match create_id(&self.engine, options, body, cancel).await {
            Ok(id) => id,
            Err(_) => return,
        };
        let created = Created::owned(&self.engine, &id);
        let mut exited = tokio::spawn(exit_code(client.clone(), id.clone()));
        let ended = async {
            client.start_container(&id, None).await?;
            let _ = (&mut exited).await;
            Ok::<_, ApiError>(())
        };
        tokio::select! {
            () = cancel.cancelled() => {},
            _ = ended => {},
        }
        exited.abort();
        created.remove().await;
    }

    /// What containers of an image run by default, asked once per run.
    async fn commands(&self, image: &Image) -> Result<Commands, ExecutorError> {
        let cell = {
            let mut commands = (self.commands.lock()).unwrap_or_else(|poison| poison.into_inner());
            commands
                .entry((image.reference.clone(), image.platform.clone()))
                .or_default()
                .clone()
        };
        let asked = cell.get_or_try_init(|| {
            Commands::of(&self.engine, &image.reference, image.platform.as_deref())
        });
        asked.await.cloned()
    }

    /// Runs a container to its end: creates it, attaches to its output before it starts, and
    /// reads the output until the wrapper says that it ended.
    async fn run_to_end(
        &self,
        run: &ContainerRun<'_>,
        nano_cpus: i64,
        time: Option<Duration>,
        cancel: &CancellationToken,
        flags: &Flags,
    ) -> Result<Ending, ExecutorError> {
        let client = self.engine.client();
        let failed = |error| image::failed(&self.engine, error);
        let options = CreateContainerOptions {
            name: Some(run.name.to_owned()),
            platform: run.image.platform.clone().unwrap_or_default(),
        };
        let mut body = run.create_with_cpus(nano_cpus);
        if let Some(config) = &mut body.host_config {
            flags.apply(config);
        }
        if let Some(platform) = run.image.platform.as_deref() {
            let image =
                image::available_for(&self.engine, &run.image.reference, Some(platform)).await?;
            body.image = Some(image.id.ok_or_else(|| {
                ExecutorError::Unsupported(format!(
                    "the engine's image {} has no ID for its requested platform",
                    run.image.reference
                ))
            })?);
        } else {
            // An image that left the engine after the run pinned it, as by a prune, comes again.
            image::available(&self.engine, &run.image.reference).await?;
        }
        let id = create_id(&self.engine, options, body, cancel)
            .await
            .map_err(|error| match error {
                ProcessError::Cancelled => ExecutorError::Cancelled,
                error => ExecutorError::Engine {
                    engine: self.engine.name.clone(),
                    message: error.to_string(),
                },
            })?;
        let created = Created::owned(&self.engine, &id);
        let attach = AttachContainerOptions {
            stream: true,
            stdout: true,
            stderr: true,
            ..AttachContainerOptions::default()
        };
        let started = async {
            let attached = client.attach_container(&id, Some(attach)).await?;
            let exited = tokio::spawn(exit_code(client.clone(), id.clone()));
            match client.start_container(&id, None).await {
                Ok(()) => Ok((attached.output, exited)),
                Err(error) => {
                    exited.abort();
                    Err(error)
                }
            }
        };
        let ending = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ExecutorError::Cancelled),
            started = started => match started {
                Ok((stream, exited)) => self.follow(&id, stream, exited, time, cancel).await,
                Err(error) => Err(
                    match error
                        .to_string()
                        .contains("\"sh\": executable file not found")
                    {
                        true => ExecutorError::NoShell(run.image.reference.clone()),
                        false => failed(error),
                    },
                ),
            }
        };
        created.remove().await;
        ending
    }

    /// Follows a started container until the wrapper says that it ended, or until the
    /// container's process ended, and stops it when `cancel` fires or its time is up.
    async fn follow(
        &self,
        container: &str,
        mut stream: impl futures_util::Stream<Item = Result<LogOutput, ApiError>> + Unpin,
        mut exited: JoinHandle<i64>,
        time: Option<Duration>,
        cancel: &CancellationToken,
    ) -> Result<Ending, ExecutorError> {
        let client = self.engine.client();
        let limit = async {
            match time {
                Some(time) => tokio::time::sleep(time).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(limit);
        let mut output = Output::default();
        let mut open = true;
        let mut status = None;
        let mut timed_out = None;
        while !output.ended && (open || status.is_none()) {
            tokio::select! {
                part = stream.next(), if open => match part {
                    Some(Ok(part)) => output.push(part.as_ref()),
                    Some(Err(_)) | None => open = false,
                },
                code = &mut exited, if status.is_none() => status = Some(code.unwrap_or(-1)),
                () = cancel.cancelled() => {
                    exited.abort();
                    return Err(ExecutorError::Cancelled);
                }
                () = &mut limit, if timed_out.is_none() => {
                    let stop = StopContainerOptions {
                        t: Some(STOP_GRACE.as_secs() as i32),
                        ..StopContainerOptions::default()
                    };
                    tokio::select! {
                        () = cancel.cancelled() => {
                            exited.abort();
                            return Err(ExecutorError::Cancelled);
                        }
                        _ = client.stop_container(container, Some(stop)) => {}
                    }
                    timed_out = time;
                }
            }
        }
        if output.ended && timed_out.is_none() {
            exited.abort();
            return Ok(Ending {
                status: 0,
                out_of_memory: false,
                timed_out,
                log: output.text(),
            });
        }
        let status = match status {
            Some(status) => status,
            None => tokio::select! {
                code = &mut exited => code.unwrap_or(-1),
                () = cancel.cancelled() => {
                    exited.abort();
                    return Err(ExecutorError::Cancelled);
                }
            },
        };
        // The engine may have removed the container already; then its lack of memory is not
        // known, and the note on the task says that it may be the cause.
        let inspected = tokio::select! {
            inspected = client.inspect_container(container, None) => inspected.ok(),
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
        };
        let state = inspected.and_then(|inspected| inspected.state);
        Ok(Ending {
            status,
            out_of_memory: state.and_then(|state| state.oom_killed).unwrap_or(false),
            timed_out,
            log: output.text(),
        })
    }

    /// What a container left behind.
    async fn finish(
        &self,
        ending: Ending,
        root: PathBuf,
        spec: &TaskSpec,
    ) -> Result<Finished, ExecutorError> {
        let control = root.join(CONTROL);
        let recorded = crate::containers::recorded(spec, &control);
        let status = i32::try_from(ending.status).unwrap_or(-1);
        let usage = Usage::of(&root);
        // The wrapper reads from the cgroup of the container whether the kernel killed a
        // process of the task for lack of memory.
        let out_of_memory =
            ending.out_of_memory || usage.is_some_and(|usage| usage.oom_kills.unwrap_or(0) > 0);
        let exit_code = match (recorded, status, ending.timed_out, out_of_memory) {
            // The wrapper recorded the tool's exit code and ended.
            (Some(code), 0, None, _) => code,
            (_, 0 | 125..=127 | -1, None, false) => {
                return Err(ExecutorError::Engine {
                    engine: self.engine.name.clone(),
                    message: match ending.log.is_empty() {
                        true => format!("the container ended with exit code {status}"),
                        false => format!(
                            "the container ended with exit code {status}: {}",
                            ending.log
                        ),
                    },
                });
            }
            // A tool that ended well while the engine stopped it still ran out of time; 143 is
            // the code of a stop by SIGTERM.
            (_, 0, Some(_), _) => 143,
            (_, status, _, _) => status,
        };
        let note = match (ending.timed_out, out_of_memory) {
            (Some(time), _) => Some(format!(
                "uwe: the task ran longer than its time of {} seconds\n",
                time.as_secs()
            )),
            (None, true) => Some("uwe: the container ran out of memory\n".to_owned()),
            (None, false) if exit_code != 0 && recorded.is_none() => Some(format!(
                "uwe: the container ended with exit code {exit_code}, maybe for lack of \
                 memory\n"
            )),
            (None, false) => None,
        };
        if let Some(note) = note {
            append(&control.join(STDERR), &note).await?;
        }
        let files = match exit_code {
            0 => parse(&read(&control.join(MANIFEST)).unwrap_or_default())?,
            _ => Vec::new(),
        };
        Ok(Finished {
            address: None,
            exit_code: Some(exit_code),
            files,
            usage,
            root,
        })
    }
}

/// The output of a container, where only the wrapper writes: its last lines, which tell why a
/// container could not run the wrapper, and whether the wrapper said that it ended.
#[derive(Default)]
struct Output {
    /// The start of a line whose end has not come yet.
    partial: Vec<u8>,
    lines: VecDeque<String>,
    ended: bool,
}

impl Output {
    /// How many of the last lines are kept.
    const KEPT: usize = 5;

    fn push(&mut self, bytes: &[u8]) {
        self.partial.extend_from_slice(bytes);
        while let Some(end) = self.partial.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            self.line(&line);
        }
    }

    fn line(&mut self, line: &[u8]) {
        let line = String::from_utf8_lossy(line);
        let line = line.trim();
        if line == ENDED {
            self.ended = true;
        } else if !line.is_empty() {
            if self.lines.len() == Self::KEPT {
                self.lines.pop_front();
            }
            self.lines.push_back(line.to_owned());
        }
    }

    /// The last lines, joined.
    fn text(mut self) -> String {
        let rest = std::mem::take(&mut self.partial);
        self.line(&rest);
        Vec::from(self.lines).join("; ")
    }
}

fn group_resources(
    cpus: u32,
    memory: Option<u64>,
    groups: usize,
) -> Result<Vec<(i64, Option<u64>)>, ExecutorError> {
    if groups == 0 {
        return Err(ExecutorError::NoImage);
    }
    let count = groups as i64;
    let nano_cpus = i64::from(cpus) * 1_000_000_000;
    if nano_cpus / count < 10_000_000 {
        return Err(ExecutorError::Unsupported(format!(
            "the granted {cpus} CPUs cannot cover {groups} image groups at the engine minimum of 0.01 CPU each"
        )));
    }
    if let Some(bytes) = memory
        && bytes / (groups as u64) < LEAST_MEMORY
    {
        return Err(ExecutorError::Unsupported(format!(
            "the granted {bytes} bytes cannot cover {groups} image groups at the engine minimum of {LEAST_MEMORY} bytes each"
        )));
    }
    Ok((0..groups)
        .map(|index| {
            (
                nano_cpus / count + i64::from((index as i64) < nano_cpus % count),
                memory.map(|bytes| {
                    bytes / (groups as u64) + u64::from((index as u64) < bytes % (groups as u64))
                }),
            )
        })
        .collect())
}

/// Opens the scratch directory `root` to every user when the container may run as another
/// user (see [`access::open`]).
async fn open(access: &Access, root: &Path) -> std::io::Result<()> {
    if access.hand_back.is_none() {
        return Ok(());
    }
    let root = root.to_owned();
    (tokio::task::spawn_blocking(move || access::open(&root)).await)
        .map_err(|error| std::io::Error::other(error.to_string()))?
}

/// The exit code of a container when it next ends; -1 when the engine does not tell it.
async fn exit_code(client: Docker, container: String) -> i64 {
    let options = WaitContainerOptions {
        condition: "next-exit".to_owned(),
    };
    let mut waiting = client.wait_container(&container, Some(options));
    match waiting.next().await {
        Some(Ok(response)) => response.status_code,
        Some(Err(ApiError::DockerContainerWaitError { code, .. })) => code,
        _ => -1,
    }
}

async fn append(path: &Path, text: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(text.as_bytes()).await?;
    // Tokio writes in the background; the caller may read the file at once.
    file.flush().await
}

/// A bind in the form of `docker run --volume`: `<source>:<target>`, with `:ro` when it is
/// read-only.
pub fn volume(mount: &Mount) -> String {
    let mode = if mount.read_only { ":ro" } else { "" };
    format!("{}:{}{mode}", text(&mount.source), mount.target)
}

/// Refuses a bind that [`volume`] cannot write: a `:` splits the parts of the spec, and for
/// the `--bind` of Apptainer a `,` starts the next bind. Neither has an escape.
pub fn check_volume(mount: &Mount, apptainer: bool) -> Result<(), ExecutorError> {
    let marks: &[char] = if apptainer { &[':', ','] } else { &[':'] };
    let source = text(&mount.source);
    match [source.as_str(), mount.target.as_str()]
        .into_iter()
        .find(|path| path.contains(marks))
    {
        Some(path) => Err(ExecutorError::Unsupported(format!(
            "the container cannot see {path:?}, as a bind of the {} command cannot hold {}",
            if apptainer { "Apptainer" } else { "engine" },
            if apptainer { "`:` or `,`" } else { "`:`" },
        ))),
        None => Ok(()),
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn respects_engine_budget() {
        let server = crate::test_engine::Server::start(0, |_| panic!("unexpected request")).await;
        let backend = LocalContainer::new(
            server.engine.clone(),
            "/scratch".into(),
            Pool::new(16, 16 << 30),
        );
        assert_eq!((backend.pool.cpus(), backend.pool.memory()), (2, 1 << 30));
        let smaller = LocalContainer::new(
            server.engine.clone(),
            "/scratch".into(),
            Pool::new(1, 64 << 20),
        );
        assert_eq!((smaller.pool.cpus(), smaller.pool.memory()), (1, 64 << 20));
        server.finish();
    }

    /// The entrypoint of an image that left the engine is read after the image is pulled
    /// again, as a task that runs the image's own command needs it first.
    #[tokio::test]
    async fn reads_removed_commands() {
        use crate::test_engine::Reply;
        let mut inspected = 0;
        let server = crate::test_engine::Server::start(3, move |request| {
            match (request.method.as_str(), request.path.contains("/images/")) {
                ("GET", true) => {
                    inspected += 1;
                    match inspected {
                        1 => Reply::Json(404, serde_json::json!({"message": "No such image"})),
                        _ => Reply::Json(
                            200,
                            serde_json::json!({"Id": "sha256:aa",
                                "Config": {"Entrypoint": ["/bin/tool"]}}),
                        ),
                    }
                }
                _ => Reply::Json(200, serde_json::json!({"status": "pulled"})),
            }
        })
        .await;
        let reference = format!("registry.test/tool@sha256:{}", "b".repeat(64));
        let commands = Commands::of(&server.engine, &reference, None).await;
        let arguments = commands.unwrap().arguments(&reference, vec!["run".into()]);
        assert_eq!(arguments.unwrap(), ["/bin/tool", "run"]);
        server.finish();
    }

    /// An image that left the engine after the run pinned it, as by a prune, is pulled again
    /// before its container is made, rather than failing every attempt alike.
    #[tokio::test]
    async fn pulls_removed_images() {
        use crate::test_engine::Reply;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let mut inspected = 0;
        let image = format!("registry.test/tool@sha256:{}", "b".repeat(64));
        let server = crate::test_engine::Server::start(4, move |request| {
            let path = request
                .path
                .split('?')
                .next()
                .unwrap_or_default()
                .to_owned();
            log.lock()
                .unwrap()
                .push(format!("{} {path}", request.method));
            match (request.method.as_str(), path.contains("/images/")) {
                ("GET", true) => {
                    inspected += 1;
                    match inspected {
                        1 => Reply::Json(404, serde_json::json!({"message": "No such image"})),
                        _ => Reply::Json(200, serde_json::json!({"Id": "sha256:aa"})),
                    }
                }
                ("POST", true) => Reply::Json(200, serde_json::json!({"status": "pulled"})),
                _ => Reply::Json(400, serde_json::json!({"message": "no container"})),
            }
        })
        .await;
        let folder = tempfile::tempdir().unwrap();
        let backend = LocalContainer::new(
            server.engine.clone(),
            folder.path().to_owned(),
            Pool::new(1, 1 << 30),
        );
        let pinned = Image {
            reference: image,
            platform: None,
        };
        let access = Access::default();
        let run = ContainerRun {
            name: "task",
            image: &pinned,
            root: folder.path(),
            workdir: None,
            mounts: &[],
            owner: (1000, 1000),
            cpus: 1,
            memory: None,
            offline: true,
            wrapper_path: None,
            access: &access,
        };
        let (cancel, flags) = (CancellationToken::new(), Flags::default());
        let ending = backend.run_to_end(&run, 1_000_000_000, None, &cancel, &flags);
        assert!(ending.await.is_err(), "the engine makes no container");
        let seen = seen.lock().unwrap().clone();
        let pulled = seen
            .iter()
            .position(|request| request.contains("/images/create"));
        let made = seen
            .iter()
            .position(|request| request.contains("/containers/create"));
        assert!(pulled.is_some() && pulled < made, "{seen:?}");
        server.finish();
    }

    #[test]
    fn rejects_local_accelerators() {
        let spec = TaskSpec {
            format: task_model::FORMAT,
            namespace: String::new(),
            image: Some(Image {
                reference: "debian:stable-slim".into(),
                platform: None,
            }),
            arguments: vec!["true".into()],
            workdir: None,
            environment: std::collections::BTreeMap::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            output_manifest: None,
            stdin: None,
            containers: Vec::new(),
            replaces_entrypoint: false,
            run_as: None,
        };
        let staging = crate::job::Staging::default();
        let placement = task_model::Placement {
            accelerators: vec![task_model::Accelerator {
                count: 1,
                kind: Some("gpu".into()),
            }],
            ..task_model::Placement::default()
        };
        let error = admit(Job {
            name: "accelerated",
            spec: &spec,
            staging: &staging,
            placement: &placement,
        })
        .expect_err("accelerators are unsupported");
        assert!(matches!(error, ExecutorError::Unsupported(_)), "{error}");

        for accelerators in [
            Vec::new(),
            vec![task_model::Accelerator {
                count: 0,
                kind: Some("gpu".into()),
            }],
        ] {
            let placement = task_model::Placement {
                accelerators,
                ..task_model::Placement::default()
            };
            assert!(
                admit(Job {
                    name: "ordinary",
                    spec: &spec,
                    staging: &staging,
                    placement: &placement,
                })
                .is_ok()
            );
        }
    }

    #[test]
    fn partitions_group_resources() {
        let image = Image {
            reference: "debian".into(),
            platform: None,
        };
        let accesses = [
            Access::of(
                (1000, 100),
                true,
                Some(task_model::RunAs::default()),
                "root",
            ),
            Access::of(
                (1000, 100),
                true,
                Some(task_model::RunAs::default()),
                "nobody",
            ),
        ];
        let grant = 2 * LEAST_MEMORY + 1;
        let resources = group_resources(1, Some(grant), 2).expect("can divide");
        let configs: Vec<_> = resources
            .iter()
            .enumerate()
            .map(|(index, &(cpus, memory))| {
                ContainerRun {
                    name: "task",
                    image: &image,
                    root: Path::new("/scratch/task"),
                    workdir: None,
                    mounts: &[],
                    owner: (1000, 100),
                    cpus: 1,
                    memory,
                    offline: false,
                    wrapper_path: None,
                    access: &accesses[index],
                }
                .create_with_cpus(cpus)
            })
            .collect();
        assert_eq!(
            configs
                .iter()
                .map(|config| config.host_config.as_ref().unwrap().nano_cpus.unwrap())
                .sum::<i64>(),
            1_000_000_000
        );
        assert_eq!(
            configs
                .iter()
                .map(|config| config.host_config.as_ref().unwrap().memory.unwrap())
                .sum::<i64>(),
            grant as i64
        );
        assert_eq!(resources[0].0, 500_000_000);
        assert_eq!(accesses[0].hand_back, None);
        assert_eq!(accesses[1].hand_back, Some((0, 0)));
        let members = [(
            "/scratch/task/control/wrapper.sh".into(),
            "/scratch/task/control/exit".into(),
        )];
        assert!(
            !crate::containers::group_wrapper(&members, &accesses[0].owner(), "/scratch/task")
                .contains("chown")
        );
        assert!(
            crate::containers::group_wrapper(&members, &accesses[1].owner(), "/scratch/task")
                .contains("chown -hR '0:0'")
        );
    }

    #[test]
    fn preserves_resource_remainders() {
        let resources = group_resources(1, Some(3 * LEAST_MEMORY + 2), 3).unwrap();
        assert_eq!(
            resources.iter().map(|(cpus, _)| cpus).sum::<i64>(),
            1_000_000_000
        );
        assert_eq!(
            resources
                .iter()
                .map(|(_, memory)| memory.unwrap())
                .sum::<u64>(),
            3 * LEAST_MEMORY + 2
        );
        assert!(
            group_resources(1, None, 2)
                .unwrap()
                .iter()
                .all(|(_, memory)| memory.is_none())
        );
        let error = group_resources(1, Some(2 * LEAST_MEMORY - 1), 2).unwrap_err();
        assert!(error.to_string().contains("engine minimum"));
        assert!(group_resources(1, None, 101).is_err());
        assert!(group_resources(1, Some(LEAST_MEMORY - 1), 1).is_err());
    }

    #[tokio::test]
    async fn opens_shared_controls() {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("task");
        std::fs::create_dir_all(root.join("control/containers/secondary")).unwrap();
        std::fs::write(
            root.join("control/containers/secondary/wrapper.sh"),
            "true\n",
        )
        .unwrap();
        let access = Access::of(
            (1000, 100),
            true,
            Some(task_model::RunAs::default()),
            "nobody",
        );
        open(&access, &root).await.unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root.join("control/containers/secondary")), 0o777);
        assert_eq!(
            mode(&root.join("control/containers/secondary/wrapper.sh")) & 0o666,
            0o666
        );
    }

    #[test]
    fn binds_relative_mounts() {
        let image = Image {
            reference: "debian".into(),
            platform: None,
        };
        let mount = |source: &str, target: &str| Mount {
            source: PathBuf::from(source),
            target: target.to_owned(),
            read_only: false,
        };
        let mounts = [mount("/project", "."), mount("/data", "data")];
        let mut run = ContainerRun {
            name: "task",
            image: &image,
            root: Path::new("/scratch/task"),
            workdir: None,
            mounts: &mounts,
            owner: (1000, 100),
            cpus: 1,
            memory: None,
            offline: false,
            wrapper_path: None,
            access: &Access::default(),
        };
        let binds =
            |run: &ContainerRun<'_>| -> Vec<String> { run.binds().iter().map(volume).collect() };
        assert_eq!(
            binds(&run),
            [
                "/scratch/task:/scratch/task",
                "/project:/scratch/task/work",
                "/data:/scratch/task/work/data",
            ]
        );
        // A task that asks for less memory than Docker gives any container gets that least.
        run.memory = Some(1024);
        let arguments = run.arguments(&EngineCommand::docker("docker"));
        let memory = arguments.iter().position(|argument| argument == "--memory");
        assert_eq!(memory.map(|at| arguments[at + 1].as_str()), Some("6291456"));
        let policy = run.create().host_config.unwrap();
        for (flag, values) in [
            ("--cap-drop", policy.cap_drop),
            ("--cap-add", policy.cap_add),
            ("--security-opt", policy.security_opt),
        ] {
            for value in values.into_iter().flatten() {
                assert!(arguments.windows(2).any(|pair| pair == [flag, &value]));
            }
        }
        run.memory = None;
        // A mount at the work directory takes the place of the work directory of the scratch.
        run.workdir = Some("/work");
        assert_eq!(
            binds(&run),
            [
                "/scratch/task:/scratch/task",
                "/project:/work",
                "/data:/work/data"
            ]
        );
    }
}

#[cfg(test)]
mod flag_tests {
    use super::*;
    #[test]
    fn maps_native_flags() {
        let spec = TaskSpec {
            format: task_model::FORMAT,
            namespace: String::new(),
            image: None,
            arguments: Vec::new(),
            workdir: None,
            environment: std::collections::BTreeMap::from([(
                FLAGS_ENV.into(),
                task_model::EnvironmentValue::Literal(
                    r#"{"shm":1073741824,"swap":2147483648,"gpus":-1}"#.into(),
                ),
            )]),
            inputs: Vec::new(),
            outputs: Vec::new(),
            output_manifest: None,
            stdin: None,
            containers: Vec::new(),
            replaces_entrypoint: false,
            run_as: None,
        };
        let flags = Flags::of(&spec).unwrap();
        let mut config = HostConfig::default();
        flags.apply(&mut config);
        assert_eq!(config.shm_size, Some(1 << 30));
        assert_eq!(config.memory_swap, Some(2 << 30));
        let request = &config.device_requests.unwrap()[0];
        assert_eq!(request.count, Some(-1));
        assert_eq!(request.capabilities, Some(vec![vec!["gpu".to_owned()]]));
    }
}
