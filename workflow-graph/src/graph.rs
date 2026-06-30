//! The execution graph of a run: the tasks, and the files and orderings between them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

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

impl ExecutionGraph {
    /// Notes that the graph comes from a simulated run, in the words of [`SIMULATION`].
    pub fn simulate(&mut self) {
        self.simulation = Some(SIMULATION.to_owned());
    }

    pub fn is_simulated(&self) -> bool {
        self.simulation.is_some()
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

    /// The graph in Graphviz DOT form, one node per task, labelled with its name.
    pub fn to_dot(&self) -> String {
        let mut dot = String::from("digraph workflow {\n  rankdir=TB;\n  node [shape=box];\n");
        for node in &self.nodes {
            let _ = writeln!(
                dot,
                "  t{} [label=\"{}\"];",
                node.task.id,
                escape(&node.task.name)
            );
        }
        for edge in &self.edges {
            match &edge.kind {
                EdgeKind::File { path } => {
                    let _ = writeln!(
                        dot,
                        "  t{} -> t{} [label=\"{}\"];",
                        edge.from,
                        edge.to,
                        escape(path)
                    );
                }
                EdgeKind::Order => {
                    let _ = writeln!(dot, "  t{} -> t{} [style=dashed];", edge.from, edge.to);
                }
            }
        }
        dot.push_str("}\n");
        dot
    }

    /// A short text summary: counts, then one line per task with its upstream tasks.
    pub fn summary(&self) -> String {
        let mut text = format!(
            "{} tasks, {} steps, {} edges\n",
            self.nodes.len(),
            self.steps().len(),
            self.edges.len()
        );
        for node in &self.nodes {
            let mut upstream: Vec<String> = self
                .edges
                .iter()
                .filter(|edge| edge.to == node.task.id)
                .map(|edge| edge.from.to_string())
                .collect();
            upstream.dedup();
            let _ = writeln!(
                text,
                "{:>5} {} <- [{}]",
                node.task.id,
                node.task.name,
                upstream.join(", ")
            );
        }
        text
    }
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}
