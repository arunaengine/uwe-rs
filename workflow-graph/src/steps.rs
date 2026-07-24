//! The graph with one node per step (process, rule, step or template), made from the graph with
//! one node per task: a step node counts its tasks, and a step edge stands for the task edges
//! between two steps.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{EdgeKind, ExecutionGraph, Language, Marker, TaskId};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StepGraph {
    pub language: Option<Language>,
    /// The workflow file or directory.
    pub workflow: String,
    /// Set for the graph of a simulated run, as in [`ExecutionGraph::simulation`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulation: Option<String>,
    /// In the order of their first task.
    pub steps: Vec<StepNode>,
    pub edges: Vec<StepEdge>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepNode {
    /// The process, rule, step or template.
    pub name: String,
    /// How many tasks of the step the run asked for.
    pub tasks: usize,
    /// How many of them ended with exit code 0.
    pub succeeded: usize,
    /// How many of them ended with another exit code; the rest did not end.
    pub failed: usize,
    /// How many of those that ended with exit code 0 an earlier result answered, from the
    /// cache store or from an earlier run.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reused: usize,
    /// How many of them have markers.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub marked: usize,
    /// The markers of its tasks, one per kind, each with the reason of the first task that
    /// has it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepEdge {
    pub from: String,
    pub to: String,
    /// How many edges between tasks of the two steps it stands for.
    pub tasks: usize,
    /// Whether a task of `to` reads a file that a task of `from` made; otherwise the tasks of
    /// `to` only wait for those of `from`.
    pub files: bool,
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

impl ExecutionGraph {
    /// The graph with one node per step. Edges between tasks of the same step are left out.
    pub fn step_graph(&self) -> StepGraph {
        let mut steps: Vec<StepNode> = Vec::new();
        let mut index: HashMap<&str, usize> = HashMap::new();
        let mut step_of: HashMap<TaskId, usize> = HashMap::new();
        for node in &self.nodes {
            let position = *index.entry(node.task.step.as_str()).or_insert_with(|| {
                steps.push(StepNode {
                    name: node.task.step.clone(),
                    ..StepNode::default()
                });
                steps.len() - 1
            });
            step_of.insert(node.task.id, position);
            let step = &mut steps[position];
            step.tasks += 1;
            match node.exit_code {
                Some(0) => step.succeeded += 1,
                Some(_) => step.failed += 1,
                None => {}
            }
            if node.reused && node.exit_code == Some(0) {
                step.reused += 1;
            }
            if !node.markers.is_empty() {
                step.marked += 1;
            }
            for marker in &node.markers {
                if !step.markers.iter().any(|known| known.kind == marker.kind) {
                    step.markers.push(marker.clone());
                }
            }
        }
        let mut edges: Vec<StepEdge> = Vec::new();
        let mut edge_index: HashMap<(usize, usize), usize> = HashMap::new();
        for edge in &self.edges {
            let (Some(&from), Some(&to)) = (step_of.get(&edge.from), step_of.get(&edge.to)) else {
                continue;
            };
            if from == to {
                continue;
            }
            let files = matches!(edge.kind, EdgeKind::File { .. });
            let position = *edge_index.entry((from, to)).or_insert_with(|| {
                edges.push(StepEdge {
                    from: steps[from].name.clone(),
                    to: steps[to].name.clone(),
                    tasks: 0,
                    files: false,
                });
                edges.len() - 1
            });
            edges[position].tasks += 1;
            edges[position].files |= files;
        }
        StepGraph {
            language: self.language,
            workflow: self.workflow.clone(),
            simulation: self.simulation.clone(),
            steps,
            edges,
        }
    }
}

impl StepGraph {
    pub fn is_simulated(&self) -> bool {
        self.simulation.is_some()
    }

    /// The sentence of [`ExecutionGraph::cache_note`] for the tasks of the steps.
    pub fn cache_note(&self) -> Option<String> {
        let reused = self.steps.iter().map(|step| step.reused).sum();
        let tasks = self.steps.iter().map(|step| step.tasks).sum();
        crate::graph::cache_note(reused, tasks, self.is_simulated())
    }
}
