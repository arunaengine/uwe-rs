//! Runs a driver against an executor and records the execution graph.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use crate::pending::{End, Pending, Slot, Take};
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
    /// The machine failed, not the task: the container engine, the TES server or the cluster.
    /// The engine runs the task again, a few times.
    #[error("task {task}: {message}")]
    Machine { task: String, message: String },
    /// The run was stopped, like with Ctrl-C.
    #[error("the run was stopped")]
    Cancelled,
}

impl ExecuteError {
    pub fn unsupported(&self) -> bool {
        match self {
            ExecuteError::Driver(error) => error.unsupported,
            ExecuteError::Unsupported { .. } => true,
            _ => false,
        }
    }

    /// Whether the machine failed rather than the task, so that another attempt may work.
    pub fn retryable(&self) -> bool {
        matches!(self, ExecuteError::Machine { .. })
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

/// How often the engine runs a task again after the machine failed, by default.
const MACHINE_RETRIES: u32 = 2;

/// A run of a driver against an executor, with its options and an observer that hears what
/// happens, like the run database.
pub struct Engine<'a> {
    executor: &'a mut dyn Executor,
    options: RunOptions,
    observer: Option<&'a mut dyn Observer>,
    retries: u32,
    /// The number of attempts of each task so far.
    attempts: HashMap<TaskId, u32>,
}

impl<'a> Engine<'a> {
    /// A run with the default options, no observer, and two retries after failures of the
    /// machine.
    pub fn new(executor: &'a mut dyn Executor) -> Self {
        Engine {
            executor,
            options: RunOptions::default(),
            observer: None,
            retries: MACHINE_RETRIES,
            attempts: HashMap::new(),
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

    /// How often a task runs again after the machine failed.
    pub fn retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    /// Runs the driver to its end and returns the graph of what ran, and the error that
    /// stopped the run if there is one. The observer hears the end of every run.
    pub fn run(mut self, workflow: &str, driver: &mut dyn Driver) -> Execution {
        let mut recorder = Recorder::new(workflow, driver.language());
        let mut pending = Pending::new(self.options.order);
        let result = self.drive(driver, &mut recorder, &mut pending);
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
        pending: &mut Pending,
    ) -> Result<(), ExecuteError> {
        let mut queue = Queue::default();
        for task in driver.start()? {
            self.request(task, recorder, &mut queue)?;
        }
        loop {
            while pending.len() < self.options.max_pending.max(1)
                && let Some(task) = queue.next()
            {
                self.start(Arc::new(task), pending);
            }
            let slot = match pending.take() {
                Take::Empty => break,
                // Each attempt ends when it starts.
                Take::Wait => {
                    return Err(ExecuteError::Executor {
                        task: String::new(),
                        message: "an attempt did not end".into(),
                    });
                }
                Take::Ended(slot) => slot,
            };
            let Slot {
                task,
                attempt,
                started,
                end,
            } = slot;
            let Some(End { result, time, .. }) = end else {
                continue;
            };
            let mut outcome = match result {
                Ok(outcome) => outcome,
                Err(error) if error.retryable() && attempt <= self.retries => {
                    let outcome = failed(&task, &error, attempt, started, time);
                    self.report(Event::Finished {
                        outcome: &outcome,
                        attempt,
                        retried: true,
                    });
                    self.start(task, pending);
                    continue;
                }
                Err(error) => return Err(error),
            };
            outcome.id = task.id;
            outcome.attempt = attempt;
            outcome.started.get_or_insert(started);
            outcome.ended.get_or_insert(time);
            recorder.finished(&outcome);
            self.report(Event::Finished {
                outcome: &outcome,
                attempt,
                retried: false,
            });
            let id = outcome.id;
            let next = driver.complete(outcome)?;
            // A driver that asks for the task again retries it; the tasks after it wait.
            let again = next.iter().any(|task| task.id == id);
            for task in next {
                self.request(task, recorder, &mut queue)?;
            }
            if !again {
                queue.finished(id);
            }
        }
        driver.finish()?;
        Ok(())
    }

    /// Runs the next attempt of a task.
    fn start(&mut self, task: Arc<Task>, pending: &mut Pending) {
        let number = self.attempts.entry(task.id).or_insert(0);
        *number += 1;
        let number = *number;
        self.report(Event::Started {
            task: task.id,
            attempt: number,
        });
        pending.start(task.clone(), number);
        let result = self.executor.execute(&task);
        pending.end(task.id, number, result);
    }
}

/// The outcome of an attempt that failed without one, for the observer: exit code -1 and the
/// error as its standard error.
fn failed(
    task: &Task,
    error: &ExecuteError,
    attempt: u32,
    started: SystemTime,
    ended: SystemTime,
) -> Outcome {
    Outcome {
        id: task.id,
        attempt,
        exit_code: -1,
        stderr: error.to_string(),
        started: Some(started),
        ended: Some(ended),
        ..Outcome::default()
    }
}
