//! Runs a task in a container with the local Docker or Podman, in a scratch directory.
// Copyright (c) 2026 The iunctus Contributors
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
    AttachContainerOptions, CreateContainerOptions, KillContainerOptions, RemoveContainerOptions,
    StopContainerOptions, WaitContainerOptions,
};
use futures_util::StreamExt;
use task_model::{Image, TaskSpec};
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::access::{self, Access, NO_NEW_PRIVILEGES};
use crate::engine::{ContainerEngine, EngineCommand};
use crate::image::{self, Commands};
use crate::job::{CONTROL, ExecutorError, Finished, HOME, Job, Usage, WORK};
use crate::manifest::parse;
use crate::pool::{Pool, Share};
use crate::process::Mount;
use crate::stage::place;
use crate::wrapper::{ENDED, Folders, MANIFEST, STDERR, wrapper};

/// How long a task may take to end after it was asked to stop, before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// The least memory that Docker and Podman give a container (6 MiB); a task that asks for
/// less, as a Snakemake rule with `mem_mb` of a file's size may, gets that much.
const LEAST_MEMORY: u64 = 6 << 20;

/// The memory of the container that hands the files of a task back to the user (64 MiB).
const HAND_BACK_MEMORY: i64 = 64 << 20;

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
    commands: Mutex<HashMap<(String, Option<String>), Arc<OnceCell<Commands>>>>,
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

    /// Runs a task in `<scratch>/<name>`, a name that must not be in use: stages its inputs,
    /// runs the wrapper in the task's image as the current user, and reads the control files.
    /// The directory stays for the caller to take the outputs from, unless the attempt fails
    /// or is cancelled.
    ///
    /// The task waits for its share of CPUs and memory, and its container gets no more. The
    /// work directory is mounted at the task's `workdir` when it has one. A task without
    /// arguments runs the image's default command. The container stops when `cancel` fires,
    /// and when the task runs longer than its time.
    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let placement = job.placement;
        // A task that waits for its share when the run is held never starts.
        let share = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
            () = self.pool.held() => return Err(ExecutorError::Cancelled),
            share = self.pool.share(placement.cpus, placement.memory_bytes, placement.priority) => share,
        };
        let root = self.scratch.join(job.name);
        let placed = {
            let (root, staging) = (root.clone(), job.staging.clone());
            tokio::task::spawn_blocking(move || place(&root, &staging))
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?
        };
        let result = match placed {
            // Another attempt has this name; its directory is not ours to remove.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(error.into());
            }
            Err(error) => Err(error.into()),
            Ok(mounts) => self.contain(job, &root, &mounts, &share, cancel).await,
        };
        drop(share);
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        result
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

    /// The folders that the container sees, each with its place inside it: the scratch
    /// directory at the same path, the work directory at the task's `workdir` when it names
    /// one, and the mounts of the inputs. A mount with a relative target is inside the work
    /// directory, and one at the work directory itself, like the working directory of a
    /// Snakemake `shell()` call that the task changes in place, takes its place.
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
                // The least permissions (decision 28): no capabilities but those the task
                // needs, and no way to gain privileges.
                cap_drop: Some(vec!["ALL".to_owned()]),
                cap_add: Some(
                    self.access
                        .capabilities
                        .iter()
                        .map(|&name| name.to_owned())
                        .collect(),
                ),
                security_opt: Some(vec![NO_NEW_PRIVILEGES.to_owned()]),
                // The engine removes the container once it ended, which takes it a while, so
                // that the task need not wait for it.
                auto_remove: Some(true),
                mounts: Some(mounts),
                nano_cpus: Some(i64::from(self.cpus) * 1_000_000_000),
                memory: self.memory.map(|bytes| bytes.max(LEAST_MEMORY) as i64),
                network_mode: self.offline.then(|| "none".to_owned()),
                ..HostConfig::default()
            }),
            ..ContainerCreateBody::default()
        }
    }

    /// The arguments of the engine's command that run the wrapper with Bash in the image.
    pub fn arguments(&self, engine: &EngineCommand) -> Vec<String> {
        let mut arguments: Vec<String> = ["run", "--rm", "--name", self.name]
            .into_iter()
            .chain(["--entrypoint", "sh"])
            .map(str::to_owned)
            .collect();
        match (self.access.run_as, &self.access.user) {
            (None, _) => arguments.extend(engine.user_options(self.owner.0, self.owner.1)),
            (Some(_), Some(user)) => arguments.extend(["--user".into(), user.clone()]),
            (Some(_), None) => {}
        }
        arguments.extend(["--cap-drop".into(), "ALL".into()]);
        for capability in &self.access.capabilities {
            arguments.extend(["--cap-add".into(), (*capability).to_owned()]);
        }
        arguments.extend(["--security-opt".into(), NO_NEW_PRIVILEGES.into()]);
        for bind in self.binds() {
            arguments.extend(["--volume".into(), volume(&bind)]);
        }
        if let Some(start) = self.start() {
            arguments.extend(["--workdir".into(), start]);
        }
        arguments.extend(["--cpus".into(), self.cpus.to_string()]);
        if let Some(memory) = self.memory {
            arguments.extend(["--memory".into(), memory.max(LEAST_MEMORY).to_string()]);
        }
        if self.offline {
            arguments.extend(["--network".into(), "none".into()]);
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
        LocalContainer {
            engine,
            scratch,
            pool,
            commands: Mutex::default(),
        }
    }

    /// Runs the container of a staged task to its end, within its share of the pool.
    async fn contain(
        &self,
        job: Job<'_>,
        root: &Path,
        mounts: &[Mount],
        share: &Share<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        if !job.spec.containers.is_empty() {
            return self.contain_several(job, root, mounts, share, cancel).await;
        }
        let image = job.spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let mut spec = job.spec.clone();
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
        let container = format!("iunctus-{}", job.name);
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
        let time = job.placement.time_seconds.map(Duration::from_secs);
        let ending = self.run_to_end(&run, time, cancel).await;
        self.hand_back(&access, image, root, owner.uid()).await;
        self.finish(ending?, root.to_owned(), &spec).await
    }

    /// Runs a task of several containers: one container of the engine for each image, all at
    /// once, on the same scratch directory and mounts, within the share of the task. When one
    /// of them ends without its wrapper, the others stop, since containers that wait for it
    /// would wait for ever.
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
        // The containers run as the main one does.
        let main = (spec.containers.first().and_then(|main| main.image.clone()))
            .ok_or(ExecutorError::NoImage)?;
        let commands = match spec.run_as {
            Some(_) => self.commands(&main).await?,
            None => Commands::default(),
        };
        let access = self
            .access(root, (owner.uid(), owner.gid()), &spec, &commands)
            .await?;
        let (control, home) = (root.join(CONTROL), text(&root.join(HOME)));
        let work = match spec.run_as {
            Some(_) => text(&root.join(WORK)),
            None => (spec.workdir.clone()).unwrap_or_else(|| text(&root.join(WORK))),
        };
        let owner_text = access.owner();
        let folders = Folders {
            work: &work,
            control: &text(&control),
            home: &home,
            outbox: "",
            owner: &owner_text,
        };
        let groups = crate::containers::write_wrappers(&spec, &control, &folders)?;
        // The folders of the wrappers are new, so they are opened after them.
        open(&access, root).await?;
        let names: Vec<String> = (0..groups.len())
            .map(|index| format!("iunctus-{}-{index}", job.name))
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
                memory: share.memory,
                offline: job.placement.offline,
                wrapper_path: Some(&paths[index]),
                access: &access,
            })
            .collect();
        let time = job.placement.time_seconds.map(Duration::from_secs);
        let stop = cancel.child_token();
        let mut running: futures_util::stream::FuturesUnordered<_> = (runs.iter())
            .map(|run| self.run_to_end(run, time, &stop))
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
        self.hand_back(&access, &main, root, owner.uid()).await;
        let ending = match (first, normal) {
            (Some(result), _) | (None, Some(result)) => result?,
            (None, None) => return Err(ExecutorError::NoImage),
        };
        self.finish(ending, root.to_owned(), &spec).await
    }

    /// How the containers of a task run, and its scratch directory `root` open to every user
    /// when they may run as another user than `owner`, the user who runs iunctus.
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

    /// Hands back to the user `user` what a container that ran as another user left in the
    /// scratch directory `root`, when its wrapper could not, with a short container of the
    /// task's image as root with only the capabilities that this needs. A failure leaves the
    /// files as they are; the wrapper let every user change them.
    async fn hand_back(&self, access: &Access, image: &Image, root: &Path, user: u32) {
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
        let name = format!("iunctus-{name}");
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
        if client.create_container(Some(options), body).await.is_err() {
            return;
        }
        let exited = tokio::spawn(exit_code(client.clone(), name.clone()));
        match client.start_container(&name, None).await {
            Ok(()) => {
                let _ = exited.await;
            }
            Err(_) => {
                exited.abort();
                let remove = RemoveContainerOptions {
                    force: true,
                    ..RemoveContainerOptions::default()
                };
                let _ = client.remove_container(&name, Some(remove)).await;
            }
        }
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
    /// reads the output until the wrapper says that it ended. The wrapper writes only that
    /// last line there, so the output tells when the task is done, without asking the engine
    /// again and again, and without waiting for the engine to take the container down. Only
    /// when the wrapper did not end does the container's exit code count; the request for it
    /// goes out before the start, as the engine removes the container once it ended.
    /// The container stops when `cancel` fires, and when it runs longer than `time`.
    async fn run_to_end(
        &self,
        run: &ContainerRun<'_>,
        time: Option<Duration>,
        cancel: &CancellationToken,
    ) -> Result<Ending, ExecutorError> {
        let client = self.engine.client();
        let failed = |error| image::failed(&self.engine, error);
        let options = CreateContainerOptions {
            name: Some(run.name.to_owned()),
            platform: run.image.platform.clone().unwrap_or_default(),
        };
        let image = image::available_for(
            &self.engine,
            &run.image.reference,
            run.image.platform.as_deref(),
        )
        .await?;
        let mut body = run.create();
        if run.image.platform.is_some() {
            body.image = Some(image.id.ok_or_else(|| {
                ExecutorError::Unsupported(format!(
                    "the engine's image {} has no ID for its requested platform",
                    run.image.reference
                ))
            })?);
        }
        client
            .create_container(Some(options), body)
            .await
            .map_err(failed)?;
        let attach = AttachContainerOptions {
            stream: true,
            stdout: true,
            stderr: true,
            ..AttachContainerOptions::default()
        };
        let started = async {
            let attached = client.attach_container(run.name, Some(attach)).await?;
            let exited = tokio::spawn(exit_code(client.clone(), run.name.to_owned()));
            match client.start_container(run.name, None).await {
                Ok(()) => Ok((attached.output, exited)),
                Err(error) => {
                    exited.abort();
                    Err(error)
                }
            }
        };
        match started.await {
            Ok((stream, exited)) => self.follow(run.name, stream, exited, time, cancel).await,
            Err(error) => {
                // The engine removes only a container that ran.
                let remove = RemoveContainerOptions {
                    force: true,
                    ..RemoveContainerOptions::default()
                };
                let _ = client.remove_container(run.name, Some(remove)).await;
                Err(
                    match error
                        .to_string()
                        .contains("\"sh\": executable file not found")
                    {
                        true => ExecutorError::NoShell(run.image.reference.clone()),
                        false => failed(error),
                    },
                )
            }
        }
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
                    let kill = KillContainerOptions::default();
                    let _ = client.kill_container(container, Some(kill)).await;
                    return Err(ExecutorError::Cancelled);
                }
                () = &mut limit, if timed_out.is_none() => {
                    let stop = StopContainerOptions {
                        t: Some(STOP_GRACE.as_secs() as i32),
                        ..StopContainerOptions::default()
                    };
                    let _ = client.stop_container(container, Some(stop)).await;
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
            None => exited.await.unwrap_or(-1),
        };
        // The engine may have removed the container already; then its lack of memory is not
        // known, and the note on the task says that it may be the cause.
        let inspected = client.inspect_container(container, None).await.ok();
        let state = inspected.and_then(|inspected| inspected.state);
        Ok(Ending {
            status,
            out_of_memory: state.and_then(|state| state.oom_killed).unwrap_or(false),
            timed_out,
            log: output.text(),
        })
    }

    /// What a container left behind. The wrapper exits with 0 whenever it recorded the tool's
    /// exit code. A container that could not run the wrapper (exit code 126 or 127, as for an
    /// image without Bash), or an end without the tool's exit code, is a failure of the
    /// machine. A container stopped by a signal, as for lack of memory, ends the task with the
    /// container's exit code, and so does one that ran out of time; that one fails even when its
    /// tool ended well while the engine stopped it.
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
                "iunctus: the task ran longer than its time of {} seconds\n",
                time.as_secs()
            )),
            (None, true) => Some("iunctus: the container ran out of memory\n".to_owned()),
            (None, false) if exit_code != 0 && recorded.is_none() => Some(format!(
                "iunctus: the container ended with exit code {exit_code}, maybe for lack of \
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

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_put_relative_mounts_in_the_work_directory() {
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
