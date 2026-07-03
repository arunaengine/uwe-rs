//! One attempt of a task as a backend runs it: the specification, the inputs to place, and what
//! the task left behind.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use task_model::{Placement, TaskSpec};

use crate::manifest::{Collected, ManifestError, parse};
use crate::wrapper::{EXIT, MANIFEST, ROOT, STDERR, STDOUT};

/// The folders of a task's scratch directory: the work directory, the control files of the
/// wrapper, and copies of inputs at absolute paths, which are mounted where the task sees them.
pub(crate) const WORK: &str = "work";
pub(crate) const CONTROL: &str = "control";
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
}

impl Finished {
    /// Where an output is on the host: a relative path in the work directory, an absolute one
    /// where the wrapper copied it.
    pub fn local_path(&self, path: &str) -> PathBuf {
        match path.strip_prefix('/') {
            Some(inside) => self.root.join(CONTROL).join(ROOT).join(inside),
            None => self.root.join(WORK).join(path),
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
    #[error("the task has no container image; every task runs in a container")]
    NoImage,
    #[error("the task has no command, and the image {0} has no default command")]
    NoCommand(String),
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
