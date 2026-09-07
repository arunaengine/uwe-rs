//! A container whose main process answers lines: it reads one line on standard input and
//! writes one reply line on standard output, like the Python process of decision 19. The
//! container keeps running between lines, and is removed when it stops. The containers talk to
//! the engine through its API, on a runtime of this module, since their callers wait for them
//! without a runtime of their own.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
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
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

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
    #[error("the container operation was cancelled")]
    Cancelled,
    #[error("the container operation exceeded its time limit")]
    Timeout,
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

const OPERATION_LIMIT: Duration = Duration::from_secs(600);

async fn bounded<T>(
    cancel: &CancellationToken,
    limit: Duration,
    operation: impl Future<Output = T>,
) -> Result<T, ProcessError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ProcessError::Cancelled),
        result = tokio::time::timeout(limit, operation) => result.map_err(|_| ProcessError::Timeout),
    }
}

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
    engine_cancellable(spec, &CancellationToken::new())
}

fn engine_cancellable(
    spec: &ContainerSpec,
    cancel: &CancellationToken,
) -> Result<ContainerEngine, ProcessError> {
    let name = Some(spec.engine.as_str()).filter(|name| !name.is_empty());
    let found = runtime()?.block_on(bounded(
        cancel,
        OPERATION_LIMIT,
        ContainerEngine::named(name),
    ))?;
    found.map_err(|error| ProcessError::NoEngine(error.to_string()))
}

/// A running container that answers lines.
pub struct ContainerProcess {
    engine: ContainerEngine,
    name: String,
    input: Option<Pin<Box<dyn AsyncWrite + Send>>>,
    /// The lines of standard output, from the task that reads what the container writes.
    lines: UnboundedReceiver<String>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    reader: Option<JoinHandle<()>>,
    cancel: CancellationToken,
}

impl ContainerProcess {
    /// Checks that the engine runs, and starts the container with an open standard input.
    pub fn start(spec: &ContainerSpec) -> Result<ContainerProcess, ProcessError> {
        Self::start_cancellable(spec, &CancellationToken::new())
    }

    /// Starts with cancellation; each engine phase has a ten-minute deadline.
    pub fn start_cancellable(
        spec: &ContainerSpec,
        cancel: &CancellationToken,
    ) -> Result<ContainerProcess, ProcessError> {
        let engine = engine_cancellable(spec, cancel)?;
        let runtime = runtime()?;
        runtime
            .block_on(bounded(
                cancel,
                OPERATION_LIMIT,
                crate::image::available(&engine, &spec.image),
            ))?
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
        let id = runtime.block_on(create_id(&engine, options, body, cancel))?;
        let attached = runtime.block_on(bounded(cancel, OPERATION_LIMIT, async {
            // Attached before it starts, so that no line gets lost.
            let attached = client.attach_container(&id, Some(attach)).await?;
            client.start_container(&id, None).await?;
            Ok(attached)
        }));
        let attached = match attached {
            Ok(Ok(attached)) => attached,
            Err(error) => {
                remove(&engine, &id);
                return Err(error);
            }
            Ok(Err(error)) => {
                remove(&engine, &id);
                return Err(failed(error));
            }
        };
        let (sender, lines) = unbounded_channel();
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
            cancel: cancel.child_token(),
        })
    }

    /// Sends one line and waits for the reply line.
    pub fn exchange(&mut self, line: &str) -> Result<String, ProcessError> {
        self.exchange_with_timeout(line, OPERATION_LIMIT)
    }

    /// Bounds the complete write and reply wait; cancellation or timeout stops the helper.
    pub fn exchange_with_timeout(
        &mut self,
        line: &str,
        limit: Duration,
    ) -> Result<String, ProcessError> {
        let (Some(input), Ok(runtime)) = (self.input.as_mut(), runtime()) else {
            return Err(self.ended());
        };
        let reply = runtime.block_on(bounded(&self.cancel, limit, async {
            input.write_all(format!("{line}\n").as_bytes()).await?;
            input.flush().await?;
            Ok::<_, std::io::Error>(self.lines.recv().await)
        }));
        match reply {
            Ok(Ok(Some(reply))) => Ok(reply),
            Ok(_) => Err(self.ended()),
            Err(error) => {
                self.cancel.cancel();
                self.stop();
                Err(error)
            }
        }
    }

    fn ended(&mut self) -> ProcessError {
        // The reader has the last lines once the container's output closed.
        if let (Some(mut reader), Ok(runtime)) = (self.reader.take(), runtime())
            && runtime
                .block_on(bounded(&self.cancel, STOP_GRACE, &mut reader))
                .is_err()
        {
            reader.abort();
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
        runtime.block_on(async {
            let _ = bounded(&self.cancel, STOP_GRACE, async {
                let _ = input.shutdown().await;
                // The container ends and is removed once its input closed; one that does not in
                // time is removed by force.
                loop {
                    match client.inspect_container(&name, None).await {
                        Ok(found)
                            if found.state.as_ref().and_then(|state| state.running)
                                == Some(true) =>
                        {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        _ => break,
                    }
                }
            })
            .await;
        });
        if let Some(reader) = reader {
            reader.abort();
        }
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
    runtime.block_on(remove_async(engine.client(), name));
}

async fn remove_async(client: &bollard::Docker, name: &str) {
    let options = RemoveContainerOptions {
        force: true,
        ..RemoveContainerOptions::default()
    };
    let _ = tokio::time::timeout(STOP_GRACE, client.remove_container(name, Some(options))).await;
}

async fn create_id(
    engine: &ContainerEngine,
    options: CreateContainerOptions,
    body: ContainerCreateBody,
    cancel: &CancellationToken,
) -> Result<String, ProcessError> {
    if cancel.is_cancelled() {
        return Err(ProcessError::Cancelled);
    }
    let client = engine.client().clone();
    let engine_name = engine.name.clone();
    let (sender, mut receiver) = unbounded_channel();
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            OPERATION_LIMIT,
            client.create_container(Some(options), body),
        )
        .await
        .map_err(|_| ProcessError::Timeout)
        .and_then(|result| {
            result
                .map(|created| created.id)
                .map_err(|error| ProcessError::Start {
                    engine: engine_name,
                    reason: error.to_string(),
                })
        });
        if let Err(unsent) = sender.send(result)
            && let Ok(id) = unsent.0
        {
            remove_async(&client, &id).await;
        }
    });
    match bounded(cancel, OPERATION_LIMIT, receiver.recv()).await {
        Ok(Some(result)) => result,
        Ok(None) => Err(ProcessError::Ended { stderr: Vec::new() }),
        Err(error) => {
            receiver.close();
            if let Ok(Ok(id)) = receiver.try_recv() {
                remove_async(engine.client(), &id).await;
            }
            Err(error)
        }
    }
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
    run_to_end_cancellable(spec, &CancellationToken::new())
}

/// Runs with cancellation and a ten-minute execution deadline.
pub fn run_to_end_cancellable(
    spec: &ContainerSpec,
    cancel: &CancellationToken,
) -> Result<(), ProcessError> {
    run_to_end_cancellable_with_timeout(spec, cancel, OPERATION_LIMIT)
}

/// Runs with an explicit execution budget; image acquisition and creation remain bounded.
pub fn run_to_end_cancellable_with_timeout(
    spec: &ContainerSpec,
    cancel: &CancellationToken,
    limit: Duration,
) -> Result<(), ProcessError> {
    let engine = engine_cancellable(spec, cancel)?;
    let runtime = runtime()?;
    runtime
        .block_on(bounded(
            cancel,
            OPERATION_LIMIT,
            crate::image::available(&engine, &spec.image),
        ))?
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
    let id = runtime.block_on(create_id(
        &engine,
        options,
        create(spec, &engine, false),
        cancel,
    ))?;
    let ended = runtime.block_on(bounded(cancel, limit, async {
        client.start_container(&id, None).await?;
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let state = client.inspect_container(&id, None).await?.state;
            let state = state.unwrap_or_default();
            if !state.running.unwrap_or(false) {
                return Ok::<_, bollard::errors::Error>(state.exit_code.unwrap_or(-1));
            }
        }
    }));
    let log = match ended {
        Ok(Ok(0)) => None,
        Ok(Ok(_)) => Some(
            runtime
                .block_on(bounded(cancel, STOP_GRACE, tail(&engine, &id)))
                .unwrap_or_else(|error| error.to_string()),
        ),
        Ok(Err(ref error)) => Some(error.to_string()),
        Err(error) => {
            remove(&engine, &id);
            return Err(error);
        }
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
    use super::*;

    #[test]
    fn cancelled_helpers_do_not_contact_an_engine() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let spec = ContainerSpec {
            engine: "/nowhere/iunctus.sock".into(),
            ..ContainerSpec::default()
        };
        assert!(matches!(
            ContainerProcess::start_cancellable(&spec, &cancel),
            Err(ProcessError::Cancelled)
        ));
        assert!(matches!(
            run_to_end_cancellable(&spec, &cancel),
            Err(ProcessError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn reply_waits_cancel_before_ready_lines_and_have_a_deadline() {
        let cancel = CancellationToken::new();
        let (sender, mut lines) = unbounded_channel();
        sender.send("reply").unwrap();
        cancel.cancel();
        assert!(matches!(
            bounded(&cancel, OPERATION_LIMIT, lines.recv()).await,
            Err(ProcessError::Cancelled)
        ));
        assert_eq!(lines.recv().await, Some("reply"));
        assert!(matches!(
            bounded(
                &CancellationToken::new(),
                Duration::from_millis(5),
                lines.recv()
            )
            .await,
            Err(ProcessError::Timeout)
        ));
    }

    #[tokio::test]
    async fn cancelled_creation_removes_a_late_accepted_id() {
        use std::io::{BufRead, BufReader, Read, Write};
        let folder = tempfile::tempdir().unwrap();
        let socket = folder.path().join("engine.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            for _ in 0..4 {
                let mut stream = loop {
                    if let Ok((stream, _)) = listener.accept() {
                        break stream;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "engine request did not arrive"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                requests.push(first.clone());
                let mut line = String::new();
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                reader.read_exact(&mut vec![0; length]).unwrap();
                let body = if first.starts_with("POST ") {
                    std::thread::sleep(Duration::from_millis(100));
                    r#"{"Id":"accepted-id","Warnings":[]}"#
                } else {
                    "{}"
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            requests
        });
        let engine = ContainerEngine::connect(socket.to_str().unwrap())
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            trigger.cancel();
        });
        let result = create_id(
            &engine,
            CreateContainerOptions {
                name: Some("unowned-name".into()),
                ..Default::default()
            },
            ContainerCreateBody::default(),
            &cancel,
        )
        .await;
        assert!(matches!(result, Err(ProcessError::Cancelled)));
        let requests = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || server.join().unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(requests[3].starts_with("DELETE "));
        assert!(requests[3].contains("/containers/accepted-id?"));
        assert!(!requests[3].contains("unowned-name"));
    }

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
