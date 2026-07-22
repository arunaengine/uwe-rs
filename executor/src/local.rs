//! Runs a task in a container with the local Docker or Podman, in a scratch directory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptions, KillContainerOptions, LogsOptions, RemoveContainerOptions,
    StopContainerOptions,
};
use futures_util::StreamExt;
use task_model::Image;
use tokio_util::sync::CancellationToken;

use crate::engine::{ContainerEngine, EngineCommand};
use crate::image;
use crate::job::{CONTROL, ExecutorError, Finished, Job, WORK};
use crate::manifest::parse;
use crate::pool::{Pool, Share};
use crate::process::Mount;
use crate::stage::place;
use crate::wrapper::{EXIT, MANIFEST, STDERR, wrapper};

/// How long a task may take to end after it was asked to stop, before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// The local container executor.
#[derive(Debug)]
pub struct LocalContainer {
    pub engine: ContainerEngine,
    /// Where each task gets its scratch directory.
    pub scratch: PathBuf,
    /// The CPUs and memory that tasks share.
    pub pool: Pool,
}

/// How long the backend waits between two looks at a running container, at most.
const POLL: Duration = Duration::from_secs(1);

/// How a container ended.
struct Ending {
    /// The exit code of the container.
    status: i64,
    /// Whether the kernel killed it for lack of memory.
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
        let share = tokio::select! {
            share = self.pool.share(placement.cpus, placement.memory_bytes) => share,
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
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
}

impl ContainerRun<'_> {
    /// Where the container sees the work directory.
    pub fn inside(&self) -> String {
        (self.workdir.map(str::to_owned)).unwrap_or_else(|| text(&self.root.join(WORK)))
    }

    /// The wrapper in the control folder, which the container runs.
    pub fn script(&self) -> String {
        text(&self.root.join(CONTROL).join("wrapper.sh"))
    }

    /// The container that runs the wrapper with Bash in the image, for the engine's API.
    pub fn create(&self, engine: &ContainerEngine) -> ContainerCreateBody {
        let root = text(self.root);
        let mut binds = vec![format!("{root}:{root}")];
        if let Some(workdir) = self.workdir {
            binds.push(format!("{}:{workdir}", text(&self.root.join(WORK))));
        }
        for mount in self.mounts {
            let mode = if mount.read_only { ":ro" } else { "" };
            binds.push(format!("{}:{}{mode}", text(&mount.source), mount.target));
        }
        ContainerCreateBody {
            image: Some(self.image.reference.clone()),
            // Bash of the image, wherever its `PATH` has it.
            entrypoint: Some(vec!["bash".to_owned()]),
            cmd: Some(vec![self.script()]),
            user: engine.user(self.owner.0, self.owner.1),
            working_dir: Some(self.inside()),
            host_config: Some(HostConfig {
                binds: Some(binds),
                nano_cpus: Some(i64::from(self.cpus) * 1_000_000_000),
                memory: self.memory.map(|bytes| bytes as i64),
                network_mode: self.offline.then(|| "none".to_owned()),
                ..HostConfig::default()
            }),
            ..ContainerCreateBody::default()
        }
    }

    /// The arguments of the engine's command that run the wrapper with Bash in the image.
    pub fn arguments(&self, engine: &EngineCommand) -> Vec<String> {
        let root = text(self.root);
        let mut arguments: Vec<String> = ["run", "--rm", "--name", self.name]
            .into_iter()
            .chain(["--entrypoint", "/bin/bash"])
            .map(str::to_owned)
            .collect();
        arguments.extend(engine.user_options(self.owner.0, self.owner.1));
        arguments.extend(["--volume".into(), format!("{root}:{root}")]);
        if let Some(workdir) = self.workdir {
            let work = text(&self.root.join(WORK));
            arguments.extend(["--volume".into(), format!("{work}:{workdir}")]);
        }
        for mount in self.mounts {
            let mode = if mount.read_only { ":ro" } else { "" };
            let volume = format!("{}:{}{mode}", text(&mount.source), mount.target);
            arguments.extend(["--volume".into(), volume]);
        }
        arguments.extend(["--workdir".into(), self.inside()]);
        arguments.extend(["--cpus".into(), self.cpus.to_string()]);
        if let Some(memory) = self.memory {
            arguments.extend(["--memory".into(), memory.to_string()]);
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
    /// Runs the container of a staged task to its end, within its share of the pool.
    async fn contain(
        &self,
        job: Job<'_>,
        root: &Path,
        mounts: &[Mount],
        share: &Share<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let image = job.spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let mut spec = job.spec.clone();
        // The command runs through the image's entrypoint, as `docker run` runs it, which
        // some images need to set up their tools.
        let entrypoint = image::entrypoint(&self.engine, &image.reference).await?;
        spec.arguments = match spec.arguments.is_empty() {
            true => image::default_command(&self.engine, &image.reference).await?,
            false => entrypoint.into_iter().chain(spec.arguments).collect(),
        };
        let owner = tokio::fs::metadata(root).await?;
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
        };
        let control = root.join(CONTROL);
        tokio::fs::write(run.script(), wrapper(&spec, &run.inside(), &text(&control))).await?;
        let options = CreateContainerOptions {
            name: Some(container.clone()),
            platform: image.platform.clone().unwrap_or_default(),
        };
        let failed = |error| image::failed(&self.engine, error);
        let client = self.engine.client();
        let body = run.create(&self.engine);
        client
            .create_container(Some(options), body)
            .await
            .map_err(failed)?;
        let time = job.placement.time_seconds.map(Duration::from_secs);
        let ended = match client.start_container(&container, None).await {
            Ok(()) => self.wait(&container, time, cancel).await,
            Err(error)
                if error
                    .to_string()
                    .contains("\"bash\": executable file not found") =>
            {
                Err(ExecutorError::NoBash(image.reference.clone()))
            }
            Err(error) => Err(failed(error)),
        };
        let remove = RemoveContainerOptions {
            force: true,
            ..RemoveContainerOptions::default()
        };
        let _ = client.remove_container(&container, Some(remove)).await;
        self.finish(ended?, root.to_owned()).await
    }
    /// Waits for the container to end, and stops it when `cancel` fires or its time is up.
    async fn wait(
        &self,
        container: &str,
        time: Option<Duration>,
        cancel: &CancellationToken,
    ) -> Result<Ending, ExecutorError> {
        let limit = async {
            match time {
                Some(time) => tokio::time::sleep(time).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(limit);
        let mut timed_out = None;
        let mut pause = Duration::from_millis(50);
        loop {
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                () = cancel.cancelled() => {
                    let kill = KillContainerOptions::default();
                    let _ = self.engine.client().kill_container(container, Some(kill)).await;
                    return Err(ExecutorError::Cancelled);
                }
                () = &mut limit, if timed_out.is_none() => {
                    let stop = StopContainerOptions {
                        t: Some(STOP_GRACE.as_secs() as i32),
                        ..StopContainerOptions::default()
                    };
                    let _ = self.engine.client().stop_container(container, Some(stop)).await;
                    timed_out = time;
                }
            }
            pause = (pause * 2).min(POLL);
            let inspected = self
                .engine
                .client()
                .inspect_container(container, None)
                .await;
            let state = inspected
                .map_err(|error| image::failed(&self.engine, error))?
                .state;
            let state = state.unwrap_or_default();
            if state.running.unwrap_or(false) {
                continue;
            }
            return Ok(Ending {
                status: state.exit_code.unwrap_or(-1),
                out_of_memory: state.oom_killed.unwrap_or(false),
                timed_out,
                log: self.log(container).await,
            });
        }
    }

    /// The last lines that a container wrote, which tell why it could not run the wrapper.
    async fn log(&self, container: &str) -> String {
        let options = LogsOptions {
            stdout: true,
            stderr: true,
            tail: "5".to_owned(),
            ..LogsOptions::default()
        };
        let mut lines = Vec::new();
        let mut output = self.engine.client().logs(container, Some(options));
        while let Some(Ok(part)) = output.next().await {
            let part = part.to_string();
            lines.extend(
                part.lines()
                    .map(str::to_owned)
                    .filter(|line| !line.trim().is_empty()),
            );
        }
        lines.join("; ")
    }

    /// What a container left behind. The wrapper exits with 0 whenever it recorded the tool's
    /// exit code. A container that could not run the wrapper (exit code 126 or 127, as for an
    /// image without Bash), or an end without the tool's exit code, is a failure of the
    /// machine. A container stopped by a signal, as for lack of memory, ends the task with the
    /// container's exit code, and so does one that ran out of time.
    async fn finish(&self, ending: Ending, root: PathBuf) -> Result<Finished, ExecutorError> {
        let control = root.join(CONTROL);
        let recorded = read(&control.join(EXIT)).and_then(|text| text.trim().parse().ok());
        let status = i32::try_from(ending.status).unwrap_or(-1);
        let exit_code = match (recorded, status, ending.timed_out, ending.out_of_memory) {
            (Some(code), 0, None, false) => code,
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
            (_, status, _, _) => status,
        };
        let note = match (ending.timed_out, ending.out_of_memory) {
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
            root,
        })
    }
}

async fn append(path: &Path, text: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(text.as_bytes()).await
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
