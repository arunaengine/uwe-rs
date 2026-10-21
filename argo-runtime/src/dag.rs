//! DAG templates (workflow/controller/dag.go and workflow/common/ancestry.go): tasks that run
//! when their `depends` expression, or their `dependencies`, allow it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::sync::OnceLock;

use argo_parser::model::DagTask;
use argo_parser::{Node, Value as Yaml, decode};
use regex::Regex;
use workflow_graph::TaskId;

use crate::items::{item_variables, items, items_unless_skipped, loops, substitute_item};
use crate::node::{NodeKind, Phase};
use crate::run::{Args, Call, Expanded, Processed, Result, Run};
use crate::scope::Scope;
use crate::steps::{base_name, exit_hook, holder};
use crate::tags::replace_node;
use crate::value::Value;
use crate::when::should_execute;

/// A DAG task's dependencies and its `depends` expression with bare names expanded.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Dependencies {
    pub names: Vec<String>,
    pub logic: String,
}

fn task_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"([a-zA-Z0-9][-a-zA-Z0-9]*?\.[A-Z][a-zA-Z]+)|([a-zA-Z0-9][-a-zA-Z0-9]*)")
            .unwrap_or_else(|_| unreachable!("the pattern is valid"))
    })
}

/// The dependencies of a task. `dependencies: [A]` means `A` succeeded, was skipped or runs
/// as a daemon, and also that it failed or errored when A continues on those; a bare name in
/// `depends` means the same.
pub(crate) fn dependencies(task: &DagTask, tasks: &[DagTask]) -> Dependencies {
    let continue_on = |name: &str| {
        tasks
            .iter()
            .find(|task| task.name == name)
            .and_then(|task| task.continue_on)
            .unwrap_or_default()
    };
    let expand = |name: &str| {
        let mut results = vec!["Succeeded", "Skipped", "Daemoned"];
        let continue_on = continue_on(name);
        if continue_on.error {
            results.push("Errored");
        }
        if continue_on.failed {
            results.push("Failed");
        }
        let alternatives: Vec<String> = results
            .iter()
            .map(|result| format!("{name}.{result}"))
            .collect();
        format!("({})", alternatives.join(" || "))
    };
    let depends = if task.depends.is_empty() {
        task.dependencies
            .iter()
            .map(|name| name.as_str())
            .collect::<Vec<_>>()
            .join(" && ")
    } else {
        task.depends.clone()
    };
    let mut names: Vec<String> = Vec::new();
    let mut logic = String::new();
    let mut last = 0;
    for captures in task_pattern().captures_iter(&depends) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        logic.push_str(&depends[last..whole.start()]);
        last = whole.end();
        if let Some(qualified) = captures.get(1) {
            let name = qualified.as_str().split('.').next().unwrap_or("");
            if !names.iter().any(|known| known == name) {
                names.push(name.to_owned());
            }
            logic.push_str(qualified.as_str());
        } else {
            let name = whole.as_str();
            if !names.iter().any(|known| known == name) {
                names.push(name.to_owned());
            }
            logic.push_str(&expand(name));
        }
    }
    logic.push_str(&depends[last..]);
    Dependencies { names, logic }
}

impl Run {
    pub fn execute_dag(&mut self, index: usize) -> Result<()> {
        let Some(processed) = self.nodes[index].processed.clone() else {
            return Ok(());
        };
        if self.fail_fast(index) {
            self.settle_fail_fast(index);
            return Ok(());
        }
        let Some(dag) = processed.template.dag.clone() else {
            return Ok(());
        };
        let tasks = dag.tasks.clone();
        let names: HashSet<&str> = tasks.iter().map(|task| task.name.as_str()).collect();
        for task in &tasks {
            if !task.depends.is_empty() && !task.dependencies.is_empty() {
                self.fail(
                    index,
                    Phase::Error,
                    format!(
                        "task {}: cannot use both depends and dependencies",
                        task.name
                    ),
                );
                return Ok(());
            }
            for dependency in dependencies(task, &tasks).names {
                if !names.contains(dependency.as_str()) {
                    self.fail(
                        index,
                        Phase::Error,
                        format!(
                            "task {}: dependency {dependency} is not a task of the DAG",
                            task.name
                        ),
                    );
                    return Ok(());
                }
            }
        }
        let targets: Vec<String> = if dag.target.trim().is_empty() {
            let depended: HashSet<String> = tasks
                .iter()
                .flat_map(|task| dependencies(task, &tasks).names)
                .collect();
            tasks
                .iter()
                .filter(|task| !depended.contains(&task.name))
                .map(|task| task.name.clone())
                .collect()
        } else {
            dag.target.split_whitespace().map(str::to_owned).collect()
        };
        let mut visited = HashSet::new();
        for target in &targets {
            self.execute_dag_task(index, &processed, &tasks, target, &mut visited)?;
        }
        let dag_name = self.nodes[index].name.clone();
        let fail_fast = dag.fail_fast.unwrap_or(!self.options.keep_going);
        match self.assess_dag(index, &tasks, &targets, fail_fast) {
            Phase::Running => return Ok(()),
            phase @ (Phase::Failed | Phase::Error) => {
                let message = targets
                    .iter()
                    .filter_map(|target| self.names.get(&format!("{dag_name}.{target}")))
                    .find(|&&node| self.nodes[node].phase.failed_or_error())
                    .map(|&node| format!("child '{}' failed", self.nodes[node].id))
                    .unwrap_or_default();
                self.set_phase(index, phase, message);
                return Ok(());
            }
            _ => {}
        }
        let mut scope = Scope::of_template(&processed);
        for task in &tasks {
            if let Some(&node) = self.names.get(&format!("{dag_name}.{}", task.name)) {
                self.task_scope(&mut scope, &task.name, node);
            }
        }
        match self.outputs_from_scope(&processed, &scope) {
            Ok(outputs) => {
                self.nodes[index].outputs = outputs;
                self.set_phase(index, Phase::Succeeded, String::new());
            }
            Err(message) => self.fail(index, Phase::Error, message),
        }
        Ok(())
    }

    /// Adds a task node to a scope as `tasks.<name>`, with the outputs of a loop as lists.
    fn task_scope(&self, scope: &mut Scope, task: &str, node: usize) {
        let prefix = format!("tasks.{task}");
        if self.nodes[node].kind == NodeKind::TaskGroup {
            let children: Vec<usize> = self.nodes[node]
                .children
                .iter()
                .copied()
                .filter(|&child| self.nodes[child].kind != NodeKind::Skipped)
                .collect();
            self.add_aggregate_scope(scope, &prefix, &children);
        }
        self.add_node_scope(scope, &prefix, node);
    }

    /// The phase of a DAG, as Argo's `assessDAGPhase` finds it: running while any of its nodes,
    /// hooks included, runs; then by the branch phases of its target tasks.
    fn assess_dag(
        &self,
        index: usize,
        tasks: &[DagTask],
        targets: &[String],
        fail_fast: bool,
    ) -> Phase {
        let dag_name = &self.nodes[index].name;
        let node_of = |task: &str| self.names.get(&format!("{dag_name}.{task}")).copied();
        let mut stack: Vec<usize> = self.nodes[index].children.clone();
        let mut seen = HashSet::new();
        while let Some(node) = stack.pop() {
            if !seen.insert(node) {
                continue;
            }
            let status = &self.nodes[node];
            if !status.phase.fulfilled() {
                return Phase::Running;
            }
            stack.extend(status.children.iter().copied());
            stack.extend(status.exit_hook);
            stack.extend(status.hooks.iter().copied());
        }
        let mut branches: BTreeMap<String, Phase> = BTreeMap::new();
        let mut ordered: Vec<String> = Vec::new();
        for target in targets {
            ancestry(target, tasks, &mut ordered);
        }
        for task in &ordered {
            let Some(node) = node_of(task) else {
                continue;
            };
            let phase = match self.nodes[node].phase {
                phase @ (Phase::Succeeded | Phase::Failed | Phase::Error) => phase,
                _ => tasks
                    .iter()
                    .find(|candidate| &candidate.name == task)
                    .map(|definition| dependencies(definition, tasks).names)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|dependency| branches.get(dependency).copied())
                    .fold(Phase::Succeeded, |worst, phase| {
                        if worst.failed_or_error() {
                            worst
                        } else {
                            phase
                        }
                    }),
            };
            branches.insert(task.clone(), phase);
        }
        let mut result = Phase::Succeeded;
        for target in targets {
            match branches.get(target) {
                None => {
                    result = Phase::Running;
                    if !fail_fast {
                        break;
                    }
                }
                Some(phase) if phase.failed_or_error() => {
                    let continue_on = tasks
                        .iter()
                        .find(|task| &task.name == target)
                        .and_then(|task| task.continue_on)
                        .unwrap_or_default();
                    let continues = match phase {
                        Phase::Failed => continue_on.failed,
                        _ => continue_on.error,
                    };
                    if continues {
                        continue;
                    }
                    result = *phase;
                    if fail_fast {
                        break;
                    }
                }
                Some(_) => {}
            }
        }
        result
    }

    /// The tasks at the end of a DAG: those that no task that ran depends on.
    pub fn dag_outbound(&self, index: usize) -> Vec<TaskId> {
        let Some(processed) = &self.nodes[index].processed else {
            return Vec::new();
        };
        let Some(dag) = &processed.template.dag else {
            return Vec::new();
        };
        let name = &self.nodes[index].name;
        let ran: Vec<(&DagTask, usize)> = dag
            .tasks
            .iter()
            .filter_map(|task| {
                self.names
                    .get(&format!("{name}.{}", task.name))
                    .map(|&node| (task, node))
            })
            .collect();
        let depended: HashSet<String> = ran
            .iter()
            .flat_map(|(task, _)| dependencies(task, &dag.tasks).names)
            .collect();
        let mut tasks = Vec::new();
        for (task, node) in ran {
            if depended.contains(&task.name) {
                continue;
            }
            for id in self.outbound(node) {
                if !tasks.contains(&id) {
                    tasks.push(id);
                }
            }
        }
        tasks
    }

    fn execute_dag_task(
        &mut self,
        dag: usize,
        processed: &Rc<Processed>,
        tasks: &[DagTask],
        name: &str,
        visited: &mut HashSet<String>,
    ) -> Result<()> {
        if !visited.insert(name.to_owned()) {
            return Ok(());
        }
        let Some(task) = tasks.iter().find(|task| task.name == name).cloned() else {
            return Ok(());
        };
        let dag_name = self.nodes[dag].name.clone();
        let node_name = format!("{dag_name}.{name}");
        let context = Rc::clone(&processed.context);
        let prefix = format!("tasks.{name}");
        if let Some(&node) = self.names.get(&node_name) {
            if !self.nodes[node].phase.fulfilled() {
                self.reenter(node)?;
                self.update_task_group(node);
            }
            if let Some(expanded) = self.nodes[node].expanded.clone() {
                self.task_hooks(node, &expanded, &context, &prefix, dag, tasks)?;
            }
            return Ok(());
        }
        let deps = dependencies(&task, tasks);
        let mut complete = true;
        for dependency in &deps.names {
            let done = self
                .names
                .get(&format!("{dag_name}.{dependency}"))
                .is_some_and(|&node| {
                    let status = &self.nodes[node];
                    status.phase.fulfilled()
                        && status
                            .exit_hook
                            .is_none_or(|exit| self.nodes[exit].phase.fulfilled())
                        && status
                            .hooks
                            .iter()
                            .all(|&hook| self.nodes[hook].phase.fulfilled())
                });
            if !done {
                complete = false;
                self.execute_dag_task(dag, processed, tasks, dependency, visited)?;
            }
        }
        if !complete {
            return Ok(());
        }
        let depth = self.nodes[dag].depth;
        let mut inbound: Vec<TaskId> = Vec::new();
        for dependency in &deps.names {
            if let Some(&node) = self.names.get(&format!("{dag_name}.{dependency}")) {
                for id in self.outbound(node) {
                    if !inbound.contains(&id) {
                        inbound.push(id);
                    }
                }
            }
        }
        if deps.names.is_empty() {
            inbound = self.nodes[dag].inbound.clone();
        }
        let call = Call {
            boundary: Some(dag),
            inbound,
            depth: depth + 1,
        };
        // Whether the task runs, by the results of its dependencies.
        match self.evaluate_depends(dag, &deps) {
            Err(message) => {
                let node = self.end_node(&node_name, Phase::Error, &call, message);
                self.add_child(dag, node);
                return Ok(());
            }
            Ok(false) => {
                let node = self.end_node(
                    &node_name,
                    Phase::Omitted,
                    &call,
                    "omitted: depends condition not met".to_owned(),
                );
                let task_holder = holder(&task.template, task.template_ref.as_ref(), None);
                self.skipped_outputs(node, &task_holder, &context, false);
                self.add_child(dag, node);
                return Ok(());
            }
            Ok(true) => {}
        }
        // The scope of the task: the DAG's inputs and what its ancestors made.
        let mut scope = Scope::of_template(processed);
        let mut ancestors = Vec::new();
        ancestry(name, tasks, &mut ancestors);
        for ancestor in ancestors
            .iter()
            .filter(|ancestor| ancestor.as_str() != name)
        {
            if let Some(&node) = self.names.get(&format!("{dag_name}.{ancestor}")) {
                self.task_scope(&mut scope, ancestor, node);
            }
        }
        let (expanded, group) = match self.expand_task(processed, &task, &scope) {
            Expansion::Ended(phase, message) => {
                let node = self.end_node(&node_name, phase, &call, message);
                self.add_child(dag, node);
                return Ok(());
            }
            Expansion::Empty(task_holder) => {
                let node = self.empty_loop(&node_name, &task_holder, &context, &call);
                self.add_child(dag, node);
                return Ok(());
            }
            Expansion::Single(entry) => (Rc::new(vec![*entry]), None),
            Expansion::Loop(entries) => {
                let group = self.new_node(&node_name, NodeKind::TaskGroup, "", &call);
                self.add_child(dag, group);
                (Rc::new(entries), Some(group))
            }
        };
        for entry in expanded.iter() {
            let child_name = format!("{dag_name}.{}", entry.name);
            let child_call = Call {
                boundary: Some(dag),
                inbound: call.inbound.clone(),
                depth: call.depth,
            };
            let child = if let Some(error) = &entry.error {
                self.end_node(&child_name, Phase::Error, &child_call, error.clone())
            } else {
                match self.decide_when(&entry.when, &child_name) {
                    Err(message) => self.end_node(&child_name, Phase::Error, &child_call, message),
                    Ok(false) => {
                        let child = self.end_node(
                            &child_name,
                            Phase::Skipped,
                            &child_call,
                            format!("when '{}' evaluated false", entry.when),
                        );
                        self.skipped_outputs(child, &entry.holder, &context, false);
                        child
                    }
                    Ok(true) => self.execute_template(
                        &child_name,
                        &entry.holder,
                        &context,
                        &entry.args,
                        &child_call,
                    )?,
                }
            };
            match group {
                Some(group) => self.add_child(group, child),
                None => self.add_child(dag, child),
            }
        }
        let node = group.unwrap_or_else(|| self.names[&node_name]);
        self.nodes[node].expanded = Some(Rc::clone(&expanded));
        if let Some(group) = group {
            self.update_task_group(group);
        }
        self.task_hooks(node, &expanded, &context, &prefix, dag, tasks)?;
        Ok(())
    }

    /// Runs the lifecycle and exit hooks of a task.
    fn task_hooks(
        &mut self,
        node: usize,
        expanded: &Rc<Vec<crate::run::Expanded>>,
        context: &Rc<crate::run::Context>,
        prefix: &str,
        dag: usize,
        tasks: &[DagTask],
    ) -> Result<()> {
        let Some(entry) = expanded.first() else {
            return Ok(());
        };
        if entry.hooks.is_empty() && entry.on_exit.is_empty() {
            return Ok(());
        }
        let processed = self.nodes[dag].processed.clone();
        let mut scope = processed
            .as_ref()
            .map(|processed| Scope::of_template(processed))
            .unwrap_or_default();
        let dag_name = self.nodes[dag].name.clone();
        let mut ancestors = Vec::new();
        ancestry(base_name(&entry.name), tasks, &mut ancestors);
        for ancestor in &ancestors {
            if let Some(&ancestor_node) = self.names.get(&format!("{dag_name}.{ancestor}")) {
                self.task_scope(&mut scope, ancestor, ancestor_node);
            }
        }
        self.lifecycle_hooks(node, &entry.hooks, &scope, prefix, context)?;
        if self.nodes[node].phase.fulfilled() {
            let hook = exit_hook(entry);
            self.exit_hook(node, hook.as_ref(), &scope, prefix, context)?;
        }
        Ok(())
    }

    /// Ends a task group when its tasks ended: failed when one failed and does not continue.
    fn update_task_group(&mut self, group: usize) {
        if self.nodes[group].kind != NodeKind::TaskGroup || self.nodes[group].phase.fulfilled() {
            return;
        }
        let expanded = self.nodes[group].expanded.clone();
        let mut phase = Phase::Succeeded;
        for &child in &self.nodes[group].children {
            let status = &self.nodes[child];
            if !status.phase.fulfilled() {
                return;
            }
            if status.phase.failed_or_error() {
                let continue_on = expanded
                    .as_ref()
                    .and_then(|expanded| expanded.first())
                    .map_or((false, false), |entry| entry.continue_on);
                let continues = match status.phase {
                    Phase::Failed => continue_on.1,
                    _ => continue_on.0,
                };
                if !continues {
                    phase = status.phase;
                }
            }
        }
        self.set_phase(group, phase, String::new());
    }

    /// Whether a task runs, by its `depends` expression over the results of its dependencies.
    fn evaluate_depends(
        &mut self,
        dag: usize,
        deps: &Dependencies,
    ) -> std::result::Result<bool, String> {
        if deps.names.is_empty() {
            return Ok(true);
        }
        let dag_name = self.nodes[dag].name.clone();
        let mut env = BTreeMap::new();
        for name in &deps.names {
            let Some(&node) = self.names.get(&format!("{dag_name}.{name}")) else {
                return Err(format!("dependency {name} has not run"));
            };
            let status = &self.nodes[node];
            let (any_succeeded, all_failed) = if status.kind == NodeKind::TaskGroup {
                let phases: Vec<Phase> = status
                    .children
                    .iter()
                    .map(|&child| self.nodes[child].phase)
                    .collect();
                (
                    phases.contains(&Phase::Succeeded),
                    !phases.is_empty() && phases.iter().all(|phase| *phase == Phase::Failed),
                )
            } else {
                (false, false)
            };
            let results: BTreeMap<String, Value> = [
                ("Succeeded", status.phase == Phase::Succeeded),
                ("Failed", status.phase == Phase::Failed),
                ("Errored", status.phase == Phase::Error),
                ("Skipped", status.phase == Phase::Skipped),
                ("Omitted", status.phase == Phase::Omitted),
                (
                    "Daemoned",
                    status.daemoned && status.phase != Phase::Pending,
                ),
                ("AnySucceeded", any_succeeded),
                ("AllFailed", all_failed),
            ]
            .into_iter()
            .map(|(key, flag)| (key.to_owned(), Value::Bool(flag)))
            .collect();
            env.insert(name.replace('-', "_"), Value::Map(results));
        }
        let logic = deps.logic.replace('-', "_");
        crate::expr::evaluate_bool(&logic, &env)
            .map_err(|error| format!("unable to evaluate expression '{logic}': {error}"))
    }

    /// Substitutes a task with its scope, resolves its artifacts and expands its loop.
    fn expand_task(
        &mut self,
        processed: &Rc<Processed>,
        task: &DagTask,
        scope: &Scope,
    ) -> Expansion {
        let source = expanded_source(processed, &task.name);
        self.note_deciding_outputs(&source, &format!("task `{}`", task.name), scope);
        let mut node = source.clone();
        let variables = self.variables(scope);
        if let Err(message) = replace_node(&mut node, &variables, true) {
            return Expansion::Ended(Phase::Error, message);
        }
        let typed = match decode::<DagTask>(&node) {
            Ok((typed, _)) => typed,
            Err(error) => return Expansion::Ended(Phase::Error, error.to_string()),
        };
        let proceed = should_execute(&typed.when).unwrap_or(true);
        let artifacts = if proceed {
            match self.step_artifacts(&typed.arguments.artifacts, scope) {
                Ok(artifacts) => artifacts,
                Err(message) => return Expansion::Ended(Phase::Error, message),
            }
        } else {
            Vec::new()
        };
        if !loops(&source) {
            return Expansion::Single(Box::new(expanded_task(
                task.name.clone(),
                node,
                &typed,
                artifacts,
            )));
        }
        let mut warnings = Vec::new();
        let list = items(
            &node,
            typed.with_sequence.as_ref(),
            self.options.assume_inputs,
            &mut |warning| warnings.push(warning),
        );
        for warning in warnings {
            self.warn(warning);
        }
        let list = match items_unless_skipped(&typed.when, list) {
            Ok(list) if list.is_empty() => {
                return Expansion::Empty(Box::new(holder(
                    &typed.template,
                    typed.template_ref.as_ref(),
                    node.get("inline"),
                )));
            }
            Ok(list) => list,
            Err(message) => return Expansion::Ended(Phase::Error, message),
        };
        let strict = !matches!(should_execute(&typed.when), Ok(false));
        let mut expanded = Vec::new();
        for (position, item) in list.iter().enumerate() {
            let (item_variables, item_name) = match item_variables(&task.name, position, item) {
                Ok(found) => found,
                Err(message) => return Expansion::Ended(Phase::Error, message),
            };
            let strict = if self.options.assume_inputs && crate::items::unknown_item(item) {
                self.warn(format!(
                    "an item of the loop {item_name} is empty; tags that read its fields stay"
                ));
                false
            } else {
                strict
            };
            let item_variables = variables.clone().with_values(item_variables);
            match substitute_item(&node, &item_variables, strict) {
                Ok(item_node) => match decode::<DagTask>(&item_node) {
                    Ok((mut item_task, _)) => {
                        item_task.template = typed.template.clone();
                        expanded.push(expanded_task(
                            item_name,
                            item_node,
                            &item_task,
                            artifacts.clone(),
                        ));
                    }
                    Err(error) => expanded.push(Expanded::error(item_name, error.to_string())),
                },
                Err(message) => expanded.push(Expanded::error(item_name, message)),
            }
        }
        Expansion::Loop(expanded)
    }
}

/// What a task expands to.
enum Expansion {
    /// The task ended at once, with an error.
    Ended(Phase, String),
    /// A loop over no items.
    Empty(Box<crate::run::Holder>),
    Single(Box<Expanded>),
    Loop(Vec<Expanded>),
}

/// A task and all tasks it depends on, dependencies first.
pub(crate) fn ancestry(name: &str, tasks: &[DagTask], out: &mut Vec<String>) {
    fn visit(name: &str, tasks: &[DagTask], out: &mut Vec<String>, visiting: &mut Vec<String>) {
        if out.iter().any(|known| known == name) || visiting.iter().any(|known| known == name) {
            return;
        }
        visiting.push(name.to_owned());
        if let Some(task) = tasks.iter().find(|task| task.name == name) {
            for dependency in dependencies(task, tasks).names {
                visit(&dependency, tasks, out, visiting);
            }
        }
        visiting.pop();
        out.push(name.to_owned());
    }
    visit(name, tasks, out, &mut Vec::new());
}

/// The task as the processed DAG template holds it.
fn expanded_source(processed: &Processed, name: &str) -> Node {
    let tasks = processed.node.get("dag").and_then(|dag| dag.get("tasks"));
    match tasks.map(|tasks| &tasks.value) {
        Some(Yaml::Sequence(tasks)) => tasks
            .iter()
            .find(|task| task.get("name").and_then(Node::as_str) == Some(name))
            .cloned()
            .unwrap_or_else(|| Node::new(Yaml::Null, argo_parser::Position::default())),
        _ => Node::new(Yaml::Null, argo_parser::Position::default()),
    }
}

fn expanded_task(
    name: String,
    node: Node,
    task: &DagTask,
    artifacts: Vec<crate::node::ArtifactValue>,
) -> Expanded {
    let continue_on = task.continue_on.map_or((false, false), |continue_on| {
        (continue_on.error, continue_on.failed)
    });
    Expanded {
        name,
        holder: holder(
            &task.template,
            task.template_ref.as_ref(),
            node.get("inline"),
        ),
        when: task.when.clone(),
        args: Args {
            parameters: task.arguments.parameters.clone(),
            artifacts,
        },
        continue_on,
        hooks: task.hooks.clone(),
        on_exit: task.on_exit.clone(),
        error: None,
    }
}

#[cfg(test)]
#[path = "dag_tests.rs"]
mod tests;
