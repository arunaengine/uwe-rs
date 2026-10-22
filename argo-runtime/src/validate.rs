//! Checks whether submitted workflows and templates can run under Argo rules.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use argo_parser::model::{Arguments, DagTask, Template, TemplateRef, WorkflowSpec, WorkflowStep};
use argo_parser::{Node, Value as Yaml, decode};

use crate::checks::{
    Names, argument_dependencies, arguments, cluster_only, field_names, hooks, invalid_field_name,
    leaf_checks, node_types, one_loop, process_args, sort_tasks, template_types, without_tags,
};
use crate::dag::ancestry;
use crate::library::{Document, Kind, Library};
use crate::references::{Call, Globals, Scope};

/// What a call supplies to the inputs of the template it calls.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Supplied<'a> {
    /// Every input, as Argo's `FakeArguments` for templates that are checked on their own.
    Anything,
    Arguments(&'a Arguments),
}

/// Where template names resolve: the checked spec, or a WorkflowTemplate that a `templateRef`
/// reached.
#[derive(Debug)]
pub(crate) struct Base {
    pub label: String,
    pub path: PathBuf,
    pub templates: Vec<Node>,
    /// `templateDefaults`, which may give container and script templates their image.
    pub defaults: Option<Node>,
}

impl Base {
    pub fn new(label: String, path: PathBuf, spec: &Node, kind: Kind) -> Base {
        let templates = match spec.get("templates").map(|node| &node.value) {
            Some(Yaml::Sequence(templates)) => templates.clone(),
            _ => Vec::new(),
        };
        // Argo applies the defaults of Workflows and WorkflowTemplates only.
        let defaults = spec
            .get("templateDefaults")
            .filter(|_| kind != Kind::ClusterWorkflowTemplate)
            .cloned();
        Base {
            label,
            path,
            templates,
            defaults,
        }
    }

    fn template(&self, name: &str) -> Option<&Node> {
        self.templates
            .iter()
            .find(|template| template.get("name").and_then(Node::as_str) == Some(name))
    }
}

/// What names the template of a call: a step, a task, a hook or the entrypoint.
struct Holder<'a> {
    template: &'a str,
    template_ref: Option<&'a TemplateRef>,
    inline: Option<&'a Template>,
    /// The source of the inline template, whose tags are checked.
    inline_node: Option<&'a Node>,
}

impl<'a> Holder<'a> {
    fn named(template: &'a str) -> Self {
        Holder {
            template,
            template_ref: None,
            inline: None,
            inline_node: None,
        }
    }

    fn of_step(step: &'a WorkflowStep, node: Option<&'a Node>) -> Self {
        Holder {
            template: &step.template,
            template_ref: step.template_ref.as_ref(),
            inline: step.inline.as_deref(),
            inline_node: node.and_then(|node| node.get("inline")),
        }
    }

    fn of_task(task: &'a DagTask, node: Option<&'a Node>) -> Self {
        Holder {
            template: &task.template,
            template_ref: task.template_ref.as_ref(),
            inline: task.inline.as_deref(),
            inline_node: node.and_then(|node| node.get("inline")),
        }
    }
}

/// Why a document cannot run: Argo rejects it, or it needs a feature that uwe does not
/// support.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub message: String,
    /// The document needs a feature that uwe leaves out on purpose, like a template that
    /// needs a Kubernetes cluster (decision 18).
    pub unsupported: bool,
}

/// Checks a document as Argo does when it is submitted or created: a Workflow or the workflow
/// spec of a CronWorkflow with its entrypoint, a WorkflowTemplate or ClusterWorkflowTemplate
/// with every template on its own. Executor plugins only need their container and a port.
pub fn validate(document: &Document, library: &Library) -> Result<(), Rejection> {
    let mut check = Check::new(library);
    check.metadata(document.node.get("metadata"));
    check_document(&mut check, document).map_err(|message| Rejection {
        message,
        unsupported: check.unsupported,
    })
}

/// The checks of [`validate`], with messages only.
fn check_document(check: &mut Check, document: &Document) -> Result<(), String> {
    let Some(_) = document.spec() else {
        if document.kind == Kind::ExecutorPlugin {
            return Ok(());
        }
        return Err(format!("{} has no spec", document.label()));
    };
    let joined = crate::start::joined_spec(document, check.library).map_err(|failure| {
        check.unsupported = failure.unsupported;
        failure.message
    })?;
    check.metadata(joined.metadata.as_ref());
    let spec_node = &joined.spec;
    let (spec, _) = decode::<WorkflowSpec>(spec_node).map_err(|error| format!("spec: {error}"))?;
    let base = Rc::new(Base::new(
        String::new(),
        joined.path,
        spec_node,
        document.kind,
    ));
    let template_kind = matches!(
        document.kind,
        Kind::WorkflowTemplate | Kind::ClusterWorkflowTemplate
    );
    if document.kind == Kind::CronWorkflow {
        let schedules = document
            .node
            .get("spec")
            .and_then(|spec| spec.get("schedules"));
        if !matches!(schedules.map(|node| &node.value), Some(Yaml::Sequence(list)) if !list.is_empty())
        {
            return Err("cron workflow must have at least one schedule".to_owned());
        }
    }
    check.template_check = template_kind;
    let result = if spec.entrypoint.is_empty() && template_kind {
        check.spec(&spec, &base, None, true)
    } else if spec.entrypoint.is_empty() {
        Err("spec.entrypoint is required".to_owned())
    } else {
        let supplied = if template_kind {
            Supplied::Anything
        } else {
            Supplied::Arguments(&spec.arguments)
        };
        let allow_empty = document.kind != Kind::CronWorkflow;
        check.spec(
            &spec,
            &base,
            Some((&spec.entrypoint, supplied)),
            allow_empty,
        )
    };
    match result {
        Err(error) if document.kind == Kind::CronWorkflow => {
            Err(format!("cannot validate Workflow: {error}"))
        }
        result => result,
    }
}

/// The state of one validation: the library, and the templates whose body was checked, as
/// Argo's `results`.
pub(crate) struct Check<'a> {
    library: &'a Library,
    /// Whether the error is about a feature that uwe does not support.
    unsupported: bool,
    checked: HashSet<(String, String)>,
    bases: Vec<((PathBuf, usize), Rc<Base>)>,
    /// `metadata` of the document, whose labels and annotations are variables.
    metadata: Option<Node>,
    /// A WorkflowTemplate checked on its own, whose parameters may come from a Workflow.
    template_check: bool,
    globals: Globals,
}

impl<'a> Check<'a> {
    pub fn new(library: &'a Library) -> Self {
        Check {
            library,
            unsupported: false,
            checked: HashSet::new(),
            bases: Vec::new(),
            metadata: None,
            template_check: false,
            globals: Globals::default(),
        }
    }

    /// Sets the `metadata` of the checked document.
    pub fn metadata(&mut self, metadata: Option<&Node>) {
        self.metadata = metadata.cloned();
    }

    /// Whether the error of a check is about a feature that uwe does not support.
    pub fn unsupported(&self) -> bool {
        self.unsupported
    }

    /// Checks a workflow spec whose templates are those of `base`.
    pub fn spec(
        &mut self,
        spec: &WorkflowSpec,
        base: &Rc<Base>,
        entry: Option<(&str, Supplied)>,
        allow_empty: bool,
    ) -> Result<(), String> {
        self.globals = Globals::of_spec(spec, self.metadata.as_ref(), self.template_check);
        if let Some(strategy) = &spec.retry_strategy {
            crate::retry::check_strategy(strategy).map_err(|failure| {
                self.unsupported = failure.unsupported;
                failure.message
            })?;
        }
        for (field, present) in [
            ("podResources", spec.pod_resources.is_some()),
            ("resourceClaims", spec.resource_claims.is_some()),
            (
                "shutdown",
                spec.shutdown
                    .as_ref()
                    .is_some_and(|node| node.as_str().is_none_or(|value| !value.is_empty())),
            ),
        ] {
            if present {
                self.unsupported = true;
                return Err(format!("spec.{field} is not supported"));
            }
        }
        if let Some(node) = &spec.synchronization {
            crate::sync::check(node).map_err(|failure| {
                self.unsupported = failure.unsupported;
                failure.message
            })?;
        }
        if let Some(value) = &spec.active_deadline_seconds
            && !crate::tags::has_tag(&value.0)
        {
            crate::timers::seconds(&value.0, true).map_err(|failure| failure.message)?;
        }
        if let Some(defaults) = &spec.template_defaults
            && let Some(reason) = cluster_only(defaults)
        {
            self.unsupported = true;
            return Err(format!("templateDefaults: {reason}"));
        }
        let names: Vec<&str> = base
            .templates
            .iter()
            .map(|template| template.get("name").and_then(Node::as_str).unwrap_or(""))
            .collect();
        field_names(&names, Names::Field).map_err(|error| format!("spec.templates{error}"))?;
        arguments("spec.arguments.", &spec.arguments, allow_empty)?;
        if let Some((entrypoint, supplied)) = entry {
            self.holder(&Holder::named(entrypoint), base, supplied)?;
        }
        if !spec.on_exit.is_empty() {
            self.globals.exit_handler(&spec.templates);
            let supplied = Supplied::Arguments(&spec.arguments);
            self.holder(&Holder::named(&spec.on_exit), base, supplied)?;
        }
        hooks(&spec.hooks, "hooks")?;
        for name in names {
            self.holder(&Holder::named(name), base, Supplied::Anything)
                .map_err(|error| format!("templates.{name} {error}"))?;
        }
        Ok(())
    }

    fn base_of(&mut self, document: &Rc<Document>) -> Rc<Base> {
        let key = (document.path.clone(), document.index);
        if let Some((_, base)) = self.bases.iter().find(|(known, _)| known == &key) {
            return Rc::clone(base);
        }
        let empty = Node::new(Yaml::Mapping(Vec::new()), Default::default());
        let spec = document.spec().unwrap_or(&empty);
        let base = Rc::new(Base::new(
            document.label(),
            document.path.clone(),
            spec,
            document.kind,
        ));
        self.bases.push((key, Rc::clone(&base)));
        base
    }

    /// Finds the template of a call and checks it, as Argo's `validateTemplateHolder`. Gives
    /// the template, unless a tag chooses it.
    fn holder(
        &mut self,
        holder: &Holder,
        base: &Rc<Base>,
        supplied: Supplied,
    ) -> Result<Option<Template>, String> {
        let (base, node, template) = if let Some(reference) = holder.template_ref {
            if !holder.template.is_empty() {
                return Err("template name cannot be specified with templateRef".to_owned());
            }
            if reference.name.is_empty() {
                return Err("resource name is required".to_owned());
            }
            if reference.template.is_empty() {
                return Err("template name is required".to_owned());
            }
            // A reference that a tag chooses is resolved when the workflow runs.
            if reference.name.contains("{{") || reference.template.contains("{{") {
                return Ok(None);
            }
            let kind = if reference.cluster_scope {
                Kind::ClusterWorkflowTemplate
            } else {
                Kind::WorkflowTemplate
            };
            let document = self
                .library
                .template_in(
                    kind,
                    &reference.name,
                    &reference.template,
                    &base.path,
                    crate::library::namespace(self.metadata.as_ref()),
                )
                .ok_or_else(|| not_found(kind, &reference.name))?;
            let base = self.base_of(&document);
            let Some(node) = base.template(&reference.template).cloned() else {
                return Err(format!(
                    "template reference {}.{} not found",
                    reference.name, reference.template
                ));
            };
            let template = decode_template(&node)?;
            (base, Some(node), template)
        } else if let Some(inline) = holder.inline {
            (Rc::clone(base), None, inline.clone())
        } else if !holder.template.is_empty() {
            let Some(node) = base.template(holder.template).cloned() else {
                return Err(format!("template name '{}' undefined", holder.template));
            };
            let template = decode_template(&node)?;
            (Rc::clone(base), Some(node), template)
        } else {
            return Err("failed to get a template".to_owned());
        };
        let types = match &node {
            Some(node) => node_types(node),
            None => template_types(&template),
        };
        if types == 0 {
            return Err(format!("template '{}' type is unknown", template.name));
        }
        if let Some(strategy) = &template.retry_strategy {
            crate::retry::check_strategy(strategy).map_err(|failure| {
                self.unsupported = failure.unsupported;
                failure.message
            })?;
        }
        let source = node.as_ref().or(holder.inline_node);
        self.template(&template, node.as_ref(), source, &base, supplied)?;
        Ok(Some(template))
    }

    /// Checks a template: its type, its inputs against what the call supplies, and once per
    /// template its body, as Argo's `validateTemplate`.
    fn template(
        &mut self,
        template: &Template,
        node: Option<&Node>,
        source: Option<&Node>,
        base: &Rc<Base>,
        supplied: Supplied,
    ) -> Result<(), String> {
        let name = &template.name;
        let types = match node {
            Some(node) => node_types(node),
            None => template_types(template),
        };
        if types > 1 {
            return Err(format!(
                "templates.{name} multiple template types specified. choose one of: container, containerSet, steps, script, resource, dag, suspend, template, template ref"
            ));
        }
        if let Some(reason) = cluster_only(template) {
            self.unsupported = true;
            return Err(reason);
        }
        let parameters: Vec<&str> = template
            .inputs
            .parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect();
        field_names(&parameters, Names::Parameter)
            .map_err(|error| format!("templates.{name}.inputs.parameters{error}"))?;
        let artifacts: Vec<&str> = template
            .inputs
            .artifacts
            .iter()
            .map(|artifact| artifact.name.as_str())
            .collect();
        field_names(&artifacts, Names::Parameter)
            .map_err(|error| format!("templates.{name}.inputs.artifacts{error}"))?;
        process_args(template, supplied).map_err(|error| format!("templates.{name} {error}"))?;
        let leaf = template.steps.is_empty() && template.dag.is_none();
        if let Some(timeout) = &template.timeout {
            if !leaf {
                let kind = if template.dag.is_some() {
                    "DAG"
                } else {
                    "Steps"
                };
                return Err(format!("{kind} template doesn't support timeout field"));
            }
            if !crate::tags::has_tag(&timeout.0)
                && !timeout.0.is_empty()
                && (timeout.0.trim().parse::<i64>().is_ok()
                    || crate::resources::duration(&timeout.0).is_none())
            {
                return Err(format!("{name} has invalid duration format in timeout"));
            }
        }
        let id = match node {
            Some(_) => name.clone(),
            None => format!("inline {template:?}"),
        };
        if !self.checked.insert((base.label.clone(), id)) {
            return Ok(());
        }
        if !leaf && template.active_deadline_seconds.is_some() {
            return Err(format!(
                "templates.{name}.activeDeadlineSeconds is only valid for leaf templates"
            ));
        }
        if let Some(value) = &template.active_deadline_seconds
            && !crate::tags::has_tag(&value.0)
        {
            crate::timers::seconds(&value.0, false).map_err(|failure| failure.message)?;
        }
        let mut scope = Scope::of_template(template);
        if !template.steps.is_empty() {
            self.steps(template, source, &mut scope, base)?;
        } else if template.dag.is_some() {
            self.dag(template, source, &mut scope, base)?;
        } else {
            if let Some(source) = source {
                (scope.resolve_node(source, &self.globals))
                    .map_err(|error| format!("templates.{name}: {error}"))?;
            }
            return leaf_checks(template, base);
        }
        match source.and_then(|source| source.get("outputs")) {
            Some(outputs) => (scope.resolve_node(outputs, &self.globals))
                .map_err(|error| format!("templates.{name}.outputs {error}")),
            None => Ok(()),
        }
    }

    fn steps(
        &mut self,
        template: &Template,
        source: Option<&Node>,
        scope: &mut Scope,
        base: &Rc<Base>,
    ) -> Result<(), String> {
        let name = &template.name;
        let mut seen: HashSet<&str> = HashSet::new();
        for (position, group) in template.steps.iter().enumerate() {
            let group_node = source
                .and_then(|source| source.get("steps"))
                .and_then(|groups| item_at(groups, position));
            for (index, step) in group.iter().enumerate() {
                let step_node = group_node.and_then(|group| item_at(group, index));
                let prefix = format!("templates.{name}.steps[{position}]");
                if step.name.is_empty() {
                    return Err(format!("{prefix}.name is required"));
                }
                if !seen.insert(&step.name) {
                    return Err(format!("{prefix}.name '{}' is not unique", step.name));
                }
                if let Some(error) = invalid_field_name(&without_tags(&step.name)) {
                    return Err(format!("{prefix}.name '{}' is invalid: {error}", step.name));
                }
                let prefix = format!("{prefix}.{}", step.name);
                let outputs = format!("steps.{}", step.name);
                scope.insert(format!("{outputs}.status"));
                one_loop(
                    &step.with_items,
                    &step.with_param,
                    step.with_sequence.is_some(),
                )
                .map_err(|error| format!("{prefix} {error}"))?;
                let sequence = step.with_sequence.is_some();
                scope.add_items(&step.with_items, &step.with_param, sequence);
                arguments(&format!("{prefix}.arguments."), &step.arguments, false)?;
                let holder = Holder::of_step(step, step_node);
                let called = (self.holder(&holder, base, Supplied::Anything))
                    .map_err(|error| format!("{prefix} {error}"))?;
                let exit_hook = !step.on_exit.is_empty() || step.hooks.contains_key("exit");
                if exit_hook {
                    scope.add_outputs(
                        called.as_ref(),
                        &outputs,
                        Call::default(),
                        &mut self.globals,
                    );
                }
                hooks(&step.hooks, &prefix)?;
                if let Some(step_node) = step_node {
                    let mut step_scope = scope.clone();
                    inline_inputs(step.inline.as_deref(), &mut step_scope);
                    (step_scope.resolve_node(step_node, &self.globals))
                        .map_err(|error| format!("templates.{name}.steps {error}"))?;
                }
                let call = Call {
                    aggregate: !step.with_items.is_empty() || !step.with_param.is_empty(),
                    ancestor: false,
                };
                scope.add_outputs(called.as_ref(), &outputs, call, &mut self.globals);
                self.holder(&holder, base, Supplied::Arguments(&step.arguments))
                    .map_err(|error| format!("{prefix} {error}"))?;
            }
        }
        Ok(())
    }

    fn dag(
        &mut self,
        template: &Template,
        source: Option<&Node>,
        scope: &mut Scope,
        base: &Rc<Base>,
    ) -> Result<(), String> {
        let name = &template.name;
        let Some(dag) = &template.dag else {
            return Ok(());
        };
        let (tasks, target) = (&dag.tasks, &dag.target);
        let task_nodes = source
            .and_then(|source| source.get("dag"))
            .and_then(|dag| dag.get("tasks"));
        if tasks.is_empty() {
            return Err(format!("templates.{name} must have at least one task"));
        }
        let names: Vec<&str> = tasks.iter().map(|task| task.name.as_str()).collect();
        sort_tasks(tasks).map_err(|error| format!("templates.{name} sorting failed: {error}"))?;
        field_names(&names, Names::Field)
            .map_err(|error| format!("templates.{name}.tasks{error}"))?;
        let depends = tasks.iter().any(|task| !task.depends.is_empty());
        let mut called = Vec::new();
        for (index, task) in tasks.iter().enumerate() {
            let prefix = format!("templates.{name}.tasks.{}", task.name);
            let ordered = depends || !task.dependencies.is_empty();
            if ordered && task.name.starts_with(|c: char| c.is_ascii_digit()) {
                return Err(format!(
                    "{prefix} name cannot begin with a digit when using either 'depends' or 'dependencies'"
                ));
            }
            if depends && !task.dependencies.is_empty() {
                return Err(format!(
                    "templates.{name} cannot use both 'depends' and 'dependencies' in the same DAG template"
                ));
            }
            if depends && task.continue_on.is_some() {
                return Err(format!(
                    "templates.{name} cannot use 'continueOn' when using 'depends'. Instead use 'dep-task.Failed'/'dep-task.Errored'"
                ));
            }
            let node = task_nodes.and_then(|tasks| item_at(tasks, index));
            let resolved = (self.holder(&Holder::of_task(task, node), base, Supplied::Anything))
                .map_err(|error| format!("{prefix} {error}"))?;
            let outputs = format!("tasks.{}", task.name);
            let call = Call {
                aggregate: loops_over(task),
                ancestor: false,
            };
            scope.add_outputs(resolved.as_ref(), &outputs, call, &mut self.globals);
            called.push(resolved);
        }
        (scope.resolve(target, &self.globals))
            .map_err(|error| format!("templates.{name}.targets {error}"))?;
        for target in target.split(' ').filter(|target| !target.is_empty()) {
            if !target.contains("{{") && !names.contains(&target) {
                return Err(format!(
                    "templates.{name}.targets: target '{target}' is not defined"
                ));
            }
        }
        for (index, task) in tasks.iter().enumerate() {
            let prefix = format!("templates.{name}.tasks.{}", task.name);
            let outputs = format!("tasks.{}", task.name);
            if !task.hooks.is_empty() {
                scope.insert(format!("{outputs}.status"));
            }
            scope.add_outputs(
                called[index].as_ref(),
                &outputs,
                Call::default(),
                &mut self.globals,
            );
            let mut ancestors = Vec::new();
            ancestry(&task.name, tasks, &mut ancestors);
            let mut task_scope = scope.clone();
            for ancestor in ancestors.iter().filter(|ancestor| **ancestor != task.name) {
                let Some(position) = tasks.iter().position(|task| &task.name == ancestor) else {
                    continue;
                };
                let call = Call {
                    aggregate: loops_over(&tasks[position]),
                    ancestor: true,
                };
                let prefix = format!("tasks.{ancestor}");
                task_scope.add_outputs(called[position].as_ref(), &prefix, call, &mut self.globals);
            }
            inline_inputs(task.inline.as_deref(), &mut task_scope);
            one_loop(
                &task.with_items,
                &task.with_param,
                task.with_sequence.is_some(),
            )
            .map_err(|error| format!("{prefix} {error}"))?;
            let sequence = task.with_sequence.is_some();
            task_scope.add_items(&task.with_items, &task.with_param, sequence);
            let node = task_nodes.and_then(|tasks| item_at(tasks, index));
            if let Some(node) = node {
                (task_scope.resolve_node(node, &self.globals))
                    .map_err(|error| format!("{prefix} {error}"))?;
            }
            arguments(&format!("{prefix}.arguments."), &task.arguments, false)?;
            argument_dependencies(&task.arguments, &ancestors)
                .map_err(|error| format!("{prefix} {error}"))?;
            self.holder(
                &Holder::of_task(task, node),
                base,
                Supplied::Arguments(&task.arguments),
            )
            .map_err(|error| format!("{prefix} {error}"))?;
        }
        Ok(())
    }
}

/// The item at a position of a sequence node.
fn item_at(node: &Node, index: usize) -> Option<&Node> {
    match &node.value {
        Yaml::Sequence(items) => items.get(index),
        _ => None,
    }
}

/// Whether a task loops, so that its outputs are lists.
fn loops_over(task: &DagTask) -> bool {
    !task.with_items.is_empty() || !task.with_param.is_empty()
}

/// Adds the input parameters of an inline template, which its call may name.
fn inline_inputs(inline: Option<&Template>, scope: &mut Scope) {
    for parameter in inline.iter().flat_map(|inline| &inline.inputs.parameters) {
        scope.insert(format!("inputs.parameters.{}", parameter.name));
    }
}

fn decode_template(node: &Node) -> Result<Template, String> {
    let name = node.get("name").and_then(Node::as_str).unwrap_or("");
    decode::<Template>(node)
        .map(|(template, _)| template)
        .map_err(|error| format!("templates.{name}: {error}"))
}

/// Argo's message for a WorkflowTemplate or ClusterWorkflowTemplate that is not there.
pub(crate) fn not_found(kind: Kind, name: &str) -> String {
    match kind {
        Kind::ClusterWorkflowTemplate => {
            format!("couldn't find cluster workflow template \"{name}\"")
        }
        _ => format!("couldn't find workflow template \"{name}\""),
    }
}

#[cfg(test)]
#[path = "validate_tests.rs"]
mod tests;
