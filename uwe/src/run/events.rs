//! Owned run and task events that an embedding host hears while a run goes on.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use serde::Serialize;
use workflow_graph::{Observer, TaskId};

/// Hears the events of a run on uwe's worker thread, while the engine waits for it. It must
/// return fast and must not block, so send the event on, for example into an unbounded channel.
pub type Listener = Arc<dyn Fn(Event) + Send + Sync>;

/// Takes what the workflow of a run prints, with the secret values of the run as `[secret]`.
/// uwe calls it on its worker thread, so it must return fast, like a [`Listener`].
#[derive(Clone)]
pub struct PrintSink(Arc<dyn Fn(&str) + Send + Sync>);

impl PrintSink {
    pub fn new(sink: impl Fn(&str) + Send + Sync + 'static) -> PrintSink {
        PrintSink(Arc::new(sink))
    }

    pub(crate) fn print(&self, text: &str) {
        (self.0)(text);
    }
}

impl std::fmt::Debug for PrintSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PrintSink")
    }
}

/// Two sinks are equal when they share their function.
impl PartialEq for PrintSink {
    fn eq(&self, other: &PrintSink) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// One event of a run. Times are milliseconds since the Unix epoch, as in the run database.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// The run is in the run database, and its tasks start next.
    Started { run: String, time: i64 },
    /// A task changed its state.
    Task(TaskEvent),
    /// The run ended, after uwe recorded all of it. `error` says why it failed or stopped.
    Ended {
        run: String,
        time: i64,
        error: Option<String>,
    },
}

/// The new state of one task.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct TaskEvent {
    pub run: String,
    /// The task's number in the run, as in the run database.
    pub task: u64,
    pub name: String,
    /// The process, rule, step or template the task is an instance of.
    pub step: String,
    pub state: TaskState,
    /// The attempt, counting from 1; `None` before the first attempt starts.
    pub attempt: Option<u32>,
    pub time: i64,
    pub started: Option<i64>,
    pub ended: Option<i64>,
    /// Whether an earlier result answered the task, so that it did not run.
    pub cached: bool,
    pub exit_code: Option<i32>,
    /// Why the executor could not run the attempt, or why the run stopped the task.
    pub error: Option<String>,
}

/// Where a task is. `Finished`, `Reused` and `Failed` can follow each other for one attempt,
/// as the workflow's own output checks accept or reject it after it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskState {
    Requested,
    Started,
    /// The machine failed and uwe runs the task again.
    Retried,
    Finished,
    Reused,
    Failed,
    /// The run ended before the task did.
    Stopped,
}

/// Turns the engine's events into owned events for the listener, then passes them on.
pub(crate) struct Forward<'a> {
    listener: &'a Listener,
    run: &'a str,
    inner: &'a mut dyn Observer,
    tasks: HashMap<TaskId, TaskEvent>,
}

impl<'a> Forward<'a> {
    pub(crate) fn new(listener: &'a Listener, run: &'a str, inner: &'a mut dyn Observer) -> Self {
        Self {
            listener,
            run,
            inner,
            tasks: HashMap::new(),
        }
    }

    fn change(&mut self, task: TaskId, state: TaskState, update: impl FnOnce(&mut TaskEvent)) {
        let Some(last) = self.tasks.get_mut(&task) else {
            return;
        };
        let before = last.state;
        last.state = state;
        last.time = milliseconds(SystemTime::now());
        update(last);
        if before != state {
            (self.listener)(Event::Task(last.clone()));
        }
    }
}

impl Observer for Forward<'_> {
    fn event(&mut self, event: workflow_graph::Event<'_>) {
        use workflow_graph::Event as Engine;
        match &event {
            Engine::Requested { task, .. } => {
                let requested = TaskEvent {
                    run: self.run.to_owned(),
                    task: task.id.0,
                    name: task.name.clone(),
                    step: task.step.clone(),
                    state: TaskState::Requested,
                    attempt: None,
                    time: milliseconds(SystemTime::now()),
                    started: None,
                    ended: None,
                    cached: false,
                    exit_code: None,
                    error: None,
                };
                (self.listener)(Event::Task(requested.clone()));
                self.tasks.insert(task.id, requested);
            }
            Engine::Started { task, attempt } => {
                self.change(*task, TaskState::Started, |last| {
                    last.attempt = Some(*attempt);
                    last.started = Some(last.time);
                    (last.ended, last.exit_code, last.error) = (None, None, None);
                    last.cached = false;
                });
            }
            Engine::Finished {
                outcome,
                attempt,
                retried,
            } => {
                let state = match (retried, outcome.cached) {
                    (true, _) => TaskState::Retried,
                    _ if outcome.exit_code != 0 || outcome.error.is_some() => TaskState::Failed,
                    (false, true) => TaskState::Reused,
                    (false, false) => TaskState::Finished,
                };
                self.change(outcome.id, state, |last| {
                    last.attempt = Some(*attempt);
                    last.started = outcome.started.map(milliseconds);
                    last.ended = outcome.ended.map(milliseconds);
                    last.cached = outcome.cached;
                    last.exit_code = Some(outcome.exit_code);
                    // The engine puts the executor's message in place of the program's output.
                    last.error =
                        (outcome.error.is_some()).then(|| outcome.stderr.trim().to_owned());
                });
            }
            Engine::Validated { task, accepted, .. } => {
                let state = match self.tasks.get(task) {
                    Some(_) if !accepted => TaskState::Failed,
                    Some(last) if last.cached => TaskState::Reused,
                    Some(_) => TaskState::Finished,
                    None => return self.inner.event(event),
                };
                self.change(*task, state, |_| {});
            }
            Engine::Ended { error } => {
                let error = error.map(ToString::to_string);
                let open: Vec<TaskId> = (self.tasks.iter())
                    .filter(|(_, last)| {
                        matches!(
                            last.state,
                            TaskState::Requested | TaskState::Started | TaskState::Retried
                        )
                    })
                    .map(|(id, _)| *id)
                    .collect();
                for task in open {
                    self.change(task, TaskState::Stopped, |last| {
                        last.error.clone_from(&error);
                    });
                }
            }
            Engine::Marked { .. } | Engine::Delivered { .. } => {}
        }
        self.inner.event(event);
    }
}

/// The run-level end, with the reason a failed or stopped run gives.
pub(crate) fn ended(run: &str, error: Option<&str>) -> Event {
    Event::Ended {
        run: run.to_owned(),
        time: milliseconds(SystemTime::now()),
        error: error.map(str::to_owned),
    }
}

/// Milliseconds since the Unix epoch, the unit of times in the run database.
pub(crate) fn milliseconds(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}
