//! Runs child Workflow resources inside the parent execution graph.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::rc::Rc;

use argo_parser::model::{ResourceTemplate, normalize};
use argo_parser::{Entry, Node, Value as Yaml, parse_yaml};
use workflow_graph::{Task, TaskId};

use crate::checks::without_tags;
use crate::conditions::{Verdict, judge};
use crate::items::yaml_value;
use crate::library::{Document, Kind};
use crate::node::Phase;
use crate::run::{Failure, Result, Run, empty_mapping, entry_mut, scalar_text};
use crate::start::name_suffix;
use crate::time::rfc3339;
use crate::value::Value;

/// Workflows that start workflows nest at most this deep.
pub(crate) const MAX_NESTING: usize = 10;

/// A workflow that a resource template of this run started.
pub(crate) struct Child {
    pub name: String,
    /// The manifest with the child's name, which the Workflow object shows.
    pub manifest: Node,
    pub run: Box<Run>,
    /// How many warnings of the child this run has passed on.
    pub warned: usize,
}

/// Whether a resource template creates, applies or gets an Argo Workflow, which the run runs
/// itself.
pub(crate) fn runs_workflow(resource: &ResourceTemplate) -> bool {
    // Tags that name no variable yet stand for text, so that the manifest parses.
    let Some(manifest) = parse_yaml(&without_tags(&resource.manifest)).ok().flatten() else {
        return false;
    };
    let text = |key: &str| manifest.get(key).and_then(Node::as_str).unwrap_or("");
    text("kind") == "Workflow"
        && text("apiVersion").starts_with("argoproj.io/")
        && matches!(resource.action.as_str(), "create" | "apply" | "get")
}

/// Sets a mapping entry.
fn set(node: &mut Node, key: &str, value: Node) {
    match entry_mut(node, key) {
        Some(existing) => *existing = value,
        None => {
            if let Yaml::Mapping(entries) = &mut node.value {
                entries.push(Entry {
                    key: key.to_owned(),
                    position: value.position,
                    value,
                });
            }
        }
    }
}

impl Run {
    /// Runs a resource template on a Workflow: `create` and `apply` start the child, `get`
    /// finds a child by name. With conditions the node waits for them; without, it succeeds.
    pub(crate) fn start_workflow(&mut self, index: usize) -> Result<()> {
        let Some(processed) = self.nodes[index].processed.clone() else {
            return Ok(());
        };
        let template = &processed.template;
        let Some(resource) = &template.resource else {
            return Ok(());
        };
        let mut manifest = parse_yaml(&resource.manifest)
            .ok()
            .flatten()
            .unwrap_or_else(empty_mapping);
        let metadata = manifest.get("metadata");
        let text = |key: &str| {
            metadata
                .and_then(|metadata| metadata.get(key))
                .map(scalar_text)
                .unwrap_or_default()
        };
        let name = match text("name") {
            name if name.is_empty() && resource.action != "get" => {
                format!(
                    "{}{}",
                    text("generateName"),
                    name_suffix(&self.nodes[index].name)
                )
            }
            name => name,
        };
        let namespace = match text("namespace") {
            namespace if namespace.is_empty() => self.document.namespace().to_owned(),
            namespace => namespace,
        };
        let known = self
            .children
            .iter()
            .position(|child| child.name == name && child.run.document.namespace() == namespace);
        let child = match (resource.action.as_str(), known) {
            ("get" | "apply", Some(child)) => child,
            ("get", None) => {
                return Err(Failure::unsupported(format!(
                    "template {} gets Workflow {name}, which this run did not start; uwe runs workflows without Kubernetes and does not support this (decision 18)",
                    template.name
                )));
            }
            (_, Some(_)) => {
                let message = format!("workflows.argoproj.io \"{name}\" already exists");
                self.fail(index, Phase::Error, message);
                return Ok(());
            }
            (_, None) => {
                if self.nesting >= MAX_NESTING {
                    return Err(Failure::error(format!(
                        "workflows start other workflows more than {MAX_NESTING} levels deep"
                    )));
                }
                if !matches!(
                    metadata.map(|metadata| &metadata.value),
                    Some(Yaml::Mapping(_))
                ) {
                    set(&mut manifest, "metadata", empty_mapping());
                }
                if let Some(metadata) = entry_mut(&mut manifest, "metadata") {
                    let position = metadata.position;
                    set(
                        metadata,
                        "name",
                        Node::new(Yaml::String(name.clone()), position),
                    );
                    set(
                        metadata,
                        "namespace",
                        Node::new(Yaml::String(namespace.clone()), position),
                    );
                }
                let mut node = manifest.clone();
                normalize(&mut node);
                let document = Rc::new(Document {
                    path: self.document.path.clone(),
                    index: self.document.index,
                    kind: Kind::Workflow,
                    name: name.clone(),
                    node,
                });
                let options = crate::Options {
                    entrypoint: None,
                    parameters: Vec::new(),
                    print_warnings: false,
                    now: Some(self.created),
                    ..self.options.clone()
                };
                let mut run = match Run::new(document, Rc::clone(&self.library), options) {
                    Ok(run) => run,
                    Err(failure) if failure.unsupported => return Err(failure),
                    Err(failure) => {
                        let message = format!("workflow {name}: {}", failure.message);
                        self.fail(index, Phase::Error, message);
                        return Ok(());
                    }
                };
                run.nesting = self.nesting + 1;
                // A held child resumes by its own name, not with the whole run.
                if run.held {
                    run.warnings.pop();
                    let message = format!(
                        "the workflow is suspended until `uwe resume {} {name}`",
                        run.run_name()
                    );
                    run.warn(message);
                }
                run.inbound = self.nodes[index].inbound.clone();
                run.files = self.files.clone();
                run.locks = self.locks.clone();
                self.children.push(Child {
                    name,
                    manifest,
                    run: Box::new(run),
                    warned: 0,
                });
                let child = self.children.len() - 1;
                self.step_child(child, Run::operate)?;
                child
            }
        };
        let waits =
            !resource.success_condition.is_empty() || !resource.failure_condition.is_empty();
        if waits {
            self.watches.push((index, child));
            self.watch(index, child)
        } else {
            self.end_watch(index, child, Phase::Succeeded, String::new());
            Ok(())
        }
    }

    /// Lets a child go on, hands out the tasks it asks for, and updates the nodes that wait
    /// for it. Task ids come from one count for the run and its children.
    pub(crate) fn step_child(
        &mut self,
        child: usize,
        step: impl FnOnce(&mut Run) -> Result<Vec<Task>>,
    ) -> Result<()> {
        let next = self.next_task;
        let entry = &mut self.children[child];
        entry.run.next_task = next;
        let requests = crate::resume::scoped(&self.requests, &entry.name);
        entry.run.requests.extend(requests);
        let tasks = step(&mut entry.run)?;
        self.next_task = entry.run.next_task;
        let name = entry.name.clone();
        let warnings = entry.run.warnings[entry.warned..].to_vec();
        entry.warned = entry.run.warnings.len();
        self.changed |= !tasks.is_empty();
        self.emitted.extend(tasks);
        for warning in warnings {
            self.warn(format!("workflow {name}: {warning}"));
        }
        for (node, watched) in self.watches.clone() {
            if watched == child {
                self.watch(node, child)?;
            }
        }
        Ok(())
    }

    /// Whether the outcome of a task belongs to this run or to one of its children.
    pub(crate) fn owns(&self, id: TaskId) -> bool {
        self.tasks.contains_key(&id) || self.children.iter().any(|child| child.run.owns(id))
    }

    /// Whether tasks of this run or of its children are still out.
    pub(crate) fn busy(&self) -> bool {
        (self.root.is_none() && self.spec.synchronization.is_some())
            || !self.tasks.is_empty()
            || !self.waits.is_empty()
            || self.held
            || self.nodes.iter().any(|node| node.phase == Phase::Pending)
            || self.children.iter().any(|child| child.run.busy())
    }

    /// The tasks that end the run: those of its exit handler, or of its root.
    pub(crate) fn last_tasks(&self) -> Vec<TaskId> {
        match self.exit_node.or(self.root) {
            Some(node) => self.outbound(node),
            None => self.inbound.clone(),
        }
    }

    /// The phase of the run as its Workflow object shows it, and its message. It is `Running`
    /// until the root and the exit handler end and no task is out, or until nothing can go on.
    fn workflow_phase(&self) -> (Phase, String) {
        let fulfilled =
            |node: Option<usize>| node.is_some_and(|node| self.nodes[node].phase.fulfilled());
        let ended = fulfilled(self.root)
            && (self.exit_node.is_none() || fulfilled(self.exit_node))
            && !self.busy();
        if !ended && self.busy() {
            return (Phase::Running, String::new());
        }
        let root = self.root.map(|root| &self.nodes[root]);
        let exit = self.exit_node.map(|exit| &self.nodes[exit]);
        let phase = match root.map(|root| root.phase) {
            Some(phase) if phase.failed_or_error() => phase,
            Some(phase) if phase.fulfilled() => exit
                .map(|exit| exit.phase)
                .filter(|phase| phase.failed_or_error())
                .unwrap_or(Phase::Succeeded),
            _ => Phase::Error,
        };
        let message = match phase {
            Phase::Succeeded => String::new(),
            _ => self
                .first_failure
                .clone()
                .or_else(|| root.map(|root| root.message.clone()))
                .unwrap_or_default(),
        };
        (phase, message)
    }

    /// The Workflow object of a child, as Argo's executor reads it from Kubernetes.
    fn workflow_object(&self, child: usize) -> Value {
        let child = &self.children[child];
        let (phase, message) = child.run.workflow_phase();
        let text = |text: &str| Value::string(text);
        let mut status = BTreeMap::from([
            ("phase".to_owned(), text(phase.name())),
            ("startedAt".to_owned(), text(&rfc3339(child.run.created))),
        ]);
        if phase != Phase::Running {
            let finished = child
                .run
                .nodes
                .iter()
                .filter_map(|node| node.finished)
                .max()
                .unwrap_or(child.run.created);
            status.insert("finishedAt".to_owned(), text(&rfc3339(finished)));
        }
        if !message.is_empty() {
            status.insert("message".to_owned(), text(&message));
        }
        let parameters: Vec<Value> = child
            .run
            .globals
            .iter()
            .filter_map(|(key, value)| {
                let name = key.strip_prefix("workflow.outputs.parameters.")?;
                Some(Value::Map(BTreeMap::from([
                    ("name".to_owned(), text(name)),
                    ("value".to_owned(), text(value)),
                ])))
            })
            .collect();
        if !parameters.is_empty() {
            let outputs = BTreeMap::from([("parameters".to_owned(), Value::Array(parameters))]);
            status.insert("outputs".to_owned(), Value::Map(outputs));
        }
        let mut object = match yaml_value(&child.manifest) {
            Value::Map(object) => object,
            _ => BTreeMap::new(),
        };
        object.insert("status".to_owned(), Value::Map(status));
        Value::Map(object)
    }

    /// Applies the conditions of a waiting resource node to its child now.
    fn watch(&mut self, index: usize, child: usize) -> Result<()> {
        if self.nodes[index].phase.fulfilled() {
            return Ok(());
        }
        let Some(processed) = self.nodes[index].processed.clone() else {
            return Ok(());
        };
        let Some(resource) = &processed.template.resource else {
            return Ok(());
        };
        let object = self.workflow_object(child);
        let verdict = judge(
            &resource.success_condition,
            &resource.failure_condition,
            &object,
        );
        let (phase, _) = self.children[child].run.workflow_phase();
        match verdict {
            Err(message) => self.fail(index, Phase::Error, message),
            Ok(Verdict::Failed(message)) => self.end_watch(index, child, Phase::Failed, message),
            Ok(Verdict::Succeeded) => self.end_watch(index, child, Phase::Succeeded, String::new()),
            Ok(Verdict::Waiting) if phase != Phase::Running => {
                let message = format!(
                    "workflow {} ended {}, and neither the success condition nor the failure condition holds; Argo would wait for it forever",
                    self.children[child].name,
                    phase.name()
                );
                self.end_watch(index, child, Phase::Error, message);
            }
            Ok(Verdict::Waiting) => {}
        }
        Ok(())
    }

    /// Ends a resource node on a child, with output parameters that read the Workflow object.
    fn end_watch(&mut self, index: usize, child: usize, phase: Phase, message: String) {
        let object = self.workflow_object(child);
        if let Some(processed) = self.nodes[index].processed.clone() {
            let outputs = self.resource_outputs(&processed.template, &object);
            self.nodes[index].outputs = outputs;
        }
        match phase {
            Phase::Succeeded => self.set_phase(index, phase, message),
            _ => self.fail(index, phase, message),
        }
    }
}
