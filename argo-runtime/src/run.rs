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

impl Run {
    pub fn new(
        document: Rc<Document>,
        library: Rc<Library>,
        options: Options,
    ) -> std::result::Result<Run, Failure> {
        let created = options.now.unwrap_or_else(Timestamp::now);
        let mut warnings = Vec::new();
        let Some(spec_node) = document.spec() else {
            return Err(Failure::error(format!("{} has no spec", document.label())));
        };
        let mut spec_node = spec_node.clone();
        let mut context_path = document.path.clone();
        let mut metadata = document.node.get("metadata").cloned();
        // A Workflow that runs a WorkflowTemplate joins its spec onto the template's.
        if let Some(reference) = spec_node.get("workflowTemplateRef") {
            let name = reference.get("name").and_then(Node::as_str).unwrap_or("");
            let cluster = matches!(
                reference.get("clusterScope").map(|node| &node.value),
                Some(Yaml::Bool(true))
            );
            let kind = if cluster {
                DocumentKind::ClusterWorkflowTemplate
            } else {
                DocumentKind::WorkflowTemplate
            };
            let Some(template) = library.find(kind, name, &document.path) else {
                return Err(Failure::error(format!(
                    "workflowTemplateRef: {} {name} not found",
                    kind.name()
                )));
            };
            let base = template.spec().cloned().unwrap_or_else(empty_mapping);
            remove_key(&mut spec_node, "workflowTemplateRef");
            spec_node = merge(&base, &spec_node);
            context_path = template.path.clone();
            if let Some(workflow_metadata) = base.get("workflowMetadata") {
                metadata = Some(match metadata {
                    Some(own) => merge(workflow_metadata, &own),
                    None => workflow_metadata.clone(),
                });
            }
        }
        let (mut spec, _) = decode::<WorkflowSpec>(&spec_node)
            .map_err(|error| Failure::error(format!("spec: {error}")))?;
        let workflow_name = workflow_name(&document, metadata.as_ref());
        let entrypoint = options
            .entrypoint
            .clone()
            .filter(|entrypoint| !entrypoint.is_empty())
            .unwrap_or_else(|| spec.entrypoint.clone());
        if entrypoint.is_empty() {
            return Err(Failure::error("spec.entrypoint is required"));
        }
        let templates = match spec_node.get("templates").map(|node| &node.value) {
            Some(Yaml::Sequence(templates)) => templates.clone(),
            _ => Vec::new(),
        };
        let root_context = Rc::new(Context {
            label: String::new(),
            path: context_path,
            templates,
        });
        // Parameters of the command line and the parameter file override the spec.
        let mut parameters = spec.arguments.parameters.clone();
        for (name, value) in &options.parameters {
            match parameters
                .iter_mut()
                .find(|parameter| &parameter.name == name)
            {
                Some(parameter) => {
                    parameter.value = Some(AnyString(value.clone()));
                    parameter.value_from = None;
                }
                None => parameters.push(Parameter {
                    name: name.clone(),
                    value: Some(AnyString(value.clone())),
                    ..Parameter::default()
                }),
            }
        }
        // The inputs of the entrypoint are workflow parameters too, supplied with `-p`.
        if let Some(entry) = root_context.template(&entrypoint)
            && let Ok((template, _)) = decode::<Template>(entry)
        {
            for input in template.inputs.parameters {
                let supplied = input.value.is_some()
                    || input.default.is_some()
                    || parameters
                        .iter()
                        .any(|parameter| parameter.name == input.name);
                if !supplied {
                    parameters.push(Parameter {
                        name: input.name.clone(),
                        enumeration: input.enumeration.clone(),
                        ..Parameter::default()
                    });
                }
            }
        }
        for parameter in &mut parameters {
            if parameter.value.is_some() {
                continue;
            }
            let from_config_map = parameter
                .value_from
                .as_ref()
                .is_some_and(|from| from.config_map_key_ref.is_some());
            let fallback = parameter
                .value_from
                .as_ref()
                .and_then(|from| from.default.clone());
            let value = if let Some(default) = parameter.default.clone().or(fallback.clone()) {
                if from_config_map {
                    warnings.push(format!(
                        "parameter {} reads a ConfigMap, which a run without Kubernetes cannot; using its default `{default}`",
                        parameter.name
                    ));
                }
                default
            } else if let Some(first) = parameter.enumeration.first() {
                warnings.push(format!(
                    "parameter {} has no value; using the first of its enum, `{first}`",
                    parameter.name
                ));
                first.clone()
            } else {
                let placeholder = AnyString(format!("{PLACEHOLDER}{}", parameter.name));
                warnings.push(format!(
                    "parameter {} has no value; using `{placeholder}`",
                    parameter.name
                ));
                placeholder
            };
            parameter.value = Some(value);
            parameter.value_from = None;
        }
        spec.arguments.parameters = parameters;
        let mut run = Run {
            workflow_name,
            options,
            library,
            document: Rc::clone(&document),
            spec,
            defaults: spec_node.get("templateDefaults").cloned(),
            root_context,
            contexts: HashMap::new(),
            globals: BTreeMap::new(),
            global_artifacts: BTreeMap::new(),
            entrypoint,
            arguments: Args::default(),
            nodes: Vec::new(),
            names: HashMap::new(),
            tasks: HashMap::new(),
            next_task: 1,
            emitted: Vec::new(),
            changed: false,
            warnings: Vec::new(),
            store: HashMap::new(),
            first_failure: None,
            root: None,
            exit_node: None,
            hook_nodes: Vec::new(),
            created,
        };
        for warning in warnings {
            run.warn(warning);
        }
        run.set_globals(metadata.as_ref());
        run.warn_ignored(&spec_node);
        let mut artifacts = Vec::new();
        for artifact in run.spec.arguments.artifacts.clone() {
            artifacts.push(run.artifact_value(&artifact)?);
        }
        run.arguments = Args {
            parameters: run.spec.arguments.parameters.clone(),
            artifacts,
        };
        // Globals reach the spec outside the templates, like the exit handler's arguments.
        let variables = Variables::new(run.globals.clone());
        let mut hooks = run.spec.hooks.clone();
        for hook in hooks.values_mut() {
            for parameter in &mut hook.arguments.parameters {
                if let Some(value) = &mut parameter.value {
                    value.0 =
                        crate::tags::replace(&value.0, &variables, true).map_err(Failure::error)?;
                }
            }
        }
        run.spec.hooks = hooks;
        Ok(run)
    }

    fn set_globals(&mut self, metadata: Option<&Node>) {
        let created = self.created;
        let namespace = metadata
            .and_then(|metadata| metadata.get("namespace"))
            .and_then(Node::as_str)
            .filter(|namespace| !namespace.is_empty())
            .unwrap_or("default")
            .to_owned();
        let mut globals = BTreeMap::new();
        globals.insert("workflow.name".into(), self.workflow_name.clone());
        globals.insert("workflow.namespace".into(), namespace);
        globals.insert("workflow.mainEntrypoint".into(), self.entrypoint.clone());
        globals.insert(
            "workflow.serviceAccountName".into(),
            self.spec.service_account_name.clone(),
        );
        globals.insert("workflow.uid".into(), uid(&self.workflow_name));
        globals.insert("workflow.creationTimestamp".into(), time::rfc3339(created));
        globals.insert(
            "workflow.creationTimestamp.RFC3339".into(),
            time::rfc3339(created),
        );
        globals.insert(
            "workflow.creationTimestamp.s".into(),
            created.as_second().to_string(),
        );
        for (letter, value) in time::strftime(created) {
            globals.insert(format!("workflow.creationTimestamp.{letter}"), value);
        }
        globals.insert("workflow.duration".into(), "0".into());
        if let Some(priority) = self.spec.priority {
            globals.insert("workflow.priority".into(), priority.to_string());
        }
        if self.document.kind == DocumentKind::CronWorkflow {
            globals.insert("workflow.scheduledTime".into(), time::rfc3339(created));
        }
        let parameters = parameters_json(&self.spec.arguments.parameters);
        globals.insert("workflow.parameters".into(), parameters.clone());
        globals.insert("workflow.parameters.json".into(), parameters);
        for parameter in &self.spec.arguments.parameters {
            let value = parameter.value.clone().unwrap_or_default().0;
            globals.insert(format!("workflow.parameters.{}", parameter.name), value);
        }
        for (field, key) in [
            ("labels", "workflow.labels"),
            ("annotations", "workflow.annotations"),
        ] {
            let mut map = BTreeMap::new();
            if let Some(Node {
                value: Yaml::Mapping(entries),
                ..
            }) = metadata.and_then(|metadata| metadata.get(field))
            {
                for entry in entries {
                    let value = scalar_text(&entry.value);
                    globals.insert(format!("{key}.{}", entry.key), value.clone());
                    map.insert(entry.key.clone(), Value::String(value));
                }
            }
            globals.insert(key.to_owned(), Value::Map(map).to_json());
        }
        // Parameter values may use other globals, like `{{workflow.name}}`.
        let variables = Variables::new(globals.clone());
        for (name, value) in globals.iter_mut() {
            if name.starts_with("workflow.parameters.")
                && value.contains("{{")
                && let Ok(resolved) = crate::tags::replace(value, &variables, true)
            {
                *value = resolved;
            }
        }
        for parameter in &mut self.spec.arguments.parameters {
            if let Some(resolved) = globals.get(&format!("workflow.parameters.{}", parameter.name))
            {
                parameter.value = Some(AnyString(resolved.clone()));
            }
        }
        self.globals = globals;
    }

    /// Warns once about fields that change what a workflow does on Kubernetes, which a run
    /// without Kubernetes leaves out.
    fn warn_ignored(&mut self, spec: &Node) {
        for (field, what) in [
            (
                "volumes",
                "workflow volumes are not mounted; tasks share files only as artifacts",
            ),
            (
                "volumeClaimTemplates",
                "volume claims are not created; tasks share files only as artifacts",
            ),
            (
                "synchronization",
                "synchronization is left out: one run holds no locks",
            ),
            (
                "suspend",
                "the workflow starts at once instead of suspended",
            ),
        ] {
            if spec.get(field).is_some_and(|node| node.value != Yaml::Null) {
                self.warn(what.to_owned());
            }
        }
    }

    pub fn warn(&mut self, message: String) {
        if !self.warnings.contains(&message) {
            if self.options.print_warnings {
                eprintln!("WARN: {message}");
            }
            self.warnings.push(message);
        }
    }

    /// Runs passes until one changes nothing, then hands out the tasks asked for.
    pub fn operate(&mut self) -> Result<Vec<Task>> {
        loop {
            self.changed = false;
            self.pass()?;
            if !self.changed {
                break;
            }
        }
        Ok(std::mem::take(&mut self.emitted))
    }

    fn pass(&mut self) -> Result<()> {
        let root = match self.root {
            Some(root) => {
                if !self.nodes[root].phase.fulfilled() {
                    self.reenter(root)?;
                }
                root
            }
            None => {
                let name = self.workflow_name.clone();
                let holder = Holder::named(&self.entrypoint.clone());
                let context = Rc::clone(&self.root_context);
                let args = self.arguments.clone();
                let root =
                    self.execute_template(&name, &holder, &context, &args, &Call::default())?;
                self.root = Some(root);
                root
            }
        };
        self.workflow_hooks(false)?;
        if !self.nodes[root].phase.fulfilled() {
            return Ok(());
        }
        self.workflow_hooks(true)?;
        self.exit_handler(root)
    }

    /// Runs the workflow's own hooks whose expression holds, like `workflow.status ==
    /// "Running"`. The exit hook is the exit handler.
    fn workflow_hooks(&mut self, done: bool) -> Result<()> {
        let hooks: Vec<(String, LifecycleHook)> = self
            .spec
            .hooks
            .iter()
            .filter(|(name, _)| name.as_str() != "exit")
            .map(|(name, hook)| (name.clone(), hook.clone()))
            .collect();
        for (name, hook) in hooks {
            let node_name = format!("{}.hooks.{name}", self.workflow_name);
            if let Some(&index) = self.names.get(&node_name) {
                if !self.nodes[index].phase.fulfilled() {
                    self.reenter(index)?;
                }
                continue;
            }
            let mut globals = self.globals.clone();
            let status = match (done, self.root) {
                (true, Some(root)) => self.nodes[root].phase.name(),
                _ => "Running",
            };
            globals.insert("workflow.status".into(), status.into());
            let variables = Variables::new(globals);
            let holds = if hook.expression.is_empty() {
                false
            } else {
                crate::expr::evaluate_bool(&hook.expression, variables.environment())
                    .unwrap_or(false)
            };
            if !holds {
                continue;
            }
            let args = self.hook_args(&hook, &Scope::default())?;
            let context = Rc::clone(&self.root_context);
            let call = Call {
                inbound: Vec::new(),
                ..Call::default()
            };
            let index =
                self.execute_template(&node_name, &Holder::of_hook(&hook), &context, &args, &call)?;
            self.hook_nodes.push(index);
        }
        Ok(())
    }

    /// The workflow's exit handler, once the entrypoint is done: `spec.onExit` or the hook
    /// `exit`. It sees `workflow.status` and `workflow.failures`.
    fn exit_handler(&mut self, root: usize) -> Result<()> {
        if let Some(index) = self.exit_node {
            if !self.nodes[index].phase.fulfilled() {
                self.reenter(index)?;
            }
            return Ok(());
        }
        let hook = match self.spec.hooks.get("exit") {
            Some(hook) => hook.clone(),
            None if !self.spec.on_exit.is_empty() => LifecycleHook {
                template: self.spec.on_exit.clone(),
                ..LifecycleHook::default()
            },
            None => return Ok(()),
        };
        let phase = self.nodes[root].phase;
        self.globals
            .insert("workflow.status".into(), phase.name().into());
        let failures = self.failures_json();
        self.globals.insert("workflow.failures".into(), failures);
        let args = self.hook_args(&hook, &Scope::default())?;
        let call = Call {
            inbound: self.outbound(root),
            ..Call::default()
        };
        let context = Rc::clone(&self.root_context);
        let name = format!("{}.onExit", self.workflow_name);
        let index =
            self.execute_template(&name, &Holder::of_hook(&hook), &context, &args, &call)?;
        self.exit_node = Some(index);
        Ok(())
    }

    /// `workflow.failures`: the nodes that failed, as JSON.
    fn failures_json(&self) -> String {
        let failures: Vec<Value> = self
            .nodes
            .iter()
            .filter(|node| node.phase.failed_or_error() && node.kind == NodeKind::Pod)
            .map(|node| {
                let mut map = BTreeMap::new();
                map.insert(
                    "displayName".into(),
                    Value::string(display_name(&node.name)),
                );
                map.insert("message".into(), Value::string(node.message.clone()));
                map.insert("templateName".into(), Value::string(node.template.clone()));
                map.insert("phase".into(), Value::string(node.phase.name()));
                map.insert(
                    "podName".into(),
                    Value::string(self.pod_name(&node.name, &node.template)),
                );
                map.insert(
                    "finishedAt".into(),
                    Value::string(time::rfc3339(node.finished.unwrap_or(node.started))),
                );
                Value::Map(map)
            })
            .collect();
        Value::Array(failures).to_json()
    }
}
