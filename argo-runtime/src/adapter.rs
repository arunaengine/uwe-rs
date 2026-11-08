//! Runs single template calls of an Argo document as the native run runs a step or task.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;
use std::rc::Rc;

use argo_parser::model::{
    AnyString, Arguments, DagTask, LifecycleHook, Sequence, TemplateRef, WorkflowSpec, WorkflowStep,
};
use argo_parser::{Entry, Node, Position, Value as Yaml, decode};
use serde_json::{Map, Value as Json};
use workflow_graph::{Delivery, DriverError, Files, Launch, Outcome, Task, TaskId};

use crate::dag::task_entry;
use crate::items::{item_variables, items, loops, substitute_item, unknown_item};
use crate::node::{ArtifactValue, Facts, Location, Phase};
use crate::run::{Call, Expanded, Failure, Holder, Run};
use crate::scope::{Scope, json_or_text};
use crate::steps::step_entry;
use crate::tags::{Variables, replace_node};
use crate::value::Value;
use crate::when::should_execute;
use crate::{Document, Library, Options, Services, artifact_value, driver_error, load_launch};

/// The template calls of one Argo document, for dialect tasks of wir. Each call is a node of
/// one native run, so its tasks, retries, acceptance and outputs are those of the native run.
pub struct Adapter {
    run: Run,
    calls: Vec<usize>,
    /// The call of each task the calls asked for.
    owners: std::collections::HashMap<TaskId, usize>,
}

/// A call of a template: a step or DAG task of a holding template, as the native run expands
/// it, or the call of the entrypoint by the workflow itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Instance {
    /// The template that holds the step or task, or the entrypoint.
    pub template: String,
    /// The WorkflowTemplate that holds that template, and whether it is cluster scoped.
    pub reference: Option<(String, bool)>,
    /// The step `[1].use` or the DAG task `.use`, or its exit hook `[1].use.onExit`; `.onExit` is
    /// the exit handler of the workflow, and anything else, like `entrypoint`, the entrypoint.
    pub call: String,
    /// The node name of the holding template after the workflow name, in parts.
    pub path: Vec<String>,
    /// The round of a loop, which names the node with its item.
    pub index: usize,
    /// The tags that the step reads, by name: values like `steps.make.outputs.result` or
    /// `item`, and artifacts like `steps.make.outputs.artifacts.data` as Files or Directories.
    pub values: Vec<(String, Json)>,
}

/// What an Argo text that was compiled into wir gives once its tags are substituted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// The text itself.
    Text,
    /// Whether the text, a `when` condition, holds.
    Condition,
    /// The items of a loop: the text is a JSON list, as `withParam` holds it, or the JSON
    /// object of a `withSequence`.
    Items,
    /// The suffixes that the items of a loop give their node names, like `(0:a)`.
    Labels,
}

/// A started call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallId(usize);

/// How a call ended: its results, its message, and its outputs by tag after the step prefix,
/// like `outputs.parameters.<name>`, `outputs.result` or `exitCode`; artifacts as Files.
#[derive(Clone, Debug, PartialEq)]
pub struct Ending {
    pub facts: Facts,
    pub message: String,
    pub outputs: Map<String, Json>,
}

impl Adapter {
    pub fn new(
        document: Rc<Document>,
        library: Rc<Library>,
        options: Options,
    ) -> Result<Adapter, DriverError> {
        let run = Run::new(document, library, options).map_err(driver_error)?;
        Ok(Adapter {
            run,
            calls: Vec::new(),
            owners: std::collections::HashMap::new(),
        })
    }

    /// The adapter of the document that a launch runs, with the services of its host.
    pub fn load(launch: &Launch, services: Services) -> Result<Adapter, DriverError> {
        let (document, library, options) = load_launch(launch, services)?;
        Adapter::new(document, Rc::new(library), options)
    }

    /// The name of the workflow, which the node names of the calls start with.
    pub fn workflow_name(&self) -> &str {
        &self.run.workflow_name
    }

    /// The spec of the workflow, joined with the WorkflowTemplate it references.
    pub fn spec(&self) -> &WorkflowSpec {
        &self.run.spec
    }

    /// The template that a run starts with.
    pub fn entrypoint(&self) -> &str {
        &self.run.entrypoint
    }

    /// A template of the workflow, or of the WorkflowTemplate `reference` names, with the
    /// template defaults of the workflow, as a call sees it.
    pub fn template(
        &mut self,
        name: &str,
        reference: Option<&(String, bool)>,
    ) -> Result<Node, String> {
        let holder = holder_of(name, reference);
        let context = Rc::clone(&self.run.root_context);
        let (_, template) = self.run.resolve(&holder, &context)?;
        Ok(match &self.run.defaults {
            Some(defaults) => crate::merge::with_defaults(&template, defaults),
            None => template,
        })
    }

    pub fn use_files(&mut self, files: std::sync::Arc<dyn Files>) {
        self.run.files = Some(files);
    }

    /// Starts a call, and gives the tasks it asks for first.
    pub fn start(&mut self, instance: &Instance) -> Result<(CallId, Vec<Task>), DriverError> {
        let base = format!("{}{}", self.run.workflow_name, instance.path.concat());
        let index = match !instance.call.starts_with(['[', '.']) {
            true => {
                let holder = Holder::named(&instance.template);
                let context = Rc::clone(&self.run.root_context);
                let args = self.run.arguments.clone();
                (self.run).execute_template(&base, &holder, &context, &args, &Call::default())
            }
            false => match instance.call.strip_suffix(".onExit") {
                Some(site) => self.exit_call(instance, site, &base),
                None => self.call_site(instance, &base),
            },
        }
        .map_err(driver_error)?;
        if self.calls.contains(&index) {
            let name = &self.run.nodes[index].name;
            return Err(DriverError::new(format!("node {name} has started already")));
        }
        self.calls.push(index);
        Ok((CallId(index), self.settle(Some(index))?))
    }

    /// Makes the node of a step or DAG task as the native run expands it: its tags, its loop
    /// item, its `when`, its arguments and artifacts, then the call of its template.
    fn call_site(&mut self, instance: &Instance, base: &str) -> crate::run::Result<usize> {
        let holder = holder_of(&instance.template, instance.reference.as_ref());
        let root = Rc::clone(&self.run.root_context);
        let (context, holding) = self.run.resolve(&holder, &root).map_err(Failure::error)?;
        let dag = !instance.call.starts_with('[');
        let step = site_node(&holding, &instance.call).ok_or_else(|| {
            Failure::error(format!(
                "no step {} in template {}",
                instance.call, instance.template
            ))
        })?;
        let scope = scope_of(&instance.values)?;
        let variables = self.run.variables(&scope);
        let mut node = step.clone();
        replace_node(&mut node, &variables, true).map_err(Failure::error)?;
        let name = format!("{base}{}", instance.call);
        let (when, template, sequence) = match dag {
            true => decode::<DagTask>(&node)
                .map(|(task, _)| (task.when, task.template, task.with_sequence)),
            false => decode::<WorkflowStep>(&node)
                .map(|(step, _)| (step.when, step.template, step.with_sequence)),
        }
        .map_err(|error| Failure::error(error.to_string()))?;
        let arguments = decode::<Arguments>(
            node.get("arguments")
                .unwrap_or(&Node::new(Yaml::Mapping(Vec::new()), Position::default())),
        )
        .map(|(arguments, _)| arguments)
        .map_err(|error| Failure::error(error.to_string()))?;
        let artifacts = match should_execute(&when).unwrap_or(true) {
            true => self
                .run
                .step_artifacts(&arguments.artifacts, &scope)
                .map_err(Failure::error)?,
            false => Vec::new(),
        };
        let build = match dag {
            true => task_entry,
            false => step_entry,
        };
        let entry = match loops(&step) {
            false => build(name.clone(), node, &template, artifacts),
            true => {
                // A given item spares listing every item of the loop for each round.
                let given = (instance.values.iter()).find(|(name, _)| name == "item");
                let item = match given {
                    Some((_, item)) => Value::from_json(item),
                    None => self.loop_item(&node, sequence.as_ref(), instance.index)?,
                };
                let (values, item_name) =
                    item_variables(&name, instance.index, &item).map_err(Failure::error)?;
                let strict = !matches!(should_execute(&when), Ok(false))
                    && !(self.run.options.assume_inputs && unknown_item(&item));
                match substitute_item(&node, &variables.with_values(values), strict) {
                    Ok(node) => build(item_name, node, &template, artifacts),
                    Err(message) => Expanded::error(item_name, message),
                }
            }
        };
        let call = Call::default();
        if let Some(error) = &entry.error {
            return Ok(self
                .run
                .end_node(&entry.name, Phase::Error, &call, error.clone()));
        }
        match self.run.decide_when(&entry.when, &entry.name) {
            Err(message) => Ok(self.run.end_node(&entry.name, Phase::Error, &call, message)),
            Ok(false) => {
                let message = format!("when '{}' evaluated false", entry.when);
                let child = self
                    .run
                    .end_node(&entry.name, Phase::Skipped, &call, message);
                self.run
                    .skipped_outputs(child, &entry.holder, &context, !dag);
                Ok(child)
            }
            Ok(true) => (self.run).execute_template(
                &entry.name,
                &entry.holder,
                &context,
                &entry.args,
                &call,
            ),
        }
    }

    /// Makes the node of the exit hook of the step or DAG task `site`, or of the exit handler of
    /// the workflow when `site` is empty, with its arguments as the native run gives them.
    fn exit_call(
        &mut self,
        instance: &Instance,
        site: &str,
        base: &str,
    ) -> crate::run::Result<usize> {
        let root = Rc::clone(&self.run.root_context);
        let (context, hook) = match site.is_empty() {
            true => {
                let spec = &self.run.spec;
                let hook = (spec.hooks.get("exit").cloned()).or_else(|| {
                    let template = spec.on_exit.clone();
                    (!template.is_empty()).then(|| exit_hook(template))
                });
                (root, hook)
            }
            false => {
                let holder = holder_of(&instance.template, instance.reference.as_ref());
                let (context, holding) =
                    self.run.resolve(&holder, &root).map_err(Failure::error)?;
                let step = site_node(&holding, site);
                let hooks = step
                    .as_ref()
                    .and_then(|step| step.get("hooks")?.get("exit").cloned());
                let hook = match hooks {
                    Some(node) => Some(decode(&node).map_err(|e| Failure::error(e.to_string()))?.0),
                    None => (step.as_ref())
                        .and_then(|step| step.get("onExit")?.as_str().map(str::to_owned))
                        .map(exit_hook),
                };
                (context, hook)
            }
        };
        let Some(hook) = hook else {
            let message = format!("no exit hook {site} in template {}", instance.template);
            return Err(Failure::error(message));
        };
        let scope = scope_of(&instance.values)?;
        let args = self.run.hook_args(&hook, &scope)?;
        let name = format!("{base}{site}.onExit");
        let holder = Holder::of_hook(&hook);
        (self.run).execute_template(&name, &holder, &context, &args, &Call::default())
    }

    /// The item of round `index` of the loop of a substituted step.
    fn loop_item(
        &mut self,
        node: &Node,
        sequence: Option<&Sequence>,
        index: usize,
    ) -> crate::run::Result<Value> {
        let mut warnings = Vec::new();
        let assume = self.run.options.assume_inputs;
        let list = items(node, sequence, assume, &mut |warning| {
            warnings.push(warning)
        });
        for warning in warnings {
            self.run.warn(warning);
        }
        let list = list.map_err(Failure::error)?;
        Ok(list.get(index).cloned().unwrap_or(Value::Nil))
    }

    /// Takes the outcome of a task of a call, and gives the tasks that its calls ask for next,
    /// like a later attempt.
    pub fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        let call = self.call_of(outcome.id).map(|call| call.0);
        self.run.record(outcome).map_err(driver_error)?;
        self.settle(call)
    }

    /// Whether an execution failure of a task comes back as an outcome, as in the native run.
    pub fn handles_error(&self, task: TaskId, error: &workflow_graph::ExecuteError) -> bool {
        crate::handled_error(Some(&self.run), task, error)
    }

    /// The started call that a task belongs to.
    pub fn call_of(&self, task: TaskId) -> Option<CallId> {
        self.owners.get(&task).map(|call| CallId(*call))
    }

    /// The started call whose node holds the node of a task.
    fn holder(&self, task: TaskId) -> Option<CallId> {
        let &(index, _) = self.run.tasks.get(&task)?;
        let name = &self.run.nodes[index].name;
        let holds = |call: &usize| {
            let rest = name.strip_prefix(&self.run.nodes[*call].name);
            rest.is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '[', '(']))
        };
        let calls = self.calls.iter().filter(|call| holds(call));
        let call = calls.max_by_key(|&&call| self.run.nodes[call].name.len())?;
        Some(CallId(*call))
    }

    /// Whether the call accepted that attempt of its task.
    pub fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.run.take_acceptance(task, attempt)
    }

    /// The results of a call so far; a daemon counts as daemoned once it runs.
    pub fn facts(&self, call: CallId) -> Facts {
        self.run.facts(call.0)
    }

    /// How a call ended, once it did.
    pub fn ending(&self, call: CallId) -> Option<Ending> {
        let node = &self.run.nodes[call.0];
        if !node.phase.fulfilled() {
            return None;
        }
        let mut outputs = Map::new();
        for (name, value) in &node.outputs.parameters {
            outputs.insert(format!("outputs.parameters.{name}"), value.as_str().into());
        }
        for artifact in &node.outputs.artifacts {
            let value = artifact_value(artifact);
            outputs.insert(format!("outputs.artifacts.{}", artifact.name), value);
        }
        if let Some(result) = &node.outputs.result {
            outputs.insert("outputs.result".into(), result.as_str().into());
        }
        if let Some(code) = &node.outputs.exit_code {
            outputs.insert("exitCode".into(), code.as_str().into());
        }
        Some(Ending {
            facts: self.run.facts(call.0),
            message: node.message.clone(),
            outputs,
        })
    }

    /// When a call waits for time to pass, like the backoff of a retry.
    pub fn wake_at(&self) -> Option<std::time::Instant> {
        self.run.wake_at()
    }

    /// Goes on after a wait, and gives the tasks that the calls ask for.
    pub fn wake(&mut self, now: std::time::Instant) -> Result<Vec<Task>, DriverError> {
        self.run.clock = self.run.clock.max(now);
        self.settle(None)
    }

    /// The tasks that the calls stopped, like those past their deadline.
    pub fn cancellations(&mut self) -> Vec<TaskId> {
        self.run.cancellations()
    }

    /// The files the calls deliver: their uploads, like artifacts to S3, and the output artifacts
    /// that [`Adapter::finish`] gave the results folder.
    pub fn take_deliveries(&mut self) -> Vec<Delivery> {
        self.run.take_deliveries()
    }

    /// Delivers the output artifacts of every step to `results`, at `<node>/<artifact>`, as the
    /// native run does once it ends.
    pub fn finish(&mut self, results: &std::path::Path) {
        let artifacts: Vec<(String, String)> = (self.run.output_artifacts().into_iter())
            .map(|(step, artifact, location)| {
                let target = results.join(step).join(artifact);
                let target = target.to_string_lossy().into_owned();
                (target, location.to_string_lossy().into_owned())
            })
            .collect();
        for (target, location) in artifacts {
            self.run.artifact_delivery(target, location);
        }
    }

    pub fn warnings(&self) -> &[String] {
        &self.run.warnings
    }

    /// Evaluates an Argo text of the step `site` as the native run expands it, with the globals
    /// and `values` by tag: a list is the outputs of a loop, a map an item, null no value.
    pub fn evaluate(
        &mut self,
        text: &str,
        form: Form,
        site: &str,
        values: &Map<String, Json>,
    ) -> Result<Json, String> {
        let mut tags = self.run.globals.clone();
        for (name, value) in values {
            tags.extend(tag_values(name, value)?);
        }
        let text = crate::tags::replace(text, &Variables::new(tags), true)?;
        let site = format!("{}{site}", self.run.workflow_name);
        if form == Form::Condition {
            return self.run.decide_when(&text, &site).map(Json::Bool);
        }
        if form == Form::Text {
            return Ok(Json::String(text));
        }
        let mut warnings = Vec::new();
        let list = loop_items(&text, &site, self.run.options.assume_inputs, &mut warnings);
        for warning in warnings {
            self.run.warn(warning);
        }
        let mut found = Vec::new();
        for (index, item) in list?.iter().enumerate() {
            found.push(match form {
                Form::Labels => Json::String(item_variables("", index, item)?.1),
                _ => serde_json::from_str(&item.to_json()).map_err(|error| error.to_string())?,
            });
        }
        Ok(Json::Array(found))
    }

    /// Goes on with the call `only`, or every call that runs, until none changes, as a pass of
    /// the native run does. Calls share no state that another call's change moves on.
    fn settle(&mut self, only: Option<usize>) -> Result<Vec<Task>, DriverError> {
        if !self.run.options.assume_inputs {
            self.run.clock = std::time::Instant::now();
        }
        loop {
            self.run.changed = false;
            self.run.expire();
            let all = self.calls.iter().copied();
            for index in only.map_or_else(|| all.collect(), |index| vec![index]) {
                if !self.run.nodes[index].phase.fulfilled() {
                    self.run.reenter(index).map_err(driver_error)?;
                }
            }
            if !self.run.changed {
                break;
            }
        }
        let emitted = std::mem::take(&mut self.run.emitted);
        for task in &emitted {
            let owner = only.map(CallId).or_else(|| self.holder(task.id));
            if let Some(owner) = owner {
                self.owners.insert(task.id, owner.0);
            }
        }
        Ok(emitted)
    }
}

/// The tags of a call by name, as a scope: values, and artifacts as Files or Directories.
fn scope_of(values: &[(String, Json)]) -> crate::run::Result<Scope> {
    let mut scope = Scope::default();
    for (name, value) in values {
        if name.contains(".artifacts.") {
            if let Some(artifact) =
                artifact_of(name, value).map_err(|error| Failure::error(error.message))?
            {
                scope.artifacts.insert(name.clone(), artifact);
            }
            continue;
        }
        (scope.values).extend(tag_values(name, value).map_err(Failure::error)?);
    }
    Ok(scope)
}

/// The exit hook that an `onExit` template name gives.
fn exit_hook(template: String) -> LifecycleHook {
    LifecycleHook {
        template,
        ..LifecycleHook::default()
    }
}

/// The argument of an input artifact from a File or Directory of wir: its location is a path,
/// or an address like `s3://bucket/key`.
fn artifact_of(name: &str, value: &Json) -> Result<Option<ArtifactValue>, DriverError> {
    if value.is_null() {
        return Ok(None);
    }
    let Some(location) = value.get("location").and_then(Json::as_str) else {
        return Err(DriverError::new(format!(
            "artifact {name} needs a File or Directory with a location"
        )));
    };
    let location = match location.strip_prefix("file://") {
        Some(path) => Location::File(PathBuf::from(path)),
        None if location.contains("://") => Location::Url(location.to_owned()),
        None => Location::File(PathBuf::from(location)),
    };
    Ok(Some(ArtifactValue {
        name: name.to_owned(),
        path: String::new(),
        location: Some(location),
        optional: false,
        directory: value.get("class").and_then(Json::as_str) == Some("Directory"),
    }))
}

/// The tags that a value of a compiled text gives: text as it is, the outputs of a loop as the
/// JSON list Argo aggregates them to, and an item with its fields.
fn tag_values(name: &str, value: &Json) -> Result<Vec<(String, String)>, String> {
    let text = |value: &Json| match value {
        Json::String(text) => text.clone(),
        other => Value::from_json(other).to_go_string(),
    };
    Ok(match value {
        Json::Null => Vec::new(),
        Json::Array(list) => {
            let results = name.ends_with(".outputs.result");
            let items = (list.iter().filter(|item| !item.is_null()))
                .map(|item| match results {
                    true => json_or_text(&text(item)),
                    false => Value::string(text(item)),
                })
                .collect();
            vec![(name.to_owned(), Value::Array(items).to_json())]
        }
        Json::Object(_) if name == "item" => {
            let (values, _) = item_variables("", 0, &Value::from_json(value))?;
            values.into_iter().collect()
        }
        other => vec![(name.to_owned(), text(other))],
    })
}

/// The items of a loop whose text is substituted, as `withParam` or `withSequence` gives them.
fn loop_items(
    text: &str,
    site: &str,
    assume_inputs: bool,
    warnings: &mut Vec<String>,
) -> Result<Vec<Value>, String> {
    let at = Position::default();
    let entry = |key: &str, value: Yaml| Entry {
        key: key.to_owned(),
        position: at,
        value: Node::new(value, at),
    };
    let mut fields = vec![entry("name", Yaml::String(site.to_owned()))];
    let sequence = match serde_json::from_str::<Json>(text) {
        Ok(Json::Object(object)) => {
            let bound = |key: &str| object.get(key).map(|value| AnyString(tag_text(value)));
            Some(Sequence {
                count: bound("count"),
                start: bound("start"),
                end: bound("end"),
                format: object.get("format").map(tag_text).unwrap_or_default(),
            })
        }
        _ => {
            fields.push(entry("withParam", Yaml::String(text.to_owned())));
            None
        }
    };
    let node = Node::new(Yaml::Mapping(fields), at);
    let mut warn = |warning| warnings.push(warning);
    items(&node, sequence.as_ref(), assume_inputs, &mut warn)
}

/// A bound of a sequence as Argo reads it: a text, or a number as its JSON text.
fn tag_text(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// What names a template of the workflow, or of the WorkflowTemplate `reference` names.
fn holder_of(name: &str, reference: Option<&(String, bool)>) -> Holder {
    Holder {
        template: name.to_owned(),
        template_ref: reference.map(|(document, cluster_scope)| TemplateRef {
            name: document.clone(),
            template: name.to_owned(),
            cluster_scope: *cluster_scope,
        }),
        inline: None,
    }
}

/// The step `[1].use` or the DAG task `.use` of a template, as written.
fn site_node(template: &Node, call: &str) -> Option<Node> {
    let named = |nodes: &[Node], name: &str| {
        (nodes.iter())
            .find(|node| node.get("name").and_then(Node::as_str) == Some(name))
            .cloned()
    };
    let list = |node: Option<&Node>| match node.map(|node| &node.value) {
        Some(Yaml::Sequence(nodes)) => nodes.clone(),
        _ => Vec::new(),
    };
    match call.strip_prefix('[') {
        Some(rest) => {
            let (group, name) = rest.split_once("].")?;
            let groups = list(template.get("steps"));
            named(&list(groups.get(group.parse::<usize>().ok()?)), name)
        }
        None => named(
            &list(template.get("dag").and_then(|dag| dag.get("tasks"))),
            call.strip_prefix('.')?,
        ),
    }
}
