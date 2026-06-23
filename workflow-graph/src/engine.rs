//! Runs a driver against an executor and records the execution graph.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::pending::Pending;
use crate::queue::Queue;
use crate::recorder::Recorder;
use crate::{Driver, DriverError, ExecutionGraph, Outcome, RunOptions, Task, TaskId};

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
    execute_with(workflow, driver, executor, &RunOptions::default())
}

/// Runs the driver to its end like [`execute`], with up to `max_pending` tasks pending at once
/// that finish in the given order.
pub fn execute_with(
    workflow: &str,
    driver: &mut dyn Driver,
    executor: &mut dyn Executor,
    options: &RunOptions,
) -> Execution {
    let mut recorder = Recorder::new(workflow, driver.language());
    let result = run(driver, executor, options, &mut recorder);
    Execution {
        graph: recorder.graph,
        error: result.err(),
    }
}

/// Starts the tasks that can start, up to the limit, then hands one outcome to the driver and
/// queues the tasks it asks for, until no task is left.
fn run(
    driver: &mut dyn Driver,
    executor: &mut dyn Executor,
    options: &RunOptions,
    recorder: &mut Recorder,
) -> Result<(), ExecuteError> {
    let mut queue = Queue::default();
    let mut pending = Pending::new(options.order);
    for task in driver.start()? {
        recorder.add(&task)?;
        queue.push(task);
    }
    loop {
        while pending.len() < options.max_pending.max(1)
            && let Some(task) = queue.next()
        {
            pending.push(executor.execute(&task)?);
        }
        let Some(outcome) = pending.take() else {
            break;
        };
        recorder.finished(&outcome);
        queue.finished(outcome.id);
        for next in driver.complete(outcome)? {
            recorder.add(&next)?;
            queue.push(next);
        }
    }
    driver.finish()?;
    Ok(())
}
