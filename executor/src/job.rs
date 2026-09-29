//! One attempt of a task as a backend runs it: the specification, the inputs to place, and what
//! the task left behind.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use task_model::{Placement, TaskSpec};

use crate::manifest::{Collected, ManifestError, parse};
use crate::wrapper::{END, EXIT, MANIFEST, ROOT, START, STDERR, STDOUT, USAGE};

/// The folders of a task's scratch directory: the work directory, the control files of the
/// wrapper, the home folder of the task, and copies of inputs at absolute paths, which are
/// mounted where the task sees them.
pub const WORK: &str = "work";
pub const CONTROL: &str = "control";
pub const HOME: &str = "home";
pub(crate) const MOUNTS: &str = "mounts";

/// One attempt of a task.
#[derive(Clone, Copy, Debug)]
pub struct Job<'a> {
    /// A name no other attempt uses, like `<run>-<task>-<attempt>`. The scratch directory and
    /// the container are named after it.
    pub name: &'a str,
    pub spec: &'a TaskSpec,
    pub staging: &'a Staging,
    pub placement: &'a Placement,
}

/// An input placed before the task starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Staged {
    /// A path relative to the work directory, or an absolute path inside the container.
    pub path: String,
    /// The local file or directory that it shows.
    pub source: PathBuf,
    pub directory: bool,
    /// Whether the task gets a copy of its own, because it may change the input or run it.
    /// Other inputs are shown read-only: as a link to the source, which the container sees at
    /// the same path.
    pub copy: bool,
    /// Whether a copy is a program the task runs.
    pub executable: bool,
    /// Whether the task changes the source where it is: the container sees it writable at the
    /// path of the input, and later tasks see the changes.
    pub in_place: bool,
}

/// What a backend places before a task starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Staging {
    pub inputs: Vec<Staged>,
    /// Directories to make, like the empty directories of a directory input.
    pub directories: Vec<String>,
    /// The local file that the task reads on standard input.
    pub stdin: Option<PathBuf>,
}

/// What a task left behind in its scratch directory.
#[derive(Debug)]
pub struct Finished {
    /// The tool's exit code, or the container's when it was stopped before the tool ended,
    /// like 137 for a container killed for lack of memory. `None` when neither is known.
    pub exit_code: Option<i32>,
    /// The collected outputs, by the paths the task named them with.
    pub files: Vec<Collected>,
    /// The scratch directory, which the caller removes once it has the outputs.
    pub root: PathBuf,
    /// What the task used, when its container had a cgroup of its own.
    pub usage: Option<Usage>,
}

/// What a task used, as the cgroup of its container counted it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// The CPU time, in microseconds.
    pub cpu_usec: Option<u64>,
    /// The peak of memory, in bytes, page cache included.
    pub memory_peak: Option<u64>,
    /// The bytes read from and written to block devices.
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
    /// How many processes of the task the kernel killed for lack of memory.
    pub oom_kills: Option<u64>,
}

impl Usage {
    /// What the wrapper recorded in the control folder of a scratch directory, if anything.
    pub fn of(root: &std::path::Path) -> Option<Usage> {
        let text = std::fs::read_to_string(root.join(CONTROL).join(USAGE)).ok()?;
        let mut usage = Usage::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once(' ') else {
                continue;
            };
            let value = value.trim().parse().ok();
            match key {
                "cpu_usec" => usage.cpu_usec = value,
                "memory_peak" => usage.memory_peak = value,
                "read_bytes" => usage.read_bytes = value,
                "write_bytes" => usage.write_bytes = value,
                "oom_kills" => usage.oom_kills = value,
                _ => {}
            }
        }
        (usage != Usage::default()).then_some(usage)
    }
}

impl Finished {
    /// When the tool of the task started and ended, as the wrapper wrote it in the control
    /// folder: seconds since the Unix epoch, with a fraction where Bash has one. `None` when
    /// the tool did not get that far.
    pub fn tool_times(&self) -> Option<(SystemTime, SystemTime)> {
        let control = self.root.join(CONTROL);
        let read = |name: &str| -> Option<SystemTime> {
            let text = std::fs::read_to_string(control.join(name)).ok()?;
            // Bash writes the fraction with the decimal separator of the locale.
            let seconds: f64 = text.trim().replace(',', ".").parse().ok()?;
            let elapsed = Duration::try_from_secs_f64(seconds).ok()?;
            SystemTime::UNIX_EPOCH.checked_add(elapsed)
        };
        let (started, ended) = (read(START)?, read(END)?);
        (ended >= started).then_some((started, ended))
    }

    /// Where an output is on the host: a relative path in the work directory, an absolute one
    /// where the wrapper copied it.
    pub fn local_path(&self, path: &str) -> PathBuf {
        match path.starts_with('/') {
            true => self
                .root
                .join(CONTROL)
                .join(ROOT)
                .join(path.trim_start_matches('/')),
            false => self.root.join(WORK).join(path),
        }
    }

    /// What the wrapper left in the control folder of a scratch directory: the tool's exit
    /// code, and after a success the manifest of its outputs. For backends whose server gives
    /// the control folder back, like TES.
    pub fn from_control(root: PathBuf) -> Result<Finished, ExecutorError> {
        let control = root.join(CONTROL);
        let read = |name: &str| std::fs::read_to_string(control.join(name)).ok();
        let exit_code = read(EXIT).and_then(|text| text.trim().parse().ok());
        let files = match exit_code {
            Some(0) => parse(&read(MANIFEST).unwrap_or_default())?,
            _ => Vec::new(),
        };
        Ok(Finished {
            exit_code,
            files,
            usage: Usage::of(&root),
            root,
        })
    }

    /// The file that captured standard output.
    pub fn stdout(&self) -> PathBuf {
        self.root.join(CONTROL).join(STDOUT)
    }

    /// The file that captured standard error.
    pub fn stderr(&self) -> PathBuf {
        self.root.join(CONTROL).join(STDERR)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("the executor does not support this task: {0}")]
    Unsupported(String),
    #[error("the task has no container image; every task runs in a container")]
    NoImage,
    #[error("the task has no command, and the image {0} has no default command")]
    NoCommand(String),
    /// The registry does not have the image, or does not let this machine pull it: another
    /// attempt would not work either.
    #[error(
        "the image {image} cannot be pulled: {reason}; check its name and tag, or log in to its \
         registry"
    )]
    MissingImage { image: String, reason: String },
    /// The image has no `sh`, which runs the wrapper of every task.
    #[error(
        "the image {0} has no sh, which iunctus needs to run a task; use an image with a shell"
    )]
    NoShell(String),
    #[error("cannot prepare the task directory: {0}")]
    Io(#[from] std::io::Error),
    /// The container engine or the machine failed, not the tool; another attempt may work.
    #[error("{engine} could not run the task: {message}")]
    Engine { engine: String, message: String },
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("the task was cancelled")]
    Cancelled,
}

impl ExecutorError {
    /// Whether the machine failed rather than the task, so that another attempt may work.
    pub fn retryable(&self) -> bool {
        matches!(self, ExecutorError::Engine { .. })
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn reads_control_usage() {
        let root = tempfile::tempdir().expect("a temporary directory");
        std::fs::create_dir_all(root.path().join(CONTROL)).expect("makes the folder");
        assert_eq!(Usage::of(root.path()), None);
        let text = "memory_peak 2048\noom_kills 1\ncpu_usec 1500\nread_bytes 0\nwrite_bytes 4096\n";
        std::fs::write(root.path().join(CONTROL).join(USAGE), text).expect("writes");
        let usage = Usage::of(root.path()).expect("a usage");
        assert_eq!(usage.memory_peak, Some(2048));
        assert_eq!(usage.cpu_usec, Some(1500));
        assert_eq!(usage.write_bytes, Some(4096));
        assert_eq!(usage.oom_kills, Some(1));
    }

    #[test]
    fn reads_control_times() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let control = root.path().join(CONTROL);
        std::fs::create_dir_all(&control).expect("makes the folder");
        let finished = Finished {
            exit_code: Some(0),
            files: Vec::new(),
            root: root.path().to_owned(),
            usage: None,
        };
        assert_eq!(finished.tool_times(), None);
        std::fs::write(control.join(START), "1700000000.250000\n").expect("writes");
        std::fs::write(control.join(END), "1700000002,750000\n").expect("writes");
        let (started, ended) = finished.tool_times().expect("times");
        let seconds = |time: SystemTime| {
            let elapsed = time.duration_since(SystemTime::UNIX_EPOCH);
            elapsed.expect("after 1970").as_secs_f64()
        };
        assert_eq!(seconds(started), 1_700_000_000.25);
        assert_eq!(seconds(ended), 1_700_000_002.75);
        std::fs::write(control.join(END), "1699999999\n").expect("writes");
        assert_eq!(finished.tool_times(), None, "an end before the start");
    }
}
