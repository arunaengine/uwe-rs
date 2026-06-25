//! Runs a task in a container with the local Docker or Podman, in a scratch directory.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use task_model::{Placement, TaskSpec};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::engine::ContainerEngine;
use crate::image;
use crate::job::{CONTROL, ExecutorError, Finished, Job, Staging, WORK};
use crate::manifest::parse;
use crate::pool::Pool;
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
    /// The directory stays for the caller to take the outputs from.
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
        let spec = job.spec;
        let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        let placement = job.placement;
        let share = tokio::select! {
            share = self.pool.share(placement.cpus, placement.memory_bytes) => share,
            () = cancel.cancelled() => return Err(ExecutorError::Cancelled),
        };
        let root = self.scratch.join(job.name);
        let mounts = {
            let (root, staging) = (root.clone(), job.staging.clone());
            tokio::task::spawn_blocking(move || place(&root, &staging))
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))??
        };
        let mut spec = spec.clone();
        if spec.arguments.is_empty() {
            spec.arguments = image::default_command(&self.engine, &image.reference).await?;
        }
        let work = root.join(WORK);
        let control = root.join(CONTROL);
        let inside = spec.workdir.clone().unwrap_or_else(|| text(&work));
        let script = control.join("wrapper.sh");
        tokio::fs::write(&script, wrapper(&spec, &inside, &text(&control))).await?;
        let owner = tokio::fs::metadata(&root).await?;
        let container = format!("iunctus-{}", job.name);
        let mut command = Command::new(&self.engine.program);
        command
            .args([
                "run",
                "--rm",
                "--name",
                &container,
                "--entrypoint",
                "/bin/bash",
            ])
            .args(self.engine.user_options(owner.uid(), owner.gid()))
            .args(["--volume", &format!("{0}:{0}", text(&root))]);
        if spec.workdir.is_some() {
            command.args(["--volume", &format!("{}:{inside}", text(&work))]);
        }
        for mount in &mounts {
            let mode = if mount.read_only { ":ro" } else { "" };
            let volume = format!("{}:{}{mode}", text(&mount.source), mount.target);
            command.args(["--volume", &volume]);
        }
        command.args(["--workdir", &inside]);
        command.args(["--cpus", &share.cpus.to_string()]);
        if let Some(memory) = share.memory {
            command.args(["--memory", &memory.to_string()]);
        }
        if let Some(platform) = &image.platform {
            command.args(["--platform", platform]);
        }
        command.arg(&image.reference).arg(text(&script));
        command.stdin(Stdio::null());
        let time = placement.time_seconds.map(Duration::from_secs);
        let ending = self.wait(command, &container, time, cancel).await?;
        drop(share);
        self.finish(ending, root).await
    }
}

impl LocalContainer {
    /// Runs a task like [`Backend::run`](crate::Backend::run), with the default placement,
    /// until the task runner runs on Tokio.
    pub fn run(
        &self,
        name: &str,
        spec: &TaskSpec,
        staging: &Staging,
    ) -> Result<Finished, ExecutorError> {
        let job = Job {
            name,
            spec,
            staging,
            placement: &Placement::default(),
        };
        block_on(crate::Backend::run(self, job, &CancellationToken::new()))
    }

    /// The image pinned to its digest, like [`Backend::pin`](crate::Backend::pin), until the
    /// task runner runs on Tokio.
    pub fn pin(&self, reference: &str) -> Result<Option<String>, ExecutorError> {
        block_on(image::pin(&self.engine, reference))
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

/// Runs a future to its end on a runtime of its own, until the task runner runs on Tokio.
fn block_on<T>(
    future: impl std::future::Future<Output = Result<T, ExecutorError>>,
) -> Result<T, ExecutorError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(future)
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
