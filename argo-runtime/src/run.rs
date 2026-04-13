//! The state of a run, after Argo's workflow controller (workflow/controller/operator.go): a
//! tree of nodes that each pass walks from the root. A pass starts the nodes whose turn has
//! come; leaf templates become tasks, and a run goes on when their outcomes arrive.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

use argo_parser::model::{
    AnyString, Artifact, LifecycleHook, Parameter, RetryStrategy, Template, TemplateRef,
    WorkflowSpec,
};
use argo_parser::{Entry, Node, Position, Value as Yaml, decode};
use jiff::Timestamp;
use workflow_graph::{Outcome, Task, TaskId};

use crate::library::{Document, Kind as DocumentKind, Library};
use crate::merge::{merge, with_defaults};
use crate::scope::Scope;
use crate::tags::{Variables, replace_node};
use crate::value::Value;
use crate::{Options, time};

/// Templates may call templates this many levels deep; deeper recursion is taken as endless.
pub(crate) const MAX_DEPTH: usize = 100;

/// A run asks for at most this many tasks, which stops endless loops of other kinds.
pub(crate) const MAX_TASKS: u64 = 100_000;

/// The start of the value of a workflow parameter that has none.
pub(crate) const PLACEHOLDER: &str = "placeholder-";

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

/// A template after its arguments are applied and its tags substituted.
#[derive(Debug)]
pub(crate) struct Processed {
    pub node: Node,
    pub template: Template,
    pub context: Rc<Context>,
    pub artifacts: Vec<ArtifactValue>,
}

/// Where template names resolve: the workflow, or a WorkflowTemplate that a `templateRef`
/// reached.
#[derive(Debug)]
pub(crate) struct Context {
    pub label: String,
    pub path: PathBuf,
    pub templates: Vec<Node>,
}

impl Context {
    fn template(&self, name: &str) -> Option<&Node> {
        self.templates
            .iter()
            .find(|template| template.get("name").and_then(Node::as_str) == Some(name))
    }
}

/// What names a template: a step, a task, a hook or the entrypoint.
#[derive(Clone, Debug, Default)]
pub(crate) struct Holder {
    pub template: String,
    pub template_ref: Option<TemplateRef>,
    pub inline: Option<Node>,
}

impl Holder {
    pub fn named(template: &str) -> Self {
        Holder {
            template: template.to_owned(),
            ..Holder::default()
        }
    }

    pub fn of_hook(hook: &LifecycleHook) -> Self {
        Holder {
            template: hook.template.clone(),
            template_ref: hook.template_ref.clone(),
            inline: None,
        }
    }
}

/// The arguments of a template call.
#[derive(Clone, Debug, Default)]
pub(crate) struct Args {
    pub parameters: Vec<Parameter>,
    pub artifacts: Vec<ArtifactValue>,
}

/// One entry of an expanded step group or DAG task: a step or task after substitution, with
/// its node name and resolved artifacts.
#[derive(Debug)]
pub(crate) struct Expanded {
    pub name: String,
    pub holder: Holder,
    pub when: String,
    pub args: Args,
    pub continue_on: (bool, bool),
    pub hooks: BTreeMap<String, LifecycleHook>,
    pub on_exit: String,
    /// The error of the step or task, which makes its node an error.
    pub error: Option<String>,
}

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
}

#[derive(Debug)]
pub(crate) struct RetryCall {
    pub processed: Rc<Processed>,
    pub strategy: RetryStrategy,
}

/// Why a run cannot go on. Unsupported features stop the run; other failures become nodes
/// in the Error phase, as in Argo.
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub message: String,
    pub unsupported: bool,
}

impl Failure {
    pub fn unsupported(message: impl Into<String>) -> Self {
        Failure {
            message: message.into(),
            unsupported: true,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Failure {
            message: message.into(),
            unsupported: false,
        }
    }
}

pub(crate) type Result<T> = std::result::Result<T, Failure>;

/// How a template is called.
#[derive(Clone, Debug, Default)]
pub(crate) struct Call {
    pub boundary: Option<usize>,
    pub inbound: Vec<TaskId>,
    pub depth: usize,
}

pub(crate) struct Run {
    pub workflow_name: String,
    pub options: Options,
    pub library: Rc<Library>,
    pub document: Rc<Document>,
    pub spec: WorkflowSpec,
    pub defaults: Option<Node>,
    pub root_context: Rc<Context>,
    contexts: HashMap<(PathBuf, usize), Rc<Context>>,
    pub globals: BTreeMap<String, String>,
    pub global_artifacts: BTreeMap<String, ArtifactValue>,
    pub entrypoint: String,
    pub arguments: Args,
    pub nodes: Vec<NodeStatus>,
    pub names: HashMap<String, usize>,
    pub tasks: HashMap<TaskId, (usize, usize)>,
    pub next_task: u64,
    pub emitted: Vec<Task>,
    pub changed: bool,
    pub warnings: Vec<String>,
    /// Artifact URLs that tasks wrote, and the local files that hold them.
    pub store: HashMap<String, PathBuf>,
    pub first_failure: Option<String>,
    pub root: Option<usize>,
    pub exit_node: Option<usize>,
    pub hook_nodes: Vec<usize>,
    pub created: Timestamp,
}
