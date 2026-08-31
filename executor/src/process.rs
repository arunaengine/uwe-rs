//! A container whose main process answers lines: it reads one line on standard input and
//! writes one reply line on standard output, like the Python process of decision 19. The
//! container keeps running between lines, and is removed when it stops. The containers talk to
//! the engine through its API, on a runtime of this module, since their callers wait for them
//! without a runtime of their own.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use bollard::container::LogOutput;
use bollard::models::{ContainerCreateBody, HostConfig, ResourcesUlimits};
use bollard::query_parameters::{
    AttachContainerOptions, CreateContainerOptions, LogsOptions, RemoveContainerOptions,
};
use futures_util::StreamExt;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use crate::engine::ContainerEngine;

/// A folder of the machine that the container sees.
#[derive(Clone, Debug)]
pub struct Mount {
    pub source: PathBuf,
    /// The path inside the container.
    pub target: String,
    pub read_only: bool,
}

/// How to start the container.
#[derive(Clone, Debug, Default)]
pub struct ContainerSpec {
    /// The container engine as `IUNCTUS_ENGINE` names it: `docker`, `podman` or a socket;
    /// empty for Docker, else Podman.
    pub engine: String,
    /// The name of the container, unique on the machine.
    pub name: String,
    pub image: String,
    /// The user and group, like `1000:1000`; the image's user when `None`. An engine without
    /// root runs the container as root, which is the user outside.
    pub user: Option<String>,
    /// The program that runs the command, instead of the image's entrypoint.
    pub entrypoint: Option<String>,
    pub command: Vec<String>,
    pub mounts: Vec<Mount>,
    pub workdir: Option<String>,
    pub environment: Vec<(String, String)>,
    /// The bytes of memory that the container may use; no limit when `None`.
    pub memory: Option<u64>,
    /// Passes what the container writes to standard error on to the standard error of
    /// iunctus; the last lines are always kept for errors.
    pub show_stderr: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("{0}")]
    NoEngine(String),
    #[error("{engine} could not start the container: {reason}")]
    Start { engine: String, reason: String },
    #[error("the container ended{}", last_lines(.stderr))]
    Ended { stderr: Vec<String> },
    #[error("the container of {image} failed: {reason}")]
    Failed { image: String, reason: String },
}

fn last_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("; it wrote:\n{}", lines.join("\n"))
    }
}

/// How many lines of standard error are kept for errors.
const KEPT_LINES: usize = 20;

/// How long a container may take to end once its standard input closed.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// The runtime on which the containers of this module talk to their engine.
fn runtime() -> Result<&'static Runtime, ProcessError> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
    let runtime = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("iunctus-containers")
            .enable_all()
            .build()
            .map_err(|error| error.to_string())
    });
    runtime
        .as_ref()
        .map_err(|message| ProcessError::NoEngine(message.clone()))
}

/// The engine that a spec names, asked on the runtime of this module.
fn engine(spec: &ContainerSpec) -> Result<ContainerEngine, ProcessError> {
    let name = Some(spec.engine.as_str()).filter(|name| !name.is_empty());
    let found = runtime()?.block_on(ContainerEngine::named(name));
    found.map_err(|error| ProcessError::NoEngine(error.to_string()))
}

/// A running container that answers lines.
pub struct ContainerProcess {
    engine: ContainerEngine,
    name: String,
    input: Option<Pin<Box<dyn AsyncWrite + Send>>>,
    /// The lines of standard output, from the task that reads what the container writes.
    lines: Receiver<String>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    reader: Option<JoinHandle<()>>,
}

impl ContainerProcess {
    /// Checks that the engine runs, and starts the container with an open standard input.
    pub fn start(spec: &ContainerSpec) -> Result<ContainerProcess, ProcessError> {
        let engine = engine(spec)?;
        let runtime = runtime()?;
        runtime
            .block_on(crate::image::available(&engine, &spec.image))
            .map_err(|error| ProcessError::Start {
                engine: engine.name.clone(),
                reason: error.to_string(),
            })?;
        let failed = |error: bollard::errors::Error| ProcessError::Start {
            engine: engine.name.clone(),
            reason: error.to_string(),
        };
        let client = engine.client().clone();
        let body = ContainerCreateBody {
            open_stdin: Some(true),
            // The container's input closes when the attached input does.
            stdin_once: Some(true),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..create(spec, &engine, true)
        };
        let options = CreateContainerOptions {
            name: Some(spec.name.clone()),
            ..CreateContainerOptions::default()
        };
        let attach = AttachContainerOptions {
            stream: true,
            stdin: true,
            stdout: true,
            stderr: true,
            ..AttachContainerOptions::default()
        };
        let created = runtime
            .block_on(client.create_container(Some(options), body))
            .map_err(failed)?;
        let id = created.id;
        let attached = runtime.block_on(async {
            // Attached before it starts, so that no line gets lost.
            let attached = client.attach_container(&id, Some(attach)).await?;
            client.start_container(&id, None).await?;
            Ok(attached)
        });
        let attached = match attached {
            Ok(attached) => attached,
            Err(error) => {
                remove(&engine, &id);
                return Err(failed(error));
            }
        };
        let (sender, lines) = channel();
        let kept = Arc::new(Mutex::new(VecDeque::new()));
        let stderr = kept.clone();
        let show = spec.show_stderr;
        let mut output = attached.output;
        let reader = runtime.spawn(async move {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            while let Some(Ok(part)) = output.next().await {
                match part {
                    LogOutput::StdOut { message } | LogOutput::Console { message } => {
                        out.extend_from_slice(&message);
                        for line in take_lines(&mut out) {
                            if sender.send(line).is_err() {
                                return;
                            }
                        }
                    }
                    LogOutput::StdErr { message } => {
                        err.extend_from_slice(&message);
                        for line in take_lines(&mut err) {
                            if show {
                                eprintln!("{line}");
                            }
                            if let Ok(mut lines) = stderr.lock() {
                                if lines.len() == KEPT_LINES {
                                    lines.pop_front();
                                }
                                lines.push_back(line);
                            }
                        }
                    }
                    LogOutput::StdIn { .. } => {}
                }
            }
        });
        Ok(ContainerProcess {
            engine,
            name: id,
            input: Some(attached.input),
            lines,
            stderr: kept,
            reader: Some(reader),
        })
    }

    /// Sends one line and waits for the reply line.
    pub fn exchange(&mut self, line: &str) -> Result<String, ProcessError> {
        let (Some(input), Ok(runtime)) = (self.input.as_mut(), runtime()) else {
            return Err(self.ended());
        };
        let written = runtime.block_on(async {
            input.write_all(format!("{line}\n").as_bytes()).await?;
            input.flush().await
        });
        if written.is_err() {
            return Err(self.ended());
        }
        match self.lines.recv() {
            Ok(reply) => Ok(reply),
            Err(_) => Err(self.ended()),
        }
    }

    fn ended(&mut self) -> ProcessError {
        // The reader has the last lines once the container's output closed.
        if let (Some(reader), Ok(runtime)) = (self.reader.take(), runtime()) {
            let _ = runtime.block_on(reader);
        }
        let stderr = self
            .stderr
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default();
        ProcessError::Ended { stderr }
    }

    /// Closes the container's standard input, waits a few seconds for it to end, and then
    /// removes it.
    pub fn stop(&mut self) {
        let Ok(runtime) = runtime() else {
            return;
        };
        let Some(mut input) = self.input.take() else {
            return;
        };
        let client = self.engine.client().clone();
        let name = self.name.clone();
        let reader = self.reader.take();
        runtime.block_on(async move {
            let _ = input.shutdown().await;
            let deadline = tokio::time::Instant::now() + STOP_GRACE;
            if let Some(reader) = reader {
                let _ = tokio::time::timeout_at(deadline, reader).await;
            }
            // The container ends and is removed once its input closed; one that does not in
            // time is removed by force.
            while tokio::time::Instant::now() < deadline {
                match client.inspect_container(&name, None).await {
                    Ok(found)
                        if found.state.as_ref().and_then(|state| state.running) == Some(true) =>
                    {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    _ => break,
                }
            }
        });
        remove(&self.engine, &self.name);
    }
}

impl Drop for ContainerProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Removes a container, which also stops it.
fn remove(engine: &ContainerEngine, name: &str) {
    let Ok(runtime) = runtime() else {
        return;
    };
    let options = RemoveContainerOptions {
        force: true,
        ..RemoveContainerOptions::default()
    };
    let _ = runtime.block_on(engine.client().remove_container(name, Some(options)));
}

/// The whole lines at the start of a buffer, which leaves the rest in it.
fn take_lines(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();
    while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
        let line: Vec<u8> = buffer.drain(..=end).collect();
        lines.push(String::from_utf8_lossy(&line[..end]).into_owned());
    }
    lines
}

/// The container of a spec and its command. Core dumps are off: a tool that crashes inside
/// such a container would only fill the journal of the machine. It has the least permissions,
/// as the containers of tasks (decision 28): no capabilities and no way to gain privileges. An
/// engine without root runs it as root, which is the user outside.
fn create(spec: &ContainerSpec, engine: &ContainerEngine, remove: bool) -> ContainerCreateBody {
    let binds = (spec.mounts.iter())
        .map(|mount| {
            let mode = if mount.read_only { ":ro" } else { "" };
            format!("{}:{}{mode}", mount.source.display(), mount.target)
        })
        .collect();
    let environment = (spec.environment.iter())
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    ContainerCreateBody {
        image: Some(spec.image.clone()),
        user: (spec.user.as_ref()).map(|user| match engine.rootless {
            true => "0:0".to_owned(),
            false => user.clone(),
        }),
        entrypoint: spec.entrypoint.clone().map(|entrypoint| vec![entrypoint]),
        cmd: Some(spec.command.clone()),
        working_dir: spec.workdir.clone(),
        env: Some(environment),
        host_config: Some(HostConfig {
            binds: Some(binds),
            memory: spec.memory.map(|bytes| bytes as i64),
            auto_remove: Some(remove),
            cap_drop: Some(vec!["ALL".to_owned()]),
            security_opt: Some(vec![crate::access::NO_NEW_PRIVILEGES.to_owned()]),
            ulimits: Some(vec![ResourcesUlimits {
                name: Some("core".to_owned()),
                soft: Some(0),
                hard: Some(0),
            }]),
            ..HostConfig::default()
        }),
        ..ContainerCreateBody::default()
    }
}

/// Runs the container of the spec to its end, and removes it; an error has the last lines
/// that it wrote to standard error.
pub fn run_to_end(spec: &ContainerSpec) -> Result<(), ProcessError> {
    let engine = engine(spec)?;
    let runtime = runtime()?;
    runtime
        .block_on(crate::image::available(&engine, &spec.image))
        .map_err(|error| ProcessError::Failed {
            image: spec.image.clone(),
            reason: error.to_string(),
        })?;
    let failed = |reason: String| ProcessError::Failed {
        image: spec.image.clone(),
        reason,
    };
    let client = engine.client();
    let options = CreateContainerOptions {
        name: Some(spec.name.clone()),
        ..CreateContainerOptions::default()
    };
    let created = runtime
        .block_on(client.create_container(Some(options), create(spec, &engine, false)))
        .map_err(|error| failed(error.to_string()))?;
    let id = created.id;
    let ended: Result<i64, bollard::errors::Error> = runtime.block_on(async {
        client.start_container(&id, None).await?;
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let state = client.inspect_container(&id, None).await?.state;
            let state = state.unwrap_or_default();
            if !state.running.unwrap_or(false) {
                return Ok(state.exit_code.unwrap_or(-1));
            }
        }
    });
    let log = match ended {
        Ok(0) => None,
        Ok(_) => Some(runtime.block_on(tail(&engine, &id))),
        Err(ref error) => Some(error.to_string()),
    };
    remove(&engine, &id);
    match log {
        None => Ok(()),
        Some(log) => Err(failed(log)),
    }
}

/// The last lines that a container wrote to standard error.
async fn tail(engine: &ContainerEngine, name: &str) -> String {
    let options = LogsOptions {
        stderr: true,
        tail: KEPT_LINES.to_string(),
        ..LogsOptions::default()
    };
    let mut text = String::new();
    let mut output = engine.client().logs(name, Some(options));
    while let Some(Ok(part)) = output.next().await {
        text.push_str(&part.to_string());
    }
    text.trim_end().to_owned()
}

/// Whether the engine answers, as `IUNCTUS_ENGINE` names it.
pub fn check_engine(engine: &str) -> Result<(), ProcessError> {
    let spec = ContainerSpec {
        engine: engine.to_owned(),
        ..ContainerSpec::default()
    };
    self::engine(&spec).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{ContainerProcess, ContainerSpec, ProcessError, take_lines};

    #[test]
    fn a_missing_engine_is_a_clear_error() {
        let spec = ContainerSpec {
            engine: "/nowhere/iunctus.sock".into(),
            ..ContainerSpec::default()
        };
        let Err(error) = ContainerProcess::start(&spec) else {
            panic!("an engine that does not exist started a container");
        };
        assert!(matches!(error, ProcessError::NoEngine(_)), "{error}");
        assert!(
            error.to_string().contains("/nowhere/iunctus.sock"),
            "{error}"
        );
    }

    #[test]
    fn output_splits_into_lines() {
        let mut buffer = b"one\ntwo\nthr".to_vec();
        assert_eq!(take_lines(&mut buffer), ["one", "two"]);
        assert_eq!(buffer, b"thr");
    }
}
