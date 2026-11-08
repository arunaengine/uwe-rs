//! Compiles the workflow and its steps and DAG templates into graphs, one per template.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use argo_parser::model::{ContinueOn, Template};
use argo_parser::{Node, Value as Yaml};
use argo_runtime::adapter::Adapter;
use wir::{Dependency, ExprForm, Graph, Kind, Port, Status, StatusGroup, Type};

use crate::sites::Site;
use crate::tags::single_tag;
use crate::texts::{VALUE, text_op};
use crate::{Kept, edge, end, op, path_type, port, text, typed};

/// Where a template is: its name, and the WorkflowTemplate that holds it with whether that
/// is cluster scoped.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Place {
    pub name: String,
    pub reference: Option<(String, bool)>,
}

/// What a call reaches: a leaf template that a dialect task runs, or the graph of a steps
/// or DAG template with its ports.
pub(crate) enum Target {
    Leaf(Box<Template>),
    Graph(String, Vec<Port>, Vec<Port>),
}

/// The values that the ops of a graph can read: the graph inputs, and what the steps or
/// tasks before made, by their prefix like `steps.make`, as an op and its outputs.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scope {
    pub inputs: Vec<Port>,
    pub made: BTreeMap<String, (String, Vec<Port>)>,
}

pub(crate) struct Compiler {
    pub adapter: Adapter,
    pub graphs: Vec<Graph>,
    /// The graph and its ports of each steps or DAG template compiled so far, by graph name.
    done: BTreeMap<String, (Vec<Port>, Vec<Port>)>,
    /// The templates being compiled, of which a call is a recursion.
    active: Vec<String>,
}

impl Compiler {
    pub fn new(adapter: Adapter) -> Self {
        Compiler {
            adapter,
            graphs: Vec::new(),
            done: BTreeMap::new(),
            active: Vec::new(),
        }
    }

    /// What a call of the template at `place` reaches; the graph of a steps or DAG template
    /// is compiled on its first call.
    pub fn target(&mut self, place: &Place, at: &str) -> Result<Target, Kept> {
        let node = (self.adapter.template(&place.name, place.reference.as_ref()))
            .map_err(|reason| kept(at, &reason))?;
        let template = typed(&node).map_err(|reason| kept(at, &reason))?;
        if template.steps.is_empty() && node.get("steps").is_none() && template.dag.is_none() {
            if let Some(reason) = leaf_reason(&template) {
                return Err(kept(at, &format!("template {}: {reason}", template.name)));
            }
            if let Some(name) = unsafe_name(&template) {
                return Err(kept(at, &format!("the name {name} holds a tag")));
            }
            return Ok(Target::Leaf(Box::new(template)));
        }
        let name = graph_name(place);
        if self.active.contains(&name) {
            let reason = format!("template {} calls itself", template.name);
            return Err(kept(at, &reason));
        }
        if let Some((takes, gives)) = self.done.get(&name) {
            return Ok(Target::Graph(name, takes.clone(), gives.clone()));
        }
        self.active.push(name.clone());
        let built = self.template_graph(place, &name, &template, &node);
        self.active.pop();
        let (takes, gives) = built?;
        self.done
            .insert(name.clone(), (takes.clone(), gives.clone()));
        Ok(Target::Graph(name, takes, gives))
    }

    /// The graph of a steps or DAG template.
    fn template_graph(
        &mut self,
        place: &Place,
        name: &str,
        template: &Template,
        node: &Node,
    ) -> Result<(Vec<Port>, Vec<Port>), Kept> {
        let at = |reason: &str| kept(name, reason);
        if let Some(name) = unsafe_name(template) {
            return Err(at(&format!("the name {name} holds a tag")));
        }
        if template.retry_strategy.is_some() {
            return Err(at(
                "a retryStrategy of a steps or DAG template retries a graph",
            ));
        }
        if template.parallelism.is_some() || template.fail_fast == Some(true) {
            return Err(at(
                "parallelism and failFast limit the children of a template together",
            ));
        }
        if template.active_deadline_seconds.is_some() || template.timeout.is_some() {
            return Err(at("a deadline of a steps or DAG template stops a graph"));
        }
        if template.synchronization.is_some() || template.memoize.is_some() {
            return Err(at("synchronization and memoize of a steps or DAG template"));
        }
        let mut inputs = vec![port("node.path", path_type())];
        for parameter in &template.inputs.parameters {
            let maybe = Type::Optional(Box::new(text()));
            let mut input = port(format!("inputs.parameters.{}", parameter.name), maybe);
            let value = parameter.value.as_ref().or(parameter.default.as_ref());
            input.default = value.map(|value| value.0.clone().into());
            if parameter.value_from.is_some() {
                return Err(at(
                    "an input parameter of a steps or DAG template reads valueFrom",
                ));
            }
            inputs.push(input);
        }
        for artifact in &template.inputs.artifacts {
            inputs.push(port(
                format!("inputs.artifacts.{}", artifact.name),
                crate::artifact(),
            ));
        }
        let mut graph = Graph {
            name: name.to_owned(),
            inputs: inputs.clone(),
            outputs: Vec::new(),
            ops: Vec::new(),
            edges: Vec::new(),
        };
        let mut scope = Scope {
            inputs,
            made: BTreeMap::new(),
        };
        let prefix = match template.dag.is_some() {
            true => "tasks",
            false => "steps",
        };
        match &template.dag {
            None => self.steps(place, &mut graph, &mut scope, node)?,
            Some(_) => self.dag(place, &mut graph, &mut scope, node)?,
        }
        for parameter in &template.outputs.parameters {
            let from = parameter.value_from.as_ref();
            let tag = from.and_then(|from| single_tag(&from.parameter));
            let plain = from.is_some_and(|from| {
                from.expression.is_empty() && from.default.is_none() && from.path.is_empty()
            });
            let source = tag
                .filter(|_| plain)
                .and_then(|tag| made_port(&scope, prefix, tag))
                .filter(|(_, found)| !matches!(found.r#type, Type::Array(_)));
            let name = format!("outputs.parameters.{}", parameter.name);
            if let Some((producer, found)) = source {
                push_output(&mut graph, &name, &producer, found)?;
                continue;
            }
            let code = output_text(parameter).map_err(|reason| at(&reason))?;
            let found = |tag: &crate::tags::Tag| crate::sites::source(&scope, tag);
            let (made, wires) =
                text_op(&name, &code, ExprForm::Text, found).map_err(|reason| at(&reason))?;
            graph.ops.push(made);
            graph.edges.extend(wires);
            graph.outputs.push(port(name.clone(), text()));
            graph
                .edges
                .push(edge(end(Some(&name), VALUE), end(None, &name)));
        }
        for artifact in &template.outputs.artifacts {
            let tag = single_tag(&artifact.from);
            let Some((producer, found)) = tag.and_then(|tag| made_port(&scope, prefix, tag)) else {
                let reason = format!("output artifact {} is no File of one tag", artifact.name);
                return Err(at(&reason));
            };
            let name = format!("outputs.artifacts.{}", artifact.name);
            push_output(&mut graph, &name, &producer, found)?;
        }
        let ports = (graph.inputs.clone(), graph.outputs.clone());
        self.graphs.push(graph);
        Ok(ports)
    }

    /// The steps of a template: each group starts once the group before ended.
    fn steps(
        &mut self,
        place: &Place,
        graph: &mut Graph,
        scope: &mut Scope,
        node: &Node,
    ) -> Result<(), Kept> {
        let mut before: Vec<(String, ContinueOn)> = Vec::new();
        for (position, group) in sequence(node.get("steps")).iter().enumerate() {
            let mut made = Vec::new();
            let mut current = Vec::new();
            for step in sequence(Some(group)) {
                let site =
                    Site::of_step(position, &step).map_err(|reason| kept(&graph.name, &reason))?;
                let built = self.site(place, graph, scope, &site)?;
                if let Some(op) = graph.ops.iter_mut().find(|op| op.path == built.op) {
                    for (earlier, continues) in &before {
                        op.after.push(Dependency {
                            op: earlier.clone(),
                            on: statuses(continues),
                        });
                    }
                }
                handle_continue(graph, &built.op, site.continue_on);
                current.push((built.op.clone(), site.continue_on));
                made.push((format!("steps.{}", site.name), built));
            }
            for (prefix, built) in made {
                scope.made.insert(prefix, (built.op, built.outputs));
            }
            before = current;
        }
        Ok(())
    }

    /// The tasks of a DAG that its targets need, each with its `depends` as control.
    fn dag(
        &mut self,
        place: &Place,
        graph: &mut Graph,
        scope: &mut Scope,
        node: &Node,
    ) -> Result<(), Kept> {
        let template = typed(node).map_err(|reason| kept(&graph.name, &reason))?;
        let Some(dag) = template.dag else {
            return Ok(());
        };
        let raw = sequence(node.get("dag").and_then(|dag| dag.get("tasks")));
        let mut ancestors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut trees = BTreeMap::new();
        for task in &dag.tasks {
            if !task.depends.is_empty() && !task.dependencies.is_empty() {
                return Err(kept(
                    &graph.name,
                    "a task with both depends and dependencies",
                ));
            }
            let (tree, names) = crate::depends::control(task, &dag.tasks)
                .map_err(|reason| kept(&graph.name, &reason))?;
            trees.insert(task.name.clone(), (tree, names));
        }
        let mut order = Vec::new();
        let targets: Vec<String> = match dag.target.trim().is_empty() {
            true => {
                let depended: BTreeSet<&String> =
                    trees.values().flat_map(|(_, names)| names).collect();
                let names = dag.tasks.iter().map(|task| task.name.clone());
                names.filter(|name| !depended.contains(name)).collect()
            }
            false => dag.target.split_whitespace().map(str::to_owned).collect(),
        };
        for target in &targets {
            visit(target, &trees, &mut order, &mut Vec::new())
                .map_err(|reason| kept(&graph.name, &reason))?;
        }
        for name in &order {
            let names = trees
                .get(name)
                .map(|(_, names)| names.clone())
                .unwrap_or_default();
            let mut all = BTreeSet::new();
            for dependency in &names {
                all.insert(dependency.clone());
                all.extend(ancestors.get(dependency).cloned().unwrap_or_default());
            }
            ancestors.insert(name.clone(), all);
        }
        let declared = (dag.tasks.iter()).filter(|task| order.contains(&task.name));
        for task in declared.collect::<Vec<_>>() {
            let raw = (raw.iter())
                .find(|node| node.get("name").and_then(Node::as_str) == Some(&task.name));
            let Some(raw) = raw else { continue };
            let site = Site::of_task(raw).map_err(|reason| kept(&graph.name, &reason))?;
            let mut visible = Scope {
                inputs: scope.inputs.clone(),
                made: BTreeMap::new(),
            };
            for ancestor in &ancestors[&task.name] {
                let prefix = format!("tasks.{ancestor}");
                if let Some(made) = scope.made.get(&prefix) {
                    visible.made.insert(prefix, made.clone());
                }
            }
            let built = self.site(place, graph, &visible, &site)?;
            let (tree, names) = &trees[&task.name];
            let tasks: BTreeSet<String> = (graph.ops.iter())
                .filter(|op| matches!(op.kind, Kind::Task(_)))
                .map(|op| op.path.clone())
                .collect();
            let tree = crate::depends::daemons(tree, &tasks);
            if let Some(op) = graph.ops.iter_mut().find(|op| op.path == built.op)
                && !names.is_empty()
            {
                op.handles = crate::depends::handled(&tree);
                op.depends = Some(tree);
            }
            handle_continue(graph, &built.op, site.continue_on);
            scope
                .made
                .insert(format!("tasks.{}", site.name), (built.op, built.outputs));
        }
        Ok(())
    }
}

/// What a site compiled into: its op, which other ops wait for, and its outputs.
pub(crate) struct Built {
    pub op: String,
    pub outputs: Vec<Port>,
}

/// The statuses of a step that let the next group start.
fn statuses(continues: &ContinueOn) -> Vec<Status> {
    let mut on = vec![Status::Succeeded, Status::Skipped];
    if continues.failed {
        on.push(Status::Failed);
    }
    if continues.error {
        on.push(Status::Errored);
    }
    on
}

/// A status group that handles the failures a step or task continues on, so that they do not
/// fail the graph, as `continueOn` lets the template go on.
fn handle_continue(graph: &mut Graph, path: &str, continues: ContinueOn) {
    let mut allow = Vec::new();
    if continues.failed {
        allow.push(Status::Failed);
    }
    if continues.error {
        allow.push(Status::Errored);
    }
    if allow.is_empty() {
        return;
    }
    let mut group = op(
        format!("{path}.continue"),
        Kind::StatusGroup(StatusGroup {
            members: vec![path.to_owned()],
            allow: allow.clone(),
        }),
    );
    group.handles.push(Dependency {
        op: path.to_owned(),
        on: allow,
    });
    graph.ops.push(group);
}

/// The tasks a target needs, dependencies first, in a DAG without cycles.
fn visit(
    name: &str,
    trees: &BTreeMap<String, (wir::Control, Vec<String>)>,
    order: &mut Vec<String>,
    visiting: &mut Vec<String>,
) -> Result<(), String> {
    if order.iter().any(|known| known == name) {
        return Ok(());
    }
    if visiting.iter().any(|known| known == name) {
        return Err(format!(
            "the tasks of the DAG depend on each other in a cycle at {name}"
        ));
    }
    let Some((_, names)) = trees.get(name) else {
        return Err(format!("target {name} is no task of the DAG"));
    };
    visiting.push(name.to_owned());
    for dependency in names {
        visit(dependency, trees, order, visiting)?;
    }
    visiting.pop();
    order.push(name.to_owned());
    Ok(())
}

/// The op output that a tag like `steps.make.outputs.parameters.text` reads.
fn made_port(scope: &Scope, prefix: &str, tag: &str) -> Option<(String, Port)> {
    let rest = tag.strip_prefix(prefix)?.strip_prefix('.')?;
    let (step, port) = rest.split_once('.')?;
    let (op, outputs) = scope.made.get(&format!("{prefix}.{step}"))?;
    let found = outputs.iter().find(|output| output.name == port)?;
    Some((op.clone(), found.clone()))
}

/// A graph output from an op output; the outputs of a loop are lists, which Argo gives as
/// JSON text.
fn push_output(graph: &mut Graph, name: &str, producer: &str, found: Port) -> Result<(), Kept> {
    if matches!(found.r#type, Type::Array(_)) {
        return Err(kept(
            &graph.name,
            &format!("{name} reads the Files of a loop, which wir does not list"),
        ));
    }
    graph.outputs.push(port(name, found.r#type));
    graph
        .edges
        .push(edge(end(Some(producer), &found.name), end(None, name)));
    Ok(())
}

/// The Argo text that gives an output parameter of a steps or DAG template.
fn output_text(parameter: &argo_parser::model::Parameter) -> Result<String, String> {
    let name = &parameter.name;
    let Some(from) = &parameter.value_from else {
        let value = parameter.value.as_ref();
        return value
            .map(|value| value.0.clone())
            .ok_or_else(|| format!("output parameter {name} has no value"));
    };
    if from.default.is_some() {
        return Err(format!(
            "output parameter {name} has a default for when its source fails"
        ));
    }
    match (from.expression.is_empty(), from.parameter.is_empty()) {
        (false, _) => Ok(format!("{{{{={}}}}}", from.expression)),
        (true, false) => Ok(from.parameter.clone()),
        (true, true) => Err(format!("output parameter {name} has no source")),
    }
}

/// A name of an input or output of a template that is no wir name, like one with a tag.
fn unsafe_name(template: &Template) -> Option<&str> {
    let inputs = &template.inputs;
    let outputs = &template.outputs;
    let parameters = (inputs.parameters.iter())
        .chain(&outputs.parameters)
        .map(|p| p.name.as_str());
    let artifacts = (inputs.artifacts.iter())
        .chain(&outputs.artifacts)
        .map(|a| a.name.as_str());
    parameters
        .chain(artifacts)
        .find(|name| crate::safe(name) != *name || name.is_empty())
}

/// Why a leaf template stays native, if it does.
fn leaf_reason(template: &Template) -> Option<&'static str> {
    if template.suspend.is_some() {
        return Some("a suspend template waits for a person or a time");
    }
    if template.resource.is_some() {
        return Some("a resource template changes Kubernetes objects");
    }
    if template.daemon == Some(true) {
        return Some("a daemon serves other steps until its template ends");
    }
    if !template.sidecars.is_empty() {
        return Some("sidecars run beside the main container until it ends");
    }
    if template.synchronization.is_some() {
        return Some("synchronization holds locks across the workflow");
    }
    if template.plugin.is_some() || template.data.is_some() {
        return Some("plugin and data templates");
    }
    let runs = template.container.is_some()
        || template.script.is_some()
        || template.container_set.is_some()
        || template.http.is_some();
    (!runs).then_some("a template without a type")
}

/// The graph name of a template: `template.<name>`, with its WorkflowTemplate before the name.
pub(crate) fn graph_name(place: &Place) -> String {
    let name = crate::safe(&place.name);
    match &place.reference {
        None => format!("template.{name}"),
        Some((document, false)) => format!("template.{}.{name}", crate::safe(document)),
        Some((document, true)) => format!("cluster.{}.{name}", crate::safe(document)),
    }
}

/// The items of a YAML sequence.
pub(crate) fn sequence(node: Option<&Node>) -> Vec<Node> {
    match node.map(|node| &node.value) {
        Some(Yaml::Sequence(items)) => items.clone(),
        _ => Vec::new(),
    }
}

pub(crate) fn kept(path: &str, reason: &str) -> Kept {
    Kept {
        path: path.to_owned(),
        reason: reason.to_owned(),
    }
}
