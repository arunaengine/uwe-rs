//! DAG templates (workflow/controller/dag.go and workflow/common/ancestry.go): tasks that run
//! when their `depends` expression, or their `dependencies`, allow it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::sync::OnceLock;

use argo_parser::model::DagTask;
use argo_parser::{Node, Value as Yaml, decode};
use regex::Regex;
use workflow_graph::TaskId;

use crate::items::{item_variables, items, loops, substitute_item};
use crate::run::{Args, Call, Expanded, NodeKind, Phase, Processed, Result, Run};
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
        match self.assess_dag(index, &tasks, &targets, dag.fail_fast != Some(false)) {
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

    /// The phase of a DAG, as Argo's `assessDAGPhase` finds it: running while any of its
    /// nodes, hooks included, runs; then by the branch phases of its target tasks. A task that
    /// completed gives its branch its own phase; a skipped or omitted task inherits the phase
    /// of its dependencies, a failure over success. A target that failed without continuing
    /// fails the DAG; under fail-fast the first such target decides at once.
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
}
