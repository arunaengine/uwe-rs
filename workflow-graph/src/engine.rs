//! Runs a driver against an executor and records the execution graph.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::attempts::Attempts;
use crate::executor::{Attempt, CancellationToken, Executor};
use crate::pending::{End, Pending, Slot, Take};
use crate::queue::Queue;
use crate::recorder::Recorder;
use crate::{
    Driver, DriverError, Event, ExecutionGraph, Files, Note, Observer, Outcome, RunOptions, Task,
    TaskId,
};

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
    /// A result could not be delivered to where the language expects it.
    #[error("cannot deliver {location} to {target}: {message}")]
    Delivery {
        location: String,
        target: String,
        message: String,
    },
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
    pub failure_hook_error: Option<DriverError>,
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

/// How long the engine waits for the attempts in flight to stop when a run stops.
const STOP_GRACE: Duration = Duration::from_secs(60);

/// A run of a driver against an executor, with its options and an observer that hears what
/// happens, like the run database.
pub struct Engine<'a> {
    executor: &'a mut dyn Executor,
    options: RunOptions,
    observer: Option<&'a mut dyn Observer>,
    cancel: CancellationToken,
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
            cancel: CancellationToken::new(),
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

    /// Stops the run when the token fires, like on Ctrl-C: the attempts in flight stop, and the
    /// run ends with [`ExecuteError::Cancelled`].
    pub fn cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// How often a task runs again after the machine failed.
    pub fn retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    /// Runs the driver to its end and returns the graph of what ran, and the error that
    /// stopped the run if there is one. On an error, the attempts in flight stop and the engine
    /// waits for them a while. The observer hears the end of every run.
    pub fn run(mut self, workflow: &str, driver: &mut dyn Driver) -> Execution {
        let mut recorder = Recorder::new(workflow, driver.language());
        let result = Attempts::new(self.cancel.clone()).and_then(|mut attempts| {
            let mut pending = Pending::new(self.options.order);
            let result = self.drive(driver, &mut recorder, &mut pending, &mut attempts);
            if result.is_err() {
                self.stop(&mut recorder, &mut pending, &mut attempts);
            }
            result
        });
        let error = result.err();
        let failure_hook_error = error.as_ref().and_then(|error| driver.failed(error).err());
        self.report(Event::Ended {
            error: error.as_ref(),
        });
        Execution {
            graph: recorder.graph,
            error,
            failure_hook_error,
        }
    }

    fn report(&mut self, event: Event<'_>) {
        if let Some(observer) = self.observer.as_deref_mut() {
            observer.event(event);
        }
    }

    fn check_cancel(&self) -> Result<(), ExecuteError> {
        if self.cancel.is_cancelled() {
            Err(ExecuteError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Records a task that the driver asked for and queues it.
    fn request(
        &mut self,
        task: Task,
        recorder: &mut Recorder,
        queue: &mut Queue,
        completed: Option<TaskId>,
    ) -> Result<(), ExecuteError> {
        let edges = recorder.add(&task, completed)?;
        self.report(Event::Requested { task: &task, edges });
        queue.push(task);
        Ok(())
    }

    /// Records and queues the tasks that one call of the driver asked for, then the notes of
    /// that call, which may name those tasks. The notes count also when the call failed, as
    /// the failure may come from what the driver read.
    fn asked(
        &mut self,
        asked: Result<Vec<Task>, DriverError>,
        notes: Vec<Note>,
        recorder: &mut Recorder,
        queue: &mut Queue,
        completed: Option<TaskId>,
    ) -> Result<(), ExecuteError> {
        let first = recorder.graph.nodes.len();
        let result = asked.map_err(ExecuteError::from).and_then(|tasks| {
            (tasks.into_iter()).try_for_each(|task| self.request(task, recorder, queue, completed))
        });
        for (task, marker) in recorder.note(notes, first) {
            self.report(Event::Marked {
                task,
                marker: &marker,
            });
        }
        result
    }

    /// Starts the tasks that can start, up to the limit, then hands one outcome to the driver
    /// and queues the tasks it asks for, until no task is left.
    fn drive(
        &mut self,
        driver: &mut dyn Driver,
        recorder: &mut Recorder,
        pending: &mut Pending,
        attempts: &mut Attempts,
    ) -> Result<(), ExecuteError> {
        self.check_cancel()?;
        driver.use_cancel(self.cancel.clone());
        let mut queue = Queue::default();
        let files = self.executor.files();
        if let Some(files) = &files {
            driver.use_files(files.clone());
        }
        let first = driver.start();
        let notes = driver.notes();
        self.asked(first, notes, recorder, &mut queue, None)?;
        self.deliver(driver, files.as_deref())?;
        let mut draining = false;
        loop {
            self.check_cancel()?;
            // After a failure that ends the run once the running tasks have ended, no task
            // starts: neither one of the queue nor one that waits in the executor.
            if !draining && driver.draining() {
                draining = true;
                queue.clear();
                self.executor.hold();
            }
            while !draining
                && pending.len() < self.options.max_pending.max(1)
                && let Some(task) = queue.next()
            {
                self.start(Arc::new(task), pending, attempts)?;
            }
            self.check_cancel()?;
            let slot = match pending.take() {
                Take::Empty => break,
                Take::Wait => {
                    let ended = attempts.next()?;
                    pending.end(ended.id, ended.attempt, ended.result);
                    continue;
                }
                Take::Ended(slot) => slot,
            };
            let Slot {
                task,
                attempt,
                started,
                end,
            } = *slot;
            let Some(End { result, time, .. }) = end else {
                continue;
            };
            let mut outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    let outcome = failed(&task, &error, attempt, started, time);
                    let retried = !draining && error.retryable() && attempt <= self.retries;
                    recorder.finished(&outcome);
                    self.report(Event::Finished {
                        outcome: &outcome,
                        attempt,
                        retried,
                    });
                    if retried {
                        self.start(task, pending, attempts)?;
                        continue;
                    }
                    if draining && (error.retryable() || matches!(error, ExecuteError::Cancelled)) {
                        continue;
                    }
                    return Err(error);
                }
            };
            outcome.id = task.id;
            outcome.attempt = attempt;
            outcome.accepted = None;
            outcome.started.get_or_insert(started);
            outcome.ended.get_or_insert(time);
            recorder.finished(&outcome);
            self.report(Event::Finished {
                outcome: &outcome,
                attempt,
                retried: false,
            });
            let id = outcome.id;
            let succeeded = outcome.exit_code == 0;
            let next = driver.complete(outcome);
            let accepted = driver.take_acceptance(id, attempt) && succeeded;
            recorder.validated(id, accepted);
            self.report(Event::Validated {
                task: id,
                attempt,
                accepted,
            });
            let settlement = self.executor.settle(id, attempt, accepted);
            crate::runtime()?.block_on(async {
                tokio::select! {
                    biased;
                    () = self.cancel.cancelled() => Err(ExecuteError::Cancelled),
                    result = settlement => result,
                }
            })?;
            let notes = driver.notes();
            // A driver that asks for the task again retries it; the tasks after it wait.
            let again = (next.as_ref()).is_ok_and(|next| next.iter().any(|task| task.id == id));
            self.asked(next, notes, recorder, &mut queue, Some(id))?;
            if !again {
                queue.finished(id);
            }
            self.deliver(driver, files.as_deref())?;
        }
        driver.finish()?;
        self.deliver(driver, files.as_deref())
    }

    /// Delivers the files that the driver asks for, when the executor has real files.
    fn deliver(
        &mut self,
        driver: &mut dyn Driver,
        files: Option<&dyn Files>,
    ) -> Result<(), ExecuteError> {
        self.check_cancel()?;
        for delivery in driver.deliveries() {
            self.check_cancel()?;
            self.report(Event::Delivered {
                delivery: &delivery,
            });
            if let Some(files) = files {
                files
                    .deliver(&delivery)
                    .map_err(|error| ExecuteError::Delivery {
                        location: delivery.location.clone(),
                        target: delivery.target.clone(),
                        message: error.to_string(),
                    })?;
                driver.delivered(&delivery)?;
            }
        }
        self.check_cancel()
    }

    /// Starts the next attempt of a task.
    fn start(
        &mut self,
        task: Arc<Task>,
        pending: &mut Pending,
        attempts: &mut Attempts,
    ) -> Result<(), ExecuteError> {
        self.check_cancel()?;
        let number = self.attempts.entry(task.id).or_insert(0);
        *number += 1;
        let number = *number;
        self.report(Event::Started {
            task: task.id,
            attempt: number,
        });
        pending.start(task.clone(), number);
        let attempt = Attempt {
            task: task.clone(),
            number,
            cancel: attempts.cancel.clone(),
        };
        if let Some(ended) = attempts.start(task.id, number, self.executor.run(attempt)) {
            pending.end(ended.id, ended.attempt, ended.result);
        }
        Ok(())
    }

    /// Stops the attempts in flight and waits a while for them to end. The observer hears how
    /// each one ended; the driver does not.
    fn stop(&mut self, recorder: &mut Recorder, pending: &mut Pending, attempts: &mut Attempts) {
        attempts.cancel.cancel();
        let deadline = Instant::now() + STOP_GRACE;
        while pending.running()
            && let Some(ended) = attempts.next_until(deadline)
        {
            pending.end(ended.id, ended.attempt, ended.result);
        }
        pending.cancel_remaining();
        while let Take::Ended(slot) = pending.take_any() {
            let Some(end) = &slot.end else { continue };
            let mut outcome = match &end.result {
                Ok(outcome) => outcome.clone(),
                Err(error) => failed(&slot.task, error, slot.attempt, slot.started, end.time),
            };
            outcome.id = slot.task.id;
            outcome.attempt = slot.attempt;
            outcome.accepted = None;
            outcome.started.get_or_insert(slot.started);
            outcome.ended.get_or_insert(end.time);
            recorder.finished(&outcome);
            self.report(Event::Finished {
                outcome: &outcome,
                attempt: slot.attempt,
                retried: false,
            });
        }
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
