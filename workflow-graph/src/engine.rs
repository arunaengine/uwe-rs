//! Runs a driver against an executor and records the execution graph.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::SystemTime;

use crate::pending::Pending;
use crate::queue::Queue;
use crate::recorder::Recorder;
use crate::{
    Driver, DriverError, Event, ExecutionGraph, Observer, Outcome, RunOptions, Task, TaskId,
};

/// Runs tasks, one call per task. Real executors (TES, local containers) and the simulator
/// share it.
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
    /// A task is to run after a task that the driver did not ask for before it.
    #[error("task {task} is to run after task {after}, which the driver did not ask for before it")]
    UnknownAfter { task: TaskId, after: TaskId },
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
/// returns the graph of what ran. A task starts only when the tasks in its `after` list have
/// finished. On an error the graph so far is kept, so that a failed run can still be shown.
pub fn execute(workflow: &str, driver: &mut dyn Driver, executor: &mut dyn Executor) -> Execution {
    Engine::new(executor).run(workflow, driver)
}

/// Runs the driver to its end like [`execute`], with up to `max_pending` tasks pending at once
/// that finish in the given order.
pub fn execute_with(
    workflow: &str,
    driver: &mut dyn Driver,
    executor: &mut dyn Executor,
    options: &RunOptions,
) -> Execution {
    Engine::new(executor)
        .options(*options)
        .run(workflow, driver)
}

/// A run of a driver against an executor, with its options and an observer that hears what
/// happens, like the run database.
pub struct Engine<'a> {
    executor: &'a mut dyn Executor,
    options: RunOptions,
    observer: Option<&'a mut dyn Observer>,
}

impl<'a> Engine<'a> {
    /// A run with the default options and no observer.
    pub fn new(executor: &'a mut dyn Executor) -> Self {
        Engine {
            executor,
            options: RunOptions::default(),
            observer: None,
        }
    }

    pub fn options(mut self, options: RunOptions) -> Self {
        self.options = options;
        self
    }

    pub fn observer(mut self, observer: &'a mut dyn Observer) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Runs the driver to its end and returns the graph of what ran, and the error that
    /// stopped the run if there is one. The observer hears the end of every run.
    pub fn run(mut self, workflow: &str, driver: &mut dyn Driver) -> Execution {
        let mut recorder = Recorder::new(workflow, driver.language());
        let result = self.drive(driver, &mut recorder);
        let error = result.err();
        self.report(Event::Ended {
            error: error.as_ref(),
        });
        Execution {
            graph: recorder.graph,
            error,
        }
    }

    fn report(&mut self, event: Event<'_>) {
        if let Some(observer) = self.observer.as_deref_mut() {
            observer.event(event);
        }
    }

    /// Records a task that the driver asked for and queues it.
    fn request(
        &mut self,
        task: Task,
        recorder: &mut Recorder,
        queue: &mut Queue,
    ) -> Result<(), ExecuteError> {
        let edges = recorder.add(&task)?;
        self.report(Event::Requested { task: &task, edges });
        queue.push(task);
        Ok(())
    }

    /// Starts the tasks that can start, up to the limit, then hands one outcome to the driver
    /// and queues the tasks it asks for, until no task is left.
    fn drive(
        &mut self,
        driver: &mut dyn Driver,
        recorder: &mut Recorder,
    ) -> Result<(), ExecuteError> {
        let mut queue = Queue::default();
        let mut pending = Pending::new(self.options.order);
        for task in driver.start()? {
            self.request(task, recorder, &mut queue)?;
        }
        loop {
            while pending.len() < self.options.max_pending.max(1)
                && let Some(task) = queue.next()
            {
                self.report(Event::Started {
                    task: task.id,
                    attempt: 1,
                });
                let started = SystemTime::now();
                let mut outcome = self.executor.execute(&task)?;
                outcome.attempt = 1;
                outcome.started.get_or_insert(started);
                outcome.ended.get_or_insert_with(SystemTime::now);
                pending.push(outcome);
            }
            let Some(outcome) = pending.take() else {
                break;
            };
            recorder.finished(&outcome);
            self.report(Event::Finished {
                outcome: &outcome,
                attempt: 1,
                retried: false,
            });
            queue.finished(outcome.id);
            for next in driver.complete(outcome)? {
                self.request(next, recorder, &mut queue)?;
            }
        }
        driver.finish()?;
        Ok(())
    }
}
