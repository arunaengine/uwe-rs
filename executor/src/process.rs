//! A container whose main process answers lines: it reads one line on standard input and
//! writes one reply line on standard output, like the Python process of decision 19. The
//! container keeps running between lines, and is removed when it stops.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
    /// The container engine command, `docker` or `podman`.
    pub engine: String,
    /// The name of the container, unique on the machine.
    pub name: String,
    pub image: String,
    /// The user and group, like `1000:1000`; the image's user when `None`.
    pub user: Option<String>,
    /// The program that runs the command, instead of the image's entrypoint.
    pub entrypoint: Option<String>,
    pub command: Vec<String>,
    pub mounts: Vec<Mount>,
    pub workdir: Option<String>,
    pub environment: Vec<(String, String)>,
    /// Passes what the container writes to standard error on to the standard error of
    /// iunctus; the last lines are always kept for errors.
    pub show_stderr: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("the container engine {engine} is not installed: {reason}")]
    NoEngine { engine: String, reason: String },
    #[error("the container engine {engine} does not run: {reason}")]
    EngineDown { engine: String, reason: String },
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

/// A running container that answers lines.
pub struct ContainerProcess {
    engine: String,
    name: String,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    reader: Option<JoinHandle<()>>,
}

impl ContainerProcess {
    /// Checks that the engine runs, and starts the container with an open standard input.
    pub fn start(spec: &ContainerSpec) -> Result<ContainerProcess, ProcessError> {
        check_engine(&spec.engine)?;
        let mut command = Command::new(&spec.engine);
        command
            .args(["run", "--interactive", "--rm"])
            .args(options(spec));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| ProcessError::Start {
            engine: spec.engine.clone(),
            reason: error.to_string(),
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err(ProcessError::Start {
                engine: spec.engine.clone(),
                reason: "no pipes to the container".into(),
            });
        };
        let kept = Arc::new(Mutex::new(VecDeque::new()));
        let lines = kept.clone();
        let show = spec.show_stderr;
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if show {
                    eprintln!("{line}");
                }
                if let Ok(mut lines) = lines.lock() {
                    if lines.len() == KEPT_LINES {
                        lines.pop_front();
                    }
                    lines.push_back(line);
                }
            }
        });
        Ok(ContainerProcess {
            engine: spec.engine.clone(),
            name: spec.name.clone(),
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stderr: kept,
            reader: Some(reader),
        })
    }

    /// Sends one line and waits for the reply line.
    pub fn exchange(&mut self, line: &str) -> Result<String, ProcessError> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(self.ended());
        };
        if writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .is_err()
        {
            return Err(self.ended());
        }
        let mut reply = String::new();
        match self.stdout.read_line(&mut reply) {
            Ok(0) | Err(_) => Err(self.ended()),
            Ok(_) => Ok(reply.trim_end_matches('\n').to_owned()),
        }
    }

    fn ended(&mut self) -> ProcessError {
        // The reader has the last lines once the container's standard error closed.
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
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
        self.stdin = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = Command::new(&self.engine)
                .args(["rm", "--force", &self.name])
                .output();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for ContainerProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The options of `run` for a container and its command. Core dumps are off: a tool that
/// crashes inside such a container would only fill the journal of the machine.
fn options(spec: &ContainerSpec) -> Vec<String> {
    let mut options = vec![
        "--name".to_owned(),
        spec.name.clone(),
        "--ulimit".to_owned(),
        "core=0".to_owned(),
    ];
    if let Some(user) = &spec.user {
        options.extend(["--user".to_owned(), user.clone()]);
    }
    if let Some(entrypoint) = &spec.entrypoint {
        options.extend(["--entrypoint".to_owned(), entrypoint.clone()]);
    }
    for mount in &spec.mounts {
        let mut volume = format!("{}:{}", mount.source.display(), mount.target);
        if mount.read_only {
            volume.push_str(":ro");
        }
        options.extend(["--volume".to_owned(), volume]);
    }
    if let Some(workdir) = &spec.workdir {
        options.extend(["--workdir".to_owned(), workdir.clone()]);
    }
    for (name, value) in &spec.environment {
        options.extend(["--env".to_owned(), format!("{name}={value}")]);
    }
    options.push(spec.image.clone());
    options.extend(spec.command.iter().cloned());
    options
}

/// Runs the container of the spec to its end, and removes it; an error has the last lines
/// that it wrote to standard error.
pub fn run_to_end(spec: &ContainerSpec) -> Result<(), ProcessError> {
    check_engine(&spec.engine)?;
    let failed = |reason: String| ProcessError::Failed {
        image: spec.image.clone(),
        reason,
    };
    let output = Command::new(&spec.engine)
        .args(["run", "--rm"])
        .args(options(spec))
        .output()
        .map_err(|error| failed(error.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    Err(failed(
        lines[lines.len().saturating_sub(KEPT_LINES)..].join("\n"),
    ))
}

/// Whether the engine is installed and its service runs.
pub fn check_engine(engine: &str) -> Result<(), ProcessError> {
    let output = Command::new(engine)
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map_err(|error| ProcessError::NoEngine {
            engine: engine.to_owned(),
            reason: error.to_string(),
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ProcessError::EngineDown {
            engine: engine.to_owned(),
            reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ContainerProcess, ContainerSpec, ProcessError};

    #[test]
    fn a_missing_engine_is_a_clear_error() {
        let spec = ContainerSpec {
            engine: "iunctus-no-such-engine".into(),
            ..ContainerSpec::default()
        };
        let Err(error) = ContainerProcess::start(&spec) else {
            panic!("an engine that does not exist started a container");
        };
        assert!(matches!(error, ProcessError::NoEngine { .. }), "{error}");
        assert!(
            error
                .to_string()
                .starts_with("the container engine iunctus-no-such-engine is not installed"),
            "{error}"
        );
    }
}
