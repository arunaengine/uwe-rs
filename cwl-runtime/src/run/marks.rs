//! Marks scatters and inputs whose simulated shape depends on real output data.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::PathBuf;

use cwl_parser::model::{Class, Step, short_name};
use workflow_graph::{Marker, MarkerKind, Note, TaskId};

use super::{JobId, Parent, Run, State};
use crate::value::{Map, Value};

/// How many steps without tasks, like expression tools, the search for the tasks behind a
/// scattered list goes back through.
const DEPTH: usize = 8;

impl Run {
    /// Notes what decides the number of jobs of a scatter of `step` in the workflow job
    /// `workflow`: the files in its scattered lists, else the tasks behind the lists.
    pub(super) fn note_scatter(
        &mut self,
        workflow: JobId,
        step: &Step,
        keys: &[String],
        gathered: &Map,
    ) {
        for key in keys {
            let mut files = Vec::new();
            if let Some(value) = gathered.get(key) {
                file_paths(value, &mut files);
            }
            if !files.is_empty() {
                self.notes.extend(files.into_iter().map(Note::Spread));
                continue;
            }
            let Some(input) = (step.inputs.iter()).find(|input| short_name(&input.id) == key)
            else {
                continue;
            };
            let reason = format!(
                "the scatter of step `{}` over `{key}` follows the outputs of this task",
                short_name(&step.id)
            );
            for task in self.source_tasks(workflow, &input.source, DEPTH) {
                let marker = Marker::new(MarkerKind::Content, reason.clone());
                self.notes.push(Note::Mark(task, marker));
            }
        }
    }

    /// The tasks whose outputs the sources of a step input name. A step without tasks of its
    /// own, like an expression tool, passes on the tasks behind its own sources, up to `depth`
    /// steps back. Inputs of the workflow have no tasks.
    fn source_tasks(&self, workflow: JobId, sources: &[String], depth: usize) -> Vec<TaskId> {
        let State::Workflow(run) = &self.jobs[workflow].state else {
            return Vec::new();
        };
        let Class::Workflow(definition) = &run.process.class else {
            return Vec::new();
        };
        let mut tasks = Vec::new();
        for source in sources {
            let Some(index) =
                (definition.steps.iter()).position(|step| step.outputs.contains(source))
            else {
                continue;
            };
            let found = self.step_tasks(workflow, index);
            if found.is_empty() && depth > 0 {
                let upstream: Vec<String> = (definition.steps[index].inputs.iter())
                    .flat_map(|input| input.source.iter().cloned())
                    .collect();
                tasks.extend(self.source_tasks(workflow, &upstream, depth - 1));
            } else {
                tasks.extend(found);
            }
        }
        tasks.sort_unstable();
        tasks.dedup();
        tasks
    }

    /// The tasks of the jobs of a step of a workflow job, also those of scatters and
    /// workflows inside it.
    fn step_tasks(&self, workflow: JobId, step: usize) -> Vec<TaskId> {
        (0..self.jobs.len())
            .filter(|&job| self.below_step(job, workflow, step))
            .filter_map(|job| self.task_of_job.get(&job).copied())
            .collect()
    }

    /// Whether a job belongs to a step of a workflow job, itself or through the jobs above it.
    fn below_step(&self, mut job: JobId, workflow: JobId, step: usize) -> bool {
        loop {
            match self.jobs[job].parent {
                Parent::Step {
                    workflow: parent,
                    step: index,
                } if parent == workflow && index == step => return true,
                Parent::Step {
                    workflow: parent, ..
                } => job = parent,
                Parent::Element { scatter, .. } => job = scatter,
                Parent::Root => return false,
            }
        }
    }
}

/// The local paths of the files and directories in a value.
fn file_paths(value: &Value, paths: &mut Vec<PathBuf>) {
    match value {
        Value::List(items) => {
            for item in items {
                file_paths(item, paths);
            }
        }
        Value::Map(map) if value.is_file_like() => {
            let location = map.get("location").and_then(Value::as_str);
            paths.extend(location.and_then(crate::files::local_path));
        }
        _ => {}
    }
}
