//! Records the execution graph of a run while it goes on.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::{Edge, EdgeKind, ExecuteError, ExecutionGraph, Language, Node, Outcome, Task, TaskId};

/// The graph so far, and what it needs to link later tasks to earlier ones.
pub(crate) struct Recorder {
    pub(crate) graph: ExecutionGraph,
    /// The task that made each output file, by its normalized local path.
    producers: HashMap<PathBuf, TaskId>,
    seen: HashSet<TaskId>,
}

impl Recorder {
    pub(crate) fn new(workflow: &str, language: Language) -> Self {
        Recorder {
            graph: ExecutionGraph {
                language: Some(language),
                workflow: workflow.to_owned(),
                ..ExecutionGraph::default()
            },
            producers: HashMap::new(),
            seen: HashSet::new(),
        }
    }

    /// Adds a task that the driver asked for, with an edge from each task that made one of its
    /// input files and from each task in its `after` list.
    pub(crate) fn add(&mut self, task: &Task) -> Result<(), ExecuteError> {
        if !self.seen.insert(task.id) {
            return Err(ExecuteError::DuplicateTask(task.id));
        }
        let mut edges: Vec<Edge> = Vec::new();
        // An input with literal content comes from no task.
        for input in task.inputs.iter().filter(|input| input.content.is_none()) {
            if let Some(from) = self.producer(Path::new(&input.source)) {
                let edge = Edge {
                    from,
                    to: task.id,
                    kind: EdgeKind::File {
                        path: input.path.clone(),
                    },
                };
                if !edges.contains(&edge) {
                    edges.push(edge);
                }
            }
        }
        for &from in &task.after {
            if !edges.iter().any(|edge| edge.from == from) {
                edges.push(Edge {
                    from,
                    to: task.id,
                    kind: EdgeKind::Order,
                });
            }
        }
        self.graph.edges.extend(edges);
        self.graph.nodes.push(Node {
            task: task.clone(),
            exit_code: None,
        });
        Ok(())
    }

    /// The task that made a path, or a directory that holds it.
    fn producer(&self, path: &Path) -> Option<TaskId> {
        let path = normalize(path);
        path.ancestors()
            .find_map(|ancestor| self.producers.get(ancestor).copied())
    }

    /// Notes the exit code of a task and the files it made.
    pub(crate) fn finished(&mut self, outcome: &Outcome) {
        if let Some(node) = self
            .graph
            .nodes
            .iter_mut()
            .find(|node| node.task.id == outcome.id)
        {
            node.exit_code = Some(outcome.exit_code);
        }
        for file in &outcome.files {
            self.producers.insert(normalize(&file.location), outcome.id);
        }
    }
}

/// An absolute path without `.` and `..`, so that the same file is found under both spellings.
fn normalize(path: &Path) -> PathBuf {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}
