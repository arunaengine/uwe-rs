//! The nodes of a run, after Argo's node status: their kind, phase and outputs, and the
//! artifacts that they pass on.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use argo_parser::model::RetryStrategy;
use jiff::Timestamp;
use workflow_graph::{Outcome, TaskId};

use crate::run::{Expanded, Processed};
use crate::volumes::Written;

/// The phase of a node, as Argo names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Pending,
    Running,
    Succeeded,
    Failed,
    Error,
    Skipped,
    Omitted,
}

impl Phase {
    pub fn fulfilled(self) -> bool {
        !matches!(self, Phase::Pending | Phase::Running)
    }

    pub fn failed_or_error(self) -> bool {
        matches!(self, Phase::Failed | Phase::Error)
    }

    pub fn name(self) -> &'static str {
        match self {
            Phase::Pending => "Pending",
            Phase::Running => "Running",
            Phase::Succeeded => "Succeeded",
            Phase::Failed => "Failed",
            Phase::Error => "Error",
            Phase::Skipped => "Skipped",
            Phase::Omitted => "Omitted",
        }
    }
}

/// The kind of a node, after Argo's node types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeKind {
    Pod,
    Steps,
    StepGroup,
    Dag,
    TaskGroup,
    Retry,
    Skipped,
    Suspend,
    Plugin,
}

/// Where an artifact comes from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Location {
    /// A local file, like the output of an earlier task.
    File(PathBuf),
    /// A URL like `s3://bucket/key`, `https://...` or a git repository.
    Url(String),
    /// Text that the workflow holds itself.
    Raw(String),
}

/// An artifact as a run passes it on: its name, the path a task sees it at, and its source.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ArtifactValue {
    pub name: String,
    pub path: String,
    pub location: Option<Location>,
    pub optional: bool,
    pub directory: bool,
}

/// What a node made: parameters, artifacts, the standard output and the exit code.
#[derive(Clone, Debug, Default)]
pub(crate) struct NodeOutputs {
    pub parameters: Vec<(String, String)>,
    pub artifacts: Vec<ArtifactValue>,
    pub result: Option<String>,
    pub exit_code: Option<String>,
}

/// A node: a template call, or a group of calls, and its state.
#[derive(Debug)]
pub(crate) struct NodeStatus {
    pub id: String,
    pub name: String,
    pub kind: NodeKind,
    pub phase: Phase,
    pub message: String,
    pub template: String,
    pub boundary: Option<usize>,
    pub children: Vec<usize>,
    pub outputs: NodeOutputs,
    pub daemoned: bool,
    /// The tasks this node waits for.
    pub inbound: Vec<TaskId>,
    pub tasks: Vec<TaskId>,
    pub outcomes: Vec<Option<Outcome>>,
    pub processed: Option<Rc<Processed>>,
    pub expanded: Option<Rc<Vec<Expanded>>>,
    pub exit_hook: Option<usize>,
    pub hooks: Vec<usize>,
    pub depth: usize,
    /// For a retry node: the call, to start more attempts.
    pub retry: Option<Rc<RetryCall>>,
    pub started: Timestamp,
    pub finished: Option<Timestamp>,
    pub started_at: std::time::Instant,
    pub finished_at: Option<std::time::Instant>,
    /// The mounts whose files go into their volumes when the node's tasks end.
    pub written: Vec<Written>,
}

/// The call that a retry node makes again for each attempt.
#[derive(Debug)]
pub(crate) struct RetryCall {
    pub processed: Rc<Processed>,
    pub strategy: RetryStrategy,
}
