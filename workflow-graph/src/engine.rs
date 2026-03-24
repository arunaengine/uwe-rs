//! Runs a driver against an executor and records the execution graph.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::{Driver, DriverError, Edge, EdgeKind, ExecutionGraph, Node, Outcome, Task, TaskId};

/// Runs tasks, one call per task. Real executors (TES, local containers) and the mock share it.
pub trait Executor {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error("task {task}: {message}")]
    Executor { task: String, message: String },
    /// The executor cannot run the task yet, like a local executor given a remote input.
    #[error("task {task}: {message}")]
    Unsupported { task: String, message: String },
    #[error("task id {0} was asked for twice")]
    DuplicateTask(TaskId),
}

impl ExecuteError {
    pub fn unsupported(&self) -> bool {
        match self {
            ExecuteError::Driver(error) => error.unsupported,
            ExecuteError::Unsupported { .. } => true,
            _ => false,
        }
    }
}

/// The graph of a run, and why it stopped early if it did.
#[derive(Debug)]
pub struct Execution {
    pub graph: ExecutionGraph,
    pub error: Option<ExecuteError>,
}

impl Execution {
    /// The graph of a run that succeeded, else the error.
    pub fn into_result(self) -> Result<ExecutionGraph, ExecuteError> {
        match self.error {
            None => Ok(self.graph),
            Some(error) => Err(error),
        }
    }
}

/// Runs the driver to its end, one task at a time in the order they were asked for, and
/// returns the graph of what ran. On an error the graph so far is kept, so that a failed run
/// can still be shown.
pub fn execute(workflow: &str, driver: &mut dyn Driver, executor: &mut dyn Executor) -> Execution {
    let mut recorder = Recorder {
        graph: ExecutionGraph {
            language: Some(driver.language()),
            workflow: workflow.to_owned(),
            ..ExecutionGraph::default()
        },
        producers: HashMap::new(),
        seen: HashSet::new(),
    };
    let result = (|| {
        let mut queue: VecDeque<Task> = VecDeque::new();
        for task in driver.start()? {
            recorder.add(&task)?;
            queue.push_back(task);
        }
        while let Some(task) = queue.pop_front() {
            let outcome = executor.execute(&task)?;
            recorder.finished(&outcome);
            for next in driver.complete(outcome)? {
                recorder.add(&next)?;
                queue.push_back(next);
            }
        }
        driver.finish()?;
        Ok::<(), ExecuteError>(())
    })();
    Execution {
        graph: recorder.graph,
        error: result.err(),
    }
}

struct Recorder {
    graph: ExecutionGraph,
    /// The task that made each output file, by its normalized local path.
    producers: HashMap<PathBuf, TaskId>,
    seen: HashSet<TaskId>,
}

impl Recorder {
    fn add(&mut self, task: &Task) -> Result<(), ExecuteError> {
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

    fn finished(&mut self, outcome: &Outcome) {
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
