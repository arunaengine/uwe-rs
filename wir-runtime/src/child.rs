//! Runs the driver of another language as a child of a parent driver, under task ids of the
//! parent.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use input_schema::Severity;
use serde_json::Value as Json;
use workflow_graph::{
    CancellationToken, Delivery, Driver, DriverError, ExecuteError, Files, Note, Outcome, Task,
    TaskId,
};

/// A language driver that a `native` op runs, with the result values it gives back.
pub trait Block: Driver {
    /// The result object of the block once it finished, by output name, as wir values, or
    /// why its results cannot be read.
    fn exported(&self) -> Result<Option<Json>, DriverError>;
}

/// The driver of a native block inside a parent driver. The parent hands it every call of
/// the engine that concerns the block's tasks; the block keeps its own acceptance rules.
pub struct Child {
    driver: Box<dyn Block>,
    /// The parent id of each task the child asked for, for the whole run: a task that the
    /// child asks for again, to retry it, keeps its parent id, and `after` lists stay valid.
    parents: HashMap<TaskId, TaskId>,
    /// The child id of each parent id.
    children: HashMap<TaskId, TaskId>,
    /// Parent ids of the tasks that were asked for and have not ended.
    running: HashSet<TaskId>,
    /// The attempts that the child accepted, by parent id.
    accepted: HashSet<(TaskId, u32)>,
    /// Deliveries handed to the parent and not acknowledged yet.
    delivering: Vec<Delivery>,
    /// Deliveries taken from the child that the parent has not asked for yet.
    held: Vec<Delivery>,
    /// The target of every delivery the child gave, in order.
    targets: Vec<String>,
    finished: bool,
}

impl Child {
    pub fn new(driver: Box<dyn Block>) -> Child {
        Child {
            driver,
            parents: HashMap::new(),
            children: HashMap::new(),
            running: HashSet::new(),
            accepted: HashSet::new(),
            delivering: Vec::new(),
            held: Vec::new(),
            targets: Vec::new(),
            finished: false,
        }
    }

    /// Hands the child the files and the cancellation of the run, and checks its inputs as
    /// uwe checks those of a workflow before any task starts.
    pub fn prepare(
        &mut self,
        files: Option<Arc<dyn Files>>,
        cancel: Option<CancellationToken>,
        simulated: bool,
    ) -> Result<(), DriverError> {
        if let Some(files) = files {
            self.driver.use_files(files);
        }
        if let Some(cancel) = cancel {
            self.driver.use_cancel(cancel);
        }
        let Some(inputs) = self.driver.inputs()? else {
            return Ok(());
        };
        let validation = input_schema::validate_json(&inputs, simulated);
        let mut errors = Vec::new();
        for diagnostic in validation.diagnostics {
            match diagnostic.severity {
                Severity::Error => errors.push(diagnostic.message),
                Severity::Warning => tracing::warn!("uwe: {}", diagnostic.message),
            }
        }
        if !errors.is_empty() {
            return Err(DriverError::new(errors.join("; ")));
        }
        self.driver.use_inputs(validation.values)
    }

    /// Starts the child; its first tasks, numbered by `next`.
    pub fn start(&mut self, next: &mut dyn FnMut() -> TaskId) -> Result<Vec<Task>, DriverError> {
        let tasks = self.driver.start()?;
        self.number(tasks, next)
    }

    /// Hands the outcome of one of its tasks to the child. Its acceptance is kept before
    /// anything else can fail, so a later error of the parent does not lose it.
    pub fn complete(
        &mut self,
        mut outcome: Outcome,
        next: &mut dyn FnMut() -> TaskId,
    ) -> Result<Vec<Task>, DriverError> {
        let parent = outcome.id;
        let Some(&child) = self.children.get(&parent) else {
            return Err(DriverError::new(format!(
                "task {parent} is not a task of the block"
            )));
        };
        self.running.remove(&parent);
        outcome.id = child;
        let attempt = outcome.attempt;
        let tasks = self.driver.complete(outcome);
        if self.driver.take_acceptance(child, attempt) {
            self.accepted.insert((parent, attempt));
        }
        self.number(tasks?, next)
    }

    pub fn wake_at(&self) -> Option<Instant> {
        if self.finished {
            return None;
        }
        self.driver.wake_at()
    }

    pub fn wake(&mut self, next: &mut dyn FnMut() -> TaskId) -> Result<Vec<Task>, DriverError> {
        let tasks = self.driver.wake()?;
        self.number(tasks, next)
    }

    /// The tasks whose attempts the child wants stopped, by parent id.
    pub fn cancellations(&mut self) -> Vec<TaskId> {
        let tasks = self.driver.cancellations();
        tasks
            .iter()
            .filter_map(|id| self.parents.get(id).copied())
            .collect()
    }

    pub fn handles_error(&self, task: TaskId, error: &ExecuteError) -> bool {
        let child = self.children.get(&task);
        child.is_some_and(|child| self.driver.handles_error(*child, error))
    }

    pub fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.accepted.remove(&(task, attempt))
    }

    /// What the child noticed, with the tasks that notes name under their parent ids.
    pub fn notes(&mut self) -> Vec<Note> {
        let notes = self.driver.notes();
        (notes.into_iter())
            .filter_map(|note| match note {
                Note::Mark(task, marker) => {
                    (self.parents.get(&task)).map(|id| Note::Mark(*id, marker))
                }
                other => Some(other),
            })
            .collect()
    }

    /// The child's deliveries since the last call. The child owns them until the parent
    /// acknowledges them with [`Child::delivered`].
    pub fn deliveries(&mut self) -> Vec<Delivery> {
        self.collect();
        std::mem::take(&mut self.held)
    }

    /// The target of every delivery the child gave, in order, like the files that a
    /// Nextflow block publishes.
    pub fn targets(&self) -> &[String] {
        &self.targets
    }

    /// Acknowledges a delivery to the child that gave it; `None` when it is not the child's.
    pub fn delivered(&mut self, delivery: &Delivery) -> Option<Result<(), DriverError>> {
        let index = self.delivering.iter().position(|item| item == delivery)?;
        self.delivering.remove(index);
        Some(self.driver.delivered(delivery))
    }

    /// Whether the task with this parent id belongs to the child.
    pub fn owns(&self, task: TaskId) -> bool {
        self.children.contains_key(&task)
    }

    /// Whether the child has started work left: a task that has not ended, or a timer.
    pub fn busy(&self) -> bool {
        !self.finished && (!self.running.is_empty() || self.driver.wake_at().is_some())
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Ends the child, once: its result values, or why it failed.
    pub fn finish(&mut self) -> Result<Option<Json>, DriverError> {
        if self.finished {
            return Err(DriverError::new("the block has already finished"));
        }
        self.finished = true;
        self.driver.finish()?;
        // The deliveries of the last task are not handed out yet; its results may name them.
        self.collect();
        self.driver.exported()
    }

    /// Ends a child that did not finish when the run failed.
    pub fn failed(&mut self, error: &ExecuteError) -> Result<(), DriverError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.driver.failed(error)
    }

    pub fn draining(&self) -> bool {
        !self.finished && self.driver.draining()
    }

    /// Takes the new deliveries of the child, and keeps them until the parent acknowledges
    /// them.
    fn collect(&mut self) {
        let deliveries = self.driver.deliveries();
        self.targets
            .extend(deliveries.iter().map(|item| item.target.clone()));
        self.delivering.extend(deliveries.iter().cloned());
        self.held.extend(deliveries);
    }

    /// Gives the tasks of the child parent ids: the same one again for a task it asked for
    /// before, a new one from `next` otherwise.
    fn number(
        &mut self,
        tasks: Vec<Task>,
        next: &mut dyn FnMut() -> TaskId,
    ) -> Result<Vec<Task>, DriverError> {
        let mut numbered = Vec::with_capacity(tasks.len());
        for mut task in tasks {
            let parent = match self.parents.get(&task.id) {
                Some(parent) => *parent,
                None => {
                    let parent = next();
                    self.parents.insert(task.id, parent);
                    self.children.insert(parent, task.id);
                    parent
                }
            };
            for after in &mut task.after {
                let Some(earlier) = self.parents.get(after) else {
                    let message = format!(
                        "task {} of the block runs after task {after}, which it did not ask for",
                        task.id
                    );
                    return Err(DriverError::new(message));
                };
                *after = *earlier;
            }
            task.id = parent;
            self.running.insert(parent);
            numbered.push(task);
        }
        Ok(numbered)
    }
}
