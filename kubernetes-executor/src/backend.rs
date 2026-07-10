//! Runs an attempt of a task as one Kubernetes Job: the inputs are placed in the scratch
//! directory of the attempt, and the pod runs the wrapper in the task's image with the folders
//! of the task as `hostPath` volumes. So the nodes must see the scratch directory and the store
//! at the same paths, as a `kind` cluster whose node mounts them, or nodes with a shared file
//! system.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use executor::manifest::parse;
use executor::process::Mount;
use executor::wrapper::{EXIT, MANIFEST, STDERR, wrapper};
use executor::{Backend, CONTROL, ContainerRun, ExecutorError, Finished, Job, place, registry};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::manifest::{Limits, job_name};
use crate::pods::{Kubectl, Look, Pods};

/// How often the cluster is asked about the pods, by default.
const POLL: Duration = Duration::from_secs(5);

/// How long a Job may have no pod, or a pod that cannot start, before it counts as failed.
const GRACE: Duration = Duration::from_secs(120);

/// How long a cancelled Job may take to remove its pod before the backend stops waiting.
const CANCEL_GRACE: Duration = Duration::from_secs(60);

/// The Kubernetes executor's backend.
#[derive(Debug)]
pub struct KubernetesBackend {
    pub kubectl: Kubectl,
    /// Where each attempt gets its scratch directory, which the nodes must see at the same
    /// path.
    pub scratch: PathBuf,
    /// How often the cluster is asked about the pods.
    pub poll: Duration,
    /// The label value of the Jobs of this backend.
    label: String,
    pods: Pods,
}

/// How the container of a Job ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// It exited with this code, for this reason.
    Exited(i32, String),
    /// The pod was stopped for this reason, like `DeadlineExceeded`.
    Stopped(String),
}

impl KubernetesBackend {
    /// A backend with its attempts in `scratch`, which reaches the cluster through `kubectl`.
    pub fn new(kubectl: Kubectl, scratch: PathBuf) -> KubernetesBackend {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        KubernetesBackend {
            kubectl,
            scratch,
            poll: POLL,
            label: format!("{}-{millis}", std::process::id()),
            pods: Pods::default(),
        }
    }
}

impl Backend for KubernetesBackend {
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

impl KubernetesBackend {
    /// Writes the wrapper of a staged attempt, creates its Job and waits for its end.
    async fn attempt(
        &self,
        job: Job<'_>,
        root: &Path,
        mounts: &[Mount],
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let image = job.spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
        // The default command of an image is not known without its engine on this machine.
        if job.spec.arguments.is_empty() {
            return Err(ExecutorError::NoCommand(image.reference.clone()));
        }
        let control = root.join(CONTROL);
        let owner = tokio::fs::metadata(root).await?;
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
        };
        let script = wrapper(job.spec, &run.inside(), &control.to_string_lossy());
        tokio::fs::write(control.join("wrapper.sh"), script).await?;
        let limits = Limits {
            gpus: job.placement.accelerators.iter().map(|gpu| gpu.count).sum(),
            seconds: job.placement.time_seconds,
        };
        let manifest = crate::manifest::job(&name, &self.label, &run, &limits);
        self.create(&manifest).await?;
        self.pods.add(&name).await;
        let ended = self.wait(&name, cancel).await;
        self.pods.remove(&name).await;
        if !matches!(ended, Err(ExecutorError::Cancelled)) {
            self.delete(&name, "background").await;
        }
        finish(ended?, root.to_owned(), limits.seconds).await
    }

    /// Creates a Job from its manifest.
    async fn create(&self, manifest: &serde_json::Value) -> Result<(), ExecutorError> {
        let mut child = (self.kubectl.command())
            .args(["create", "--filename", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| machine(format!("cannot run kubectl: {error}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(manifest.to_string().as_bytes()).await?;
        }
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(machine(format!(
                "kubectl could not create the Job: {}",
                error.trim()
            )));
        }
        Ok(())
    }

    /// Deletes a Job and its pod, without waiting.
    async fn delete(&self, name: &str, cascade: &str) {
        let _ = (self.kubectl.command())
            .args(["delete", "job", name, "--wait=false", "--ignore-not-found"])
            .arg(format!("--cascade={cascade}"))
            .output()
            .await;
    }

    /// Waits for the container of a Job to end, and deletes the Job when `cancel` fires.
    async fn wait(&self, name: &str, cancel: &CancellationToken) -> Result<Ended, ExecutorError> {
        let mut pause = Duration::from_millis(250);
        let mut trouble: Option<Instant> = None;
        loop {
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                () = cancel.cancelled() => {
                    self.cancel(name).await;
                    return Err(ExecutorError::Cancelled);
                }
            }
            pause = (pause * 2).min(self.poll);
            let problem = match (self.pods.look(name, self.poll, &self.kubectl, &self.label)).await
            {
                Look::Active | Look::Unknown => None,
                Look::Ended { exit_code, reason } => return Ok(Ended::Exited(exit_code, reason)),
                Look::Stopped(reason) => return Ok(Ended::Stopped(reason)),
                Look::Stuck(reason) => Some(format!("the pod cannot start: {reason}")),
                Look::Missing => Some("the Job has no pod".to_owned()),
            };
            // A pod may appear late, and an image may be pulled at the next try.
            match (problem, trouble) {
                (None, _) => trouble = None,
                (Some(_), None) => trouble = Some(Instant::now()),
                (Some(problem), Some(since)) if since.elapsed() >= GRACE => {
                    self.delete(name, "background").await;
                    return Err(machine(problem));
                }
                (Some(_), Some(_)) => {}
            }
        }
    }

    /// Deletes a Job and waits until its pod is gone, so that its container stopped.
    async fn cancel(&self, name: &str) {
        self.delete(name, "foreground").await;
        let deadline = Instant::now() + CANCEL_GRACE;
        while Instant::now() < deadline {
            tokio::time::sleep(self.poll.min(Duration::from_secs(1))).await;
            let look = self
                .pods
                .look(name, self.poll, &self.kubectl, &self.label)
                .await;
            if look == Look::Missing {
                break;
            }
        }
    }
}

/// What an attempt left behind, by how its container ended.
async fn finish(ended: Ended, root: PathBuf, time: Option<u64>) -> Result<Finished, ExecutorError> {
    let control = root.join(CONTROL);
    let recorded = (tokio::fs::read_to_string(control.join(EXIT)).await.ok())
        .and_then(|text| text.trim().parse().ok());
    let (exit_code, note) = outcome(recorded, &ended, time).map_err(machine)?;
    if let Some(note) = note {
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
        root,
    })
}

/// The exit code of the task and a note for its standard error, from the exit code that the
/// wrapper recorded and the end of the container; `Err` when the machine failed rather than
/// the task.
///
/// The wrapper records the tool's exit code and ends with 0. A container killed for lack of
/// memory or stopped by a signal ends the task with its own exit code, and a pod that ran out
/// of time with 143. Other ends are failures of the machine, like an image without Bash, or a
/// pod that was evicted.
fn outcome(
    recorded: Option<i32>,
    ended: &Ended,
    time: Option<u64>,
) -> Result<(i32, Option<String>), String> {
    match ended {
        Ended::Exited(0, _) => recorded
            .map(|code| (code, None))
            .ok_or_else(|| "the container ended without the exit code of the task".to_owned()),
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
        Ended::Exited(status, reason) => Err(format!(
            "the container could not run the task (exit code {status}, {reason})"
        )),
        Ended::Stopped(reason) if reason == "DeadlineExceeded" => Ok((
            143,
            Some(match time {
                Some(time) => {
                    format!("iunctus: the task ran longer than its time of {time} seconds\n")
                }
                None => "iunctus: the task ran longer than Kubernetes allows\n".to_owned(),
            }),
        )),
        Ended::Stopped(reason) => Err(format!("the pod was stopped: {reason}")),
    }
}

fn machine(message: String) -> ExecutorError {
    ExecutorError::Engine {
        engine: "Kubernetes".into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_end_tasks_or_fail_the_machine() {
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
        let late = Ended::Stopped("DeadlineExceeded".into());
        assert_eq!(outcome(None, &late, Some(9)).map(|(code, _)| code), Ok(143));
        assert!(outcome(None, &Ended::Stopped("Evicted".into()), None).is_err());
    }
}
