//! The execution graph of a run: the tasks, and the files and orderings between them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{Language, Marker, SIMULATION, Task, TaskId};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExecutionGraph {
    pub language: Option<Language>,
    /// The workflow file or directory.
    pub workflow: String,
    /// Set for the graph of a simulated run: says in plain words that no task ran, that the
    /// outputs are made-up files, and that a run with real data refines the graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulation: Option<String>,
    /// In the order the driver asked for them.
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub task: Task,
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
    /// Whether an earlier result answered the task, from the cache store or from an earlier
    /// run, so that it did not run.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reused: bool,
    /// The points where the tasks that follow depend on the outputs of this task, so that a
    /// run with real data can have more or fewer tasks after it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: TaskId,
    pub to: TaskId,
    pub kind: EdgeKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// `to` reads a file that `from` made; the path is the one `to` sees.
    File { path: String },
    /// `to` waits for `from` without reading its files.
    Order,
}

fn is_false(flag: &bool) -> bool {
    !flag
}

/// The sentence of [`ExecutionGraph::cache_note`] for `reused` of `total` tasks.
pub(crate) fn cache_note(reused: usize, total: usize, simulated: bool) -> Option<String> {
    let tasks = |number: usize| match number {
        1 => "1 task".to_owned(),
        _ => format!("{number} tasks"),
    };
    let left = total.saturating_sub(reused);
    match (reused, simulated) {
        (0, _) => None,
        (_, true) if left == 0 => Some(format!(
            "A real run would take all {} from the cache; none would run.",
            tasks(total)
        )),
        (_, true) => Some(format!(
            "A real run would take {reused} of {} from the cache; {left} would run.",
            tasks(total)
        )),
        (_, false) => Some(format!("{reused} of {} came from the cache.", tasks(total))),
    }
}

impl ExecutionGraph {
    /// Notes that the graph comes from a simulated run, in the words of [`SIMULATION`].
    pub fn simulate(&mut self) {
        self.simulation = Some(SIMULATION.to_owned());
    }

    pub fn is_simulated(&self) -> bool {
        self.simulation.is_some()
    }

    /// How many tasks an earlier result answered, from the cache store or from an earlier run.
    pub fn reused(&self) -> usize {
        self.nodes.iter().filter(|node| node.reused).count()
    }

    /// One sentence about the tasks that an earlier result answered, when any did. For a
    /// simulated graph it tells how much of the work a real run would take from the cache, as a
    /// preview before any compute is spent.
    pub fn cache_note(&self) -> Option<String> {
        cache_note(self.reused(), self.nodes.len(), self.is_simulated())
    }

    /// The tasks with markers.
    pub fn marked(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(|node| !node.markers.is_empty())
    }

    pub fn node(&self, id: TaskId) -> Option<&Node> {
        self.nodes.iter().find(|node| node.task.id == id)
    }

    /// The distinct steps (processes, rules, steps or templates) that ran, in first-use order.
    pub fn steps(&self) -> Vec<&str> {
        let mut seen: HashSet<&str> = HashSet::new();
        self.nodes
            .iter()
            .map(|node| node.task.step.as_str())
            .filter(|step| seen.insert(step))
            .collect()
    }

    /// Checks that every edge joins known tasks and that the graph has no cycle.
    pub fn check(&self) -> Result<(), String> {
        let positions: HashMap<TaskId, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(position, node)| (node.task.id, position))
            .collect();
        for edge in &self.edges {
            let (Some(&from), Some(&to)) = (positions.get(&edge.from), positions.get(&edge.to))
            else {
                return Err(format!(
                    "the edge {} -> {} names an unknown task",
                    edge.from, edge.to
                ));
            };
            // Tasks are recorded when they are asked for, so a producer always comes first.
            if from >= to {
                return Err(format!(
                    "the edge {} -> {} goes backwards",
                    edge.from, edge.to
                ));
            }
        }
        Ok(())
    }
}
