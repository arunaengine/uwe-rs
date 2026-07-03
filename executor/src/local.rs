//! Runs a task in a container with the local Docker or Podman, in a scratch directory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use task_model::Image;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::engine::ContainerEngine;
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

/// How a container ended.
enum Ending {
    Exited(Output),
    /// The task ran longer than its time, so it was stopped.
    TimedOut(Output, Duration),
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

    /// The arguments of the engine's command that run the wrapper with Bash in the image.
    pub fn arguments(&self, engine: &ContainerEngine) -> Vec<String> {
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
        if spec.arguments.is_empty() {
            spec.arguments = image::default_command(&self.engine, &image.reference).await?;
        }
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
        };
        let control = root.join(CONTROL);
        tokio::fs::write(run.script(), wrapper(&spec, &run.inside(), &text(&control))).await?;
        let mut command = Command::new(&self.engine.program);
        command.args(run.arguments(&self.engine));
        command.stdin(Stdio::null());
        let time = job.placement.time_seconds.map(Duration::from_secs);
        let ending = self.wait(command, &container, time, cancel).await?;
        self.finish(ending, root.to_owned()).await
    }
    /// Runs the container to its end, stopping it when `cancel` fires or its time is up.
    async fn wait(
        &self,
        mut command: Command,
        container: &str,
        time: Option<Duration>,
        cancel: &CancellationToken,
    ) -> Result<Ending, ExecutorError> {
        let failed = |error| image::failed(&self.engine, error);
        let output = command.output();
        tokio::pin!(output);
        let limit = async {
            match time {
                Some(time) => tokio::time::sleep(time).await,
                None => std::future::pending().await,
            }
        };
        let ended = tokio::select! {
            output = &mut output => Ending::Exited(output.map_err(failed)?),
            () = cancel.cancelled() => {
                self.stop(container, Duration::ZERO).await;
                let _ = output.await;
                return Err(ExecutorError::Cancelled);
            }
            () = limit => {
                self.stop(container, STOP_GRACE).await;
                let output = output.await.map_err(failed)?;
                Ending::TimedOut(output, time.unwrap_or_default())
            }
        };
        Ok(ended)
    }

    /// Stops a container: asks it to end, and kills it after the grace time.
    async fn stop(&self, container: &str, grace: Duration) {
        let seconds = grace.as_secs().to_string();
        let _ = Command::new(&self.engine.program)
            .args(["stop", "--time", &seconds, container])
            .output()
            .await;
    }

    /// What a container left behind. The wrapper exits with 0 whenever it recorded the tool's
    /// exit code. An engine error (125 to 127), or an end without the tool's exit code, is a
    /// failure of the machine. A container stopped by a signal, as for lack of memory, ends
    /// the task with the container's exit code, and so does one that ran out of time.
    async fn finish(&self, ending: Ending, root: PathBuf) -> Result<Finished, ExecutorError> {
        let control = root.join(CONTROL);
        let (output, timed_out) = match ending {
            Ending::Exited(output) => (output, None),
            Ending::TimedOut(output, time) => (output, Some(time)),
        };
        let recorded = read(&control.join(EXIT)).and_then(|text| text.trim().parse().ok());
        let exit_code = match (recorded, output.status.code(), timed_out) {
            (Some(code), Some(0), None) => code,
            (_, Some(125..=127) | Some(0) | None, None) => {
                return Err(image::engine_error(&self.engine, &output));
            }
            (_, status, _) => status.unwrap_or(-1),
        };
        let note = match timed_out {
            Some(time) => Some(format!(
                "iunctus: the task ran longer than its time of {} seconds\n",
                time.as_secs()
            )),
            None if exit_code != 0 && recorded.is_none() => Some(format!(
                "iunctus: the container ended with exit code {exit_code}, maybe for lack of \
                 memory\n"
            )),
            None => None,
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
