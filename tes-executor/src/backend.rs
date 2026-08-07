//! Runs an attempt of a task as one TES task: the wrapper is its one executor, the inputs come
//! from their `file://` places, and the outputs and control files go back to the scratch
//! directory of the attempt. So the server must see the files of this machine at the same
//! paths, as Funnel on this machine or on a cluster with a shared file system does.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use executor::{Backend, ExecutorError, Finished, Job, registry, wrapper};
use tokio_util::sync::CancellationToken;

use crate::client::{Client, ExecutorSpec, FileType, Input, Output, Resources, State, Task};

/// The folders of a task in its container.
const WORK: &str = "/iunctus/work";
const CONTROL: &str = "/iunctus/control";
const OUTBOX: &str = "/iunctus/outbox";
const HOME: &str = "/iunctus/home";
const WRAPPER: &str = "/iunctus/wrapper.sh";

/// How long the backend waits between two looks at a task, at most.
const POLL: Duration = Duration::from_secs(5);

/// How long a cancelled task may take to end before the backend stops waiting for it.
const CANCEL_GRACE: Duration = Duration::from_secs(60);

/// The TES executor's backend.
#[derive(Debug)]
pub struct TesBackend {
    pub client: Client,
    /// Where each attempt gets its scratch directory, which the server reads and writes.
    pub scratch: PathBuf,
}

impl Backend for TesBackend {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        registry::pin(image).await
    }

    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let root = self.scratch.join(job.name);
        tokio::fs::create_dir_all(root.join("empty")).await?;
        let result = self.attempt(job, &root, cancel).await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        result
    }
}

impl TesBackend {
    async fn attempt(
        &self,
        job: Job<'_>,
        root: &Path,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let task = task(job, root)?;
        let folders = wrapper::Folders {
            work: &workdir(job),
            control: CONTROL,
            home: HOME,
            outbox: OUTBOX,
        };
        let script = wrapper::wrapper(job.spec, &folders);
        tokio::fs::write(root.join("wrapper.sh"), script).await?;
        let id = self.client.create(&task).await.map_err(machine)?;
        let status = self.wait(&id, cancel).await?;
        match status.state {
            State::Complete | State::ExecutorError => Finished::from_control(root.to_owned())
                .and_then(|finished| match finished.exit_code {
                    Some(_) => Ok(finished),
                    None => Err(failure(&status, "the task ended without an exit code")),
                }),
            State::Canceled => Err(ExecutorError::Cancelled),
            _ => Err(failure(&status, "the TES server could not run the task")),
        }
    }

    /// Waits for a task to end, and cancels it when `cancel` fires.
    async fn wait(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<crate::client::Status, ExecutorError> {
        let mut pause = Duration::from_millis(250);
        loop {
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                () = cancel.cancelled() => {
                    let _ = self.client.cancel(id).await;
                    let deadline = tokio::time::Instant::now() + CANCEL_GRACE;
                    while tokio::time::Instant::now() < deadline {
                        match self.client.status(id).await {
                            Ok(status) if status.state.is_final() => break,
                            _ => tokio::time::sleep(Duration::from_secs(1)).await,
                        }
                    }
                    return Err(ExecutorError::Cancelled);
                }
            }
            let status = self.client.status(id).await.map_err(machine)?;
            if status.state.is_final() {
                return Ok(status);
            }
            pause = (pause * 2).min(POLL);
        }
    }
}

/// The work directory of a task in its container.
fn workdir(job: Job<'_>) -> String {
    job.spec.workdir.clone().unwrap_or_else(|| WORK.to_owned())
}

/// The TES task of an attempt whose scratch directory is `root`.
fn task(job: Job<'_>, root: &Path) -> Result<Task, ExecutorError> {
    let spec = job.spec;
    let image = spec.image.as_ref().ok_or(ExecutorError::NoImage)?;
    // TES needs a command; the default command of an image is not known without it.
    if spec.arguments.is_empty() {
        return Err(ExecutorError::NoCommand(image.reference.clone()));
    }
    let work = workdir(job);
    let inside = |path: &str| match path.trim_end_matches('/') {
        "." => work.clone(),
        absolute if absolute.starts_with('/') => absolute.to_owned(),
        relative => format!("{}/{relative}", work.trim_end_matches('/')),
    };
    let mut inputs = vec![Input {
        path: WRAPPER.into(),
        url: file_url(&root.join("wrapper.sh")),
        kind: FileType::File,
    }];
    // An input that the task changes where it is goes back to its place as an output too.
    let mut written_back = Vec::new();
    for staged in &job.staging.inputs {
        let kind = match staged.directory {
            true => FileType::Directory,
            false => FileType::File,
        };
        inputs.push(Input {
            path: inside(&staged.path),
            url: file_url(&staged.source),
            kind,
        });
        if staged.in_place {
            written_back.push(Output {
                path: inside(&staged.path),
                url: file_url(&staged.source),
                kind,
            });
        }
    }
    // A directory that the task gets empty is an empty folder of the scratch directory.
    for directory in &job.staging.directories {
        inputs.push(Input {
            path: inside(directory),
            url: file_url(&root.join("empty")),
            kind: FileType::Directory,
        });
    }
    if let Some(stdin) = &job.staging.stdin {
        inputs.push(Input {
            path: format!("{CONTROL}/{}", wrapper::STDIN),
            url: file_url(stdin),
            kind: FileType::File,
        });
    }
    let mut outputs = vec![
        Output {
            path: OUTBOX.into(),
            url: file_url(&root.join("work")),
            kind: FileType::Directory,
        },
        Output {
            path: CONTROL.into(),
            url: file_url(&root.join("control")),
            kind: FileType::Directory,
        },
    ];
    outputs.extend(written_back);
    let placement = job.placement;
    const GIB: f64 = (1u64 << 30) as f64;
    let resources = Resources {
        cpu_cores: Some(placement.cpus.max(1)),
        ram_gb: placement.memory_bytes.map(|bytes| bytes as f64 / GIB),
        disk_gb: placement.disk_bytes.map(|bytes| bytes as f64 / GIB),
    };
    Ok(Task {
        name: job.name.to_owned(),
        inputs,
        outputs,
        resources: Some(resources),
        executors: vec![ExecutorSpec {
            image: image.reference.clone(),
            command: vec!["sh".into(), WRAPPER.into()],
            workdir: Some(work.clone()),
        }],
        // The work directory, the home folder and `/tmp` are writable, also when the image's
        // own files are not.
        volumes: vec![work, HOME.into(), "/tmp".into()],
        tags: BTreeMap::from([("iunctus-attempt".to_owned(), job.name.to_owned())]),
    })
}

/// A `file://` URL of a local path, with the characters that have a meaning in URLs encoded.
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' => {
                url.push(char::from(byte))
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// A failure of the server, which another attempt may not have.
fn machine(error: crate::TesError) -> ExecutorError {
    ExecutorError::Engine {
        engine: "TES".into(),
        message: error.to_string(),
    }
}

/// A task that the server could not finish, with what its logs say: the lines about errors,
/// or else the last lines.
fn failure(status: &crate::client::Status, message: &str) -> ExecutorError {
    let all: Vec<&str> = (status.logs.iter())
        .flat_map(|log| log.system_logs.iter().map(String::as_str))
        .collect();
    let errors: Vec<&str> = (all.iter().copied())
        .filter(|line| line.to_lowercase().contains("error"))
        .collect();
    let logs = match errors.is_empty() {
        true => &all[all.len().saturating_sub(5)..],
        false => &errors[..],
    };
    ExecutorError::Engine {
        engine: "TES".into(),
        message: match logs.is_empty() {
            true => format!("{message} ({:?})", status.state),
            false => format!("{message} ({:?}): {}", status.state, logs.join("; ")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use executor::{Staged, Staging};
    use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};

    #[test]
    fn attempts_become_tes_tasks() {
        let spec = TaskSpec {
            format: FORMAT,
            namespace: String::new(),
            image: Some(Image {
                reference: "debian@sha256:ab".into(),
                platform: None,
            }),
            arguments: vec!["bash".into(), "-c".into(), "true".into()],
            workdir: None,
            environment: BTreeMap::new(),
            inputs: Vec::new(),
            outputs: vec![OutputRule {
                pattern: "out.txt".into(),
            }],
            stdin: None,
        };
        let staging = Staging {
            inputs: vec![Staged {
                path: "in dir/a.txt".into(),
                source: "/store/runs/x/a.txt".into(),
                directory: false,
                copy: false,
                executable: false,
                ..Default::default()
            }],
            directories: vec!["made".into()],
            stdin: None,
        };
        let placement = Placement {
            cpus: 2,
            memory_bytes: Some(3 << 30),
            ..Placement::default()
        };
        let job = Job {
            name: "run-P-1-1",
            spec: &spec,
            staging: &staging,
            placement: &placement,
        };
        let task = task(job, Path::new("/scratch/run-P-1-1")).expect("a task");
        let paths: Vec<(&str, &str)> = (task.inputs.iter())
            .map(|input| (input.path.as_str(), input.url.as_str()))
            .collect();
        let expected = [
            (
                "/iunctus/wrapper.sh",
                "file:///scratch/run-P-1-1/wrapper.sh",
            ),
            ("/iunctus/work/in dir/a.txt", "file:///store/runs/x/a.txt"),
            ("/iunctus/work/made", "file:///scratch/run-P-1-1/empty"),
        ];
        assert_eq!(paths, expected);
        assert_eq!(task.outputs[0].url, "file:///scratch/run-P-1-1/work");
        let resources = task.resources.expect("resources");
        assert_eq!(
            (resources.cpu_cores, resources.ram_gb),
            (Some(2), Some(3.0))
        );
        assert_eq!(file_url(Path::new("/a b/c")), "file:///a%20b/c");
    }
}
