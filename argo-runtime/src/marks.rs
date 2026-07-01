//! The markers of an Argo run: a `withParam`, a `withSequence` or a `when` that reads the
//! outputs of earlier steps or tasks decides how many tasks follow from those outputs, so a run
//! with real outputs can start more or fewer tasks there than a simulated run.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::Node;
use workflow_graph::{Marker, MarkerKind, Note, TaskId};

use crate::run::Run;
use crate::scope::Scope;

/// The fields of a step or DAG task that decide whether it runs, and how many times.
const DECIDING: [&str; 3] = ["withParam", "withSequence", "when"];

impl Run {
    /// Notes the tasks whose outputs the loop or the condition of a step or DAG task reads,
    /// like `{{steps.gen.outputs.result}}` in a `withParam`. `source` is the step or task
    /// before substitution, and `what` names it, like "step `fan`".
    pub(crate) fn note_deciding_outputs(&mut self, source: &Node, what: &str, scope: &Scope) {
        for field in DECIDING {
            let Some(value) = source.get(field) else {
                continue;
            };
            let text = match value.as_str() {
                Some(text) => text.to_owned(),
                None => crate::pod::node_json(value),
            };
            for (prefix, nodes) in &scope.nodes {
                if !text.contains(&format!("{prefix}.outputs")) {
                    continue;
                }
                let reason = format!("the `{field}` of {what} reads the outputs of this task");
                for task in self.tasks_under(nodes) {
                    let marker = Marker::new(MarkerKind::Content, reason.clone());
                    self.notes.push(Note::Mark(task, marker));
                }
            }
        }
    }

    /// The tasks of nodes and of the nodes below them.
    fn tasks_under(&self, nodes: &[usize]) -> Vec<TaskId> {
        let mut tasks = Vec::new();
        let mut open: Vec<usize> = nodes.to_vec();
        while let Some(index) = open.pop() {
            let Some(node) = self.nodes.get(index) else {
                continue;
            };
            tasks.extend(&node.tasks);
            open.extend(&node.children);
        }
        tasks
    }
}
