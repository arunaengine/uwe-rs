//! Owned run and task events that an embedding host hears while a run goes on.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

pub use executor::LogStream;
use executor::{Live, LiveFeed};
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
    /// New output of a running attempt: whole lines, at most 4 KiB, with the secret values of
    /// the run as `[secret]`. A listener that falls behind misses some.
    Log {
        run: String,
        task: u64,
        attempt: u32,
        stream: LogStream,
        chunk: String,
        time: i64,
    },
    /// What the container of a running attempt uses now: its CPU share, where 100 is one whole
    /// CPU, and its memory in bytes.
    Usage {
        run: String,
        task: u64,
        attempt: u32,
        cpu_percent: Option<f64>,
        memory_bytes: Option<u64>,
        time: i64,
    },
}

/// How many live events wait for a listener before more of them are dropped.
const LIVE_QUEUE: usize = 1024;

/// The live output and resource use of the attempts of the run `run` as events of the
/// listener, on a thread of their own, with the values of `secrets` as `[secret]`.
pub(crate) fn live(
    listener: &Listener,
    run: &str,
    secrets: &config_file::SecretValues,
) -> std::io::Result<LiveFeed> {
    let (listener, run) = (listener.clone(), run.to_owned());
    let values: Vec<String> = secrets
        .names()
        .filter_map(|name| secrets.get(name))
        .collect();
    let feed = LiveFeed::bounded(LIVE_QUEUE, move |task, attempt, live| {
        let time = milliseconds(SystemTime::now());
        let run = run.clone();
        listener(match live {
            Live::Log { stream, chunk } => Event::Log {
                run,
                task,
                attempt,
                stream,
                chunk,
                time,
            },
            Live::Usage {
                cpu_percent,
                memory_bytes,
            } => Event::Usage {
                run,
                task,
                attempt,
                cpu_percent,
                memory_bytes,
                time,
            },
        });
    })?;
    // The sink of each attempt hides the values, also those that the end of a chunk splits.
    Ok(feed.hiding(values))
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
                let cancelled = outcome.error == Some(workflow_graph::TaskError::Cancelled);
                let state = match (retried, outcome.cached) {
                    (true, _) => TaskState::Retried,
                    // The stop of the run ended the task, which did not fail.
                    _ if cancelled => TaskState::Stopped,
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use workflow_graph::Event as Engine;

    use super::*;

    struct Ignored;

    impl Observer for Ignored {
        fn event(&mut self, _: Engine<'_>) {}
    }

    /// Live output reaches the listener without the secret values of the run, with usage.
    #[test]
    fn hides_live_secrets() {
        let (sender, heard) = std::sync::mpsc::channel();
        let sender = Mutex::new(sender);
        let listener: Listener = Arc::new(move |event| {
            let _ = sender.lock().unwrap().send(event);
        });
        let mut secrets = config_file::SecretValues::default();
        secrets.insert("TOKEN", "hunter2").unwrap();
        let feed = live(&listener, "run", &secrets).unwrap();
        let sink = feed.sink(4, 2);
        sink.log(LogStream::Stderr, "token=hunter2\n");
        sink.send(Live::Usage {
            cpu_percent: Some(50.0),
            memory_bytes: Some(1 << 20),
        });
        // The channel closes once the feed, its thread and this listener let go of it.
        drop((feed, sink, listener));
        let events: Vec<Event> = heard.iter().collect();
        let Event::Log {
            task,
            attempt,
            stream,
            chunk,
            ..
        } = &events[0]
        else {
            panic!("a log event first: {events:?}");
        };
        assert_eq!((*task, *attempt, *stream), (4, 2, LogStream::Stderr));
        assert_eq!(chunk, "token=[secret]\n");
        assert!(matches!(
            &events[1],
            Event::Usage {
                cpu_percent: Some(50.0),
                memory_bytes: Some(1048576),
                ..
            }
        ));
    }

    /// A secret that the end of a 4 KiB chunk splits is hidden as a whole.
    #[test]
    fn hides_split_secrets() {
        let (sender, heard) = std::sync::mpsc::channel();
        let sender = Mutex::new(sender);
        let listener: Listener = Arc::new(move |event| {
            let _ = sender.lock().unwrap().send(event);
        });
        let mut secrets = config_file::SecretValues::default();
        secrets.insert("TOKEN", "hunter2").unwrap();
        let feed = live(&listener, "run", &secrets).unwrap();
        let sink = feed.sink(1, 1);
        let filler = "x".repeat(executor::live::CHUNK - 3);
        sink.log(LogStream::Stdout, &format!("{filler}hunter2\n"));
        drop((feed, sink, listener));
        let shown: String = (heard.iter())
            .filter_map(|event| match event {
                Event::Log { chunk, .. } => Some(chunk),
                _ => None,
            })
            .collect();
        assert_eq!(shown, format!("{filler}[secret]\n"));
    }

    /// A task that the stop of a run ended, like with Ctrl-C, is stopped rather than failed.
    #[test]
    fn stops_cancelled_tasks() {
        let states = Arc::new(Mutex::new(Vec::new()));
        let seen = states.clone();
        let listener: Listener = Arc::new(move |event| {
            if let Event::Task(task) = event {
                seen.lock().unwrap().push(task.state);
            }
        });
        let mut ignored = Ignored;
        let mut forward = Forward::new(&listener, "run", &mut ignored);
        let task = workflow_graph::Task {
            id: TaskId(1),
            name: "a".into(),
            step: "a".into(),
            ..workflow_graph::Task::default()
        };
        forward.event(Engine::Requested {
            task: &task,
            edges: &[],
        });
        forward.event(Engine::Started {
            task: TaskId(1),
            attempt: 1,
        });
        let stopped = workflow_graph::Outcome {
            id: TaskId(1),
            exit_code: -1,
            error: Some(workflow_graph::TaskError::Cancelled),
            stderr: "the run was stopped".into(),
            ..workflow_graph::Outcome::default()
        };
        forward.event(Engine::Finished {
            outcome: &stopped,
            attempt: 1,
            retried: false,
        });
        forward.event(Engine::Ended { error: None });
        let states = states.lock().unwrap().clone();
        assert_eq!(states.last(), Some(&TaskState::Stopped), "{states:?}");
        assert!(!states.contains(&TaskState::Failed), "{states:?}");
    }
}
