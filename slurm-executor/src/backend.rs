//! Runs an attempt of a task as one SLURM batch job: the inputs are placed in the scratch
//! directory of the attempt, and the job runs the wrapper in the task's container on a node.
//! So the nodes must see the scratch directory and the store at the same paths, as on a
//! cluster with a shared file system.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use executor::manifest::parse;
use executor::wrapper::{EXIT, MANIFEST, STDERR};
use executor::{Backend, CONTROL, ContainerRun, ExecutorError, Finished, Job, place, registry};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::queue::{Look, Queue};
use crate::script::{CONTAINER_EXIT, Container, LOG, SCRIPT, job_script, sbatch_options};

/// How often the queue is asked about the jobs, by default.
const POLL: Duration = Duration::from_secs(10);

/// How long a job that ended may take to show the record of its container, which a shared
/// file system may show late.
const EXIT_GRACE: Duration = Duration::from_secs(30);

/// How long a cancelled job may take to leave the queue before the backend stops waiting.
const CANCEL_GRACE: Duration = Duration::from_secs(60);

/// The tools of SLURM that the backend runs.
pub const TOOLS: [&str; 4] = ["sbatch", "squeue", "sacct", "scancel"];

/// Whether a program can run: a path to an executable file, or a name that the PATH has.
pub fn executable(program: &Path) -> bool {
    let runs = |path: &Path| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    };
    if program.components().count() > 1 {
        return runs(program);
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|folder| runs(&folder.join(program))))
}

/// The SLURM executor's backend.
#[derive(Debug)]
pub struct SlurmBackend {
    /// Where each attempt gets its scratch directory, which the nodes must see at the same
    /// path.
    pub scratch: PathBuf,
    pub container: Container,
    /// More options of `sbatch`, like `--account=lab`.
    pub options: Vec<String>,
    /// The folder of `sbatch`, `squeue`, `sacct` and `scancel`, when they are not on the PATH.
    pub commands: Option<PathBuf>,
    /// How often the queue is asked about the jobs.
    pub poll: Duration,
    queue: Queue,
}

/// How a job ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The job script recorded the exit code of the container.
    Done(i32),
    /// The job ended without that record, in this state when SLURM tells it.
    Stopped(Option<String>),
}

impl SlurmBackend {
    /// A backend with its attempts in `scratch`, whose jobs run their tasks with `container`.
    pub fn new(scratch: PathBuf, container: Container) -> SlurmBackend {
        SlurmBackend {
            scratch,
            container,
            options: Vec::new(),
            commands: None,
            poll: POLL,
            queue: Queue::default(),
        }
    }

    /// The tools of SLURM that are not there: not in the folder of `commands`, or not on the
    /// PATH. Runs need `sbatch`, `squeue` and `scancel`; `sacct` only tells how a job ended
    /// that SLURM has forgotten.
    pub fn missing_tools(&self) -> Vec<&'static str> {
        TOOLS
            .into_iter()
            .filter(|tool| !executable(&self.command(tool)))
            .collect()
    }

    fn command(&self, name: &str) -> PathBuf {
        match &self.commands {
            Some(folder) => folder.join(name),
            None => PathBuf::from(name),
        }
    }
}

impl Backend for SlurmBackend {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        registry::pin(image).await
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
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
            Ok(mounts) => self.attempt(job, &root, &mounts, cancel).await,
        };
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        result
    }
}

impl SlurmBackend {
    /// Writes the wrapper and the job script of a staged attempt, submits the job and waits
    /// for its end.
    async fn attempt(
        &self,
        job: Job<'_>,
        root: &Path,
        mounts: &[executor::process::Mount],
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let image = job.spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        // The default command of an image is not known without its engine on this machine.
        if job.spec.arguments.is_empty() {
            return Err(ExecutorError::NoCommand(image.reference.clone()));
        }
        let control = root.join(CONTROL);
        let owner = tokio::fs::metadata(root).await?;
        let name = format!("iunctus-{}", job.name);
        let run = ContainerRun {
            name: &name,
            image,
            root,
            workdir: job.spec.workdir.as_deref(),
            mounts,
            owner: (owner.uid(), owner.gid()),
            cpus: job.placement.cpus.max(1),
            memory: job.placement.memory_bytes,
            offline: job.placement.offline,
            wrapper_path: None,
        };
        tokio::fs::write(run.script(), run.wrapper(job.spec)).await?;
        let gpus = job.placement.accelerators.iter().any(|gpu| gpu.count > 0);
        let script = job_script(&self.container, &run, gpus);
        tokio::fs::write(control.join(SCRIPT), script).await?;
        let id = self.submit(job, root).await?;
        self.queue.add(&id).await;
        let ended = self.wait(&id, &control, cancel).await;
        self.queue.remove(&id).await;
        let time = job.placement.time_seconds;
        finish(ended?, root.to_owned(), time).await
    }

    /// Submits the job of an attempt and gives its id.
    async fn submit(&self, job: Job<'_>, root: &Path) -> Result<String, ExecutorError> {
        let output = Command::new(self.command("sbatch"))
            .args(sbatch_options(job, root, &self.options))
            .arg(root.join(CONTROL).join(SCRIPT))
            .output()
            .await
            .map_err(|error| machine(format!("cannot run sbatch: {error}")))?;
        let text = String::from_utf8_lossy(&output.stdout);
        // `--parsable` gives `<id>` or `<id>;<cluster>`.
        let id = text.trim().split(';').next().unwrap_or_default();
        if !output.status.success() || id.is_empty() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(machine(format!(
                "sbatch did not take the job: {}",
                error.trim()
            )));
        }
        Ok(id.to_owned())
    }

    /// Waits for a job to end, and cancels it when `cancel` fires.
    async fn wait(
        &self,
        id: &str,
        control: &Path,
        cancel: &CancellationToken,
    ) -> Result<Ended, ExecutorError> {
        let squeue = self.command("squeue");
        let mut pause = Duration::from_millis(250);
        let mut ended: Option<(Instant, Option<String>)> = None;
        loop {
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                () = cancel.cancelled() => {
                    self.cancel(id).await;
                    return Err(ExecutorError::Cancelled);
                }
            }
            pause = (pause * 2).min(self.poll);
            if let Some(status) = read_number(&control.join(CONTAINER_EXIT)).await {
                return Ok(Ended::Done(status));
            }
            if let Some((since, state)) = &ended {
                if since.elapsed() >= EXIT_GRACE {
                    return Ok(Ended::Stopped(state.clone()));
                }
                continue;
            }
            let state = match self.queue.look(id, self.poll, &squeue).await {
                Look::Active | Look::Unknown => continue,
                Look::Ended(state) => Some(state),
                Look::Gone => self.queue.accounted(&self.command("sacct"), id).await,
            };
            // A job that ended well has recorded its container; the shared file system may
            // show the record late. Other ends need no wait.
            match state.as_deref() {
                Some("COMPLETED") | None => ended = Some((Instant::now(), state)),
                Some(_) => return Ok(Ended::Stopped(state)),
            }
        }
    }

    /// Cancels a job and waits for it to leave the queue, so that its container is gone.
    async fn cancel(&self, id: &str) {
        let _ = Command::new(self.command("scancel")).arg(id).output().await;
        let squeue = self.command("squeue");
        let deadline = Instant::now() + CANCEL_GRACE;
        while Instant::now() < deadline {
            tokio::time::sleep(self.poll.min(Duration::from_secs(1))).await;
            if matches!(
                self.queue.look(id, self.poll, &squeue).await,
                Look::Ended(_) | Look::Gone
            ) {
                break;
            }
        }
    }
}

/// What an attempt left behind, by how its job ended.
async fn finish(ended: Ended, root: PathBuf, time: Option<u64>) -> Result<Finished, ExecutorError> {
    let control = root.join(CONTROL);
    let recorded = read_number(&control.join(EXIT)).await;
    let (exit_code, note) = match outcome(recorded, &ended, time) {
        Ok(outcome) => outcome,
        Err(message) => return Err(machine(explain(message, &root).await)),
    };
    if let Some(note) = note {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(control.join(STDERR))
            .await?;
        file.write_all(note.as_bytes()).await?;
    }
    let files = match exit_code {
        0 => {
            let manifest = tokio::fs::read_to_string(control.join(MANIFEST)).await;
            parse(&manifest.unwrap_or_default())?
        }
        _ => Vec::new(),
    };
    Ok(Finished {
        exit_code: Some(exit_code),
        files,
        usage: executor::Usage::of(&root),
        root,
    })
}

/// The exit code of the task and a note for its standard error, from the exit code that the
/// wrapper recorded and the end of the job; `Err` when the machine failed rather than the task.
///
/// The wrapper records the tool's exit code and ends with 0. A container stopped by a signal,
/// as for lack of memory, ends the task with its own exit code. A job that ran out of time or
/// memory ends the task as a stopped container would. Other ends are failures of the machine,
/// like a container runtime that could not start the image, or a node that failed.
fn outcome(
    recorded: Option<i32>,
    ended: &Ended,
    time: Option<u64>,
) -> Result<(i32, Option<String>), String> {
    match ended {
        Ended::Done(0) => recorded
            .map(|code| (code, None))
            .ok_or_else(|| "the container ended without the exit code of the task".to_owned()),
        Ended::Done(status @ 129..=192) => Ok((
            *status,
            Some(format!(
                "iunctus: the container ended with exit code {status}, maybe for lack of memory\n"
            )),
        )),
        Ended::Done(status) => Err(format!(
            "the container could not run the task (exit code {status})"
        )),
        Ended::Stopped(Some(state)) if state == "TIMEOUT" => Ok((
            143,
            Some(match time {
                Some(time) => {
                    format!("iunctus: the task ran longer than its time of {time} seconds\n")
                }
                None => "iunctus: the task ran longer than SLURM allows\n".to_owned(),
            }),
        )),
        Ended::Stopped(Some(state)) if state == "OUT_OF_MEMORY" => Ok((
            137,
            Some("iunctus: SLURM stopped the task for lack of memory\n".to_owned()),
        )),
        Ended::Stopped(Some(state)) => Err(format!("the job ended as {state}")),
        Ended::Stopped(None) => Err("the job ended without a record of its container".into()),
    }
}

/// A failure of the machine with the end of the job's log, or a hint when the job left none.
async fn explain(message: String, root: &Path) -> String {
    let log = tokio::fs::read_to_string(root.join(CONTROL).join(LOG)).await;
    let log = log.unwrap_or_default();
    let lines: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
    match lines.len() {
        0 => format!(
            "{message}, and left no log; do the nodes see {} at the same path?",
            root.display()
        ),
        count => format!("{message}: {}", lines[count.saturating_sub(5)..].join("; ")),
    }
}

async fn read_number(path: &Path) -> Option<i32> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    text.trim().parse().ok()
}

fn machine(message: String) -> ExecutorError {
    ExecutorError::Engine {
        engine: "SLURM".into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_end_tasks_or_fail_the_machine() {
        let done = |status| Ended::Done(status);
        let stopped = |state: &str| Ended::Stopped(Some(state.to_owned()));
        assert_eq!(outcome(Some(3), &done(0), None), Ok((3, None)));
        assert!(outcome(None, &done(0), None).is_err());
        assert_eq!(
            outcome(None, &done(137), None).map(|(code, _)| code),
            Ok(137)
        );
        assert!(outcome(None, &done(255), None).is_err());
        assert!(outcome(Some(0), &done(125), None).is_err());
        let timeout = outcome(Some(0), &stopped("TIMEOUT"), Some(60)).expect("an end");
        assert_eq!(timeout.0, 143);
        assert!(timeout.1.expect("a note").contains("60 seconds"));
        let memory = outcome(None, &stopped("OUT_OF_MEMORY"), None);
        assert_eq!(memory.map(|(code, _)| code), Ok(137));
        for state in ["NODE_FAIL", "PREEMPTED", "BOOT_FAIL", "CANCELLED", "FAILED"] {
            assert!(outcome(None, &stopped(state), None).is_err(), "{state}");
        }
        assert!(outcome(None, &Ended::Stopped(None), None).is_err());
    }
}
