//! The state of a run, after Argo's workflow controller (workflow/controller/operator.go): a
//! tree of nodes that each pass walks from the root. A pass starts the nodes whose turn has
//! come; leaf templates become tasks, and a run goes on when their outcomes arrive.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

use argo_parser::model::{
    AnyString, Artifact, LifecycleHook, Parameter, Template, TemplateRef, WorkflowSpec,
};
use argo_parser::{Node, Position, Value as Yaml, decode};
use jiff::Timestamp;
use workflow_graph::{Outcome, Task, TaskId};

use crate::Options;
use crate::checks::cluster_only;
use crate::library::{Document, Kind as DocumentKind, Library};
use crate::merge::with_defaults;
use crate::node::{ArtifactValue, Location, NodeKind, NodeOutputs, NodeStatus, Phase, RetryCall};
use crate::start::{Given, Joined, joined_spec, workflow_name, workflow_parameters};
use crate::tags::Variables;
use crate::validate::{Base, Check, Supplied};
use crate::value::Value;

/// Templates may call templates this many levels deep; deeper recursion is taken as endless.
pub(crate) const MAX_DEPTH: usize = 100;

/// A run asks for at most this many tasks, which stops endless loops of other kinds.
pub(crate) const MAX_TASKS: u64 = 100_000;

/// The start of the value of a workflow parameter that has none.
pub(crate) const PLACEHOLDER: &str = "placeholder_";

/// The value of a parameter that has none: one word, like `placeholder_run_data`, so that a
/// `when` condition reads it as a string.
pub(crate) fn placeholder(name: &str) -> AnyString {
    let word: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    AnyString(format!("{PLACEHOLDER}{word}"))
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
        let Joined {
            spec: spec_node,
            path: context_path,
            metadata,
        } = joined_spec(&document, &library)?;
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
        let entry = root_context.template(&entrypoint);
        let Given {
            parameters,
            entry_artifacts,
        } = workflow_parameters(
            &spec,
            entry,
            &options,
            (&library, &document.path),
            &mut warnings,
        )?;
        spec.arguments.parameters = parameters;
        // The workflow is checked as Argo checks it when it is submitted, with the parameters
        // and artifacts it got.
        let mut supplied = spec.arguments.clone();
        for name in &entry_artifacts {
            supplied.artifacts.push(Artifact {
                name: name.clone(),
                ..Artifact::default()
            });
        }
        let base = Rc::new(Base::new(
            String::new(),
            root_context.path.clone(),
            &spec_node,
            document.kind,
        ));
        let mut check = Check::new(&library);
        let entry = Some((entrypoint.as_str(), Supplied::Arguments(&supplied)));
        if let Err(message) = check.spec(&spec, &base, entry, false) {
            return Err(if check.unsupported() {
                Failure::unsupported(message)
            } else {
                Failure::error(message)
            });
        }
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
        for name in entry_artifacts {
            let url = format!("placeholder://{name}");
            run.warn(format!(
                "input artifact {name} of the entrypoint is not supplied; using `{url}`"
            ));
            artifacts.push(ArtifactValue {
                name,
                path: String::new(),
                location: Some(Location::Url(url)),
                optional: false,
                directory: false,
            });
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

    /// Calls a template: the node exists already and goes on, or it is made now.
    pub fn execute_template(
        &mut self,
        name: &str,
        holder: &Holder,
        context: &Rc<Context>,
        args: &Args,
        call: &Call,
    ) -> Result<usize> {
        if let Some(&index) = self.names.get(name) {
            if !self.nodes[index].phase.fulfilled() {
                self.reenter(index)?;
            }
            return Ok(index);
        }
        if call.depth > MAX_DEPTH {
            let template = holder
                .template_ref
                .as_ref()
                .map_or(holder.template.as_str(), |reference| {
                    reference.template.as_str()
                });
            return Err(Failure::error(format!(
                "templates call each other more than {MAX_DEPTH} levels deep, at template {template} of step {}; the recursion does not end",
                display_name(name)
            )));
        }
        let (context, template) = match self.resolve(holder, context) {
            Ok(found) => found,
            Err(message) => return Ok(self.error_node(name, "", call, message)),
        };
        let template = match &self.defaults {
            Some(defaults) => with_defaults(&template, defaults),
            None => template,
        };
        let template_name = template
            .get("name")
            .and_then(Node::as_str)
            .unwrap_or("")
            .to_owned();
        let processed = match self.process_args(name, &template, &context, args, None) {
            Ok(processed) => Rc::new(processed),
            Err(failure) if failure.unsupported => return Err(failure),
            Err(failure) => {
                return Ok(self.error_node(name, &template_name, call, failure.message));
            }
        };
        let strategy = processed
            .template
            .retry_strategy
            .clone()
            .or_else(|| self.spec.retry_strategy.clone());
        if let Some(strategy) = strategy {
            let index = self.new_node(name, NodeKind::Retry, &template_name, call);
            self.nodes[index].retry = Some(Rc::new(RetryCall {
                processed,
                strategy,
            }));
            self.continue_retry(index)?;
            return Ok(index);
        }
        let index = self.new_node(name, NodeKind::Pod, &template_name, call);
        self.start_node(index, processed)?;
        Ok(index)
    }

    /// Starts a node for its processed template.
    pub(crate) fn start_node(&mut self, index: usize, processed: Rc<Processed>) -> Result<()> {
        let template = &processed.template;
        // Templates that a tag chose come here without the checks at the start.
        if let Some(reason) = cluster_only(template) {
            return Err(Failure::unsupported(reason));
        }
        let kind = if !template.steps.is_empty() || processed.node.get("steps").is_some() {
            NodeKind::Steps
        } else if template.dag.is_some() {
            NodeKind::Dag
        } else if template.suspend.is_some() {
            NodeKind::Suspend
        } else if template.container.is_some()
            || template.script.is_some()
            || template.container_set.is_some()
            || template.resource.is_some()
            || template.http.is_some()
            || template.data.is_some()
        {
            NodeKind::Pod
        } else if template.plugin.is_some() {
            NodeKind::Plugin
        } else {
            let message = format!(
                "template {} has no type: it needs container, script, steps, dag or another",
                self.nodes[index].template
            );
            self.fail(index, Phase::Error, message);
            return Ok(());
        };
        self.nodes[index].kind = kind;
        self.nodes[index].processed = Some(Rc::clone(&processed));
        self.nodes[index].daemoned = template.daemon == Some(true);
        match kind {
            NodeKind::Steps => self.execute_steps(index),
            NodeKind::Dag => self.execute_dag(index),
            NodeKind::Suspend => {
                self.suspend(index);
                Ok(())
            }
            NodeKind::Plugin => self.start_plugin(index),
            _ => self.start_pod(index),
        }
    }

    /// Goes on with a node that runs.
    pub fn reenter(&mut self, index: usize) -> Result<()> {
        match self.nodes[index].kind {
            NodeKind::Steps => self.execute_steps(index),
            NodeKind::Dag => self.execute_dag(index),
            NodeKind::Retry => self.continue_retry(index),
            NodeKind::TaskGroup => {
                for child in self.nodes[index].children.clone() {
                    if !self.nodes[child].phase.fulfilled() {
                        self.reenter(child)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Ends a loop over no items: skipped, with empty lists as its result and output
    /// parameters, which is what later steps that read the loop's outputs get.
    pub fn empty_loop(
        &mut self,
        name: &str,
        holder: &Holder,
        context: &Rc<Context>,
        call: &Call,
    ) -> usize {
        let index = self.end_node(
            name,
            Phase::Skipped,
            call,
            "Skipped, empty params".to_owned(),
        );
        let mut outputs = NodeOutputs {
            result: Some("[]".to_owned()),
            ..NodeOutputs::default()
        };
        if let Ok((_, template)) = self.resolve(holder, context)
            && let Ok((typed, _)) = decode::<Template>(&template)
        {
            for parameter in typed.outputs.parameters {
                outputs.parameters.push((parameter.name, "[]".to_owned()));
            }
        }
        self.nodes[index].outputs = outputs;
        index
    }

    /// Gives a skipped or omitted node the outputs that Argo's `addSkippedNodeOutputsToScope`
    /// gives it: the declared `valueFrom.default` of its output parameters, and for steps empty
    /// placeholders for its output artifacts. Outputs without a default stay absent.
    pub fn skipped_outputs(
        &mut self,
        index: usize,
        holder: &Holder,
        context: &Rc<Context>,
        artifacts: bool,
    ) {
        if !matches!(self.nodes[index].phase, Phase::Skipped | Phase::Omitted) {
            return;
        }
        let Ok((_, template)) = self.resolve(holder, context) else {
            return;
        };
        let Ok((typed, _)) = decode::<Template>(&template) else {
            return;
        };
        let mut outputs = NodeOutputs::default();
        for parameter in &typed.outputs.parameters {
            if let Some(default) = parameter
                .value_from
                .as_ref()
                .and_then(|from| from.default.as_ref())
            {
                outputs
                    .parameters
                    .push((parameter.name.clone(), default.0.clone()));
            }
        }
        if artifacts {
            for artifact in &typed.outputs.artifacts {
                outputs.artifacts.push(ArtifactValue {
                    name: artifact.name.clone(),
                    path: artifact.path.clone(),
                    location: None,
                    optional: true,
                    directory: false,
                });
            }
        }
        self.nodes[index].outputs = outputs;
    }

    fn suspend(&mut self, index: usize) {
        let Some(processed) = self.nodes[index].processed.clone() else {
            return;
        };
        let mut parameters = Vec::new();
        for parameter in &processed.template.outputs.parameters {
            let supplied = parameter.value_from.as_ref();
            let value = parameter
                .value
                .clone()
                .or_else(|| supplied.and_then(|from| from.default.clone()))
                .or_else(|| parameter.default.clone())
                .or_else(|| parameter.enumeration.first().cloned());
            let value = match value {
                Some(value) => value.0,
                None => {
                    self.warn(format!(
                        "suspend template {}: output parameter {} is supplied on resume; it is empty",
                        processed.template.name, parameter.name
                    ));
                    String::new()
                }
            };
            parameters.push((parameter.name.clone(), value));
        }
        if processed
            .template
            .suspend
            .as_ref()
            .is_some_and(|suspend| suspend.duration.is_none())
        {
            self.warn(format!(
                "suspend template {} resumes at once, as if `argo resume` came",
                processed.template.name
            ));
        }
        self.nodes[index].outputs.parameters = parameters;
        self.set_phase(index, Phase::Succeeded, String::new());
    }

    /// Finds the template a holder names, and where its own references resolve.
    fn resolve(
        &mut self,
        holder: &Holder,
        context: &Rc<Context>,
    ) -> std::result::Result<(Rc<Context>, Node), String> {
        if let Some(inline) = &holder.inline {
            return Ok((Rc::clone(context), inline.clone()));
        }
        if let Some(reference) = &holder.template_ref {
            let kind = if reference.cluster_scope {
                DocumentKind::ClusterWorkflowTemplate
            } else {
                DocumentKind::WorkflowTemplate
            };
            let Some(document) = self.library.find_template(
                kind,
                &reference.name,
                &reference.template,
                &context.path,
            ) else {
                return Err(format!(
                    "templateRef: {} {} not found",
                    kind.name(),
                    reference.name
                ));
            };
            let context = self.context_of(&document);
            let Some(template) = context.template(&reference.template).cloned() else {
                return Err(format!(
                    "templateRef: template {} not found in {}",
                    reference.template,
                    document.label()
                ));
            };
            return Ok((context, template));
        }
        match context.template(&holder.template) {
            Some(template) => Ok((Rc::clone(context), template.clone())),
            None if context.label.is_empty() => {
                Err(format!("template {} not found", holder.template))
            }
            None => Err(format!(
                "template {} not found in {}",
                holder.template, context.label
            )),
        }
    }

    fn context_of(&mut self, document: &Rc<Document>) -> Rc<Context> {
        let key = (document.path.clone(), document.index);
        if let Some(context) = self.contexts.get(&key) {
            return Rc::clone(context);
        }
        let templates = match document
            .spec()
            .and_then(|spec| spec.get("templates"))
            .map(|node| &node.value)
        {
            Some(Yaml::Sequence(templates)) => templates.clone(),
            _ => Vec::new(),
        };
        let context = Rc::new(Context {
            label: document.label(),
            path: document.path.clone(),
            templates,
        });
        self.contexts.insert(key, Rc::clone(&context));
        context
    }

    /// The id of a new task. A run asks for at most [`MAX_TASKS`] tasks.
    pub fn next_task_id(&mut self) -> Result<TaskId> {
        if self.next_task > MAX_TASKS {
            return Err(Failure::error(format!(
                "the run asks for more than {MAX_TASKS} tasks; does a loop not end?"
            )));
        }
        let id = TaskId(self.next_task);
        self.next_task += 1;
        Ok(id)
    }

    /// Asks for a task of a node; its outcome comes back to the node.
    pub fn ask(&mut self, index: usize, task: Task) {
        let slot = self.nodes[index].outcomes.len();
        self.tasks.insert(task.id, (index, slot));
        self.nodes[index].tasks.push(task.id);
        self.nodes[index].outcomes.push(None);
        self.emitted.push(task);
        self.changed = true;
    }

    pub fn new_node(&mut self, name: &str, kind: NodeKind, template: &str, call: &Call) -> usize {
        let index = self.nodes.len();
        self.nodes.push(NodeStatus {
            id: self.node_id(name),
            name: name.to_owned(),
            kind,
            phase: Phase::Running,
            message: String::new(),
            template: template.to_owned(),
            boundary: call.boundary,
            children: Vec::new(),
            outputs: NodeOutputs::default(),
            daemoned: false,
            inbound: call.inbound.clone(),
            tasks: Vec::new(),
            outcomes: Vec::new(),
            processed: None,
            expanded: None,
            exit_hook: None,
            hooks: Vec::new(),
            depth: call.depth,
            retry: None,
            started: self.created,
            finished: None,
        });
        self.names.insert(name.to_owned(), index);
        self.changed = true;
        index
    }

    /// A node that ended before it started: skipped, omitted, or an error.
    pub fn end_node(&mut self, name: &str, phase: Phase, call: &Call, message: String) -> usize {
        if let Some(&index) = self.names.get(name) {
            return index;
        }
        let index = self.new_node(name, NodeKind::Skipped, "", call);
        if phase.failed_or_error() {
            self.fail(index, phase, message);
        } else {
            self.set_phase(index, phase, message);
        }
        index
    }

    fn error_node(&mut self, name: &str, template: &str, call: &Call, message: String) -> usize {
        let index = self.end_node(name, Phase::Error, call, message);
        self.nodes[index].template = template.to_owned();
        index
    }

    pub fn set_phase(&mut self, index: usize, phase: Phase, message: String) {
        let node = &mut self.nodes[index];
        if node.phase != phase {
            node.phase = phase;
            node.message = message;
            if phase.fulfilled() {
                node.finished = Some(self.created);
            }
            self.changed = true;
        }
    }

    /// Ends a node as failed or errored, and remembers the first such message of the run.
    pub fn fail(&mut self, index: usize, phase: Phase, message: String) {
        if self.first_failure.is_none() && !message.is_empty() {
            self.first_failure = Some(format!(
                "{}: {message}",
                display_name(&self.nodes[index].name)
            ));
        }
        self.set_phase(index, phase, message);
    }

    fn node_id(&self, name: &str) -> String {
        if name == self.workflow_name {
            name.to_owned()
        } else {
            format!("{}-{}", self.workflow_name, fnv32a(name))
        }
    }

    /// Argo's pod name, version 2: the workflow, the template and a hash of the node name.
    pub fn pod_name(&self, node: &str, template: &str) -> String {
        if node == self.workflow_name {
            return node.to_owned();
        }
        let mut prefix = if node.contains(".inline") || template.is_empty() {
            self.workflow_name.clone()
        } else {
            format!("{}-{template}", self.workflow_name)
        };
        if prefix.len() > 242 {
            prefix.truncate(242);
        }
        format!("{prefix}-{}", fnv32a(node))
    }

    /// The tasks at the end of a node, which the next nodes wait for. A node without tasks
    /// passes on the tasks it waited for itself.
    pub fn outbound(&self, index: usize) -> Vec<TaskId> {
        let node = &self.nodes[index];
        let mut tasks = match node.kind {
            NodeKind::Pod => node.tasks.clone(),
            // The calls of plugins follow each other; the last one gave the reply.
            NodeKind::Plugin => node.tasks.last().copied().into_iter().collect(),
            NodeKind::Retry => node
                .children
                .last()
                .map(|&child| self.outbound(child))
                .unwrap_or_default(),
            NodeKind::Steps => node
                .children
                .iter()
                .rev()
                .map(|&child| self.outbound(child))
                .find(|tasks| !tasks.is_empty())
                .unwrap_or_default(),
            NodeKind::StepGroup | NodeKind::TaskGroup => {
                let mut tasks = Vec::new();
                for &child in &node.children {
                    for task in self.outbound(child) {
                        if !tasks.contains(&task) {
                            tasks.push(task);
                        }
                    }
                }
                tasks
            }
            NodeKind::Dag => self.dag_outbound(index),
            NodeKind::Skipped | NodeKind::Suspend => Vec::new(),
        };
        if tasks.is_empty() {
            tasks = node.inbound.clone();
        }
        // The exit hook runs after the node, so it ends the node.
        match node.exit_hook {
            Some(exit) => self.outbound(exit),
            None => tasks,
        }
    }

    /// Takes the outcome of a task.
    pub fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>> {
        let Some((index, slot)) = self.tasks.remove(&outcome.id) else {
            return Err(Failure::error(format!(
                "the outcome of task {} arrived, which this run did not ask for",
                outcome.id
            )));
        };
        let node = &mut self.nodes[index];
        if let Some(entry) = node.outcomes.get_mut(slot) {
            *entry = Some(outcome);
        }
        if node.outcomes.iter().all(Option::is_some) {
            if node.kind == NodeKind::Plugin {
                self.finish_plugin(index)?;
            } else {
                self.finish_pod(index)?;
            }
        }
        self.operate()
    }

    /// The phase of the whole run and its message, once nothing runs.
    pub fn result(&self) -> std::result::Result<(), String> {
        let Some(root) = self.root else {
            return Err("the workflow did not start".into());
        };
        let root = &self.nodes[root];
        if !root.phase.fulfilled() {
            let waiting = self.tasks.len();
            return Err(format!(
                "the workflow did not finish: {} is {} with {waiting} tasks outstanding",
                root.name,
                root.phase.name()
            ));
        }
        let exit = self.exit_node.map(|exit| &self.nodes[exit]);
        let failed = match root.phase {
            Phase::Failed | Phase::Error => Some(root.phase),
            _ => exit
                .filter(|exit| exit.phase.failed_or_error())
                .map(|exit| exit.phase),
        };
        match failed {
            None => Ok(()),
            Some(phase) => {
                let message = self
                    .first_failure
                    .clone()
                    .unwrap_or_else(|| root.message.clone());
                Err(format!(
                    "workflow {}: {message}",
                    phase.name().to_lowercase()
                ))
            }
        }
    }
}

pub(crate) fn is_pod(template: &Template) -> bool {
    template.container.is_some()
        || template.script.is_some()
        || template.container_set.is_some()
        || template.resource.is_some()
        || template.http.is_some()
        || template.data.is_some()
}

/// The name of a node without the names of its parents, as Argo shows it.
pub(crate) fn display_name(name: &str) -> String {
    let mut depth = 0;
    for (index, character) in name.char_indices().rev() {
        match character {
            ')' => depth += 1,
            '(' => depth -= 1,
            '.' if depth == 0 => return name[index + 1..].to_owned(),
            _ => {}
        }
    }
    name.to_owned()
}

pub(crate) fn entry_mut<'a>(node: &'a mut Node, key: &str) -> Option<&'a mut Node> {
    match &mut node.value {
        Yaml::Mapping(entries) => entries
            .iter_mut()
            .find(|entry| entry.key == key)
            .map(|entry| &mut entry.value),
        _ => None,
    }
}

pub(crate) fn remove_key(node: &mut Node, key: &str) {
    if let Yaml::Mapping(entries) = &mut node.value {
        entries.retain(|entry| entry.key != key);
    }
}

pub(crate) fn empty_mapping() -> Node {
    Node::new(Yaml::Mapping(Vec::new()), Position::default())
}

/// A scalar as the text Argo sees after turning YAML into JSON.
pub(crate) fn scalar_text(node: &Node) -> String {
    match &node.value {
        Yaml::Null => String::new(),
        Yaml::Bool(flag) => flag.to_string(),
        Yaml::Integer(number) => number.to_string(),
        Yaml::Unsigned(number) => number.to_string(),
        Yaml::Float(number) => argo_parser::model::json_float(*number),
        Yaml::String(text) => text.clone(),
        _ => crate::pod::node_json(node),
    }
}

/// Parameters as Argo marshals them for `workflow.parameters` and `inputs.parameters`.
pub(crate) fn parameters_json(parameters: &[Parameter]) -> String {
    let list: Vec<Value> = parameters
        .iter()
        .map(|parameter| {
            let mut map = BTreeMap::new();
            map.insert("name".to_owned(), Value::string(parameter.name.clone()));
            if let Some(default) = &parameter.default {
                map.insert("default".to_owned(), Value::string(default.0.clone()));
            }
            if let Some(value) = &parameter.value {
                map.insert("value".to_owned(), Value::string(value.0.clone()));
            }
            if !parameter.enumeration.is_empty() {
                map.insert(
                    "enum".to_owned(),
                    Value::Array(
                        parameter
                            .enumeration
                            .iter()
                            .map(|item| Value::string(item.0.clone()))
                            .collect(),
                    ),
                );
            }
            Value::Map(map)
        })
        .collect();
    Value::Array(list).to_json()
}

/// The FNV-1a hash of a text, which Argo uses for node IDs and pod names.
pub(crate) fn fnv32a(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// The IP address the run gives the pod of a node.
pub(crate) fn pod_ip(node: &str) -> String {
    let hash = fnv32a(node).to_be_bytes();
    format!("10.{}.{}.{}", hash[1], hash[2], hash[3])
}

/// A stable UUID-shaped identifier for `workflow.uid` and the uid of a pod.
pub(crate) fn uid(name: &str) -> String {
    let parts: Vec<u32> = (0..4)
        .map(|round| fnv32a(&format!("{round}{name}")))
        .collect();
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:04x}{:08x}",
        parts[0],
        parts[1] >> 16,
        (parts[1] & 0x0fff) | 0x4000,
        (parts[2] >> 16 & 0x3fff) | 0x8000,
        parts[2] & 0xffff,
        parts[3]
    )
}
