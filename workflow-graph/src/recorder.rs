//! Records the execution graph of a run while it goes on.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::simulator::glob_matches;
use crate::{
    Edge, EdgeKind, ExecuteError, ExecutionGraph, Language, Marker, MarkerKind, Node, Note,
    Outcome, Task, TaskId,
};

/// The graph so far, and what it needs to link later tasks to earlier ones.
pub(crate) struct Recorder {
    pub(crate) graph: ExecutionGraph,
    /// The task that made each output file, and the path the task gave it, by its normalized
    /// local path.
    producers: HashMap<PathBuf, (TaskId, String)>,
    /// The paths of the output files of each finished task.
    outputs: HashMap<TaskId, Vec<String>>,
    seen: HashSet<TaskId>,
    /// The position of each task's node in the graph, so that a run of many tasks stays fast.
    positions: HashMap<TaskId, usize>,
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
            outputs: HashMap::new(),
            seen: HashSet::new(),
            positions: HashMap::new(),
        }
    }

    /// Adds a task that the driver asked for, with an edge from each task that made one of its
    /// input files and from each task in its `after` list. Each task in that list must have
    /// been asked for before, so that no task waits for one that never comes. The result is
    /// the edges into the task.
    ///
    /// A task whose attempt failed may be asked for again, to retry it: its node takes the new
    /// task, and it gets no new edges.
    pub(crate) fn add(&mut self, task: &Task) -> Result<&[Edge], ExecuteError> {
        if self.seen.contains(&task.id) {
            let node = (self.positions.get(&task.id))
                .and_then(|position| self.graph.nodes.get_mut(*position))
                .filter(|node| node.exit_code.is_some_and(|code| code != 0))
                .ok_or(ExecuteError::DuplicateTask(task.id))?;
            node.task = task.clone();
            node.exit_code = None;
            return Ok(&[]);
        }
        if let Some(&after) = task.after.iter().find(|id| !self.seen.contains(id)) {
            return Err(ExecuteError::UnknownAfter {
                task: task.id,
                after,
            });
        }
        self.seen.insert(task.id);
        let mut edges: Vec<Edge> = Vec::new();
        // An input with literal content comes from no task.
        for input in task.inputs.iter().filter(|input| input.content.is_none()) {
            if let Some((from, _)) = self.producer(Path::new(&input.source)) {
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
        let first = self.graph.edges.len();
        self.graph.edges.extend(edges);
        self.positions.insert(task.id, self.graph.nodes.len());
        self.graph.nodes.push(Node {
            task: task.clone(),
            exit_code: None,
            markers: Vec::new(),
        });
        Ok(&self.graph.edges[first..])
    }

    /// The task that made a path, or a directory that holds it, and the path the task gave
    /// the file.
    fn producer(&self, path: &Path) -> Option<(TaskId, String)> {
        let path = normalize(path);
        path.ancestors().find_map(|ancestor| {
            let (task, output) = self.producers.get(ancestor)?;
            let relative = match path.strip_prefix(ancestor) {
                Ok(rest) if !rest.as_os_str().is_empty() => {
                    Path::new(output).join(rest).to_string_lossy().into_owned()
                }
                _ => output.clone(),
            };
            Some((*task, relative))
        })
    }

    /// Turns what the driver noticed while it planned into markers on the tasks that made the
    /// files, once per task, kind and reason. The tasks from position `asked` on are those
    /// that the driver asked for in the same call.
    pub(crate) fn note(&mut self, notes: Vec<Note>, asked: usize) {
        for note in notes {
            let marked = match note {
                Note::Read(path) => self.read(&path),
                Note::Spread(path) => self.spread(&path, asked),
                Note::Mark(task, marker) => Some((task, marker)),
            };
            let Some((task, marker)) = marked else {
                continue;
            };
            if let Some(node) = self
                .positions
                .get(&task)
                .and_then(|position| self.graph.nodes.get_mut(*position))
                && !node.markers.contains(&marker)
            {
                node.markers.push(marker);
            }
        }
    }

    /// The marker of a read of a task output; none for a file that no task made.
    fn read(&self, path: &Path) -> Option<(TaskId, Marker)> {
        let (task, relative) = self.producer(path)?;
        let what = if path.is_dir() {
            format!("listed the folder `{relative}`")
        } else {
            format!("read `{relative}`")
        };
        let reason = format!("the workflow {what}, an output of this task, to plan the next tasks");
        Some((task, Marker::new(MarkerKind::Content, reason)))
    }

    /// The marker of a task output that started tasks of its own, when an output pattern with
    /// a wildcard matched it; a pattern without one names exactly one file. A task asked for in
    /// the same call must read the file, else the spread items went on to be collected, as for
    /// a report of all files, and the number of tasks does not change.
    fn spread(&self, path: &Path, asked: usize) -> Option<(TaskId, Marker)> {
        let file = normalize(path);
        let asked = self.graph.nodes.get(asked..).unwrap_or_default();
        let started = asked.iter().any(|node| {
            node.task.inputs.iter().any(|input| {
                input.content.is_none() && file.starts_with(normalize(Path::new(&input.source)))
            })
        });
        if !started {
            return None;
        }
        let (task, relative) = self.producer(path)?;
        let node = self.graph.nodes.get(*self.positions.get(&task)?)?;
        let pattern = node.task.outputs.iter().find(|pattern| {
            let pattern = bare(pattern);
            pattern.contains(['*', '?', '[', '{'])
                && Path::new(&relative)
                    .ancestors()
                    .any(|path| glob_matches(pattern, &path.to_string_lossy()))
        })?;
        let count = self.outputs.get(&task).map_or(0, |outputs| {
            outputs
                .iter()
                .filter(|output| glob_matches(bare(pattern), output))
                .count()
        });
        let files = if count == 1 { "file" } else { "files" };
        let reason = format!(
            "the output pattern `{pattern}` matched {count} {files}, and each started tasks of \
             its own"
        );
        Some((task, Marker::new(MarkerKind::Wildcard, reason)))
    }

    /// Notes the exit code of a task and the files it made.
    pub(crate) fn finished(&mut self, outcome: &Outcome) {
        if let Some(node) = self
            .positions
            .get(&outcome.id)
            .and_then(|position| self.graph.nodes.get_mut(*position))
        {
            node.exit_code = Some(outcome.exit_code);
        }
        for file in &outcome.files {
            let path = file.path.trim_start_matches('/').to_owned();
            self.producers
                .insert(normalize(&file.location), (outcome.id, path.clone()));
            self.outputs.entry(outcome.id).or_default().push(path);
        }
    }
}

/// An output pattern without a leading `/` or `./` and a trailing `/`, as the paths of output
/// files are.
fn bare(pattern: &str) -> &str {
    pattern
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
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
